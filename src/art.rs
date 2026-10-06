use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::load::{BytesLoadResult, BytesLoader, BytesPoll, LoadError};

enum Entry {
    Pending,
    Ready(Arc<[u8]>),
    Failed,
}

/// Loads Tidal artwork for egui through a disk cache. The app forgets all images on
/// every page change, so memory holds only what the current page shows.
pub struct Art {
    dir: PathBuf,
    http: reqwest::Client,
    rt: tokio::runtime::Handle,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

impl Art {
    pub fn new(dir: PathBuf, rt: tokio::runtime::Handle) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self { dir, http: reqwest::Client::new(), rt, entries: Default::default() }
    }
}

fn ready(bytes: &Arc<[u8]>) -> BytesPoll {
    BytesPoll::Ready { size: None, bytes: egui::load::Bytes::Shared(bytes.clone()), mime: None }
}

impl BytesLoader for Art {
    fn id(&self) -> &str {
        egui::generate_loader_id!(Art)
    }

    fn load(&self, ctx: &egui::Context, uri: &str) -> BytesLoadResult {
        let Some(name) = uri.strip_prefix("https://resources.tidal.com/images/") else {
            return Err(LoadError::NotSupported);
        };
        let mut entries = self.entries.lock().unwrap();
        match entries.get(uri) {
            Some(Entry::Ready(bytes)) => return Ok(ready(bytes)),
            Some(Entry::Pending) => return Ok(BytesPoll::Pending { size: None }),
            Some(Entry::Failed) => return Err(LoadError::Loading("artwork unavailable".into())),
            None => {}
        }
        let path = self.dir.join(name.replace('/', "_"));
        if let Ok(bytes) = std::fs::read(&path) {
            let bytes: Arc<[u8]> = bytes.into();
            entries.insert(uri.into(), Entry::Ready(bytes.clone()));
            return Ok(ready(&bytes));
        }
        entries.insert(uri.into(), Entry::Pending);
        let (http, entries, ctx, uri) = (self.http.clone(), self.entries.clone(), ctx.clone(), uri.to_string());
        self.rt.spawn(async move {
            let fetched = async { http.get(&uri).send().await?.error_for_status()?.bytes().await }.await;
            let entry = match fetched {
                Ok(bytes) => {
                    let _ = std::fs::write(&path, &bytes);
                    Entry::Ready(bytes.to_vec().into())
                }
                Err(_) => Entry::Failed,
            };
            entries.lock().unwrap().insert(uri, entry);
            ctx.request_repaint();
        });
        Ok(BytesPoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.entries.lock().unwrap().remove(uri);
    }

    fn forget_all(&self) {
        self.entries.lock().unwrap().retain(|_, e| matches!(e, Entry::Pending));
    }

    fn byte_size(&self) -> usize {
        let entries = self.entries.lock().unwrap();
        entries.values().map(|e| if let Entry::Ready(b) = e { b.len() } else { 0 }).sum()
    }
}
