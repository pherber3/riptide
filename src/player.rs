use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::sleep;
use std::time::Duration;

use anyhow::Result;
use fastframe_audio::{Buffer, BufferSize, Output, OutputOptions, Render};

use crate::cache::Reader;
use crate::decode::{Decoder, Resampler, map_channels};

const RING: usize = 192_000 * 2;

pub struct Sink {
    rx: rtrb::Consumer<f32>,
    volume: Arc<AtomicU32>,
}

impl Render for Sink {
    fn configure(&mut self, _sample_rate: u32, _channels: u16) {}

    fn render(&mut self, out: &mut [f32]) {
        let volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
        for s in out {
            *s = self.rx.pop().map_or(0.0, |x| x * volume);
        }
    }
}

/// Plays one track to the default output, blocking until it ends.
pub fn play(reader: Reader) -> Result<()> {
    let mut decoder = Decoder::open(reader)?;
    let (mut tx, rx) = rtrb::RingBuffer::new(RING);
    let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
    let options = OutputOptions {
        buffer: Buffer::FixedOnWindows(BufferSize::Duration(Duration::from_millis(100))),
        ..Default::default()
    };
    let mut output = Output::open(options, Sink { rx, volume })?;
    let (rate, channels) = (output.sample_rate(), usize::from(output.channels()));
    let i = &decoder.info;
    println!(
        "{} {}-bit {} Hz, {} -> {} ({rate} Hz)",
        i.codec,
        i.bits.map_or("?".into(), |b| b.to_string()),
        i.sample_rate,
        i.duration.map_or("?".into(), |d| format!("{}:{:02}", d.as_secs() / 60, d.as_secs() % 60)),
        output.device_name()
    );
    let mut resampler = Resampler::new(i.sample_rate, rate, channels)?;
    let clock = output.clock();
    let (mut packet, mut mapped, mut ready) = (Vec::new(), Vec::new(), Vec::new());
    loop {
        let more = decoder.next(&mut packet)?;
        mapped.clear();
        ready.clear();
        if more {
            map_channels(&packet, decoder.info.channels, channels, &mut mapped);
            resampler.push(&mapped, &mut ready)?;
        } else {
            resampler.flush(&mut ready)?;
        }
        let mut sent = 0;
        while sent < ready.len() {
            match tx.push(ready[sent]) {
                Ok(()) => sent += 1,
                Err(_) => {
                    output.maintain();
                    let t = clock.played().as_secs();
                    print!("\r{}:{:02}", t / 60, t % 60);
                    let _ = std::io::stdout().flush();
                    sleep(Duration::from_millis(20));
                }
            }
        }
        if !more {
            break;
        }
    }
    while tx.slots() < RING {
        output.maintain();
        sleep(Duration::from_millis(50));
    }
    println!();
    Ok(())
}
