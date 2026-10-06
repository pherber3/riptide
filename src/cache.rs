use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};

use anyhow::Result;
use futures_util::{StreamExt, stream};

use crate::tidal::{HTTP, Parts, Quality, Tidal};

/// Hi-res segments fetched at once; more barely helps and just competes with everything else.
const PARALLEL: usize = 4;

/// Serialises starting downloads, so two requests for one track (prefetch, then play) can't both start it.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Downloads in progress, so a second reader (prefetch, then play) shares the first download, and
/// so downloads of tracks skipped past can be cancelled.
static ACTIVE: LazyLock<Mutex<HashMap<PathBuf, Active>>> = LazyLock::new(Default::default);

type Active = (Shared, Option<tokio::task::AbortHandle>);

#[derive(Default)]
struct Progress {
    written: u64,
    done: bool,
    failed: Option<String>,
}

type Shared = Arc<(Mutex<Progress>, Condvar)>;

/// Writes a download. However it ends (finished, failed, or cancelled by dropping it), its readers
/// are told, so none waits forever.
pub struct Writer {
    file: File,
    shared: Shared,
    path: PathBuf,
    held: Option<u64>,
    result: Result<(), String>,
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
    ACTIVE.lock().unwrap().insert(path.into(), (shared.clone(), None));
    let result = Err("download cancelled".into());
    Ok((Writer { file, shared, path: path.into(), held: None, result }, reader))
}

/// A reader for a finished or in-progress download.
pub fn open(path: &Path) -> Option<Reader> {
    if let Some((shared, _)) = ACTIVE.lock().unwrap().get(path) {
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

    pub fn finish(mut self, result: Result<(), String>) {
        self.result = result;
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let result = std::mem::replace(&mut self.result, Ok(()));
        let result = result.and_then(|()| File::create(marker(&self.path)).map(drop).map_err(|e| e.to_string()));
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

pub fn path(dir: &Path, id: u64, quality: Quality) -> PathBuf {
    dir.join(format!("{id}-{quality:?}"))
}

/// Cancels the downloads of every track but these, so skipping ahead doesn't leave old tracks
/// competing for bandwidth.
pub fn keep_only(keep: &[PathBuf]) {
    let active = ACTIVE.lock().unwrap();
    let cancel: Vec<_> = active.iter().filter(|(path, _)| !keep.contains(path)).filter_map(|(_, (_, task))| task.clone()).collect();
    // Unlocked first: a cancelled download's writer takes the lock as it finishes.
    drop(active);
    cancel.iter().for_each(tokio::task::AbortHandle::abort);
}

/// A reader for a track, downloading it into the cache unless it is already there or on its way.
pub async fn track(tidal: &Tidal, dir: &Path, id: u64, quality: Quality) -> Result<Reader> {
    let path = path(dir, id, quality);
    if let Some(reader) = open(&path) {
        return Ok(reader);
    }
    let _starting = STARTING.lock().await;
    if let Some(reader) = open(&path) {
        return Ok(reader);
    }
    let parts = tidal.stream(id, quality).await?;
    let (writer, reader) = create(&path)?;
    let task = tokio::spawn(async move { fetch(&parts, writer).await });
    if let Some(active) = ACTIVE.lock().unwrap().get_mut(&path) {
        active.1 = Some(task.abort_handle());
    }
    Ok(reader)
}

/// Watches a download from outside its reader.
#[derive(Clone)]
pub struct Download(Shared);

impl Download {
    /// Finished, successfully or not.
    pub fn done(&self) -> bool {
        self.0.0.lock().unwrap().done
    }

    pub async fn finished(&self) {
        while !self.done() {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }
}

impl Reader {
    pub fn download(&self) -> Download {
        Download(self.shared.clone())
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

async fn fetch(parts: &Parts, mut writer: Writer) {
    let result = fetch_into(parts, &mut writer).await;
    writer.finish(result.map_err(|e| e.to_string()));
}

async fn fetch_into(parts: &Parts, w: &mut Writer) -> anyhow::Result<()> {
    let http = &*HTTP;
    match parts {
        Parts::Urls(urls) => {
            for url in urls {
                copy(http.get(url).send().await?.error_for_status()?, w).await?;
            }
        }
        Parts::Segments { init, template, start, count } => {
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
            // A few segments in flight at once, written in order.
            anyhow::ensure!(*count > 0, "the stream manifest lists no segments");
            let mut segments = stream::iter(*start..start + count)
                .map(|n| async move {
                    let resp = http.get(template.replace("$Number$", &n.to_string())).send().await?;
                    anyhow::Ok(resp.error_for_status()?.bytes().await?)
                })
                .buffered(PARALLEL);
            while let Some(segment) = segments.next().await {
                let segment = segment?;
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
