//! Downloading a track from Tidal into the cache, in a form that plays while it downloads.

use std::path::Path;

use anyhow::Result;
use futures_util::{StreamExt, TryStreamExt, stream};

use crate::cache::{self, Reader, Writer};
use crate::tidal::{self, HTTP, Parts, Quality, Tidal};

/// Hi-res segments fetched at once; more barely helps and just competes with everything else.
const PARALLEL: usize = 4;

/// A reader for a track, downloading it into the cache unless it is already there or on its way.
/// A failed download (the stream refused, the network gone) fails its reader.
pub fn track(tidal: &Tidal, dir: &Path, id: u64, quality: Quality) -> Result<Reader> {
    let path = cache::path(dir, id, quality);
    let (reader, writer) = cache::open(&path)?;
    if let Some(mut writer) = writer {
        let tidal = tidal.clone();
        let task = tokio::spawn(async move {
            let result = async { fetch(&tidal.stream(id, quality).await?, &mut writer).await }.await;
            writer.finish(result.map_err(|e| format!("{e:#}")));
        });
        cache::running(&path, task.abort_handle());
    }
    Ok(reader)
}

async fn fetch(parts: &Parts, w: &mut Writer) -> Result<()> {
    match parts {
        Parts::Urls(urls) => {
            for url in urls {
                let mut resp = HTTP.get(url).send().await?.error_for_status()?;
                while let Some(chunk) = resp.chunk().await? {
                    w.append(&chunk)?;
                }
            }
        }
        Parts::Segments { init, template, start, count } => {
            let init = tidal::fetch(init).await?;
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
            let mut segments = stream::iter(*start..start + count).map(|n| tidal::fetch(template.replace("$Number$", &n.to_string()))).buffered(PARALLEL);
            while let Some(segment) = segments.try_next().await? {
                if dfla.is_none() {
                    w.append(&segment)?;
                    continue;
                }
                for (_, mdat) in boxes(&segment).filter(|(kind, _)| *kind == b"mdat") {
                    w.append(mdat)?;
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
