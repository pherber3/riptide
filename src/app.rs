use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::Result;
use egui::{Align, Color32, Key, Layout, RichText, Sense, Ui, Vec2, vec2};
use fastframe_now_playing as np;

use crate::art::Art;
use crate::cache;
use crate::decode::Decoder;
use crate::player::{Cmd, Event, Player};
use crate::queue::{self, Queue, Repeat};
use crate::tidal::{self, Card, Lyrics, Mix, Playlist, Quality, Shelf, Tidal, Track};
use crate::view::{Sort, View};
use crate::theme::{self, ACCENT, BAR, DANGER, DIM, Icon, LINE, SECONDARY, SIDEBAR, TEXT, bold, semibold};
use crate::widgets::{Rows, art, bar, clickable, clock, heart, icon_button, link_text, nav_item, picture, pill, play_disc, search_field, section, tier_color};

const HISTORY: usize = 30;
const ROOT: &str = "root";
const ALBUM_SORTS: &[Sort] = &[Sort::Added, Sort::Title, Sort::Artist, Sort::Year];
const NAME_SORTS: &[Sort] = &[Sort::Added, Sort::Title];

/// Tidal content the app can open as a page or play.
#[derive(Clone, PartialEq)]
pub enum Source {
    Home,
    Search(String),
    Album(u64),
    Artist(u64),
    Playlist(String),
    Mix(Mix),
    /// A playlist folder: id ("root" is the top level) and name.
    Folder(String, String),
    Tracks,
    Albums,
    Artists,
    TrackRadio(u64),
    ArtistRadio(u64),
}

impl Source {
    pub fn open(card: &Card) -> Self {
        match card {
            Card::Album(a) => Self::Album(a.id),
            Card::Artist(a) => Self::Artist(a.id),
            Card::Playlist(p) => Self::Playlist(p.id.clone()),
            Card::Mix(m) => Self::Mix(m.clone()),
            Card::Folder { id, name, .. } => Self::Folder(id.clone(), name.clone()),
        }
    }

    /// What a card's play button plays: its tracks, or an artist's radio.
    pub fn play(card: &Card) -> Option<Self> {
        match card {
            Card::Folder { .. } => None,
            Card::Artist(a) => Some(Self::ArtistRadio(a.id)),
            _ => Some(Self::open(card)),
        }
    }

    /// The remembered sort of a page that can be filtered and sorted.
    fn sort_key(&self) -> Option<&'static str> {
        match self {
            Self::Tracks => Some("tracks"),
            Self::Albums => Some("albums"),
            Self::Artists => Some("artists"),
            Self::Folder(..) => Some("playlists"),
            Self::Playlist(_) => Some("playlist"),
            _ => None,
        }
    }

    fn playlists() -> Self {
        Self::Folder(ROOT.into(), "Playlists".into())
    }
}

/// A page's title, artwork (with whether it is round) and radio.
pub struct Head {
    pub kind: &'static str,
    pub title: String,
    pub subtitle: String,
    pub art: Option<(Option<String>, bool)>,
    pub radio: Option<Source>,
}

impl Head {
    fn title(title: impl Into<String>) -> Option<Self> {
        Some(Self { kind: "", title: title.into(), subtitle: String::new(), art: None, radio: None })
    }
}

