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
