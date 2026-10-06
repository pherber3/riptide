use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use fastframe_audio::{Buffer, BufferSize, Output, OutputOptions, Render};

use crate::decode::{Decoder, Resampler, map_channels};
use crate::tidal::Quality;

const RING: usize = 192_000 * 2;

/// Commands for the audio thread. It never waits on the network: decoders arrive already opened,
/// and a seek into a track still downloading is held until the download is done.
pub enum Cmd {
    Load(Box<Decoder>),
    Toggle,
    Seek(f64),
    Stop,
}

pub enum Event {
    Ended,
    Error(String),
}

/// What the UI reads without talking to the audio thread.
pub struct Status {
    pub playing: AtomicBool,
    pub volume: AtomicU32,
    played: AtomicU64,
    samples_per_second: AtomicU64,
    /// A seek waiting for the download to finish, as f64 bits (NaN when none).
    pending_seek: AtomicU64,
    /// The tier and format actually playing, which can be lower than the one asked for.
    pub format: Mutex<(Quality, String)>,
}

impl Status {
    pub fn position(&self) -> f64 {
        let pending = f64::from_bits(self.pending_seek.load(Relaxed));
        if !pending.is_nan() {
            return pending;
        }
        self.played.load(Relaxed) as f64 / self.samples_per_second.load(Relaxed).max(1) as f64
    }

    pub fn set_volume(&self, volume: f32) {
        self.volume.store(volume.to_bits(), Relaxed);
    }

    fn set_pending_seek(&self, seconds: Option<f64>) {
        self.pending_seek.store(seconds.unwrap_or(f64::NAN).to_bits(), Relaxed);
    }
}

pub struct Player {
    tx: mpsc::Sender<Cmd>,
    pub status: Arc<Status>,
}

impl Player {
    pub fn start(events: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Status {
            playing: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            played: AtomicU64::new(0),
            samples_per_second: AtomicU64::new(1),
            pending_seek: AtomicU64::new(f64::NAN.to_bits()),
            format: Mutex::new((Quality::Max, String::new())),
        });
        let shared = status.clone();
        std::thread::spawn(move || {
            if let Err(e) = run(rx, shared, &events) {
                events(Event::Error(format!("audio output: {e:#}")));
            }
        });
        Self { tx, status }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }
}

struct Sink {
    rx: Arc<Mutex<rtrb::Consumer<f32>>>,
    status: Arc<Status>,
}

impl Render for Sink {
    fn configure(&mut self, _sample_rate: u32, _channels: u16) {}

    fn render(&mut self, out: &mut [f32]) {
        let Ok(mut rx) = self.rx.try_lock() else {
            return out.fill(0.0);
        };
        let volume = f32::from_bits(self.status.volume.load(Relaxed));
        let mut played = 0;
        for s in out {
            *s = rx.pop().map_or(0.0, |x| {
                played += 1;
                x * volume
            });
        }
        self.status.played.fetch_add(played, Relaxed);
    }
}

struct Track {
    decoder: Box<Decoder>,
    resampler: Resampler,
    tx: rtrb::Producer<f32>,
    packet: Vec<f32>,
    mapped: Vec<f32>,
    ready: Vec<f32>,
    sent: usize,
    ended: bool,
    pending_seek: Option<f64>,
}