pub enum Body {
    Tracks { tracks: Vec<Track>, album_column: bool },
    Grid { cards: Vec<Card>, sorts: &'static [Sort] },
    Shelves(Vec<Shelf>),
}

impl Body {
    /// What the page's Play and Shuffle buttons play.
    pub fn tracks(&self) -> &[Track] {
        match self {
            Self::Tracks { tracks, .. } => tracks,
            Self::Shelves(shelves) => shelves.iter().find(|s| !s.tracks.is_empty()).map_or(&[], |s| &s.tracks),
            Self::Grid { .. } => &[],
        }
    }
}

pub struct Page {
    pub source: Source,
    pub head: Option<Head>,
    pub body: Body,
    /// Filter and sort, on pages that have them.
    pub view: Option<View>,
}

/// Loads any page. Playing a source loads its page too and plays the page's tracks.
async fn load(tidal: Tidal, source: Source) -> Result<Page> {
    let tracks = |tracks, album_column| Body::Tracks { tracks, album_column };
    let (head, body) = match &source {
        Source::Home => match tidal.home().await {
            Ok(shelves) if !shelves.is_empty() => (None, Body::Shelves(shelves)),
            _ => return Box::pin(load(tidal, Source::playlists())).await,
        },
        Source::Search(query) => (Head::title(format!("Results for “{query}”")), Body::Shelves(tidal.search(query).await?)),
        Source::Album(id) => {
            let (album, list) = tidal.album(*id).await?;
            let subtitle = [album.artist.as_str(), &album.year].iter().filter(|s| !s.is_empty()).copied().collect::<Vec<_>>().join(" · ");
            let art = Some((art(album.cover.as_deref(), 640), false));
            (Some(Head { kind: "ALBUM", title: album.title, subtitle, art, radio: None }), tracks(list, false))
        }
        Source::Artist(id) => {
            let (artist, top, albums) = tidal.artist(*id).await?;
            let art = Some((art(artist.picture.as_deref(), 480), true));
            let head = Head { kind: "ARTIST", title: artist.name, subtitle: String::new(), art, radio: Some(Source::ArtistRadio(*id)) };
            let top = Shelf { title: "Top tracks".into(), cards: Vec::new(), tracks: top };
            let albums = Shelf { title: "Albums".into(), cards: albums.into_iter().map(Card::Album).collect(), tracks: Vec::new() };
            (Some(head), Body::Shelves(vec![top, albums]))
        }
        Source::Playlist(id) => {
            let (playlist, list) = tidal.playlist(id).await?;
            let art = Some((art(playlist.cover.as_deref(), 640), false));
            (Some(Head { kind: "PLAYLIST", title: playlist.title, subtitle: format!("{} tracks", playlist.count), art, radio: None }), tracks(list, true))
        }
        Source::Mix(mix) => {
            let head = Head { kind: "MIX", title: mix.title.clone(), subtitle: mix.subtitle.clone(), art: Some((mix.image.clone(), false)), radio: None };
            (Some(head), tracks(tidal.mix_tracks(&mix.id).await?, true))
        }
        Source::Tracks => (Head::title("Tracks"), tracks(tidal.favorite_tracks().await?, true)),
        Source::Albums => {
            let cards = tidal.favorite_albums().await?.into_iter().map(Card::Album).collect();
            (Head::title("Albums"), Body::Grid { cards, sorts: ALBUM_SORTS })
        }
        Source::Artists => {
            let cards = tidal.favorite_artists().await?.into_iter().map(Card::Artist).collect();
            (Head::title("Artists"), Body::Grid { cards, sorts: NAME_SORTS })
        }
        Source::Folder(id, name) => (Head::title(name.clone()), Body::Grid { cards: tidal.folder(id).await?, sorts: NAME_SORTS }),
        Source::TrackRadio(id) => (Head::title("Radio"), tracks(tidal.radio("tracks", *id).await?, true)),
        Source::ArtistRadio(id) => (Head::title("Radio"), tracks(tidal.radio("artists", *id).await?, true)),
    };
    Ok(Page { source, head, body, view: None })
}

enum Msg {
    SignedIn(Tidal),
    Page(Box<Page>),
    /// Tracks to play now.
    Tracks(Vec<Track>),
    /// Radio to keep playing after the queue's last track (whose id comes first).
    Continue(u64, Vec<Track>),
    Ready(u64, Box<Decoder>),
    Favorites(HashSet<u64>),
    Folders(Vec<Card>),
    Playlists(Vec<Playlist>),
    /// A playlist just made, with a track already in it.
    Created(Playlist),
    /// A short confirmation for the top bar.
    Notice(String),
    Lyrics(u64, Lyrics),
    Error(String),
    Player(Event),
    Done,
}

pub enum Action {
    Open(Source),
    Play(Source),
    PlayTracks(Vec<Track>, usize),
    ShuffleTracks(Vec<Track>),
    /// Queue a track next (true) or last.
    Enqueue(Track, bool),
    Jump(usize),
    Move(usize, usize),
    Remove(usize),
    Favorite(u64, bool),
    /// Add a track to one of the user's playlists (by id).
    AddToPlaylist(String, u64),
    /// Make a playlist with this name and add a track to it.
    CreatePlaylist(String, u64),
    Toggle,
    Next,
    Prev,
    Seek(f64),
    Shuffle,
    Repeat,
    Sort(Sort),
    /// Back (true) or forward through history.
    Step(bool),
    Quality(Quality),
    /// Show or hide the queue panel.
    Queue,
    /// Open or close the full-window lyrics.
    Lyrics,
    FocusSearch,
}

/// A sign-in in progress and what the user pasted.
struct Login {
    flow: Option<tidal::Login>,
    pasted: String,
}

pub struct App {
    rt: tokio::runtime::Runtime,
    ctx: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    session: PathBuf,
    cache: PathBuf,
    tidal: Option<Tidal>,
    login: Option<Login>,
    busy: bool,
    error: Option<String>,
    player: Player,
    controls: np::NowPlaying,
    /// The track and playback state last given to the media controls.
    shown: (Option<u64>, np::Playback),
    page: Option<Page>,
    back: Vec<Page>,
    forward: Vec<Page>,
    loading: bool,
    /// The sort each kind of list page was last left in.
    sorts: HashMap<String, (Sort, bool)>,
    query: String,
    queue: Queue,
    favorites: HashSet<u64>,
    /// Top-level playlist folders and playlists, for the sidebar.
    folders: Vec<Card>,
    /// The user's own playlists, for "Add to playlist".
    playlists: Vec<Playlist>,
    notice: Option<String>,
    queue_open: bool,
    lyrics_open: bool,
    quality: Quality,
    volume: f32,
    dragging: Option<f64>,
    /// Where to resume after reloading the current track in another quality.
    resume_at: Option<f64>,
    /// Lyrics for a track id; None while they load.
    lyrics: Option<(u64, Option<Lyrics>)>,
    lyric_line: Option<usize>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, dir: &Path) -> Result<Self> {
        let ctx = cc.egui_ctx.clone();
        theme::install(&ctx);
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
        let cache = dir.join("cache");
        std::fs::create_dir_all(&cache)?;
        let dirs = (cache.clone(), cache.join("art"));
        rt.spawn_blocking(move || {
            let _ = cache::evict(&dirs.0, crate::CACHE_BYTES);
            let _ = cache::evict(&dirs.1, crate::ART_BYTES);
        });
        crate::fonts::install(&ctx);
        ctx.add_image_loader(Arc::new(Art::new(cache.join("art"), rt.handle().clone())));
        let (tx, rx) = channel();
        let player = Player::start({
            let (tx, ctx) = (tx.clone(), ctx.clone());
            move |event| {
                let _ = tx.send(Msg::Player(event));
                ctx.request_repaint();
            }
        });
        let controls = np::NowPlaying::start(np::App::new("riptide", "Riptide"), {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        });
        let session = tidal::session_path(dir);
        let (quality, volume, sorts) = load_settings(&session);
        player.status.set_volume(volume * volume);
        let app = Self {
            rt,
            ctx,
            tx,
            rx,
            busy: session.exists(),
            session,
            cache,
            tidal: None,
            login: None,
            error: None,
            player,
            controls,
            shown: (None, np::Playback::Stopped),
            page: None,
            back: Vec::new(),
            forward: Vec::new(),
            loading: true,
            sorts,
            query: String::new(),
            queue: Queue::default(),
            favorites: HashSet::new(),
            folders: Vec::new(),
            playlists: Vec::new(),
            notice: None,
            queue_open: false,
            lyrics_open: false,
            quality,
            volume,
            dragging: None,
            resume_at: None,
            lyrics: None,
            lyric_line: None,
        };
        if app.busy {
            let session = app.session.clone();
            app.spawn(async move { Ok(Msg::SignedIn(Tidal::load(&session).await?)) });
        }
        Ok(app)
    }

