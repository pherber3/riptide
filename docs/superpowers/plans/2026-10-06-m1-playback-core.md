# Milestone 1: Playback Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A CLI, `tidalfast login` / `tidalfast play <track-id> [high|lossless|max]`, that signs in to Tidal and plays a track (hi-res FLAC included) through the default Windows output.

**Architecture:** `tidal` wraps tidlers and returns a stream's URL parts. `cache` downloads those parts into a file and hands out a reader that blocks until bytes arrive, so playback starts before the download ends. `decode` turns the reader into interleaved f32 at the device rate (symphonia + rubato). `player` feeds a lock-free ring buffer read by a `fastframe_audio::Render`.

**Tech Stack:** Rust 2024 (stable, installed at `D:\dev\rust`), tidlers 0.5, tokio, reqwest 0.13, symphonia 0.5, rubato 0.16, rtrb 0.3, fastframe-audio (git tag v0.4.1).

**Spec:** `docs/superpowers/specs/2026-10-06-tidalfast-design.md`

## Global Constraints

- Minimal code: YAGNI/KISS, no speculative abstractions, comments only where non-obvious.
- Only `src/tidal.rs` imports `tidlers`.
- App data and cache live under the executable's directory (`<exe dir>/data`, `<exe dir>/cache`), never on C:.
- Cache cap: 2 GiB, oldest evicted at start of `play`.
- Default quality: `max`.
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Run cargo from `D:\Projects\tidalfast`; cargo is at `D:\dev\rust\cargo\bin` (on PATH in new shells).

## Review Focus

1. Segmented (DASH) streams have no segment count in tidlers' parse: the download must stop at the first 4xx after segment one and still mark the file complete. (Task 2 test `fetch_segments_stops_at_client_error`.)
2. Network failure mid-download: the reader returns the bytes it has, then an error, never hangs. (Task 2 test `reader_errors_after_written_data_on_failure`.)
3. A partial download must never be treated as complete on the next run. (Task 2 test `create_clears_stale_done_marker`.)
4. Track rate ≠ device rate (e.g. 96 kHz track, 48 kHz device) must keep duration/pitch. (Task 3 test `resampler_preserves_duration`.)
5. Mono or multichannel sources must play on a stereo device. (Task 3 test `map_channels_mono_and_surround`.)

---

### Task 1: Scaffold and Tidal sign-in

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`, `src/main.rs`, `src/tidal.rs`

**Interfaces:**
- Produces:
  - `tidal::Quality { High, Lossless, Max }`, `Quality::parse(&str) -> Option<Quality>`
  - `tidal::Parts::{Urls(Vec<String>), Segments { init: String, template: String, start: u32 }}`
  - `tidal::Stream { parts: Parts, quality: String, codec: String }`
  - `tidal::Tidal::login(&Path) -> Result<Tidal>`, `Tidal::load(&Path) -> Result<Tidal>`, `Tidal::stream(&mut self, &str, Quality) -> Result<Stream>`

- [ ] **Step 1: Write project files**

`Cargo.toml`:
```toml
[package]
name = "tidalfast"
version = "0.1.0"
edition = "2024"
license = "MIT"

[dependencies]
anyhow = "1"
tidlers = "0.5"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
reqwest = "0.13"
open = "5"
symphonia = { version = "0.5", default-features = false, features = ["flac", "isomp4", "aac"] }
rubato = "0.16"
rtrb = "0.3"
fastframe-audio = { git = "https://github.com/crmne/fastframe", tag = "v0.4.1" }

[dev-dependencies]
symphonia = { version = "0.5", default-features = false, features = ["flac", "isomp4", "aac", "wav", "pcm"] }
tempfile = "3"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "net", "io-util"] }

[profile.dev.package."*"]
opt-level = 2

[profile.release]
lto = "thin"
codegen-units = 1
strip = true
```

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]
```

`.gitignore`:
```
/target
/data
/cache
```

- [ ] **Step 2: Write the failing test** (bottom of `src/tidal.rs`; create the file with just this plus `use` lines and an empty `Quality` stub so it compiles to a failure)

```rust
#[cfg(test)]
mod tests {
    use super::Quality;

    #[test]
    fn parses_quality_names() {
        assert_eq!(Quality::parse("max"), Some(Quality::Max));
        assert_eq!(Quality::parse("lossless"), Some(Quality::Lossless));
        assert_eq!(Quality::parse("high"), Some(Quality::High));
        assert_eq!(Quality::parse("ultra"), None);
    }
}
```

