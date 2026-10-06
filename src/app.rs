use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::Result;
use egui::{Align, Color32, Key, Layout, RichText, Sense, Ui, Vec2, vec2};
use fastframe_now_playing as np;

use crate::art::Art;
use crate::cache;
use crate::decode::Decoder;
use crate::lastfm::{self, LastFm};
use crate::player::{Cmd, Event, Player};
use crate::queue::{self, Queue, Repeat};
use crate::tidal::{self, Card, Item, Lyrics, Mix, Playlist, Quality, Shelf, Tidal, Track};
use crate::view::{Sort, View};
use crate::theme::{self, ACCENT, BAR, DANGER, DIM, Icon, LINE, SECONDARY, SIDEBAR, TEXT, bold, semibold};
use crate::widgets::{PlaylistForm, Target, playlist_actions, Rows, art, bar, clickable, clock, heart, icon_button, link_text, nav_item, picture, pill, play_disc, search_field, section, tier_color};

const HISTORY: usize = 30;
/// How long typing has to pause before searching, in seconds.
const SEARCH_PAUSE: f64 = 0.25;
pub const ROOT: &str = "root";

/// A later launch asked for the window (see `main`), and the context to wake for it.
static SURFACE: AtomicBool = AtomicBool::new(false);
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

/// Answers a later launch: bring this copy's window up.
pub fn surface_request(request: &str) -> Option<String> {
    (request == "show").then(|| {
        SURFACE.store(true, Relaxed);
        CONTEXT.get().map(egui::Context::request_repaint);
        "ok".into()
    })
}

fn tray_icon(size: usize) -> Vec<u8> {
    let image = image::load_from_memory(include_bytes!("../assets/riptide.png")).expect("bundled icon");
    image.resize_exact(size as u32, size as u32, image::imageops::FilterType::Lanczos3).to_rgba8().into_raw()
}
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
    Settings,
    /// A browse page by its path: Explore, a genre, a "View all" (see `Tidal::page`).
    Page(String),
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
    /// What the header's heart saves, or for the user's own playlist, what its menu changes.
    pub item: Option<Item>,
    pub title: String,
    pub subtitle: String,
    pub art: Option<(Option<String>, bool)>,
    pub radio: Option<Source>,
}

impl Head {
    fn title(title: impl Into<String>) -> Option<Self> {
        Some(Self { kind: "", item: None, title: title.into(), subtitle: String::new(), art: None, radio: None })
    }
}