    fn spawn(&self, task: impl Future<Output = Result<Msg>> + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let _ = tx.send(task.await.unwrap_or_else(|e| Msg::Error(format!("{e:#}"))));
            ctx.request_repaint();
        });
    }

    fn save_settings(&self) {
        let mut text = format!("quality={}\nvolume={}\n", self.quality.name(), self.volume);
        for (page, (sort, reversed)) in &self.sorts {
            text += &format!("sort.{page}={sort:?}{}\n", if *reversed { " reversed" } else { "" });
        }
        let _ = std::fs::write(settings_path(&self.session), text);
    }

    /// Shows a newly loaded page, in its kind's remembered sort, and files the old one in history.
    fn show(&mut self, mut page: Page) {
        page.view = page.source.sort_key().map(|key| View::new(key, self.sorts.get(key).copied().unwrap_or_default()));
        if let Some(old) = self.page.replace(page) {
            self.back.push(old);
            if self.back.len() > HISTORY {
                self.back.remove(0);
            }
        }
        self.forward.clear();
        self.loading = false;
        crate::art::forget(&self.ctx);
    }

    /// Steps through history like a browser.
    fn step(&mut self, back: bool) {
        let (from, to) = if back { (&mut self.back, &mut self.forward) } else { (&mut self.forward, &mut self.back) };
        if let Some(page) = from.pop() {
            to.extend(self.page.replace(page));
            crate::art::forget(&self.ctx);
        }
    }

    fn play(&mut self, index: usize) {
        let Some(tidal) = self.tidal.clone() else { return };
        self.queue.index = Some(index);
        self.error = None;
        // The lyrics view never changes page, so drop the last track's big cover here.
        if self.lyrics_open {
            crate::art::forget(&self.ctx);
        }
        let (dir, quality) = (self.cache.clone(), self.quality);
        let id = self.queue.tracks[index].id;
        let next = self.queue.tracks.get(index + 1).map(|t| t.id);
        cache::keep_only(&[id, next.unwrap_or(id)].map(|id| cache::path(&dir, id, quality)));
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
            Ok(Msg::Ready(id, Box::new(decoder)))
        });
    }

    fn stop(&mut self) {
        self.queue.index = None;
        self.player.send(Cmd::Stop);
    }

    /// The next track, or radio from the last one when the queue runs out (autoplay).
    fn next(&mut self) {
        match (self.queue.after(), self.queue.current().map(|t| t.id), self.tidal.clone()) {
            (Some(i), ..) => self.play(i),
            (None, Some(id), Some(tidal)) => self.spawn(async move { Ok(Msg::Continue(id, tidal.radio("tracks", id).await?)) }),
            _ => self.stop(),
        }
    }

    fn prev(&mut self) {
        match self.queue.index {
            Some(i) if i > 0 && self.player.status.position() < 3.0 => self.play(i - 1),
            Some(_) => self.player.send(Cmd::Seek(0.0)),
            None => {}
        }
    }

    fn apply(&mut self, action: Action) {
        let Some(tidal) = self.tidal.clone() else { return };
        match action {
            Action::Open(source) => {
                (self.loading, self.error, self.notice) = (true, None, None);
                self.lyrics_open = false;
                self.spawn(async move { Ok(Msg::Page(Box::new(load(tidal, source).await?))) });
            }
            Action::Play(source) => self.spawn(async move { Ok(Msg::Tracks(load(tidal, source).await?.body.tracks().to_vec())) }),
            Action::PlayTracks(tracks, index) => {
                let start = self.queue.replace(tracks, index, false);
                self.play(start);
            }
            Action::ShuffleTracks(tracks) if !tracks.is_empty() => {
                let start = self.queue.replace(tracks.clone(), queue::random(tracks.len()), true);
                self.play(start);
            }
            Action::ShuffleTracks(_) => {}
            Action::Enqueue(track, next) => {
                if !self.queue.enqueue(track, next) {
                    self.play(0);
                }
            }
            Action::Jump(i) => self.play(i),
            Action::Move(from, to) => self.queue.move_track(from, to),
            Action::Remove(i) => self.queue.remove(i),
            Action::Favorite(id, on) => {
                if on { self.favorites.insert(id) } else { self.favorites.remove(&id) };
                self.spawn(async move {
                    tidal.set_favorite(id, on).await?;
                    Ok(Msg::Done)
                });
            }
            Action::AddToPlaylist(id, track) => {
                // The playlist moves to the top of the menu, as it was just changed.
                let Some(at) = self.playlists.iter().position(|p| p.id == id) else { return };
                let playlist = self.playlists.remove(at);
                let notice = format!("Added to {}", playlist.title);
                self.playlists.insert(0, playlist);
                self.spawn(async move {
                    tidal.add_to_playlist(&id, track).await?;
                    Ok(Msg::Notice(notice))
                });
            }
            Action::CreatePlaylist(name, track) => self.spawn(async move {
                let playlist = tidal.create_playlist(&name).await?;
                tidal.add_to_playlist(&playlist.id, track).await?;
                Ok(Msg::Created(playlist))
            }),
            Action::Toggle => self.player.send(Cmd::Toggle),
            Action::Next => self.next(),
            Action::Prev => self.prev(),
            Action::Seek(seconds) => self.player.send(Cmd::Seek(seconds)),
            Action::Shuffle => self.queue.set_shuffle(!self.queue.shuffled()),
            Action::Repeat => self.queue.cycle_repeat(),
            // Sorting by the current column again reverses it, as Tidal does.
            Action::Sort(sort) => {
                if let Some(view) = self.page.as_mut().and_then(|p| p.view.as_mut()) {
                    view.reverse = view.sort == sort && !view.reverse;
                    view.sort = sort;
                    self.sorts.insert(view.key.into(), (sort, view.reverse));
                    self.save_settings();
                }
            }
            Action::Step(back) => {
                self.lyrics_open = false;
                self.step(back);
            }
            Action::Quality(quality) => {
                self.quality = quality;
                self.save_settings();
                if let Some(i) = self.queue.index {
                    self.resume_at = Some(self.player.status.position());
                    self.play(i);
                }
            }
            Action::Queue => self.queue_open = !self.queue_open,
            Action::Lyrics => (self.lyrics_open, self.lyric_line) = (!self.lyrics_open, None),
            Action::FocusSearch => self.ctx.memory_mut(|m| m.request_focus(egui::Id::new("search"))),
        }
    }

    fn receive(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::SignedIn(tidal) => {
                    self.tidal = Some(tidal.clone());
                    (self.busy, self.login) = (false, None);
                    let (t, u, v) = (tidal.clone(), tidal.clone(), tidal.clone());
                    self.spawn(async move { Ok(Msg::Page(Box::new(load(tidal, Source::Home).await?))) });
                    self.spawn(async move { Ok(Msg::Favorites(t.favorite_ids().await?)) });
                    self.spawn(async move { Ok(Msg::Folders(u.folder(ROOT).await?)) });
                    self.spawn(async move { Ok(Msg::Playlists(v.my_playlists().await?)) });
                }
                Msg::Page(page) => self.show(*page),
                Msg::Tracks(tracks) if !tracks.is_empty() => self.apply(Action::PlayTracks(tracks, 0)),
                Msg::Tracks(_) => self.error = Some("Nothing to play here.".into()),
                Msg::Continue(after, tracks) if self.queue.current().is_some_and(|t| t.id == after) => match self.queue.extend_new(tracks) {
                    Some(i) => self.play(i),
                    None => self.stop(),
                },
                Msg::Ready(id, decoder) if self.queue.current().is_some_and(|t| t.id == id) => {
                    self.player.send(Cmd::Load(decoder));
                    if let Some(seconds) = self.resume_at.take() {
                        self.player.send(Cmd::Seek(seconds));
                    }
                }
                Msg::Continue(..) | Msg::Ready(..) | Msg::Done => {}
                Msg::Favorites(ids) => self.favorites = ids,
                Msg::Folders(cards) => self.folders = cards,
                Msg::Playlists(playlists) => self.playlists = playlists,
                Msg::Notice(text) => self.notice = Some(text),
                Msg::Created(playlist) => {
                    self.notice = Some(format!("Added to {}", playlist.title));
                    self.folders.insert(0, Card::Playlist(playlist.clone()));
                    self.playlists.insert(0, playlist);
                }
                Msg::Lyrics(id, lyrics) => (self.lyrics, self.lyric_line) = (Some((id, Some(lyrics))), None),
                Msg::Error(e) => (self.busy, self.loading, self.error) = (false, false, Some(e)),
                Msg::Player(Event::Ended) if self.queue.repeat == Repeat::One => {
                    if let Some(i) = self.queue.index {
                        self.play(i);
                    }
                }
                Msg::Player(Event::Ended) => self.next(),
                Msg::Player(Event::Error(e)) => self.error = Some(e),
            }
        }
    }

    fn media_keys(&mut self) {
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
    }

    fn login_ui(&mut self, ui: &mut Ui) {
        let mut finish = None;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.label(RichText::new("riptide").font(bold(44.0)).color(TEXT));
                ui.label(RichText::new("Your Tidal library, light and fast").size(15.0).color(SECONDARY));
                ui.add_space(32.0);
                if self.busy {
                    ui.spinner();
                } else if let Some(login) = &mut self.login {
                    ui.label(RichText::new("Sign in in your browser, then paste the address of the page you land on:").color(SECONDARY));
                    ui.add_space(8.0);
                    ui.add(egui::TextEdit::singleline(&mut login.pasted).desired_width(520.0).margin(vec2(12.0, 8.0)));
                    if let Some(flow) = &login.flow {
                        ui.hyperlink_to("Open the sign-in page again", &flow.url);
                    }
                    ui.add_space(12.0);
                    if pill(ui, Icon::Forward, "Continue", true).clicked() {
                        finish = login.flow.take().map(|flow| (flow, login.pasted.clone()));
                    }
                } else if pill(ui, Icon::Music, "Sign in with Tidal", true).clicked() {
                    let flow = tidal::Login::start();
                    let _ = open::that(&flow.url);
                    self.login = Some(Login { flow: Some(flow), pasted: String::new() });
                }
                if let Some(e) = &self.error {
                    ui.add_space(12.0);
                    ui.colored_label(DANGER, e);
                }
            });
        });
        if let Some((flow, pasted)) = finish {
            (self.busy, self.error, self.login) = (true, None, None);
            let session = self.session.clone();
            self.spawn(async move { Ok(Msg::SignedIn(flow.finish(&pasted, &session).await?)) });
        }
    }

    fn sidebar(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let open = self.page.as_ref().map(|p| &p.source);
        let frame = egui::Frame::new().fill(SIDEBAR).inner_margin(egui::Margin { left: 12, right: 12, top: 20, bottom: 8 });
        egui::Panel::left("nav").exact_size(232.0).frame(frame).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            let heading = |ui: &mut Ui, text: &str| {
                ui.add_space(22.0);
                ui.horizontal(|ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new(text).font(semibold(11.0)).color(DIM));
                });
                ui.add_space(4.0);
            };
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("riptide").font(bold(20.0)).color(TEXT));
            });
            ui.add_space(16.0);
            let search = matches!(open, Some(Source::Search(_)));
            for (icon, text, selected, action) in [(Icon::Home, "Home", open == Some(&Source::Home), Action::Open(Source::Home)), (Icon::Search, "Search", search, Action::FocusSearch)] {
                if nav_item(ui, icon, text, selected).clicked() {
                    actions.push(action);
                }
            }
            heading(ui, "COLLECTION");
            for (icon, text, source) in [(Icon::Music, "Tracks", Source::Tracks), (Icon::Disc, "Albums", Source::Albums), (Icon::Artists, "Artists", Source::Artists), (Icon::Playlists, "Playlists", Source::playlists())] {
                if nav_item(ui, icon, text, open == Some(&source)).clicked() {
                    actions.push(Action::Open(source));
                }
            }
            heading(ui, "PLAYLISTS");
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for card in &self.folders {
                    let (icon, text, selected) = match card {
                        Card::Folder { id, name, .. } => (Icon::Folder, name, matches!(open, Some(Source::Folder(o, _)) if o == id)),
                        Card::Playlist(p) => (Icon::Playlists, &p.title, matches!(open, Some(Source::Playlist(o)) if *o == p.id)),
                        _ => continue,
                    };
                    let mut response = nav_item(ui, icon, text, selected);
                    if let Card::Folder { count, .. } = card {
                        response = response.on_hover_text(format!("{count} playlists"));
                    }
                    if response.clicked() {
                        actions.push(Action::Open(Source::open(card)));
                    }
                }
            });
        });
    }

    fn player_bar(&mut self, ui: &mut Ui, mood: Option<Color32>, actions: &mut Vec<Action>) {
        let status = self.player.status.clone();
        let mut save = false;
        let frame = egui::Frame::new().fill(mood.map_or(BAR, |c| c.lerp_to_gamma(Color32::BLACK, 0.25))).inner_margin(egui::Margin::symmetric(16, 0));
        egui::Panel::bottom("player").exact_size(84.0).frame(frame).show(ui, |ui| {
            let edge = ui.clip_rect();
            ui.painter().hline(edge.x_range(), edge.top(), egui::Stroke::new(1.0, LINE));
            ui.columns(3, |cols| {
                let track = self.queue.current();
                cols[0].horizontal_centered(|ui| {
                    let Some(t) = track else { return };
                    let cover = picture(ui, art(t.cover.as_deref(), 160), 56.0, false);
                    if cover.hovered() {
                        ui.painter().rect_filled(cover.rect, 6.0, Color32::from_black_alpha(90));
                    }
                    let mut album = clickable(cover).clicked();
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.set_max_width(ui.available_width() - 40.0);
                        ui.spacing_mut().item_spacing.y = 2.0;
                        ui.add_space(23.0);
                        album |= link_text(ui, RichText::new(&t.title).font(semibold(14.0)).color(TEXT)).clicked();
                        if link_text(ui, RichText::new(&t.artist).size(13.0).color(SECONDARY)).clicked()
                            && let Some(id) = t.artist_id
                        {
                            actions.push(Action::Open(Source::Artist(id)));
                        }
                    });
                    heart(ui, t.id, &self.favorites, true, actions);
                    if album && let Some(id) = t.album_id {
                        actions.push(Action::Open(Source::Album(id)));
                    }
                });
                cols[1].vertical_centered(|ui| {
                    ui.add_space(12.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 14.0;
                        ui.add_space((ui.available_width() - 200.0) / 2.0);
                        let on = |on: bool| if on { ACCENT } else { SECONDARY };
                        let toggle = if status.playing.load(Relaxed) { Icon::Pause } else { Icon::Play };
                        let repeat = if self.queue.repeat == Repeat::One { Icon::RepeatOne } else { Icon::Repeat };
                        let buttons = [
                            (Icon::Shuffle, 16.0, on(self.queue.shuffled()), Action::Shuffle),
                            (Icon::Prev, 18.0, TEXT, Action::Prev),
                            (toggle, 18.0, TEXT, Action::Toggle),
                            (Icon::Next, 18.0, TEXT, Action::Next),
                            (repeat, 16.0, on(self.queue.repeat != Repeat::Off), Action::Repeat),
                        ];
                        for (icon, size, color, action) in buttons {
                            let response = if matches!(action, Action::Toggle) {
                                let (rect, response) = ui.allocate_exact_size(Vec2::splat(36.0), Sense::click());
                                play_disc(ui, rect.center(), 17.0, response.hovered(), icon);
                                clickable(response)
                            } else {
                                icon_button(ui, icon, size, color)
                            };
                            if response.clicked() {
                                actions.push(action);
                            }
                        }
                    });
                    let total = track.map_or(0.0, |t| f64::from(t.duration));
                    let mut position = self.dragging.unwrap_or_else(|| status.position()).min(total);
                    ui.horizontal(|ui| {
                        let time = |text: String| RichText::new(text).size(11.0).color(SECONDARY);
                        ui.label(time(clock(position)));
                        let seek = bar(ui, &mut position, total, ui.available_width() - 40.0, track.is_some());
                        if seek.dragged() {
                            self.dragging = Some(position);
                        }
                        if seek.drag_stopped() || seek.clicked() {
                            self.dragging = None;
                            actions.push(Action::Seek(position));
                        }
                        ui.label(time(clock(total)));
                    });
                });
                cols[2].with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let mut volume = f64::from(self.volume);
                    let slider = bar(ui, &mut volume, 1.0, 96.0, true);
                    if slider.changed() {
                        self.volume = volume as f32;
                        status.set_volume(self.volume * self.volume);
                    }
                    save = slider.drag_stopped() || slider.clicked();
                    ui.add(if self.volume > 0.0 { Icon::Volume } else { Icon::Muted }.image(SECONDARY, 18.0));
                    ui.add_space(6.0);
                    for (open, icon, hint, action) in [(self.queue_open, Icon::Queue, "Queue", Action::Queue), (self.lyrics_open, Icon::Lyrics, "Lyrics", Action::Lyrics)] {
                        if icon_button(ui, icon, 18.0, if open { ACCENT } else { SECONDARY }).on_hover_text(hint).clicked() {
                            actions.push(action);
                        }
                    }
                    ui.add_space(6.0);
                    let (tier, format) = status.format.lock().unwrap().clone();
                    let (tier, format) = if track.is_some() { (tier, format) } else { (self.quality, String::new()) };
                    let color = tier_color(tier);
                    let badge = egui::Button::new(RichText::new(tier.name().to_uppercase()).font(bold(11.0)).color(color)).fill(color.gamma_multiply(0.14)).corner_radius(4.0);
                    egui::containers::menu::MenuButton::from_button(badge).ui(ui, |ui| {
                        for q in Quality::ALL {
                            if ui.radio(self.quality == q, RichText::new(q.name()).color(tier_color(q))).clicked() {
                                actions.push(Action::Quality(q));
                                ui.close();
                            }
                        }
                    });
                    ui.label(RichText::new(format).size(11.0).color(mood.map_or(DIM, |_| Color32::from_white_alpha(150))));
                });
            });
        });
        if save {
            self.save_settings();
        }
    }

    /// The queue, beside the page.
    fn queue_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if !self.queue_open {
            return;
        }
        let rows = Rows { playing: self.queue.current().map(|t| t.id), favorites: &self.favorites, playlists: &self.playlists, queue: true };
        let frame = egui::Frame::new().fill(SIDEBAR).inner_margin(egui::Margin::symmetric(20, 0));
        egui::Panel::right("side").default_size(420.0).resizable(true).frame(frame).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                section(ui, "Queue");
                if self.queue.tracks.is_empty() {
                    ui.label(RichText::new("Nothing queued. Right-click a track to add it.").color(SECONDARY));
                }
                rows.show(ui, &self.queue.tracks, None, false, None, actions);
            });
        });
    }

    /// The playing track's artwork and lyrics over the whole window, in the artwork's colour.
    fn now_playing(&mut self, ui: &mut Ui, mood: Option<Color32>, actions: &mut Vec<Action>) {
        let (soft, faint) = (Color32::from_white_alpha(180), Color32::from_white_alpha(110));
        let frame = egui::Frame::new().fill(mood.unwrap_or(BAR)).inner_margin(egui::Margin { left: 56, right: 40, top: 16, bottom: 0 });
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if icon_button(ui, Icon::Down, 24.0, soft).on_hover_text("Close (Esc)").clicked() {
                    actions.push(Action::Lyrics);
                }
            });
            let Some(t) = self.queue.current() else {
                ui.centered_and_justified(|ui| ui.label(RichText::new("Nothing is playing.").font(semibold(20.0)).color(soft)));
                return;
            };
            let height = ui.available_height();
            let side = (ui.available_width() * 0.42).min(height - 100.0).max(120.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(side);
                    ui.add_space(((height - side - 70.0) / 2.0).max(0.0));
                    picture(ui, art(t.cover.as_deref(), 640), side, false);
                    ui.add_space(16.0);
                    ui.add(egui::Label::new(RichText::new(&t.title).font(semibold(20.0)).color(Color32::WHITE)).truncate());
                    ui.add(egui::Label::new(RichText::new(&t.artist).size(15.0).color(soft)).truncate());
                });
                ui.add_space(64.0);
                let lyrics = self.lyrics.as_ref().filter(|(id, _)| *id == t.id).and_then(|(_, l)| l.as_ref());
                let scroll = egui::ScrollArea::vertical().auto_shrink(false).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
                ui.vertical(|ui| scroll.show(ui, |ui| {
                    ui.add_space(height * 0.2);
                    let line = |text: &str, color| RichText::new(text).font(bold(34.0)).color(color);
                    match lyrics {
                        None => {
                            ui.spinner();
                        }
                        Some(l) if l.synced.is_empty() && l.text.is_empty() => {
                            ui.label(line("No lyrics for this track.", soft));
                        }
                        Some(l) if l.synced.is_empty() => {
                            ui.label(RichText::new(&l.text).font(semibold(24.0)).color(Color32::WHITE));
                        }
                        Some(l) => {
                            let position = self.player.status.position();
                            let now = l.synced.iter().rposition(|(at, _)| *at <= position);
                            for (n, (at, words)) in l.synced.iter().enumerate() {
                                let color = if Some(n) == now { Color32::WHITE } else { faint };
                                let words = if words.is_empty() { "♪" } else { words };
                                let response = ui.add(egui::Label::new(line(words, color)).selectable(false).sense(Sense::click()));
                                if Some(n) == now && self.lyric_line != now {
                                    response.scroll_to_me(Some(Align::Center));
                                }
                                if clickable(response).clicked() {
                                    actions.push(Action::Seek(*at));
                                }
                                ui.add_space(18.0);
                            }
                            self.lyric_line = now;
                        }
                    }
                    ui.add_space(height / 2.0);
                }));
            });
        });
    }

    fn content(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let rows = Rows { playing: self.queue.current().map(|t| t.id), favorites: &self.favorites, playlists: &self.playlists, queue: false };
        let frame = egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin { left: 28, right: 28, top: 14, bottom: 0 });
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            // The page's artwork colour, glowing down from the top.
            let cover = self.page.as_ref().and_then(|p| p.head.as_ref()?.art.as_ref()?.0.as_deref());
            if let Some(color) = cover.and_then(crate::art::tint) {
                let full = ui.clip_rect();
                let rect = egui::Rect::from_min_size(full.min, vec2(full.width(), 460.0));
                ui.painter().add(egui::Shape::gradient_rect(rect, egui::Direction::TopDown, [color.gamma_multiply(0.6), Color32::TRANSPARENT]));
            }
            ui.horizontal(|ui| {
                for (icon, enabled, back) in [(Icon::Back, !self.back.is_empty(), true), (Icon::Forward, !self.forward.is_empty(), false)] {
                    if ui.add_enabled_ui(enabled, |ui| icon_button(ui, icon, 20.0, SECONDARY)).inner.clicked() {
                        actions.push(Action::Step(back));
                    }
                }
                ui.add_space(8.0);
                let search = search_field(ui, &mut self.query, "Search", 340.0, egui::Id::new("search"));
                if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.query.trim().is_empty() {
                    actions.push(Action::Open(Source::Search(self.query.trim().into())));
                }
                if self.loading {
                    ui.spinner();
                }
                if let Some(e) = &self.error {
                    ui.add(egui::Label::new(RichText::new(e).color(DANGER)).truncate());
                } else if let Some(notice) = &self.notice {
                    ui.add(egui::Label::new(RichText::new(notice).color(SECONDARY)).truncate());
                }
            });
            ui.add_space(4.0);
            if let Some(page) = &mut self.page {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| crate::widgets::page(ui, page, &rows, actions));
            }
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.receive();
        self.media_keys();
        if self.tidal.is_none() {
            return self.login_ui(ui);
        }
        let mut actions = Vec::new();
        let typing = ui.ctx().memory(|m| m.focused().is_some());
        ui.input(|i| {
            if i.key_pressed(Key::Space) && !typing {
                actions.push(Action::Toggle);
            }
            if i.key_pressed(Key::Escape) && self.lyrics_open {
                actions.push(Action::Lyrics);
            }
            // The mouse's side buttons, Alt+arrows and the keyboard's Back key, as in a web browser.
            let alt = |key| i.modifiers.alt && i.key_pressed(key);
            if i.pointer.button_pressed(egui::PointerButton::Extra1) || alt(Key::ArrowLeft) || i.key_pressed(Key::BrowserBack) {
                actions.push(Action::Step(true));
            }
            if i.pointer.button_pressed(egui::PointerButton::Extra2) || alt(Key::ArrowRight) {
                actions.push(Action::Step(false));
            }
        });
        if self.lyrics_open
            && let (Some(id), Some(tidal)) = (self.queue.current().map(|t| t.id), self.tidal.clone())
            && self.lyrics.as_ref().is_none_or(|(loaded, _)| *loaded != id)
        {
            self.lyrics = Some((id, None));
            self.spawn(async move { Ok(Msg::Lyrics(id, tidal.lyrics(id).await?)) });
        }
        // While the lyrics are open the window takes on the artwork's colour, as in Tidal.
        let cover = self.queue.current().filter(|_| self.lyrics_open).and_then(|t| art(t.cover.as_deref(), 640));
        let mood = cover.and_then(|url| crate::art::tint(&url));
        self.player_bar(ui, mood, &mut actions);
        if self.lyrics_open {
            self.now_playing(ui, mood, &mut actions);
        } else {
            self.sidebar(ui, &mut actions);
            self.queue_panel(ui, &mut actions);
            self.content(ui, &mut actions);
        }
        for action in actions {
            self.apply(action);
        }
        // The clock moves once a second; open lyrics also wake for their next line.
        if self.player.status.playing.load(Relaxed) {
            let position = self.player.status.position();
            let lyrics = self.lyrics.as_ref().and_then(|(_, l)| l.as_ref()).filter(|_| self.lyrics_open);
            let next_line = lyrics.and_then(|l| l.synced.iter().find(|(at, _)| *at > position)).map(|(at, _)| at - position);
            ui.ctx().request_repaint_after(Duration::from_secs_f64(next_line.unwrap_or(1.0).clamp(0.02, 1.0)));
        }
    }
}

fn settings_path(session: &Path) -> PathBuf {
    session.with_file_name("settings.txt")
}

/// `key=value` lines: quality, volume and each page kind's sort (`sort.albums=Title reversed`).
fn load_settings(session: &Path) -> (Quality, f32, HashMap<String, (Sort, bool)>) {
    let text = std::fs::read_to_string(settings_path(session)).unwrap_or_default();
    let (mut quality, mut volume, mut sorts) = (Quality::Max, 1.0, HashMap::new());
    for (key, value) in text.lines().filter_map(|l| l.split_once('=')) {
        match key {
            "quality" => quality = Quality::parse(value).unwrap_or(quality),
            "volume" => volume = value.parse().unwrap_or(volume),
            _ => {
                let (name, reversed) = value.split_once(' ').map_or((value, false), |(n, r)| (n, r == "reversed"));
                if let (Some(page), Some(sort)) = (key.strip_prefix("sort."), Sort::ALL.into_iter().find(|s| format!("{s:?}") == name)) {
                    sorts.insert(page.to_string(), (sort, reversed));
                }
            }
        }
    }
    (quality, volume, sorts)
}
