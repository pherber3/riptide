use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use fastframe_audio::{Buffer, BufferSize, Device, Maintained, Output, OutputOptions, Render};

use crate::decode::{Decoder, Resampler, map_channels};
use crate::tidal::Quality;

const RING: usize = 192_000 * 2;

/// Commands for the audio thread. It never waits on the network: decoders arrive already opened,
/// and a seek into a track still downloading is held until the download is done.
pub enum Cmd {
    /// A track to play from its start, playing or paused.
    Load(Box<Decoder>, bool),
    Toggle,
    Seek(f64),
    Stop,
    /// Play through this device (by name), or the system default.
    Device(Option<String>),
}

pub enum Event {
    Ended,
    Error(String),
}

/// What the UI reads without talking to the audio thread.
pub struct Status {
    pub playing: AtomicBool,
    pub volume: AtomicU32,
    /// The playing track's normalization scale, as f32 bits.
    gain: AtomicU32,
    /// How loud the music last sent to the output was (RMS, before the volume), as f32 bits.
    level: AtomicU32,
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

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Relaxed))
    }

    pub fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Relaxed);
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
    pub fn start(device: Option<String>, events: impl Fn(Event) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::channel();
        let status = Arc::new(Status {
            playing: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            gain: AtomicU32::new(1.0f32.to_bits()),
            level: AtomicU32::new(0),
            played: AtomicU64::new(0),
            samples_per_second: AtomicU64::new(1),
            pending_seek: AtomicU64::new(f64::NAN.to_bits()),
            format: Mutex::new((Quality::Max, String::new())),
        });
        let shared = status.clone();
        std::thread::spawn(move || {
            if let Err(e) = run(device, rx, shared, &events) {
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
        let gain = f32::from_bits(self.status.gain.load(Relaxed));
        let volume = f32::from_bits(self.status.volume.load(Relaxed)) * gain;
        let n = rx.slots().min(out.len());
        if let Ok(chunk) = rx.read_chunk(n) {
            let (a, b) = chunk.as_slices();
            let mut energy = 0.0;
            for (o, s) in out.iter_mut().zip(a.iter().chain(b)) {
                *o = s * volume;
                energy += s * s;
            }
            chunk.commit_all();
            let level = if n > 0 { (energy / n as f32).sqrt() * gain } else { 0.0 };
            self.status.level.store(level.to_bits(), Relaxed);
        }
        out[n..].fill(0.0);
        self.status.played.fetch_add(n as u64, Relaxed);
    }
}

struct Track {
    decoder: Box<Decoder>,
    resampler: Resampler,
    tx: rtrb::Producer<f32>,
    mapped: Vec<f32>,
    ready: Vec<f32>,
    sent: usize,
    ended: bool,
    pending_seek: Option<f64>,
}

fn run(device: Option<String>, rx: mpsc::Receiver<Cmd>, status: Arc<Status>, events: &impl Fn(Event)) -> Result<()> {
    let consumer = Arc::new(Mutex::new(rtrb::RingBuffer::new(1).1));
    let open = |device: Option<String>| {
        let options = OutputOptions {
            device: device.map_or(Device::Default, Device::Named),
            buffer: Buffer::FixedOnWindows(BufferSize::Duration(Duration::from_millis(100))),
            ..Default::default()
        };
        Output::open(options, Sink { rx: consumer.clone(), status: status.clone() })
    };
    let mut output = open(device)?;
    output.pause();
    // The output's rate and channels, which change when it moves to another device.
    let mut format = (output.sample_rate(), usize::from(output.channels()));
    status.samples_per_second.store(u64::from(format.0) * format.1 as u64, Relaxed);
    // A fresh ring for each track or seek drops whatever was queued for the old position.
    let ring = |seconds: f64, (rate, channels): (u32, usize)| {
        let (tx, rx) = rtrb::RingBuffer::new(RING);
        *consumer.lock().unwrap() = rx;
        status.played.store((seconds * f64::from(rate)) as u64 * channels as u64, Relaxed);
        tx
    };
    let seek = |t: &mut Track, seconds: f64, format: (u32, usize)| -> Result<()> {
        if let Err(e) = t.decoder.seek(seconds) {
            events(Event::Error(format!("seek failed: {e:#}")));
        }
        t.resampler = Resampler::new(t.decoder.info.sample_rate, format.0, format.1)?;
        (t.tx, t.sent, t.ended, t.pending_seek) = (ring(seconds, format), 0, false, None);
        t.ready.clear();
        status.set_pending_seek(None);
        Ok(())
    };
    // After the output moves to another rate or channel count, the track carries on from where it
    // was heard.
    let reformat = |track: &mut Option<Track>, at: f64, format: (u32, usize)| -> Result<()> {
        status.samples_per_second.store(u64::from(format.0) * format.1 as u64, Relaxed);
        let Some(t) = track else { return Ok(()) };
        if t.decoder.can_seek() {
            return seek(t, at, format);
        }
        t.resampler = Resampler::new(t.decoder.info.sample_rate, format.0, format.1)?;
        (t.tx, t.sent) = (ring(at, format), 0);
        t.ready.clear();
        Ok(())
    };
    let mut track: Option<Track> = None;
    loop {
        // Wake when about half the buffered audio has played (commands still wake at once).
        let per_second = status.samples_per_second.load(Relaxed).max(1);
        let timeout = match &track {
            Some(t) if t.pending_seek.is_some() => 10,
            Some(t) if status.playing.load(Relaxed) => ((RING - t.tx.slots()) as u64 * 500 / per_second).clamp(10, 250),
            _ => 1000,
        };
        match rx.recv_timeout(Duration::from_millis(timeout)) {
            Ok(Cmd::Load(decoder, play)) => {
                let i = &decoder.info;
                let bits = i.bits.map_or(String::new(), |b| format!("{b}-bit "));
                let tier = match (i.codec, i.bits, i.sample_rate) {
                    ("flac", Some(16), ..=48_000) => Quality::High,
                    ("flac", ..) => Quality::Max,
                    _ => Quality::Low,
                };
                let label = format!("{} {bits}{:.1} kHz", i.codec.to_uppercase(), i.sample_rate as f32 / 1000.0);
                *status.format.lock().unwrap() = (tier, label);
                let resampler = Resampler::new(i.sample_rate, format.0, format.1)?;
                let empty = Vec::new;
                track = Some(Track {
                    decoder,
                    resampler,
                    tx: ring(0.0, format),
                    mapped: empty(),
                    ready: empty(),
                    sent: 0,
                    ended: false,
                    pending_seek: None,
                });
                status.set_pending_seek(None);
                status.playing.store(play, Relaxed);
                if play { output.resume() } else { output.pause() }
            }
            Ok(Cmd::Toggle) if track.is_some() => {
                let play = !status.playing.load(Relaxed);
                status.playing.store(play, Relaxed);
                if play { output.resume() } else { output.pause() }
            }
            Ok(Cmd::Seek(seconds)) => {
                if let Some(t) = &mut track {
                    if t.decoder.can_seek() {
                        seek(t, seconds, format)?;
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
                ring(0.0, format);
                output.pause();
            }
            Ok(Cmd::Device(device)) => {
                let at = status.position();
                output = open(device)?;
                if !status.playing.load(Relaxed) {
                    output.pause();
                }
                format = (output.sample_rate(), usize::from(output.channels()));
                reformat(&mut track, at, format)?;
            }
            Ok(Cmd::Toggle) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if let Maintained::Reopened { sample_rate, channels, .. } = output.maintain()
            && (sample_rate, usize::from(channels)) != format
        {
            format = (sample_rate, usize::from(channels));
            reformat(&mut track, status.position(), format)?;
        }
        if let Some(t) = &mut track
            && let Some(seconds) = t.pending_seek
            && t.decoder.can_seek()
        {
            seek(t, seconds, format)?;
        }
        let Some(t) = track.as_mut().filter(|_| status.playing.load(Relaxed)) else { continue };
        if let Err(e) = fill(t, format.1) {
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
        let n = t.tx.slots().min(t.ready.len() - t.sent);
        if n > 0 {
            let chunk = t.tx.write_chunk_uninit(n).expect("free slots were counted");
            chunk.fill_from_iter(t.ready[t.sent..].iter().copied());
            t.sent += n;
        }
        if t.sent < t.ready.len() || t.ended {
            return Ok(());
        }
        t.sent = 0;
        t.ready.clear();
        let from = t.decoder.info.channels;
        match t.decoder.next()? {
            Some(samples) if from == channels => t.resampler.push(samples, &mut t.ready)?,
            Some(samples) => {
                t.mapped.clear();
                map_channels(samples, from, channels, &mut t.mapped);
                t.resampler.push(&t.mapped, &mut t.ready)?;
            }
            None => {
                t.resampler.flush(&mut t.ready)?;
                t.ended = true;
            }
        }
    }
}