- [ ] **Step 3: Run it, expect FAIL** — `cargo test parses_quality_names`

- [ ] **Step 4: Implement `src/tidal.rs`**

```rust
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tidlers::TidalClient;
use tidlers::auth::TidalAuth;
use tidlers::client::models::playback::AudioQuality;
use tidlers::client::models::track::playback::ParsedTrackManifest;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Quality {
    High,
    Lossless,
    Max,
}

impl Quality {
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "high" => Some(Self::High),
            "lossless" => Some(Self::Lossless),
            "max" => Some(Self::Max),
            _ => None,
        }
    }

    fn tidlers(self) -> AudioQuality {
        match self {
            Self::High => AudioQuality::High,
            Self::Lossless => AudioQuality::Lossless,
            Self::Max => AudioQuality::HiRes,
        }
    }
}

pub enum Parts {
    Urls(Vec<String>),
    Segments { init: String, template: String, start: u32 },
}

pub struct Stream {
    pub parts: Parts,
    pub quality: String,
    pub codec: String,
}

pub struct Tidal {
    client: TidalClient,
}

impl Tidal {
    pub async fn login(session: &Path) -> Result<Self> {
        let mut client = TidalClient::new(&TidalAuth::with_pkce());
        let url = client.initiate_pkce_login()?;
        println!("Sign in in your browser, then paste the address of the page you land on:\n{url}");
        let _ = open::that(&url);
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        client.finish_pkce_login(line.trim()).await?;
        save(session, &client)?;
        Ok(Self { client })
    }

    pub async fn load(session: &Path) -> Result<Self> {
        let json = std::fs::read_to_string(session).context("not signed in; run `tidalfast login`")?;
        let mut client = TidalClient::from_json(&json)?;
        if client.refresh_access_token(false).await? {
            save(session, &client)?;
        }
        Ok(Self { client })
    }

    pub async fn stream(&mut self, track_id: &str, quality: Quality) -> Result<Stream> {
        self.client.set_audio_quality(quality.tidlers());
        let info = self.client.get_track_postpaywall_playback_info(track_id, None).await?;
        let codec = info.get_codecs().unwrap_or_default();
        let parts = match info.manifest_parsed {
            Some(ParsedTrackManifest::Json(m)) => Parts::Urls(m.urls),
            Some(ParsedTrackManifest::Dash(m)) => Parts::Segments {
                init: m.get_init_url().context("DASH manifest has no init segment")?.clone(),
                template: m.get_media_template().context("DASH manifest has no media template")?.clone(),
                start: m.start_number.unwrap_or(1),
            },
            None => bail!("Tidal returned no stream manifest"),
        };
        Ok(Stream { parts, quality: info.audio_quality, codec })
    }
}

fn save(session: &Path, client: &TidalClient) -> Result<()> {
    std::fs::create_dir_all(session.parent().unwrap_or(Path::new(".")))?;
    std::fs::write(session, client.get_json())?;
    Ok(())
}

pub fn session_path(dir: &Path) -> PathBuf {
    dir.join("data").join("session.json")
}
```

`src/main.rs` (login only for now):
```rust
mod tidal;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let dir = std::env::current_exe()?.parent().expect("exe has a directory").to_path_buf();
    let session = tidal::session_path(&dir);
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["login"] => {
            tidal::Tidal::login(&session).await?;
            println!("Signed in.");
        }
        _ => eprintln!("usage: tidalfast login | tidalfast play <track-id> [high|lossless|max]"),
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests, expect PASS** — `cargo test` (first build downloads crates; allow several minutes)

- [ ] **Step 6: Manual check** — `cargo run -- login`, sign in, paste the redirect URL. Expected: `Signed in.` and `target/debug/data/session.json` exists. The page Tidal redirects to may show an error ("Oops"); that is expected — copy its address bar. If tidlers errors, record the message verbatim.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat: scaffold and Tidal PKCE sign-in"
```

---

### Task 2: Download cache with growing-file reader

**Files:**
- Create: `src/cache.rs`
- Modify: `src/main.rs` (add `mod cache;`)

**Interfaces:**
- Consumes: `tidal::Parts`
- Produces:
  - `cache::create(&Path) -> io::Result<(Writer, Reader)>`
  - `cache::open_complete(&Path) -> Option<Reader>`
  - `cache::fetch(http: &reqwest::Client, parts: &Parts, writer: Writer)` (async; always finishes the writer)
  - `cache::evict(dir: &Path, max_bytes: u64) -> io::Result<()>`
  - `cache::Reader: Read + Seek + symphonia::core::io::MediaSource`

