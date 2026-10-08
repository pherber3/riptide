//! Downloading a track from Tidal into the cache, in a form that plays while it downloads.

use std::path::Path;

use anyhow::Result;
use futures_util::{StreamExt, stream};

use crate::cache::{self, Reader, Writer};
use crate::tidal::{HTTP, Parts, Quality, Tidal};

/// Hi-res segments fetched at once; more barely helps and just competes with everything else.
const PARALLEL: usize = 4;

/// Serialises starting downloads, so two requests for one track (prefetch, then play) can't both start it.
static STARTING: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A reader for a track, downloading it into the cache unless it is already there or on its way.
pub async fn track(tidal: &Tidal, dir: &Path, id: u64, quality: Quality) -> Result<Reader> {
    let path = cache::path(dir, id, quality);
    if let Some(reader) = cache::open(&path) {
        return Ok(reader);
    }
    let _starting = STARTING.lock().await;
    if let Some(reader) = cache::open(&path) {
        return Ok(reader);
    }
    let parts = tidal.stream(id, quality).await?;
    let (mut writer, reader) = cache::create(&path)?;
    let task = tokio::spawn(async move {
        let result = fetch(&parts, &mut writer).await;
        writer.finish(result.map_err(|e| e.to_string()));
    });
    cache::running(&path, task.abort_handle());
    Ok(reader)
}

async fn fetch(parts: &Parts, w: &mut Writer) -> Result<()> {
    let http = &*HTTP;
    match parts {
        Parts::Urls(urls) => {
            for url in urls {
                let mut resp = http.get(url).send().await?.error_for_status()?;
                while let Some(chunk) = resp.chunk().await? {
                    w.append(&chunk)?;
                }
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
