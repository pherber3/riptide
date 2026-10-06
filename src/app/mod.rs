mod library;
mod page;
mod panels;
mod playback;
mod window;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::Result;
use egui::{Key, Ui};
use fastframe_now_playing as np;

pub use library::Library;
pub use page::{Body, Head, Page, Source};
pub use window::surface_request;

use crate::art::Art;
use crate::cache;
use crate::dialogs::{self, Dialog};
use crate::lastfm::LastFm;
use crate::player::{Cmd, Player};
use crate::queue::Queue;
use crate::settings::Settings;
use crate::theme;
use crate::tidal::{self, Item, Lyrics, Quality, Tidal, Track};
use crate::view::{Sort, View};
use crate::widgets::art;
use page::load;

const HISTORY: usize = 30;
/// How long typing has to pause before searching, in seconds.
const SEARCH_PAUSE: f64 = 0.25;
pub const ROOT: &str = "root";

/// What a finished background task does to the app, on the UI thread.
type Update = Box<dyn FnOnce(&mut App) + Send>;

fn then(update: impl FnOnce(&mut App) + Send + 'static) -> Update {
    Box::new(update)
}

pub enum Action {
    Open(Source),
    Play(Source),
    /// Play these tracks from `index`, or shuffled (true) with that one first.
    PlayTracks(Vec<Track>, usize, bool),
    /// Queue a track next (true) or last.
    Enqueue(Track, bool),
    Jump(usize),
    Move(usize, usize),
    Remove(usize),
    /// Save something to the collection, or take it out.
    Save(Item, bool),
    /// Add a track to one of the user's playlists (by id).
    AddToPlaylist(String, u64),
    /// Remove the track at this position from one of the user's playlists.
    RemoveFromPlaylist(String, usize),
    /// Move a track of the user's playlist from one position to another.
    MoveInPlaylist(String, usize, usize),
    /// Move a playlist into a folder (by id; ROOT is the top level).
    MovePlaylist(String, String),
    DeletePlaylist(String),
    DeleteFolder(String),
    Dialog(Box<Dialog>),
    /// Show a track's credits (id and title).
    Credits(u64, String),
    CopyLink(String),
    ConnectLastFm,
    DisconnectLastFm,
    Toggle,
    Next,
    Prev,
    Seek(f64),
    Shuffle,
    Repeat,
    /// Sort by a column: then the other way, then back to the list's own order.
    Sort(Sort),
    ResetSort,
    /// Back (true) or forward through history.
    Step(bool),
    Quality(Quality),
    /// Show or hide the queue panel.
    Queue,
    /// Open or close the full-window lyrics.
    Lyrics,
}

pub struct App {
    rt: tokio::runtime::Runtime,
    ctx: egui::Context,
    tx: Sender<Update>,
    rx: Receiver<Update>,
    /// Where the sign-in, settings, queue and Last.fm files are kept.
    data: PathBuf,
    cache: PathBuf,
    settings: Settings,
    tidal: Option<Tidal>,
    /// A sign-in in progress: the browser flow, and the address pasted back from it.
    login: Option<(tidal::Login, String)>,
    busy: bool,
    /// A line for the top bar, and whether it is an error.
    message: Option<(String, bool)>,
    player: Player,
    controls: np::NowPlaying,
    /// The track and playback state last given to the media controls.
    shown: (Option<u64>, np::Playback),
    page: Option<Page>,
    back: Vec<Page>,
    forward: Vec<Page>,
    loading: bool,
    query: String,
    /// When to search for what's being typed (egui time), once typing pauses.
    search_due: Option<f64>,
    queue: Queue,
    /// Where the restored queue's current track was left, until it plays again.
    restored: Option<f64>,
    /// Where to pick the current track up once it loads, and whether it should be playing.
    resume_at: Option<(f64, bool)>,
    /// The track being listened to and when it started (Unix seconds), to scrobble when it ends.
    listening: Option<(Track, u64)>,
    library: Library,
    dialog: Option<Dialog>,
    queue_open: bool,
    lyrics_open: bool,
    /// Lyrics for a track id; None while they load.
    lyrics: Option<(u64, Option<Lyrics>)>,
    lyric_line: Option<usize>,
    /// How high the lyrics view's tide stands, 0 to 1, easing toward the music's loudness.
    tide: f32,
    /// The seek bar's position while it is being dragged.
    dragging: Option<f64>,
    tray: Option<fastframe_tray::Tray>,
    hidden: bool,
    quitting: bool,
    /// Scrobbling, when `data/lastfm.txt` has an API account.
    lastfm: Option<LastFm>,
    /// The palette files in `data/themes`, the shared ones among them.
    themes: fastframe_theme::Catalog<theme::Palette>,
}

