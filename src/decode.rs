use std::io;

use anyhow::{Context, Result};
use rubato::{FftFixedIn, Resampler as _};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use crate::cache::Reader;

pub struct Info {
    pub sample_rate: u32,
    pub channels: usize,
    pub bits: Option<u32>,
    pub codec: &'static str,
}

pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track: u32,
    download: crate::cache::Download,
    pub info: Info,
}

impl Decoder {
    pub fn open(reader: Reader) -> Result<Self> {
        let download = reader.download();
        let mss = MediaSourceStream::new(Box::new(reader), Default::default());
        let format = symphonia::default::get_probe()
            .format(&Hint::new(), mss, &FormatOptions::default(), &MetadataOptions::default())?
            .format;
        let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL).context("no audio track")?;
        let p = &track.codec_params;
        let info = Info {
            sample_rate: p.sample_rate.context("unknown sample rate")?,
            channels: p.channels.map_or(2, |c| c.count()),
            bits: p.bits_per_sample,
            codec: symphonia::default::get_codecs().get_codec(p.codec).map_or("?", |c| c.short_name),
        };
        let decoder = symphonia::default::get_codecs().make(p, &DecoderOptions::default())?;
        let track = track.id;
        Ok(Self { format, decoder, track, download, info })
    }

    /// Seeking needs the stream's full length, which is known once the download is done.
    pub fn can_seek(&self) -> bool {
        self.download.done()
    }

    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        let to = SeekTo::Time { time: Time::from(seconds), track_id: Some(self.track) };
        self.format.seek(SeekMode::Coarse, to)?;
        self.decoder.reset();
        Ok(())
    }

    /// Decodes the next packet into `out` as interleaved samples; false at the end.
    pub fn next(&mut self, out: &mut Vec<f32>) -> Result<bool> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(Error::IoError(e)) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
                Err(e) => return Err(e.into()),
            };
            if packet.track_id() != self.track {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(audio) => {
                    let mut buf = SampleBuffer::<f32>::new(audio.capacity() as u64, *audio.spec());
                    buf.copy_interleaved_ref(audio);
                    out.clear();
                    out.extend_from_slice(buf.samples());
                    return Ok(true);
                }
                Err(Error::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
}

pub fn map_channels(input: &[f32], from: usize, to: usize, out: &mut Vec<f32>) {
    for frame in input.chunks(from) {
        out.extend((0..to).map(|c| frame[c.min(from - 1)]));
    }
}

pub struct Resampler {
    inner: Option<FftFixedIn<f32>>,
    pending: Vec<Vec<f32>>,
}

impl Resampler {
    pub fn new(from: u32, to: u32, channels: usize) -> Result<Self> {
        let inner = (from != to).then(|| FftFixedIn::new(from as usize, to as usize, 1024, 2, channels)).transpose()?;
        Ok(Self { inner, pending: vec![Vec::new(); channels] })
    }

    pub fn push(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        let Some(r) = &mut self.inner else {
            out.extend_from_slice(input);
            return Ok(());
        };
        let channels = self.pending.len();
        for frame in input.chunks(channels) {
            for (c, s) in frame.iter().enumerate() {
                self.pending[c].push(*s);
            }
        }
        while self.pending[0].len() >= r.input_frames_next() {
            let n = r.input_frames_next();
            let chunk: Vec<Vec<f32>> = self.pending.iter_mut().map(|p| p.drain(..n).collect()).collect();
            interleave(&r.process(&chunk, None)?, out);
        }
        Ok(())
    }

    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<()> {
        if let Some(r) = &mut self.inner {
            let chunk: Vec<Vec<f32>> = self.pending.iter_mut().map(std::mem::take).collect();
            interleave(&r.process_partial(Some(&chunk), None)?, out);
        }
        Ok(())
    }
}

fn interleave(channels: &[Vec<f32>], out: &mut Vec<f32>) {
    for i in 0..channels[0].len() {
        out.extend(channels.iter().map(|c| c[i]));
    }
}
