#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod art;
mod cache;
mod decode;
mod dialogs;
mod discord;
mod fonts;
mod json;
mod lastfm;
mod player;
mod queue;
mod settings;
mod stream;
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

/// Where Riptide keeps its data and cache: beside the program on Windows, so one folder holds all
/// of it; elsewhere in the usual per-user place, since the program's own folder may be read-only.
fn home() -> Result<std::path::PathBuf> {
    if cfg!(windows) {
        return Ok(std::env::current_exe()?.parent().context("the program has no folder")?.into());
    }
    let user = std::path::PathBuf::from(std::env::var_os("HOME").context("no home folder")?);
    Ok(if cfg!(target_os = "macos") {
        user.join("Library/Application Support/Riptide")
    } else {
        std::env::var_os("XDG_DATA_HOME").map_or_else(|| user.join(".local/share"), Into::into).join("riptide")
    })
}

fn main() -> Result<()> {
    let dir = home()?;
    let data = dir.join("data");
    std::fs::create_dir_all(&data)?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => {
            // One copy at a time: launching again brings the running one's window up.
            let slot = fastframe_instance::Slot::at(&data, "riptide");
            let _guard = match slot.claim("show", app::surface_request) {
                fastframe_instance::Claim::First(guard) => guard,
                _ => return Ok(()),
            };
            let settings = settings::Settings::load(&data);
            let icon = egui::IconData { rgba: theme::logo(256), width: 256, height: 256 };
            let mut viewport = egui::ViewportBuilder::default()
                .with_title("Riptide")
                .with_icon(icon)
                .with_inner_size([1200.0, 800.0])
                .with_min_inner_size([800.0, 500.0])
                .with_maximized(settings.maximized);
            if let Some([x, y, width, height]) = settings.window {
                viewport = viewport.with_position([x, y]).with_inner_size([width, height]);
            }
            let options = eframe::NativeOptions { viewport, ..Default::default() };
            eframe::run_native("riptide", options, Box::new(move |cc| Ok(Box::new(app::App::new(cc, &dir, settings)?)))).map_err(|e| anyhow!("{e}"))?;
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
            let tidal = rt.block_on(Tidal::load(&tidal::session_path(&data)))?;
            let reader = {
                let _runtime = rt.enter();
                stream::track(&tidal, &cache_dir, id.parse()?, quality)?
            };
            let (tx, rx) = channel();
            let player = Player::start(None, move |e| {
                let _ = tx.send(e);
            });
            player.send(Cmd::Load(Box::new(decode::Decoder::open(reader)?), true));
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