pub enum Body {
    Tracks { tracks: Vec<Track>, album_column: bool },
    Grid { cards: Vec<Card>, sorts: &'static [Sort] },
    Shelves(Vec<Shelf>),
    /// Drawn by the app from its own state rather than loaded.
    Settings,
}

impl Body {
    /// What the page's Play and Shuffle buttons play.
    pub fn tracks(&self) -> &[Track] {
        match self {
            Self::Tracks { tracks, .. } => tracks,
            Self::Shelves(shelves) => shelves.iter().find(|s| !s.tracks.is_empty()).map_or(&[], |s| &s.tracks),
            Self::Grid { .. } | Self::Settings => &[],
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
            (Some(Head { kind: "ALBUM", item: Some(Item::Album(*id)), title: album.title, subtitle, art, radio: None }), tracks(list, false))
        }
        Source::Artist(id) => {
            let (artist, shelves) = tidal.artist(*id).await?;
            let art = Some((art(artist.picture.as_deref(), 480), true));
            let head = Head { kind: "ARTIST", item: Some(Item::Artist(*id)), title: artist.name, subtitle: String::new(), art, radio: Some(Source::ArtistRadio(*id)) };
            (Some(head), Body::Shelves(shelves))
        }
        Source::Playlist(id) => {
            let (playlist, list) = tidal.playlist(id).await?;
            let art = Some((art(playlist.cover.as_deref(), 640), false));
            (Some(Head { kind: "PLAYLIST", item: Some(Item::Playlist(id.clone())), title: playlist.title, subtitle: format!("{} tracks", playlist.count), art, radio: None }), tracks(list, true))
        }
        Source::Mix(mix) => {
            let head = Head { kind: "MIX", item: None, title: mix.title.clone(), subtitle: mix.subtitle.clone(), art: Some((mix.image.clone(), false)), radio: None };
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
        Source::Settings => (Head::title("Settings"), Body::Settings),
        Source::Page(path) => {
            let (title, mut shelves) = tidal.page(path).await?;
            // A page of one list ("View all") shows as a grid or a track list under that list's name.
            let title = if title.is_empty() { shelves.first().map(|s| s.title.clone()).unwrap_or_default() } else { title };
            let body = match shelves.as_mut_slice() {
                [only] if only.tracks.is_empty() && only.links.is_empty() && !only.cards.is_empty() => Body::Grid { cards: std::mem::take(&mut only.cards), sorts: &[] },
                [only] if only.cards.is_empty() && only.links.is_empty() && !only.tracks.is_empty() => tracks(std::mem::take(&mut only.tracks), true),
                _ => Body::Shelves(shelves),
            };
            (Head::title(title), body)
        }
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
    Favorites((HashSet<u64>, HashSet<Item>)),
    Folders(Vec<Card>),
    Playlists(Vec<Playlist>),
    /// A playlist just made, with a track already in it.
    Created(Playlist),
    Credits(String, Vec<(String, String)>),
    LastFm(LastFm),
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
    /// Open the playlist dialog for this, with this title filled in.
    PlaylistForm(Target, String),
    ConnectLastFm,
    CloseToTray(bool),
    /// Show a track's credits (id and title).
    Credits(u64, String),
    CopyLink(String),
    /// Save an album, artist or playlist to the collection, or take it out.
    Save(Item, bool),
    /// Remove the track at this index from one of the user's playlists.
    RemoveFromPlaylist(String, usize),
    RenamePlaylist(String, String),
    /// Move a playlist into a folder (by id; ROOT is the top level).
    MovePlaylist(String, String),
    DeletePlaylist(String),
    DisconnectLastFm,
    /// Play through this output device, or the system default.
    Device(Option<String>),
    Normalize(bool),
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

/// A playlist being dragged in the sidebar: id and title.
struct Dragged(String, String);

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
    /// Saved albums, artists and playlists.
    saved: HashSet<Item>,
    /// Top-level playlist folders and playlists, for the sidebar.
    folders: Vec<Card>,
    /// A track's credits (title, then role and names), while they are shown.
    credits: Option<(String, Vec<(String, String)>)>,
    /// The Create / Rename playlist dialog, while it is open.
    form: Option<PlaylistForm>,
    /// The user's own playlists, for "Add to playlist".
    playlists: Vec<Playlist>,
    notice: Option<String>,
    queue_open: bool,
    lyrics_open: bool,
    quality: Quality,
    volume: f32,
    dragging: Option<f64>,
    /// Where to pick the current track up once it loads, and whether it should be playing.
    resume_at: Option<(f64, bool)>,
    /// Lyrics for a track id; None while they load.
    lyrics: Option<(u64, Option<Lyrics>)>,
    lyric_line: Option<usize>,
    /// When to search for what's being typed (egui time), once typing pauses.
    search_due: Option<f64>,
    /// Where the restored queue's current track was left, until it plays again.
    restored: Option<f64>,
    /// The window's last position and size while neither maximized nor minimized, and whether
    /// it is maximized.
    window: Option<egui::Rect>,
    maximized: bool,
    tray: Option<fastframe_tray::Tray>,
    /// Whether closing the window hides it in the tray rather than quitting.
    close_to_tray: bool,
    hidden: bool,
    quitting: bool,
    /// The output device by name, or the system default.
    device: Option<String>,
    /// Scale each track to Tidal's reference loudness.
    normalize: bool,
    /// Scrobbling, when `data/lastfm.txt` has an API account.
    lastfm: Option<LastFm>,
    /// The track being listened to and when it started (Unix seconds), to scrobble when it ends.
    listening: Option<(Track, u64)>,
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
        let session = tidal::session_path(dir);
        let Saved { quality, volume, sorts, device, normalize, close_to_tray } = load_settings(&session);
        let _ = CONTEXT.set(ctx.clone());
        let tray = fastframe_tray::Tray::spawn(
            fastframe_tray::Config {
                id: "riptide",
                title: "Riptide".into(),
                icon: tray_icon,
                template_icon: None,
                themed_icon: false,
                menu_on_click: false,
                menu: [("show", "Show Riptide"), ("play", "Play / Pause"), ("next", "Next"), ("previous", "Previous"), ("quit", "Quit")]
                    .into_iter()
                    .map(|(id, label)| fastframe_tray::MenuItem::action(id, label))
                    .collect(),
            },
            {
                let ctx = ctx.clone();
                move || ctx.request_repaint()
            },
        );
        let player = Player::start(device.clone(), {
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
        let (queue, restored) = Queue::load(&queue_path(&session)).map_or((Queue::default(), None), |(q, at)| (q, Some(at)));
        let (window, maximized) = saved_window(&session).map_or((None, false), |(rect, max)| (Some(rect), max));
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
            queue,
            restored,
            search_due: None,
            window,
            maximized,
            device,
            normalize,
            tray,
            close_to_tray,
            hidden: false,
            quitting: false,
            favorites: HashSet::new(),
            saved: HashSet::new(),
            folders: Vec::new(),
            playlists: Vec::new(),
            form: None,
            credits: None,
            notice: None,
            queue_open: false,
            lyrics_open: false,
            quality,
            volume,
            dragging: None,
            resume_at: None,
            lyrics: None,
            lyric_line: None,
            lastfm: LastFm::load(LastFm::path(&dir.join("data"))),
            listening: None,
        };
        // Scrobbles left over from last time, if any.
        if let Some(lastfm) = app.lastfm.clone().filter(|l| l.session.is_some()) {
            app.spawn(async move {
                lastfm.scrobble(None).await?;
                Ok(Msg::Done)
            });
        }
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
        let mut text = format!("quality={}\nvolume={}\nmaximized={}\n", self.quality.name(), self.volume, self.maximized);
        text += &format!("normalize={}\nclose_to_tray={}\n", self.normalize, self.close_to_tray);
        if let Some(device) = &self.device {
            text += &format!("device={device}\n");
        }
        if let Some(r) = self.window {
            text += &format!("window={},{},{},{}\n", r.min.x, r.min.y, r.width(), r.height());
        }
        for (page, (sort, reversed)) in &self.sorts {
            text += &format!("sort.{page}={sort:?}{}\n", if *reversed { " reversed" } else { "" });
        }
        let _ = std::fs::write(settings_path(&self.session), text);
    }

    /// Shows a newly loaded page, in its kind's remembered sort, and files the old one in history.
    fn show(&mut self, mut page: Page) {
        page.view = page.source.sort_key().map(|key| View::new(key, self.sorts.get(key).copied().unwrap_or_default()));
        // Results that refine the ones showing replace them, so typing doesn't fill the history.
        let refining = [Some(&page.source), self.page.as_ref().map(|p| &p.source)].iter().all(|s| matches!(s, Some(Source::Search(_))));
        if refining {
            self.page = Some(page);
        } else if let Some(old) = self.page.replace(page) {
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

    /// A scrobble for the track that was playing, if enough of it played.
    fn finish_listening(&mut self) -> Option<impl Future<Output = Result<()>> + Send + 'static> {
        let played = self.player.status.position();
        let (track, started) = self.listening.take()?;
        let lastfm = self.lastfm.clone().filter(|l| l.session.is_some())?;
        lastfm::counts(&track, played).then_some(async move { lastfm.scrobble(Some((&track, started))).await })
    }

    fn scrobble(&mut self) {
        if let Some(scrobble) = self.finish_listening() {
            self.spawn(async move {
                scrobble.await?;
                Ok(Msg::Done)
            });
        }
    }

    fn play(&mut self, index: usize) {
        let Some(tidal) = self.tidal.clone() else { return };
        self.restored = None;
        // Reloading the same track in another quality is still the same listen.
        if self.resume_at.is_none() {
            self.scrobble();
        }
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

    /// Makes the playlist the dialog describes (adding its track), or renames one.
    fn save_playlist(&mut self, form: PlaylistForm) {
        let PlaylistForm { target, title, description, public } = form;
        match target {
            Target::Rename(id) => self.apply(Action::RenamePlaylist(id, title.trim().into())),
            Target::Create(track) => {
                let Some(tidal) = self.tidal.clone() else { return };
                self.spawn(async move {
                    let playlist = tidal.create_playlist(title.trim(), description.trim(), public).await?;
                    if let Some(track) = track {
                        tidal.add_to_playlist(&playlist.id, track).await?;
                    }
                    Ok(Msg::Created(playlist))
                });
            }
        }
    }

    /// What reopening restores: settings, window, queue and position.
    fn save_state(&self) {
        self.save_settings();
        self.queue.save(&queue_path(&self.session), self.restored.unwrap_or_else(|| self.player.status.position()));
    }

    /// The tray's menu, a later launch asking for the window, and closing the window (which hides
    /// it in the tray instead, when that is turned on).
    fn window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use egui::ViewportCommand as Command;
        let mut show = SURFACE.swap(false, Relaxed);
        let mut hide = false;
        for event in self.tray.as_ref().map(fastframe_tray::Tray::events).unwrap_or_default() {
            match event {
                fastframe_tray::Event::Show => show = true,
                fastframe_tray::Event::Toggle | fastframe_tray::Event::Menu("show") => (show, hide) = (self.hidden, !self.hidden),
                fastframe_tray::Event::Menu("play") => actions.push(Action::Toggle),
                fastframe_tray::Event::Menu("next") => actions.push(Action::Next),
                fastframe_tray::Event::Menu("previous") => actions.push(Action::Prev),
                fastframe_tray::Event::Menu("quit") => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(Command::Close);
                }
                fastframe_tray::Event::Menu(_) => {}
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.close_to_tray && !self.quitting && self.tray.is_some() {
            ctx.send_viewport_cmd(Command::CancelClose);
            hide = true;
        }
        if hide {
            self.hidden = true;
            self.save_state();
            ctx.send_viewport_cmd(Command::Visible(false));
        }
        if show {
            self.hidden = false;
            for command in [Command::Visible(true), Command::Minimized(false), Command::Focus] {
                ctx.send_viewport_cmd(command);
            }
        }
    }

    /// The playing track's normalization, or none.
    fn apply_gain(&self) {
        let gain = self.queue.current().and_then(|t| t.gain).filter(|_| self.normalize);
        self.player.status.set_gain(gain.unwrap_or(1.0));
    }

    fn stop(&mut self) {
        self.scrobble();
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
            Action::Credits(id, title) => self.spawn(async move { Ok(Msg::Credits(title, tidal.credits(id).await?)) }),
            Action::CopyLink(link) => {
                self.ctx.copy_text(link);
                self.notice = Some("Link copied".into());
            }
            Action::PlaylistForm(target, title) => self.form = Some(PlaylistForm { target, title, description: String::new(), public: false }),
            Action::ConnectLastFm => match self.lastfm.clone() {
                Some(lastfm) => {
                    self.notice = Some("Approve Riptide in the Last.fm page that just opened".into());
                    self.spawn(async move { Ok(Msg::LastFm(lastfm.connect().await?)) });
                }
                None => {
                    let path = LastFm::path(self.session.parent().expect("data directory"));
                    self.error = Some(format!("Add your Last.fm API key and secret to {} first", path.display()));
                }
            },
            Action::Device(device) => {
                self.player.send(Cmd::Device(device.clone()));
                self.device = device;
                self.save_settings();
            }
            Action::CloseToTray(on) => {
                self.close_to_tray = on;
                self.save_settings();
            }
            Action::Normalize(on) => {
                self.normalize = on;
                self.apply_gain();
                self.save_settings();
            }
            Action::DisconnectLastFm => {
                if let Some(Err(e)) = self.lastfm.as_mut().map(LastFm::disconnect) {
                    self.error = Some(format!("{e:#}"));
                }
            }
            // A queue restored from last time starts where it was left.
            Action::Save(item, on) => {
                if on { self.saved.insert(item.clone()) } else { self.saved.remove(&item) };
                self.spawn(async move {
                    tidal.set_saved(&item, on).await?;
                    // A saved playlist joins the sidebar.
                    Ok(match item {
                        Item::Playlist(_) => Msg::Folders(tidal.folder(ROOT).await?),
                        _ => Msg::Done,
                    })
                });
            }
            Action::RemoveFromPlaylist(id, index) => {
                if let Some(Page { source: Source::Playlist(open), body: Body::Tracks { tracks, .. }, .. }) = &mut self.page
                    && *open == id
                    && index < tracks.len()
                {
                    tracks.remove(index);
                }
                self.spawn(async move {
                    tidal.remove_from_playlist(&id, index).await?;
                    Ok(Msg::Done)
                });
            }
            Action::RenamePlaylist(id, name) => {
                if let Some(Page { source: Source::Playlist(open), head: Some(head), .. }) = &mut self.page
                    && *open == id
                {
                    head.title = name.clone();
                }
                for p in self.playlists.iter_mut().chain(self.folders.iter_mut().filter_map(|c| match c {
                    Card::Playlist(p) => Some(p),
                    _ => None,
                })) {
                    if p.id == id {
                        p.title = name.clone();
                    }
                }
                self.spawn(async move {
                    tidal.rename_playlist(&id, &name).await?;
                    Ok(Msg::Notice(format!("Renamed to {name}")))
                });
            }
            Action::MovePlaylist(id, folder) => self.spawn(async move {
                tidal.arrange("move", &id, Some(&folder)).await?;
                Ok(Msg::Folders(tidal.folder(ROOT).await?))
            }),
            Action::DeletePlaylist(id) => {
                self.playlists.retain(|p| p.id != id);
                self.folders.retain(|c| !matches!(c, Card::Playlist(p) if p.id == id));
                if matches!(self.page.as_ref().map(|p| &p.source), Some(Source::Playlist(open)) if *open == id) {
                    self.step(true);
                }
                self.spawn(async move {
                    tidal.arrange("remove", &id, None).await?;
                    Ok(Msg::Notice("Playlist deleted".into()))
                });
            }
            Action::Toggle => match (self.restored, self.queue.index) {
                (Some(at), Some(i)) => {
                    self.resume_at = Some((at, true));
                    self.play(i);
                }
                _ => self.player.send(Cmd::Toggle),
            },
            Action::Next => self.next(),
            Action::Prev => self.prev(),
            Action::Seek(seconds) if self.restored.is_some() => self.restored = Some(seconds),
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
                // Reload the current track in the new quality, as it was: same spot, playing or
                // paused. A track restored from last time isn't loaded yet, so it just waits.
                if let (None, Some(i)) = (self.restored, self.queue.index) {
                    self.resume_at = Some((self.player.status.position(), self.player.status.playing.load(Relaxed)));
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
                // Results for an older query than the one typed now arrived late: drop them.
                Msg::Page(page) if matches!(&page.source, Source::Search(q) if q != self.query.trim()) => {}
                Msg::Page(page) => self.show(*page),
                Msg::Tracks(tracks) if !tracks.is_empty() => self.apply(Action::PlayTracks(tracks, 0)),
                Msg::Tracks(_) => self.error = Some("Nothing to play here.".into()),
                Msg::Continue(after, tracks) if self.queue.current().is_some_and(|t| t.id == after) => match self.queue.extend_new(tracks) {
                    Some(i) => self.play(i),
                    None => self.stop(),
                },
                Msg::Ready(id, decoder) if self.queue.current().is_some_and(|t| t.id == id) => {
                    self.apply_gain();
                    let (at, play) = self.resume_at.take().map_or((None, true), |(at, play)| (Some(at), play));
                    self.player.send(Cmd::Load(decoder, play));
                    if let Some(seconds) = at {
                        self.player.send(Cmd::Seek(seconds));
                    }
                    if self.listening.is_none()
                        && let Some(track) = self.queue.current().cloned()
                    {
                        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
                        self.listening = Some((track.clone(), now));
                        if let Some(lastfm) = self.lastfm.clone().filter(|l| l.session.is_some()) {
                            self.spawn(async move {
                                lastfm.now_playing(&track).await?;
                                Ok(Msg::Done)
                            });
                        }
                    }
                }
                Msg::Continue(..) | Msg::Ready(..) | Msg::Done => {}
                Msg::Favorites((tracks, saved)) => (self.favorites, self.saved) = (tracks, saved),
                Msg::Folders(cards) => self.folders = cards,
                Msg::Playlists(playlists) => self.playlists = playlists,
                Msg::Notice(text) => self.notice = Some(text),
                Msg::LastFm(lastfm) => {
                    self.notice = lastfm.session.as_ref().map(|(_, user)| format!("Scrobbling to Last.fm as {user}"));
                    self.lastfm = Some(lastfm);
                }
                Msg::Credits(title, credits) => self.credits = Some((title, credits)),
                Msg::Created(playlist) => {
                    self.notice = Some(format!("Created {}", playlist.title));
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
            let explore = Source::Page("pages/explore".into());
            let top = [
                (Icon::Home, "Home", open == Some(&Source::Home), Action::Open(Source::Home)),
                (Icon::Explore, "Explore", open == Some(&explore), Action::Open(explore.clone())),
                (Icon::Search, "Search", search, Action::FocusSearch),
            ];
            for (icon, text, selected, action) in top {
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
            ui.add_space(22.0);
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("PLAYLISTS").font(semibold(11.0)).color(DIM));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Plus, 16.0, SECONDARY).on_hover_text("Create playlist").clicked() {
                        actions.push(Action::PlaylistForm(Target::Create(None), String::new()));
                    }
                });
            });
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for card in &self.folders {
                    let (icon, text, selected) = match card {
                        Card::Folder { id, name, .. } => (Icon::Folder, name, matches!(open, Some(Source::Folder(o, _)) if o == id)),
                        Card::Playlist(p) => (Icon::Playlists, &p.title, matches!(open, Some(Source::Playlist(o)) if *o == p.id)),
                        _ => continue,
                    };
                    let mut response = nav_item(ui, icon, text, selected);
                    match card {
                        // A playlist dragged onto a folder moves into it.
                        Card::Folder { id, count, .. } => {
                            if response.dnd_hover_payload::<Dragged>().is_some() {
                                ui.painter().rect_stroke(response.rect, 8.0, egui::Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
                            }
                            if let Some(dragged) = response.dnd_release_payload::<Dragged>() {
                                actions.push(Action::MovePlaylist(dragged.0.clone(), id.clone()));
                            }
                            response = response.on_hover_text(format!("{count} playlists"));
                        }
                        Card::Playlist(p) => {
                            if response.drag_started() {
                                egui::DragAndDrop::set_payload(ui.ctx(), Dragged(p.id.clone(), p.title.clone()));
                            }
                            let mine = self.playlists.iter().any(|own| own.id == p.id);
                            response.context_menu(|ui| match mine {
                                true => playlist_actions(ui, &p.id, &p.title, &self.folders, actions),
                                false => {
                                    if ui.button("Remove from your Playlists").clicked() {
                                        actions.push(Action::Save(Item::Playlist(p.id.clone()), false));
                                        ui.close();
                                    }
                                }
                            });
                        }
                        _ => {}
                    }
                    if response.clicked() {
                        actions.push(Action::Open(Source::open(card)));
                    }
                }
            });
        });
    }

    /// The playlist being dragged in the sidebar, drawn under the pointer.
    fn drag_label(&self, ctx: &egui::Context) {
        let (Some(dragged), Some(at)) = (egui::DragAndDrop::payload::<Dragged>(ctx), ctx.pointer_interact_pos()) else { return };
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("dragged playlist")));
        let galley = painter.layout_no_wrap(dragged.1.clone(), semibold(13.0), TEXT);
        let rect = egui::Rect::from_min_size(at + vec2(14.0, 6.0), galley.size() + vec2(20.0, 12.0));
        painter.rect_filled(rect, 8.0, theme::HOVER);
        painter.galley(rect.min + vec2(10.0, 6.0), galley, TEXT);
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
                    let mut position = self.dragging.or(self.restored).unwrap_or_else(|| status.position()).min(total);
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
                    egui::containers::menu::MenuButton::from_button(badge).ui(ui, |ui| quality_choices(ui, self.quality, actions));
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
        let rows = Rows { playing: self.queue.current().map(|t| t.id), favorites: &self.favorites, playlists: &self.playlists, saved: &self.saved, folders: &self.folders, editing: None, queue: true };
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
        let editing = match self.page.as_ref().map(|p| &p.source) {
            Some(Source::Playlist(id)) if self.playlists.iter().any(|p| p.id == *id) => Some(id.clone()),
            _ => None,
        };
        let rows = Rows {
            playing: self.queue.current().map(|t| t.id),
            favorites: &self.favorites,
            playlists: &self.playlists,
            saved: &self.saved,
            folders: &self.folders,
            editing: editing.as_deref(),
            queue: false,
        };
        let (quality, lastfm_user) = (self.quality, self.lastfm.as_ref().and_then(|l| l.session.as_ref()).map(|(_, user)| user.clone()));
        let (device, normalize, close_to_tray) = (self.device.clone(), self.normalize, self.close_to_tray);
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
                if search.changed() {
                    self.search_due = Some(ui.input(|i| i.time) + SEARCH_PAUSE);
                }
                if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    self.search_due = Some(0.0);
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if icon_button(ui, Icon::Settings, 20.0, SECONDARY).on_hover_text("Settings").clicked() {
                        actions.push(Action::Open(Source::Settings));
                    }
                    ui.add_space(8.0);
                    if self.loading {
                        ui.spinner();
                    }
                    let message = |text: &str, color| egui::Label::new(RichText::new(text).color(color)).truncate();
                    if let Some(e) = &self.error {
                        ui.add(message(e, DANGER));
                    } else if let Some(notice) = &self.notice {
                        ui.add(message(notice, SECONDARY));
                    }
                });
            });
            ui.add_space(4.0);
            if let Some(page) = &mut self.page {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    crate::widgets::page(ui, page, &rows, actions);
                    if matches!(page.body, Body::Settings) {
                        let data = self.session.parent().and_then(Path::parent).unwrap_or(Path::new("."));
                        let state = crate::settings::State { quality, device: device.as_deref(), normalize, close_to_tray, lastfm_user: lastfm_user.as_deref(), data };
                        crate::settings::page(ui, &state, actions);
                    }
                });
            }
        });
    }
}