impl App {
    /// The app in `home` (beside the exe): `data` for small files, `cache` for audio and art.
    pub fn new(cc: &eframe::CreationContext<'_>, home: &Path, settings: Settings) -> Result<Self> {
        let ctx = cc.egui_ctx.clone();
        let data = home.join("data");
        // The chosen theme is read now, so the first frame is already in its colours.
        let file = settings.theme.as_ref().and_then(|name| std::fs::read_to_string(data.join("themes").join(name)).ok());
        theme::install(&ctx, file.and_then(|text| fastframe_theme::parse_palette(&text).ok()).unwrap_or(theme::DARK));
        let mut themes = fastframe_theme::Catalog::default();
        themes.enable_desktop_themes(fastframe_theme::DesktopThemes {
            slug: "riptide",
            omarchy_template: fastframe_theme::omarchy::BASE_TEMPLATE,
            omarchy_previous_templates: &[],
            presets: true,
        });
        crate::fonts::install(&ctx);
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
        let cache = home.join("cache");
        std::fs::create_dir_all(&cache)?;
        let art_dir = cache.join("art");
        let evicting = (cache.clone(), art_dir.clone());
        rt.spawn_blocking(move || {
            let _ = cache::evict(&evicting.0, crate::CACHE_BYTES);
            let _ = cache::evict(&evicting.1, crate::ART_BYTES);
        });
        ctx.add_image_loader(Arc::new(Art::new(art_dir, rt.handle().clone())));
        let (tx, rx) = channel::<Update>();
        let player = Player::start(settings.device.clone(), {
            let (tx, ctx) = (tx.clone(), ctx.clone());
            move |event| {
                let _ = tx.send(then(move |app| app.player_event(event)));
                ctx.request_repaint();
            }
        });
        player.status.set_volume(settings.volume * settings.volume);
        let controls = np::NowPlaying::start(np::App::new("riptide", "Riptide"), {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        });
        let (queue, restored) = Queue::load(&data.join("queue.json")).map_or((Queue::default(), None), |(q, at)| (q, Some(at)));
        let mut app = Self {
            themes,
            busy: tidal::session_path(&data).exists(),
            lastfm: LastFm::load(LastFm::path(&data)),
            tray: window::tray(&ctx),
            rt,
            ctx,
            tx,
            rx,
            data,
            cache,
            settings,
            tidal: None,
            login: None,
            message: None,
            player,
            controls,
            shown: (None, np::Playback::Stopped),
            page: None,
            back: Vec::new(),
            forward: Vec::new(),
            loading: true,
            query: String::new(),
            search_due: None,
            queue,
            restored,
            resume_at: None,
            listening: None,
            library: Library::default(),
            dialog: None,
            queue_open: false,
            lyrics_open: false,
            lyrics: None,
            lyric_line: None,
            tide: 0.0,
            dragging: None,
            hidden: false,
            quitting: false,
        };
        // Scrobbles left over from last time, if any.
        if let Some(lastfm) = app.scrobbler() {
            app.run(async move { lastfm.scrobble(None).await }, None);
        }
        app.scan_themes();
        if app.busy {
            let session = tidal::session_path(&app.data);
            app.spawn(async move {
                let tidal = Tidal::load(&session).await?;
                Ok(then(move |app| app.signed_in(tidal)))
            });
        }
        Ok(app)
    }

