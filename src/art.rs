use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use egui::ColorImage;
use egui::load::{ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};

enum Entry {
    Pending,
    Ready(Arc<ColorImage>),
    Failed,
}

/// Loads Tidal artwork through a disk cache, decoding off the UI thread. Each decoded image is
/// handed to egui's texture cache once and then dropped, so memory holds only the textures of
/// what is on screen (the app forgets all images on every page change).
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

fn decode(bytes: &[u8]) -> Option<ColorImage> {
    let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], &rgba))
}

impl ImageLoader for Art {
    fn id(&self) -> &str {
        egui::generate_loader_id!(Art)
    }

    fn load(&self, ctx: &egui::Context, uri: &str, _: SizeHint) -> ImageLoadResult {
        let tidal = ["https://resources.tidal.com/images/", "https://images.tidal.com/"];
        let Some(name) = tidal.iter().find_map(|prefix| uri.strip_prefix(prefix)) else {
            return Err(LoadError::NotSupported);
        };
        let mut entries = self.entries.lock().unwrap();
        match entries.remove(uri) {
            Some(Entry::Ready(image)) => return Ok(ImagePoll::Ready { image }),
            Some(Entry::Pending) => {
                entries.insert(uri.into(), Entry::Pending);
                return Ok(ImagePoll::Pending { size: None });
            }
            Some(Entry::Failed) => {
                entries.insert(uri.into(), Entry::Failed);
                return Err(LoadError::Loading("artwork unavailable".into()));
            }
            None => {}
        }
        entries.insert(uri.into(), Entry::Pending);
        let path = self.dir.join(name.replace(|c: char| !c.is_ascii_alphanumeric() && c != '.', "_"));
        let (http, entries, ctx, uri) = (self.http.clone(), self.entries.clone(), ctx.clone(), uri.to_string());
        self.rt.spawn(async move {
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(_) => match async { http.get(&uri).send().await?.error_for_status()?.bytes().await }.await {
                    Ok(bytes) => {
                        let _ = std::fs::write(&path, &bytes);
                        Some(bytes.to_vec())
                    }
                    Err(_) => None,
                },
            };
            let image = match bytes {
                Some(bytes) => tokio::task::spawn_blocking(move || decode(&bytes)).await.ok().flatten(),
                None => None,
            };
            let entry = image.map_or(Entry::Failed, |image| Entry::Ready(Arc::new(image)));
            entries.lock().unwrap().insert(uri, entry);
            ctx.request_repaint();
        });
        Ok(ImagePoll::Pending { size: None })
    }

    fn forget(&self, uri: &str) {
        self.entries.lock().unwrap().remove(uri);
    }

    fn forget_all(&self) {
        self.entries.lock().unwrap().retain(|_, e| matches!(e, Entry::Pending));
    }

    fn byte_size(&self) -> usize {
        let entries = self.entries.lock().unwrap();
        entries.values().map(|e| if let Entry::Ready(image) = e { image.pixels.len() * 4 } else { 0 }).sum()
    }
}