impl eframe::App for App {
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.save_state();
        if let Some(scrobble) = self.finish_listening() {
            let _ = self.rt.block_on(tokio::time::timeout(Duration::from_secs(3), scrobble));
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.receive();
        // Search once typing pauses (or at Enter), unless those results are already showing.
        if let Some(due) = self.search_due {
            let now = ui.input(|i| i.time);
            if now < due {
                ui.ctx().request_repaint_after(Duration::from_secs_f64(due - now));
            } else {
                self.search_due = None;
                let query = self.query.trim().to_string();
                let showing = matches!(self.page.as_ref().map(|p| &p.source), Some(Source::Search(q)) if *q == query);
                if !query.is_empty() && !showing {
                    self.apply(Action::Open(Source::Search(query)));
                }
            }
        }
        self.media_keys();
        ui.input(|i| {
            let v = i.viewport();
            self.maximized = v.maximized.unwrap_or(false);
            if !self.maximized && v.minimized != Some(true) && v.outer_rect.is_some() {
                self.window = v.outer_rect.zip(v.inner_rect).map(|(outer, inner)| egui::Rect::from_min_size(outer.min, inner.size()));
            }
        });
        if self.tidal.is_none() {
            return self.login_ui(ui);
        }
        let mut actions = Vec::new();
        self.window(&ui.ctx().clone(), &mut actions);
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
        self.drag_label(ui.ctx());
        if let Some((title, credits)) = &self.credits
            && crate::widgets::credits_dialog(ui.ctx(), title, credits)
        {
            self.credits = None;
        }
        if let Some(form) = &mut self.form {
            match crate::widgets::playlist_dialog(ui.ctx(), form) {
                Some(true) => {
                    let form = self.form.take().expect("dialog is open");
                    self.save_playlist(form);
                }
                Some(false) => self.form = None,
                None => {}
            }
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

/// The settings menu behind the gear at the top right.
fn quality_choices(ui: &mut Ui, current: Quality, actions: &mut Vec<Action>) {
    for q in Quality::ALL {
        if ui.radio(current == q, RichText::new(q.name()).color(tier_color(q))).clicked() {
            actions.push(Action::Quality(q));
            ui.close();
        }
    }
}

fn queue_path(session: &Path) -> PathBuf {
    session.with_file_name("queue.json")
}

/// The window as it was left: outer position and inner size, and whether it was maximized.
pub fn saved_window(session: &Path) -> Option<(egui::Rect, bool)> {
    let text = std::fs::read_to_string(settings_path(session)).ok()?;
    let value = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('='));
    let n: Vec<f32> = value("window")?.split(',').filter_map(|n| n.parse().ok()).collect();
    let [x, y, w, h] = n[..] else { return None };
    Some((egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)), value("maximized") == Some("true")))
}