    /// Runs `task` in the background; what it returns is applied on the UI thread, and an error
    /// shows in the top bar.
    fn spawn(&self, task: impl Future<Output = Result<Update>> + Send + 'static) {
        let (tx, ctx) = (self.tx.clone(), self.ctx.clone());
        self.rt.spawn(async move {
            let update = task.await.unwrap_or_else(|e| then(move |app| app.fail(format!("{e:#}"))));
            let _ = tx.send(update);
            ctx.request_repaint();
        });
    }

    /// A background call with no result but, once it's done, an optional note in the top bar.
    fn run(&self, task: impl Future<Output = Result<()>> + Send + 'static, notice: Option<String>) {
        self.spawn(async move {
            task.await?;
            Ok(then(move |app| app.note(notice)))
        });
    }

    /// A change to the library: after `task`, the library is fetched again, and so is the open page
    /// when it is part of the collection (a playlist, a folder, the saved tracks, albums or artists).
    fn changed(&self, task: impl Future<Output = Result<()>> + Send + 'static, notice: Option<String>) {
        let Some(tidal) = self.tidal.clone() else { return };
        let source = self.page.as_ref().map(|p| p.source.clone()).filter(|s| s.sort_key().is_some());
        self.spawn(async move {
            task.await?;
            let library = Library::load(&tidal).await?;
            let page = match source {
                Some(source) => Some(load(tidal, source).await?),
                None => None,
            };
            Ok(then(move |app| {
                app.library = library;
                app.note(notice);
                // The page as it is now, in place: no history, and the same filter and sort.
                if let (Some(mut page), Some(open)) = (page, app.page.as_mut())
                    && page.source == open.source
                {
                    page.view = open.view.take();
                    *open = page;
                }
            }))
        });
    }

    /// Lists the palette files again, picking up any added or edited.
    fn scan_themes(&mut self) {
        let ctx = self.ctx.clone();
        let waker = fastframe_theme::Waker::new(move || ctx.request_repaint());
        self.themes.start(self.data.join("themes"), self.settings.theme.clone(), &waker);
    }

    /// The chosen theme's colours, or Riptide's own.
    fn apply_theme(&self) {
        let chosen = self.settings.theme.as_ref().and_then(|name| self.themes.find(name));
        let palette = chosen.map_or(theme::DARK, |theme| theme.palette);
        if palette != theme::p() {
            theme::apply(&self.ctx, palette);
        }
    }

    /// Plays a new queue, remembering the page it came from.
    fn start(&mut self, tracks: Vec<Track>, index: usize, shuffle: bool, from: Option<(Source, String)>) {
        if !tracks.is_empty() {
            let start = self.queue.replace(tracks, index, shuffle);
            self.queue.from = from;
            self.play(start);
        }
    }

    fn note(&mut self, notice: Option<String>) {
        if let Some(text) = notice {
            self.message = Some((text, false));
        }
    }

    fn fail(&mut self, error: String) {
        (self.busy, self.loading, self.message) = (false, false, Some((error, true)));
    }

    fn signed_in(&mut self, tidal: Tidal) {
        (self.busy, self.login) = (false, None);
        self.tidal = Some(tidal.clone());
        self.apply(Action::Open(Source::Home));
        self.spawn(async move {
            let library = Library::load(&tidal).await?;
            Ok(then(move |app| app.library = library))
        });
    }

    /// Shows a newly loaded page, in its kind's remembered sort, and files the old one in history.
    fn show(&mut self, mut page: Page) {
        page.view = page.source.sort_key().map(|key| View::new(key, self.settings.sorts.get(key).copied().unwrap_or_default()));
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
    }

    /// Steps through history like a browser.
    fn step(&mut self, back: bool) {
        let (from, to) = if back { (&mut self.back, &mut self.forward) } else { (&mut self.forward, &mut self.back) };
        if let Some(page) = from.pop() {
            to.extend(self.page.replace(page));
        }
    }

