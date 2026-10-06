use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};

use anyhow::Result;

use crate::tidal::{Parts, Quality, Tidal};

/// Downloads in progress, so a second reader (prefetch, then play) shares the first download.
static ACTIVE: LazyLock<Mutex<HashMap<PathBuf, Shared>>> = LazyLock::new(Default::default);

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
    path: PathBuf,
    held: Option<u64>,
}

/// Reads a file that may still be downloading, blocking until bytes arrive.
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
    ACTIVE.lock().unwrap().insert(path.into(), shared.clone());
    Ok((Writer { file, shared, path: path.into(), held: None }, reader))
}

/// A reader for a finished or in-progress download.
pub fn open(path: &Path) -> Option<Reader> {
    if let Some(shared) = ACTIVE.lock().unwrap().get(path) {
        return Some(Reader { file: File::open(path).ok()?, pos: 0, shared: shared.clone() });
    }
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
        if let Some(held) = &mut self.held {
            *held += bytes.len() as u64;
            return Ok(());
        }
        let (lock, cv) = &*self.shared;
        lock.lock().unwrap().written += bytes.len() as u64;
        cv.notify_all();
        Ok(())
    }

    /// Keeps readers waiting until the download is complete, for formats that need the whole file.
    pub fn hold(&mut self) {
        self.held = Some(0);
    }

    pub fn finish(self, result: Result<(), String>) {
        let result = result.and_then(|()| {
            self.file.sync_all().and_then(|()| File::create(marker(&self.path)).map(drop)).map_err(|e| e.to_string())
        });
        let (lock, cv) = &*self.shared;
        let mut p = lock.lock().unwrap();
        p.written += self.held.unwrap_or(0);
        p.done = true;
        p.failed = result.err();
        cv.notify_all();
        drop(p);
        ACTIVE.lock().unwrap().remove(&self.path);
    }
}

/// A reader for a track, downloading it into the cache unless it is already there or on its way.
pub async fn track(tidal: &tokio::sync::Mutex<Tidal>, dir: &Path, id: u64, quality: Quality) -> Result<Reader> {
    let path = dir.join(format!("{id}-{quality:?}"));
    let mut tidal = tidal.lock().await;
    if let Some(reader) = open(&path) {
        return Ok(reader);
    }
    let stream = tidal.stream(id, quality).await?;
    let (writer, reader) = create(&path)?;
    tokio::spawn(async move { fetch(&reqwest::Client::new(), &stream.parts, writer).await });
    Ok(reader)
}

impl Reader {
    /// Blocks until the download has finished; seeking needs the full length.
    pub fn waiter(&self) -> impl Fn() + Send + Sync + 'static {
        let shared = self.shared.clone();
        move || {
            let (lock, cv) = &*shared;
            drop(cv.wait_while(lock.lock().unwrap(), |p| !p.done).unwrap());
        }
    }

    fn wait_until(&self, ready: impl Fn(&Progress) -> bool) -> MutexGuard<'_, Progress> {
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

async fn fetch(http: &reqwest::Client, parts: &Parts, mut writer: Writer) {
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
            let init = http.get(init).send().await?.error_for_status()?.bytes().await?;
            // Hi-res FLAC arrives as fragmented MP4. Rewrap it as a native FLAC stream so it
            // decodes progressively; symphonia's MP4 reader wants the whole file first, so
            // anything else (AAC on tracks without FLAC) is played once fully downloaded.
            let dfla = find_box(&init, b"dfLa");
            match dfla {
                Some(dfla) => w.append(&[b"fLaC", &dfla[4..]].concat())?,
                None => {
                    w.hold();
                    w.append(&init)?;
                }
            }
            for n in *start.. {
                let resp = http.get(template.replace("$Number$", &n.to_string())).send().await?;
                // tidlers doesn't parse the segment timeline, so the first 4xx after segment one is the end.
                if resp.status().is_client_error() && n > *start {
                    break;
                }
                let segment = resp.error_for_status()?.bytes().await?;
                if dfla.is_none() {
                    w.append(&segment)?;
                    continue;
                }
                for (kind, body) in boxes(&segment) {
                    if kind == b"mdat" {
                        w.append(body)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Top-level MP4 boxes as (type, payload).
fn boxes(mut data: &[u8]) -> impl Iterator<Item = (&[u8], &[u8])> {
    std::iter::from_fn(move || {
        let size = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
        if size < 8 || size > data.len() {
            return None;
        }
        let (kind, body) = (&data[4..8], &data[8..size]);
        data = &data[size..];
        Some((kind, body))
    })
}

/// Payload of the first box of this type anywhere in `data`.
fn find_box<'a>(data: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    let at = data.windows(4).position(|w| w == kind)?.checked_sub(4)?;
    let size = u32::from_be_bytes(data[at..at + 4].try_into().ok()?) as usize;
    data.get(at + 8..at + size)
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