fn settings_path(session: &Path) -> PathBuf {
    session.with_file_name("settings.txt")
}

/// `key=value` lines: quality, volume and each page kind's sort (`sort.albums=Title reversed`).
/// What `settings.txt` holds, besides the window.
struct Saved {
    close_to_tray: bool,
    quality: Quality,
    volume: f32,
    sorts: HashMap<String, (Sort, bool)>,
    device: Option<String>,
    normalize: bool,
}

fn load_settings(session: &Path) -> Saved {
    let text = std::fs::read_to_string(settings_path(session)).unwrap_or_default();
    let mut s = Saved { quality: Quality::Max, volume: 1.0, sorts: HashMap::new(), device: None, normalize: false, close_to_tray: false };
    for (key, value) in text.lines().filter_map(|l| l.split_once('=')) {
        match key {
            "quality" => s.quality = Quality::parse(value).unwrap_or(s.quality),
            "volume" => s.volume = value.parse().unwrap_or(s.volume),
            "device" => s.device = Some(value.into()),
            "normalize" => s.normalize = value == "true",
            "close_to_tray" => s.close_to_tray = value == "true",
            _ => {
                let (name, reversed) = value.split_once(' ').map_or((value, false), |(n, r)| (n, r == "reversed"));
                if let (Some(page), Some(sort)) = (key.strip_prefix("sort."), Sort::ALL.into_iter().find(|s| format!("{s:?}") == name)) {
                    s.sorts.insert(page.to_string(), (sort, reversed));
                }
            }
        }
    }
    s
}