fn run(rx: mpsc::Receiver<Cmd>, status: Arc<Status>, events: &impl Fn(Event)) -> Result<()> {
    let consumer = Arc::new(Mutex::new(rtrb::RingBuffer::new(1).1));
    let options = OutputOptions {
        buffer: Buffer::FixedOnWindows(BufferSize::Duration(Duration::from_millis(100))),
        ..Default::default()
    };
    let mut output = Output::open(options, Sink { rx: consumer.clone(), status: status.clone() })?;
    output.pause();
    let (rate, channels) = (output.sample_rate(), usize::from(output.channels()));
    status.samples_per_second.store(u64::from(rate) * channels as u64, Relaxed);
    // A fresh ring for each track or seek drops whatever was queued for the old position.
    let ring = |seconds: f64| {
        let (tx, rx) = rtrb::RingBuffer::new(RING);
        *consumer.lock().unwrap() = rx;
        status.played.store((seconds * f64::from(rate)) as u64 * channels as u64, Relaxed);
        tx
    };
    let seek = |t: &mut Track, seconds: f64| -> Result<()> {
        if let Err(e) = t.decoder.seek(seconds) {
            events(Event::Error(format!("seek failed: {e:#}")));
        }
        t.resampler = Resampler::new(t.decoder.info.sample_rate, rate, channels)?;
        (t.tx, t.sent, t.ended, t.pending_seek) = (ring(seconds), 0, false, None);
        t.ready.clear();
        status.set_pending_seek(None);
        Ok(())
    };
    let mut track: Option<Track> = None;
    loop {
        let playing = track.is_some() && status.playing.load(Relaxed);
        let waiting = track.as_ref().is_some_and(|t| t.pending_seek.is_some());
        let timeout = if playing || waiting { 10 } else { 1000 };
        match rx.recv_timeout(Duration::from_millis(timeout)) {
            Ok(Cmd::Load(decoder)) => {
                let i = &decoder.info;
                let bits = i.bits.map_or(String::new(), |b| format!("{b}-bit "));
                let tier = match (i.codec, i.bits, i.sample_rate) {
                    ("flac", Some(16), ..=48_000) => Quality::High,
                    ("flac", ..) => Quality::Max,
                    _ => Quality::Low,
                };
                let format = format!("{} {bits}{:.1} kHz", i.codec.to_uppercase(), i.sample_rate as f32 / 1000.0);
                *status.format.lock().unwrap() = (tier, format);
                let resampler = Resampler::new(i.sample_rate, rate, channels)?;
                let empty = Vec::new;
                track = Some(Track {
                    decoder,
                    resampler,
                    tx: ring(0.0),
                    packet: empty(),
                    mapped: empty(),
                    ready: empty(),
                    sent: 0,
                    ended: false,
                    pending_seek: None,
                });
                status.set_pending_seek(None);
                status.playing.store(true, Relaxed);
                output.resume();
            }
            Ok(Cmd::Toggle) if track.is_some() => {
                let play = !status.playing.load(Relaxed);
                status.playing.store(play, Relaxed);
                if play { output.resume() } else { output.pause() }
            }
            Ok(Cmd::Seek(seconds)) => {
                if let Some(t) = &mut track {
                    if t.decoder.can_seek() {
                        seek(t, seconds)?;
                    } else {
                        t.pending_seek = Some(seconds);
                        status.set_pending_seek(Some(seconds));
                    }
                }
            }
            Ok(Cmd::Stop) => {
                track = None;
                status.playing.store(false, Relaxed);
                status.set_pending_seek(None);
                ring(0.0);
                output.pause();
            }
            Ok(Cmd::Toggle) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        output.maintain();
        if let Some(t) = &mut track
            && let Some(seconds) = t.pending_seek
            && t.decoder.can_seek()
        {
            seek(t, seconds)?;
        }
        let Some(t) = track.as_mut().filter(|_| status.playing.load(Relaxed)) else { continue };
        if let Err(e) = fill(t, channels) {
            events(Event::Error(format!("playback stopped: {e:#}")));
            t.ended = true;
        }
        if t.ended && t.sent == t.ready.len() && t.tx.slots() == RING {
            track = None;
            status.playing.store(false, Relaxed);
            output.pause();
            events(Event::Ended);
        }
    }
}

/// Decodes until the ring is full or the track ends.
fn fill(t: &mut Track, channels: usize) -> Result<()> {
    loop {
        while t.sent < t.ready.len() {
            if t.tx.push(t.ready[t.sent]).is_err() {
                return Ok(());
            }
            t.sent += 1;
        }
        if t.ended {
            return Ok(());
        }
        t.sent = 0;
        t.ready.clear();
        t.mapped.clear();
        if t.decoder.next(&mut t.packet)? {
            map_channels(&t.packet, t.decoder.info.channels, channels, &mut t.mapped);
            t.resampler.push(&t.mapped, &mut t.ready)?;
        } else {
            t.resampler.flush(&mut t.ready)?;
            t.ended = true;
        }
    }
}
