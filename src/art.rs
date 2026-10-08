use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};

use egui::load::{ImageLoadResult, ImageLoader, ImagePoll, LoadError, SizeHint};
use egui::{Color32, ColorImage};

enum Entry {
    Pending,
    Ready(Arc<ColorImage>),
    Failed,
}

/// Loads Tidal artwork through a disk cache, decoding off the UI thread. Each decoded image is
/// handed to egui's texture cache once and then dropped, and textures not drawn for a while are let
/// go (see `sweep`), so memory holds little more than what is on screen.
pub struct Art {
    dir: PathBuf,
    rt: tokio::runtime::Handle,
    entries: Arc<Mutex<HashMap<String, Entry>>>,
}

impl Art {
    pub fn new(dir: PathBuf, rt: tokio::runtime::Handle) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        Self { dir, rt, entries: Default::default() }
    }
}

/// Each artwork decoded and not yet let go: its colour, for the glow behind its page, and when it
/// was last drawn (egui time).
static SHOWN: LazyLock<Mutex<HashMap<String, (Color32, f64)>>> = LazyLock::new(Default::default);
/// How long artwork stays after it was last drawn, in seconds.
const KEEP: f64 = 10.0;
/// Downloads at once, so scrolling a long list doesn't queue hundreds against the track stream.
static FETCHES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(6);

pub fn tint(uri: &str) -> Option<Color32> {
    SHOWN.lock().unwrap().get(uri).map(|(tint, _)| *tint)
}

/// Notes that the artwork was drawn this frame.
pub fn drawn(ctx: &egui::Context, uri: &str) {
    if let Some((_, at)) = SHOWN.lock().unwrap().get_mut(uri) {
        *at = ctx.input(|i| i.time);
    }
}

/// Lets go of artwork not drawn for a while, so scrolling through a long list doesn't keep every
/// cover it passed. Checks every couple of seconds.
pub fn sweep(ctx: &egui::Context) {
    static SWEPT: Mutex<f64> = Mutex::new(0.0);
    let now = ctx.input(|i| i.time);
    let mut swept = SWEPT.lock().unwrap();
    if now - *swept < 2.0 {
        return;
    }
    *swept = now;
    let stale: Vec<String> = SHOWN.lock().unwrap().extract_if(|_, (_, at)| now - *at > KEEP).map(|(uri, _)| uri).collect();
    stale.iter().for_each(|uri| ctx.forget_image(uri));
}

/// The artwork's average colour, weighted toward its most colourful pixels and set to one
/// brightness: vivid, but dark enough to sit behind white text.
fn mood(image: &ColorImage) -> Color32 {
    let mut sum = [0.0f32; 3];
    for p in image.pixels.iter().step_by((image.pixels.len() / 2048).max(1)) {
        let [r, g, b, _] = p.to_array().map(f32::from);
        let weight = r.max(g).max(b) - r.min(g).min(b) + 8.0;
        sum = [sum[0] + r * weight, sum[1] + g * weight, sum[2] + b * weight];
    }
    let scale = 150.0 / sum.iter().copied().fold(1.0, f32::max);
    let [r, g, b] = sum.map(|c| (c * scale) as u8);
    Color32::from_rgb(r, g, b)
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
        let Some(name) = tidal.iter().find_map(|prefix| uri.strip_prefix(prefix)) else { return Err(LoadError::NotSupported) };
        let mut entries = self.entries.lock().unwrap();
        match entries.get(uri) {
            // A decoded image is handed over once; egui keeps the texture from then on.
            Some(Entry::Ready(image)) => {
                let image = image.clone();
                entries.remove(uri);
                return Ok(ImagePoll::Ready { image });
            }
            Some(Entry::Pending) => return Ok(ImagePoll::Pending { size: None }),
            Some(Entry::Failed) => return Err(LoadError::Loading("artwork unavailable".into())),
            None => {}
        }
        entries.insert(uri.into(), Entry::Pending);
        let path = self.dir.join(name.replace(|c: char| !c.is_ascii_alphanumeric() && c != '.', "_"));
        let (entries, ctx, uri) = (self.entries.clone(), ctx.clone(), uri.to_string());
        self.rt.spawn(async move {
            // File work and decoding run on the blocking pool, off the async workers.
            let cached = path.clone();
            let mut image = tokio::task::spawn_blocking(move || decode(&std::fs::read(cached).ok()?)).await.ok().flatten();
            let fetch = |uri: String| async move {
                let _permit = FETCHES.acquire().await;
                crate::tidal::fetch(uri).await
            };
            // New artwork (a playlist just made) can be missing at the bigger sizes for a while.
            let smaller = uri.rsplit_once('/').filter(|(_, file)| *file != "320x320.jpg").map(|(base, _)| format!("{base}/320x320.jpg"));
            if image.is_none()
                && let Ok(bytes) = match fetch(uri.clone()).await {
                    Err(_) if let Some(smaller) = smaller => fetch(smaller).await,
                    result => result,
                }
            {
                image = tokio::task::spawn_blocking(move || {
                    let _ = std::fs::write(&path, &bytes);
                    decode(&bytes)
                })
                .await
                .ok()
                .flatten();
            }
            if let Some(image) = &image {
                SHOWN.lock().unwrap().insert(uri.clone(), (mood(image), ctx.input(|i| i.time)));
            }
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
