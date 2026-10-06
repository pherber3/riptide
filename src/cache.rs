use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

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