- [ ] **Step 1: Write the failing tests** (bottom of `src/cache.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Seek, SeekFrom};
    use std::time::{Duration, SystemTime};

    #[test]
    fn reader_waits_for_writer() {
        let dir = tempfile::tempdir().unwrap();
        let (mut w, mut r) = create(&dir.path().join("a")).unwrap();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            w.append(b"abc").unwrap();
            w.finish(Ok(()));
        });
        let mut out = Vec::new();
        r.read_to_end(&mut out).unwrap();
        t.join().unwrap();
        assert_eq!(out, b"abc");
    }

    #[test]
    fn reader_errors_after_written_data_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let (mut w, mut r) = create(&dir.path().join("a")).unwrap();
        w.append(b"ab").unwrap();
        w.finish(Err("network down".into()));
        let mut buf = [0; 10];
        assert_eq!(r.read(&mut buf).unwrap(), 2);
        assert!(r.read(&mut buf).is_err());
    }

    #[test]
    fn seek_from_end_waits_for_length() {
        let dir = tempfile::tempdir().unwrap();
        let (mut w, mut r) = create(&dir.path().join("a")).unwrap();
        w.append(b"hello").unwrap();
        w.finish(Ok(()));
        assert_eq!(r.seek(SeekFrom::End(-2)).unwrap(), 3);
        let mut s = String::new();
        r.read_to_string(&mut s).unwrap();
        assert_eq!(s, "lo");
    }

    #[test]
    fn complete_only_after_finish() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        let (mut w, _r) = create(&path).unwrap();
        w.append(b"x").unwrap();
        assert!(open_complete(&path).is_none());
        w.finish(Ok(()));
        assert!(open_complete(&path).is_some());
    }

    #[test]
    fn create_clears_stale_done_marker() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        let (w, _r) = create(&path).unwrap();
        w.finish(Ok(()));
        let (_w, _r) = create(&path).unwrap();
        assert!(open_complete(&path).is_none());
    }

    #[test]
    fn evict_removes_oldest_until_under_cap() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        for (i, name) in ["old", "mid", "new"].iter().enumerate() {
            let f = std::fs::File::create(dir.path().join(name)).unwrap();
            f.set_len(10).unwrap();
            f.set_modified(now - Duration::from_secs(100 - i as u64 * 10)).unwrap();
        }
        evict(dir.path(), 20).unwrap();
        assert!(!dir.path().join("old").exists());
        assert!(dir.path().join("mid").exists());
        assert!(dir.path().join("new").exists());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn fetch_segments_stops_at_client_error() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut req = [0; 1024];
                let n = s.read(&mut req).await.unwrap();
                let path = String::from_utf8_lossy(&req[..n]).split_whitespace().nth(1).unwrap().to_string();
                let (status, body) = match path.as_str() {
                    "/init" => ("200 OK", "I"),
                    "/seg-1" => ("200 OK", "1"),
                    "/seg-2" => ("200 OK", "2"),
                    _ => ("403 Forbidden", ""),
                };
                let resp = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                s.write_all(resp.as_bytes()).await.unwrap();
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        let (w, mut r) = create(&path).unwrap();
        let parts = Parts::Segments { init: format!("{base}/init"), template: format!("{base}/seg-$Number$"), start: 1 };
        fetch(&reqwest::Client::new(), &parts, w).await;
        let mut out = String::new();
        r.read_to_string(&mut out).unwrap();
        assert_eq!(out, "I12");
        assert!(open_complete(&path).is_some());
    }
}
```

- [ ] **Step 2: Run, expect FAIL (does not compile)** — `cargo test cache::`

- [ ] **Step 3: Implement** (top of `src/cache.rs`)

```rust
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use crate::tidal::Parts;

#[derive(Default)]
struct Progress {
    written: u64,
    done: bool,
    failed: Option<String>,
}

type Shared = Arc<(Mutex<Progress>, Condvar)>;

pub struct Writer {
    file: File,
    shared: Shared,
    marker: PathBuf,
}

pub struct Reader {
    file: File,
    pos: u64,
    shared: Shared,
}

fn marker(path: &Path) -> PathBuf {
    path.with_extension("done")
}

pub fn create(path: &Path) -> io::Result<(Writer, Reader)> {
    let _ = fs::remove_file(marker(path));
    let file = File::create(path)?;
    let shared = Shared::default();
    let reader = Reader { file: File::open(path)?, pos: 0, shared: shared.clone() };
    Ok((Writer { file, shared, marker: marker(path) }, reader))
}

pub fn open_complete(path: &Path) -> Option<Reader> {
    if !marker(path).exists() {
        return None;
    }
    let file = File::open(path).ok()?;
    let written = file.metadata().ok()?.len();
    let progress = Progress { written, done: true, failed: None };
    Some(Reader { file, pos: 0, shared: Arc::new((Mutex::new(progress), Condvar::new())) })
}

impl Writer {
    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        let (lock, cv) = &*self.shared;
        lock.lock().unwrap().written += bytes.len() as u64;
        cv.notify_all();
        Ok(())
    }

    pub fn finish(self, result: Result<(), String>) {
        let result = result.and_then(|()| {
            self.file.sync_all().and_then(|()| File::create(&self.marker).map(drop)).map_err(|e| e.to_string())
        });
        let (lock, cv) = &*self.shared;
        let mut p = lock.lock().unwrap();
        p.done = true;
        p.failed = result.err();
        cv.notify_all();
    }
}

impl Reader {
    fn wait_until(&self, ready: impl Fn(&Progress) -> bool) -> std::sync::MutexGuard<'_, Progress> {
        let (lock, cv) = &*self.shared;
        cv.wait_while(lock.lock().unwrap(), |p| !ready(p) && !p.done).unwrap()
    }
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let pos = self.pos;
        let p = self.wait_until(|p| p.written > pos);
        let available = p.written.saturating_sub(pos);
        if available == 0 {
            return match &p.failed {
                Some(e) => Err(io::Error::other(e.clone())),
                None => Ok(0),
            };
        }
        drop(p);
        let n = buf.len().min(available as usize);
        self.file.seek(SeekFrom::Start(pos))?;
        let n = self.file.read(&mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Reader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let (base, offset) = match to {
            SeekFrom::Start(n) => (n, 0),
            SeekFrom::Current(d) => (self.pos, d),
            SeekFrom::End(d) => {
                let p = self.wait_until(|_| false);
                if let Some(e) = &p.failed {
                    return Err(io::Error::other(e.clone()));
                }
                (p.written, d)
            }
        };
        self.pos = base.checked_add_signed(offset).ok_or_else(|| io::Error::other("seek before start"))?;
        Ok(self.pos)
    }
}

impl symphonia::core::io::MediaSource for Reader {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        let p = self.shared.0.lock().unwrap();
        p.done.then_some(p.written)
    }
}

pub async fn fetch(http: &reqwest::Client, parts: &Parts, mut writer: Writer) {
    let result = fetch_into(http, parts, &mut writer).await;
    writer.finish(result.map_err(|e| e.to_string()));
}

async fn fetch_into(http: &reqwest::Client, parts: &Parts, w: &mut Writer) -> anyhow::Result<()> {
    match parts {
        Parts::Urls(urls) => {
            for url in urls {
                copy(http.get(url).send().await?.error_for_status()?, w).await?;
            }
        }
        Parts::Segments { init, template, start } => {
            copy(http.get(init).send().await?.error_for_status()?, w).await?;
            for n in *start.. {
                let resp = http.get(template.replace("$Number$", &n.to_string())).send().await?;
                // tidlers doesn't parse the segment timeline, so the first 4xx after segment one is the end.
                if resp.status().is_client_error() && n > *start {
                    break;
                }
                copy(resp.error_for_status()?, w).await?;
            }
        }
    }
    Ok(())
}

async fn copy(mut resp: reqwest::Response, w: &mut Writer) -> anyhow::Result<()> {
    while let Some(chunk) = resp.chunk().await? {
        w.append(&chunk)?;
    }
    Ok(())
}

pub fn evict(dir: &Path, max_bytes: u64) -> io::Result<()> {
    let mut files: Vec<_> = fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            if m.is_file() { Some((m.modified().ok()?, m.len(), e.path())) } else { None }
        })
        .collect();
    files.sort();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    for (_, len, path) in files {
        if total <= max_bytes {
            break;
        }
        if fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
    Ok(())
}
```

Add `mod cache;` to `src/main.rs`.

- [ ] **Step 4: Run, expect PASS** — `cargo test cache::`

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: on-disk stream cache with growing-file reader"
```

---

### Task 3: Decoding, channel mapping, resampling

**Files:**
- Create: `src/decode.rs`
- Modify: `src/main.rs` (add `mod decode;`)

**Interfaces:**
- Consumes: `cache::Reader`, `cache::create`, `cache::open_complete` (tests)
- Produces:
  - `decode::Info { sample_rate: u32, channels: usize, bits: Option<u32>, codec: &'static str, duration: Option<Duration> }`
  - `decode::Decoder::open(Reader) -> Result<Decoder>`, field `info: Info`, `Decoder::next(&mut self, out: &mut Vec<f32>) -> Result<bool>` (false = end; `out` holds interleaved samples of one packet)
  - `decode::map_channels(input: &[f32], from: usize, to: usize, out: &mut Vec<f32>)`
  - `decode::Resampler::new(from: u32, to: u32, channels: usize) -> Result<Resampler>`, `push(&mut self, &[f32], &mut Vec<f32>) -> Result<()>`, `flush(&mut self, &mut Vec<f32>) -> Result<()>`

- [ ] **Step 1: Write the failing tests** (bottom of `src/decode.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn wav(frames: u32, rate: u32) -> Vec<u8> {
        let data = frames * 4;
        let mut v = Vec::new();
        v.extend(b"RIFF");
        v.extend((36 + data).to_le_bytes());
        v.extend(b"WAVEfmt ");
        v.extend(16u32.to_le_bytes());
        v.extend(1u16.to_le_bytes());
        v.extend(2u16.to_le_bytes());
        v.extend(rate.to_le_bytes());
        v.extend((rate * 4).to_le_bytes());
        v.extend(4u16.to_le_bytes());
        v.extend(16u16.to_le_bytes());
        v.extend(b"data");
        v.extend(data.to_le_bytes());
        v.resize(v.len() + data as usize, 0);
        v
    }

    #[test]
    fn decodes_every_frame_from_cache_reader() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t");
        let (mut w, _) = crate::cache::create(&path).unwrap();
        w.append(&wav(1000, 44100)).unwrap();
        w.finish(Ok(()));
        let mut d = Decoder::open(crate::cache::open_complete(&path).unwrap()).unwrap();
        assert_eq!((d.info.sample_rate, d.info.channels, d.info.bits), (44100, 2, Some(16)));
        let (mut total, mut buf) = (0, Vec::new());
        while d.next(&mut buf).unwrap() {
            total += buf.len();
        }
        assert_eq!(total, 2000);
    }

    #[test]
    fn map_channels_mono_and_surround() {
        let mut out = Vec::new();
        map_channels(&[1.0, 2.0], 1, 2, &mut out);
        assert_eq!(out, [1.0, 1.0, 2.0, 2.0]);
        out.clear();
        map_channels(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 6, 2, &mut out);
        assert_eq!(out, [1.0, 2.0]);
        out.clear();
        map_channels(&[1.0, 2.0], 2, 2, &mut out);
        assert_eq!(out, [1.0, 2.0]);
    }

    #[test]
    fn resampler_passes_through_equal_rates() {
        let mut r = Resampler::new(48000, 48000, 2).unwrap();
        let mut out = Vec::new();
        r.push(&[0.1, 0.2, 0.3, 0.4], &mut out).unwrap();
        assert_eq!(out, [0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn resampler_preserves_duration() {
        let mut r = Resampler::new(96000, 48000, 2).unwrap();
        let mut out = Vec::new();
        r.push(&vec![0.0; 96000 * 2], &mut out).unwrap();
        r.flush(&mut out).unwrap();
        let frames = out.len() / 2;
        assert!((47000..=50000).contains(&frames), "{frames} frames");
    }
}
```

- [ ] **Step 2: Run, expect FAIL** — `cargo test decode::`

- [ ] **Step 3: Implement** (top of `src/decode.rs`)

```rust
use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use rubato::{FftFixedIn, Resampler as _};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::cache::Reader;

pub struct Info {
    pub sample_rate: u32,
    pub channels: usize,
    pub bits: Option<u32>,
    pub codec: &'static str,
    pub duration: Option<Duration>,
}

pub struct Decoder {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track: u32,
    pub info: Info,
}

impl Decoder {
    pub fn open(reader: Reader) -> Result<Self> {
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
            duration: p.n_frames.zip(p.time_base).map(|(n, tb)| {
                let t = tb.calc_time(n);
                Duration::from_secs_f64(t.seconds as f64 + t.frac)
            }),
        };
        let decoder = symphonia::default::get_codecs().make(p, &DecoderOptions::default())?;
        let track = track.id;
        Ok(Self { format, decoder, track, info })
    }

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
            let chunk: Vec<Vec<f32>> = self.pending.iter_mut().map(|p| p.drain(..).collect()).collect();
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
```

Add `mod decode;` to `src/main.rs`.

- [ ] **Step 4: Run, expect PASS** — `cargo test decode::`

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat: symphonia decoding, channel mapping, rubato resampling"
```

---

### Task 4: Audio output and `play` command

**Files:**
- Create: `src/player.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `decode::{Decoder, Resampler, map_channels}`, `cache::{create, open_complete, fetch, evict}`, `tidal::{Tidal, Quality}`
- Produces: `player::Sink` (a `fastframe_audio::Render`), `player::play(reader: cache::Reader) -> Result<()>` (blocks until the track ends)

- [ ] **Step 1: Write the failing test** (bottom of `src/player.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use fastframe_audio::Render;

    #[test]
    fn sink_plays_queued_samples_then_silence() {
        let (mut tx, rx) = rtrb::RingBuffer::new(8);
        tx.push(0.5).unwrap();
        tx.push(-0.5).unwrap();
        let mut sink = Sink { rx, volume: Arc::new(AtomicU32::new(0.5f32.to_bits())) };
        let mut out = [9.0; 4];
        sink.render(&mut out);
        assert_eq!(out, [0.25, -0.25, 0.0, 0.0]);
    }
}
```

- [ ] **Step 2: Run, expect FAIL** — `cargo test player::`

- [ ] **Step 3: Implement `src/player.rs`**

```rust
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
        "{} {}-bit {} Hz -> {} ({rate} Hz)",
        i.codec,
        i.bits.map_or("?".into(), |b| b.to_string()),
        i.sample_rate,
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
                    print!("\r{:>4}s", clock.played().as_secs());
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
```

Replace `src/main.rs`:

```rust
mod cache;
mod decode;
mod player;
mod tidal;

use anyhow::{Context, Result};
use tidal::{Quality, Tidal};

const CACHE_BYTES: u64 = 2 << 30;

#[tokio::main]
async fn main() -> Result<()> {
    let dir = std::env::current_exe()?.parent().expect("exe has a directory").to_path_buf();
    let session = tidal::session_path(&dir);
    let cache_dir = dir.join("cache");
    std::fs::create_dir_all(&cache_dir)?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["login"] => {
            Tidal::login(&session).await?;
            println!("Signed in.");
        }
        ["play", id, rest @ ..] => {
            let quality = match rest.first() {
                Some(name) => Quality::parse(name).context("quality must be high, lossless or max")?,
                None => Quality::Max,
            };
            cache::evict(&cache_dir, CACHE_BYTES)?;
            let path = cache_dir.join(format!("{id}-{quality:?}"));
            let reader = match cache::open_complete(&path) {
                Some(reader) => reader,
                None => {
                    let stream = Tidal::load(&session).await?.stream(id, quality).await?;
                    println!("Tidal: {} {}", stream.quality, stream.codec);
                    let (writer, reader) = cache::create(&path)?;
                    tokio::spawn(async move { cache::fetch(&reqwest::Client::new(), &stream.parts, writer).await });
                    reader
                }
            };
            tokio::task::spawn_blocking(move || player::play(reader)).await??;
        }
        _ => eprintln!("usage: tidalfast login | tidalfast play <track-id> [high|lossless|max]"),
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests, expect PASS** — `cargo test` then `cargo clippy --all-targets -- -D warnings` (fix anything it flags)

- [ ] **Step 5: Manual verification on real Tidal** (find a track ID in a Tidal share link `tidal.com/track/<id>`; pick one labelled MAX in the official app)
  - `cargo run --release -- play <id>` → prints `Tidal: HI_RES_LOSSLESS flac`, then `flac 24-bit 96000 Hz -> <device> (… Hz)`, and audio plays at correct pitch/speed.
  - `cargo run --release -- play <id> lossless` → 16-bit 44100.
  - `cargo run --release -- play <id> high` → aac.
  - Run the first command again → starts instantly from cache (no `Tidal:` line).
  - Note peak RAM in Task Manager while playing (target: well under 100 MB for this CLI).
  - If symphonia rejects the hi-res stream (fMP4 FLAC), record the exact error; the fallback is to demux with `symphonia`'s `isomp4` at a newer version or extract FLAC frames from `mdat` — stop and report before changing approach.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat: play command with WASAPI shared output"
```
