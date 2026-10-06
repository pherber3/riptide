#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod art;
mod cache;
mod decode;
mod fonts;
mod lastfm;
mod player;
mod queue;
mod settings;
mod theme;
mod tidal;
mod view;
mod widgets;

use std::io::Write;
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use player::{Cmd, Event, Player};
use tidal::{Quality, Tidal};

pub const CACHE_BYTES: u64 = 2 << 30;
pub const ART_BYTES: u64 = 256 << 20;

/// The window and taskbar icon.
fn icon() -> egui::IconData {
    let image = image::load_from_memory(include_bytes!("../assets/riptide.png")).expect("bundled icon").to_rgba8();
    egui::IconData { width: image.width(), height: image.height(), rgba: image.into_raw() }
}

fn main() -> Result<()> {
    let dir = std::env::current_exe()?.parent().expect("exe has a directory").to_path_buf();
    let session = tidal::session_path(&dir);
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => {
            let mut viewport = egui::ViewportBuilder::default()
                .with_title("Riptide")
                .with_icon(icon())
                .with_inner_size([1200.0, 800.0])
                .with_min_inner_size([800.0, 500.0]);
            if let Some((rect, maximized)) = app::saved_window(&session) {
                viewport = viewport.with_position(rect.min).with_inner_size(rect.size()).with_maximized(maximized);
            }
            let options = eframe::NativeOptions {
                viewport,
                ..Default::default()
            };
            eframe::run_native("riptide", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, &dir)?))))
                .map_err(|e| anyhow!("{e}"))?;
        }
        ["play", id, rest @ ..] => {
            let quality = match rest.first() {
                Some(name) => Quality::parse(name).context("quality must be low, high or max")?,
                None => Quality::Max,
            };
            let cache_dir = dir.join("cache");
            std::fs::create_dir_all(&cache_dir)?;
            cache::evict(&cache_dir, CACHE_BYTES)?;
            let rt = tokio::runtime::Runtime::new()?;
            let tidal = rt.block_on(Tidal::load(&session))?;
            let reader = rt.block_on(cache::track(&tidal, &cache_dir, id.parse()?, quality))?;
            let (tx, rx) = channel();
            let player = Player::start(move |e| {
                let _ = tx.send(e);
            });
            player.send(Cmd::Load(Box::new(decode::Decoder::open(reader)?)));
            loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(Event::Ended) | Err(RecvTimeoutError::Disconnected) => break,
                    Ok(Event::Error(e)) => bail!(e),
                    Err(RecvTimeoutError::Timeout) => {
                        let (_, format) = player.status.format.lock().unwrap().clone();
                        print!("\r{:>5.0}s {format}", player.status.position());
                        std::io::stdout().flush()?;
                    }
                }
            }
            println!();
        }
        _ => eprintln!("usage: riptide [play <track-id> [low|high|max]]"),
    }
    Ok(())
}
