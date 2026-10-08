//! Files on disk that can be read while they are still being written: a track starts playing as
//! soon as its first bytes arrive, and a finished one is played from disk next time.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};

use crate::tidal::Quality;

/// Downloads in progress, so a second reader (prefetch, then play) shares the first download, and
/// so downloads of tracks skipped past can be cancelled.
static ACTIVE: LazyLock<Mutex<HashMap<PathBuf, Active>>> = LazyLock::new(Default::default);

/// A download's progress and the task writing it.
type Active = (Arc<Shared>, Option<tokio::task::AbortHandle>);

/// A download's progress, shared by its writer and readers.
#[derive(Default)]
struct Shared {
    progress: Mutex<Progress>,
    /// Wakes readers blocked on the audio thread when bytes arrive or the download ends.
    arrived: Condvar,
    /// Wakes async waiters when the download ends.
    ended: tokio::sync::Notify,
}

#[derive(Default)]
struct Progress {
    written: u64,
    done: bool,
    failed: Option<String>,
}

/// Writes a download. However it ends (finished, failed, or cancelled by dropping it), its readers
/// are told, so none waits forever.
pub struct Writer {
    file: File,
    shared: Arc<Shared>,
    path: PathBuf,
    held: Option<u64>,
    result: Result<(), String>,
}

/// Reads a file that may still be downloading, blocking until bytes arrive.
pub struct Reader {
    file: File,
    pos: u64,
    shared: Arc<Shared>,
}

pub fn path(dir: &Path, id: u64, quality: Quality) -> PathBuf {
    dir.join(format!("{id}-{quality:?}"))
}

/// Marks a file as completely downloaded.
fn marker(path: &Path) -> PathBuf {
    path.with_extension("done")
}

/// Starts a new download into `path`, with a reader for it.
pub fn create(path: &Path) -> io::Result<(Writer, Reader)> {
    let _ = fs::remove_file(marker(path));
    let file = File::create(path)?;
    let shared = Arc::new(Shared::default());
    let reader = Reader { file: File::open(path)?, pos: 0, shared: shared.clone() };
    ACTIVE.lock().unwrap().insert(path.into(), (shared.clone(), None));
    Ok((Writer { file, shared, path: path.into(), held: None, result: Err("download cancelled".into()) }, reader))
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
    let shared = Shared { progress: Mutex::new(Progress { written, done: true, failed: None }), ..Default::default() };
    Some(Reader { file, pos: 0, shared: Arc::new(shared) })
}

/// Notes the task downloading into `path`, so `keep_only` can cancel it.
pub fn running(path: &Path, task: tokio::task::AbortHandle) {
    if let Some(active) = ACTIVE.lock().unwrap().get_mut(path) {
        active.1 = Some(task);
    }
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

/// Deletes the least recently written files until the folder holds at most `max_bytes`.
pub fn evict(dir: &Path, max_bytes: u64) -> io::Result<()> {
    let mut files: Vec<_> = fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| e.metadata().ok().filter(fs::Metadata::is_file).and_then(|m| Some((m.modified().ok()?, m.len(), e.path()))))
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

impl Writer {
    pub fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.file.write_all(bytes)?;
        if let Some(held) = &mut self.held {
            *held += bytes.len() as u64;
            return Ok(());
        }
        self.shared.progress.lock().unwrap().written += bytes.len() as u64;
        self.shared.arrived.notify_all();
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
        let result = std::mem::replace(&mut self.result, Ok(())).and_then(|()| File::create(marker(&self.path)).map(drop).map_err(|e| e.to_string()));
        let mut p = self.shared.progress.lock().unwrap();
        p.written += self.held.unwrap_or(0);
        p.done = true;
        p.failed = result.err();
        drop(p);
        self.shared.arrived.notify_all();
        self.shared.ended.notify_waiters();
        ACTIVE.lock().unwrap().remove(&self.path);
    }
}

/// Watches a download from outside its reader.
#[derive(Clone)]
pub struct Download(Arc<Shared>);

impl Download {
    /// Finished, successfully or not.
    pub fn done(&self) -> bool {
        self.0.progress.lock().unwrap().done
    }

    pub async fn finished(&self) {
        // Waiting starts before the check, so an end between the two isn't missed.
        let ended = self.0.ended.notified();
        if !self.done() {
            ended.await;
        }
    }
}

impl Reader {
    pub fn download(&self) -> Download {
        Download(self.shared.clone())
    }

    fn wait_until(&self, ready: impl Fn(&Progress) -> bool) -> MutexGuard<'_, Progress> {
        let progress = self.shared.progress.lock().unwrap();
        self.shared.arrived.wait_while(progress, |p| !ready(p) && !p.done).unwrap()
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
        let p = self.shared.progress.lock().unwrap();
        p.done.then_some(p.written)
    }
}
