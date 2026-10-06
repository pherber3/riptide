# tidalfast — design

A light native Tidal desktop client for personal use. Reference: crmne/spotifast (MIT), cloned at `D:\Projects\spotifast`.

## Goals
- Low RAM (target 100–250 MB) and sub-second startup; snappy UI.
- Features: library (favorites, playlists), search, album/artist/playlist pages, queue (next, add, reorder, shuffle, repeat), synced lyrics, radio, media keys + Windows media overlay.
- Quality: Max (hi-res FLAC up to 24/192), Lossless (16/44 FLAC), High (AAC). Default Max.
- Out of scope: Dolby Atmos (DRM + licensed decoder), visualizer, Connect/remote control, tray, exclusive mode (maybe later, with in-app volume).

## Platform
Windows first (WASAPI shared mode). Cross-platform-friendly crates; only Windows is built and tested.

## Architecture
Three threads, Spotifast's split:
- **UI** (egui/eframe): never blocks; sends `Command`, redraws on `Event`.
- **Backend** (tokio): `tidal` module is the only code that touches `tidlers`; it exposes our own types. Auth, queue, artwork.
- **Audio**: cached stream → symphonia decode (FLAC, AAC, fMP4) → rubato resample to device rate when different → `fastframe-audio` output.

Streams download into an on-disk cache file and play while the file grows (`cache::Download`), which keeps RAM flat, makes seeking simple, and lets the next track prefetch. Artwork uses Spotifast's disk cache + 64 MB in-memory budget (`images.rs`), ported. Rule: if Spotifast has it and the port is easy, port it.

## Auth
PKCE login (needed for hi-res): open browser, user pastes the redirect URL back. Session JSON (tidlers `get_json`) saved to a file in the app data dir; token refreshed on start.

## Data location
App data and cache live next to the executable (`<exe dir>/data`, `<exe dir>/cache`) so nothing lands on C:. Cache capped at 2 GB, oldest files evicted on start.

## Errors
One status line. Token refresh is silent; a track that fails to play shows a message and skips.

## Testing
Unit tests for pure logic (manifest/parts, cache reader, resampler, channel mapping, queue, LRC parsing). Playback and UI verified by running the app.

## Milestones
1. Playback core CLI: `tidalfast login`, `tidalfast play <track-id>`.
2. App shell: window, player bar, search, album/artist/playlist pages, media keys.
3. Library + queue.
4. Lyrics + radio. (tidlers' lyrics model lacks synced `subtitles`; call the endpoint ourselves.)

Each milestone gets its own plan, written after the previous one lands.
