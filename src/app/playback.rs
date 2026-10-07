use std::future::Future;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

use anyhow::Result;
use fastframe_now_playing as np;

use super::{Action, App, Source, then};
use crate::cache;
use crate::decode::Decoder;
use crate::lastfm;
use crate::player::{Cmd, Event};
use crate::queue::Repeat;
use crate::tidal;

impl App {
    pub(super) fn play(&mut self, index: usize) {
        let Some(tidal) = self.tidal.clone() else { return };
        self.restored = None;
        // Reloading the same track in another quality is still the same listen.
        if self.resume_at.is_none() {
            self.scrobble();
        }
        self.queue.index = Some(index);
        self.message = None;
        let (dir, quality) = (self.cache.clone(), self.settings.quality);
        let id = self.queue.tracks[index].id;
        let next = self.queue.tracks.get(index + 1).map(|t| t.id);
        cache::keep_only(&[id, next.unwrap_or(id)].map(|id| cache::path(&dir, id, quality)));
        let evicting = dir.clone();
        self.rt.spawn_blocking(move || cache::evict(&evicting, crate::CACHE_BYTES));
        self.spawn(async move {
            let reader = cache::track(&tidal, &dir, id, quality).await?;
            // Download the next track once this one is in, so it starts instantly without slowing this one.
            if let Some(next) = next {
                let download = reader.download();
                tokio::spawn(async move {
                    download.finished().await;
                    cache::track(&tidal, &dir, next, quality).await
                });
            }
            // Opening reads the stream's first bytes, so it happens here rather than on the audio thread.
            let decoder = tokio::task::spawn_blocking(move || Decoder::open(reader)).await??;
            Ok(then(move |app| app.ready(id, Box::new(decoder))))
        });
    }

    /// A track opened and ready: it plays (or waits, paused, after a quality change), and its listen begins.
    fn ready(&mut self, id: u64, decoder: Box<Decoder>) {
        if self.queue.current().is_none_or(|t| t.id != id) {
            return;
        }
        self.apply_gain();
        let (at, play) = self.resume_at.take().map_or((None, true), |(at, play)| (Some(at), play));
        self.player.send(Cmd::Load(decoder, play));
        if let Some(seconds) = at {
            self.player.send(Cmd::Seek(seconds));
        }
        if self.listening.is_none()
            && let Some(track) = self.queue.current().cloned()
        {
            self.listening = Some((track.clone(), tidal::now()));
            if let Some(lastfm) = self.scrobbler() {
                self.run(async move { lastfm.now_playing(&track).await }, None);
            }
        }
    }

    pub(super) fn player_event(&mut self, event: Event) {
        match event {
            Event::Ended if self.queue.repeat == Repeat::One => {
                if let Some(i) = self.queue.index {
                    self.play(i);
                }
            }
            Event::Ended => self.next(),
            Event::Error(e) => self.fail(e),
        }
    }

    /// The playing track's normalization, or none.
    pub(super) fn apply_gain(&self) {
        let gain = self.queue.current().and_then(|t| t.gain).filter(|_| self.settings.normalize);
        self.player.status.set_gain(gain.unwrap_or(1.0));
    }

    pub(super) fn scrobbler(&self) -> Option<lastfm::LastFm> {
        self.lastfm.clone().filter(|l| l.session.is_some())
    }

    /// A scrobble for the track that was playing, if enough of it played.
    pub(super) fn finish_listening(&mut self) -> Option<impl Future<Output = Result<()>> + Send + 'static> {
        let played = self.player.status.position();
        let (track, started) = self.listening.take()?;
        let lastfm = self.scrobbler()?;
        lastfm::counts(&track, played).then_some(async move { lastfm.scrobble(Some((&track, started))).await })
    }

    fn scrobble(&mut self) {
        if let Some(scrobble) = self.finish_listening() {
            self.run(scrobble, None);
        }
    }

    pub(super) fn stop(&mut self) {
        self.scrobble();
        self.queue.index = None;
        self.player.send(Cmd::Stop);
    }

    /// The next track, or radio from the last one when the queue runs out (autoplay), unless the
    /// queue was a radio already: that ends where it ends.
    pub(super) fn next(&mut self) {
        let radio = matches!(self.queue.from, Some((Source::TrackRadio(_) | Source::ArtistRadio(_), _)));
        match (self.queue.after(), self.queue.current().map(|t| t.id), self.tidal.clone()) {
            (Some(i), ..) => self.play(i),
            (None, Some(id), Some(tidal)) if !radio => self.spawn(async move {
                let radio = tidal.radio("tracks", id).await?;
                Ok(then(move |app| {
                    if app.queue.current().is_some_and(|t| t.id == id) {
                        match app.queue.extend_new(radio) {
                            Some(i) => app.play(i),
                            None => app.stop(),
                        }
                    }
                }))
            }),
            _ => self.stop(),
        }
    }

    pub(super) fn prev(&mut self) {
        match self.queue.index {
            Some(i) if i > 0 && self.player.status.position() < 3.0 => self.play(i - 1),
            Some(_) => self.player.send(Cmd::Seek(0.0)),
            None => {}
        }
    }

    /// The system's media keys and overlay, commands in and what is playing out; and what is playing
    /// on Discord.
    pub(super) fn media_keys(&mut self) {
        let playing = self.player.status.playing.load(Relaxed);
        for command in self.controls.commands() {
            match command {
                np::Command::PlayPause => self.apply(Action::Toggle),
                np::Command::Play if !playing => self.apply(Action::Toggle),
                np::Command::Pause | np::Command::Stop if playing => self.apply(Action::Toggle),
                np::Command::Next => self.next(),
                np::Command::Previous => self.prev(),
                np::Command::SetPosition { position, .. } => self.apply(Action::Seek(position.as_secs_f64())),
                np::Command::Raise => self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus),
                _ => {}
            }
        }
        let current = self.queue.current();
        let playback = match (current, playing) {
            (None, _) => np::Playback::Stopped,
            (Some(_), true) => np::Playback::Playing,
            (Some(_), false) => np::Playback::Paused,
        };
        let shown = (current.map(|t| t.id), playback);
        if shown != self.shown {
            let track = current.map(|t| np::Track {
                id: t.id.to_string(),
                title: t.title.clone(),
                artists: vec![t.artist.clone()],
                album: t.album.clone(),
                duration: Some(Duration::from_secs(t.duration.into())),
                art_url: t.cover.as_deref().map(|c| tidal::image(c, 640)),
                ..Default::default()
            });
            let position = Duration::from_secs_f64(self.player.status.position());
            self.controls.update(np::State { playback, track, position, ..Default::default() });
            self.shown = shown;
        }
        let started = tidal::now() as i64 - self.player.status.position().round() as i64;
        self.discord.show(current.filter(|_| playing && self.settings.discord).map(|t| (t, started)));
    }
}