    /// What reopening restores: settings, window, queue and position.
    fn save_state(&self) {
        self.settings.save(&self.data);
        self.queue.save(&self.data.join("queue.json"), self.restored.unwrap_or_else(|| self.player.status.position()));
    }

    /// Searches once typing pauses (or at Enter), unless those results are already showing.
    fn search(&mut self, ctx: &egui::Context) {
        let Some(due) = self.search_due else { return };
        let now = ctx.input(|i| i.time);
        if now < due {
            return ctx.request_repaint_after(Duration::from_secs_f64(due - now));
        }
        self.search_due = None;
        let query = self.query.trim().to_string();
        if !query.is_empty() && !matches!(self.page.as_ref().map(|p| &p.source), Some(Source::Search(q)) if *q == query) {
            self.apply(Action::Open(Source::Search(query)));
        }
    }

    fn apply(&mut self, action: Action) {
        let Some(tidal) = self.tidal.clone() else { return };
        match action {
            Action::Open(source) => {
                (self.loading, self.message, self.lyrics_open) = (true, None, false);
                if source == Source::Settings {
                    self.scan_themes();
                }
                self.spawn(async move {
                    let page = load(tidal, source).await?;
                    // Results for an older query than the one typed now are dropped.
                    Ok(then(move |app| {
                        if !matches!(&page.source, Source::Search(q) if *q != app.query.trim()) {
                            app.show(page);
                        }
                    }))
                });
            }
            Action::Play(source) => self.spawn(async move {
                let page = load(tidal, source).await?;
                let from = page.origin();
                let tracks = page.body.into_tracks();
                Ok(then(move |app| match tracks.is_empty() {
                    true => app.fail("Nothing to play here.".into()),
                    false => app.start(tracks, 0, false, from),
                }))
            }),
            Action::PlayTracks(tracks, index, shuffle) => {
                let from = self.page.as_ref().and_then(Page::origin);
                self.start(tracks, index, shuffle, from);
            }
            Action::Enqueue(track, next) => {
                if !self.queue.enqueue(track, next) {
                    self.play(0);
                }
            }
            Action::Jump(i) => self.play(i),
            Action::Move(from, to) => self.queue.move_track(from, to),
            Action::Remove(i) => self.queue.remove(i),
            Action::Dialog(dialog) => self.dialog = Some(*dialog),
            Action::Credits(id, title) => self.spawn(async move {
                let credits = tidal.credits(id).await?;
                Ok(then(move |app| app.dialog = Some(Dialog::Credits(title, credits))))
            }),
            Action::CopyLink(link) => {
                self.ctx.copy_text(link);
                self.note(Some("Link copied".into()));
            }
            Action::ConnectLastFm => match self.lastfm.clone() {
                Some(lastfm) => {
                    self.note(Some("Approve Riptide in the Last.fm page that just opened".into()));
                    self.spawn(async move {
                        let lastfm = lastfm.connect().await?;
                        Ok(then(move |app| {
                            app.note(lastfm.session.as_ref().map(|(_, user)| format!("Scrobbling to Last.fm as {user}")));
                            app.lastfm = Some(lastfm);
                        }))
                    });
                }
                None => self.fail(format!("Add your Last.fm API key and secret to {} first", LastFm::path(&self.data).display())),
            },
            Action::DisconnectLastFm => {
                if let Some(Err(e)) = self.lastfm.as_mut().map(LastFm::disconnect) {
                    self.fail(format!("{e:#}"));
                }
            }
            // A queue restored from last time starts where it was left.
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
            Action::Sort(_) | Action::ResetSort => {
                if let Some(view) = self.page.as_mut().and_then(|p| p.view.as_mut()) {
                    (view.sort, view.reverse) = match action {
                        Action::Sort(sort) if view.sort != sort => (sort, false),
                        Action::Sort(sort) if !view.reverse => (sort, true),
                        _ => (Sort::Added, false),
                    };
                    self.settings.sorts.insert(view.key.into(), (view.sort, view.reverse));
                    self.settings.save(&self.data);
                }
            }
            Action::Step(back) => {
                self.lyrics_open = false;
                self.step(back);
            }
            Action::Quality(quality) => {
                self.settings.quality = quality;
                self.settings.save(&self.data);
                // Reload the current track in the new quality, as it was: same spot, playing or
                // paused. A track restored from last time isn't loaded yet, so it just waits.
                if let (None, Some(i)) = (self.restored, self.queue.index) {
                    self.resume_at = Some((self.player.status.position(), self.player.status.playing.load(Relaxed)));
                    self.play(i);
                }
            }
            Action::Queue => self.queue_open = !self.queue_open,
            Action::Lyrics => (self.lyrics_open, self.lyric_line) = (!self.lyrics_open, None),
            library => self.edit(tidal, library),
        }
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
        while let Ok(update) = self.rx.try_recv() {
            update(self);
        }
        let ctx = ui.ctx().clone();
        if self.themes.poll() {
            self.apply_theme();
        }
        self.search(&ctx);
        self.media_keys();
        crate::art::sweep(&ctx);
        if self.tidal.is_none() {
            return self.login_ui(ui);
        }
        let mut actions = Vec::new();
        self.window(&ctx, &mut actions);
        let typing = ctx.memory(|m| m.focused().is_some());
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
            self.spawn(async move {
                let lyrics = tidal.lyrics(id).await?;
                Ok(then(move |app| (app.lyrics, app.lyric_line) = (Some((id, Some(lyrics))), None)))
            });
        }
        // While the lyrics are open the window takes on the artwork's colour, as in Tidal.
        let cover = self.queue.current().filter(|_| self.lyrics_open).and_then(|t| art(t.cover.as_deref(), 640));
        let mood = cover.and_then(|url| crate::art::tint(&url));
        let before = (self.settings.device.clone(), self.settings.theme.clone(), self.settings.normalize, self.settings.close_to_tray);
        // The bar takes the tint too, where its text stays light on it.
        self.player_bar(ui, mood.filter(|_| theme::p().dark), &mut actions);
        if self.lyrics_open {
            self.now_playing(ui, mood, &mut actions);
        } else {
            self.sidebar(ui, &mut actions);
            self.queue_panel(ui, &mut actions);
            self.content(ui, &mut actions);
        }
        // What the settings page changed takes effect, and is kept.
        if before != (self.settings.device.clone(), self.settings.theme.clone(), self.settings.normalize, self.settings.close_to_tray) {
            if before.0 != self.settings.device {
                self.player.send(Cmd::Device(self.settings.device.clone()));
            }
            self.apply_theme();
            self.apply_gain();
            self.settings.save(&self.data);
        }
        self.drag_label(&ctx);
        if let Some(dialog) = &mut self.dialog
            && let Some(accepted) = dialogs::show(&ctx, dialog)
        {
            match (self.dialog.take(), accepted) {
                (Some(Dialog::Confirm { then, .. }), true) => actions.push(then),
                (Some(Dialog::Form(form)), true) => self.save_form(form),
                _ => {}
            }
        }
        for action in actions {
            self.apply(action);
        }
        // The clock moves once a second; open lyrics also wake for their next line. Nothing to
        // redraw while the window is hidden in the tray.
        if self.player.status.playing.load(Relaxed) && !self.hidden {
            let position = self.player.status.position();
            let lyrics = self.lyrics.as_ref().and_then(|(_, l)| l.as_ref()).filter(|_| self.lyrics_open);
            let next_line = lyrics.and_then(|l| l.synced.iter().find(|(at, _)| *at > position)).map(|(at, _)| at - position);
            ctx.request_repaint_after(Duration::from_secs_f64(next_line.unwrap_or(1.0).clamp(0.02, 1.0)));
        }
    }
}
