use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use egui::{Align, Color32, Key, Layout, Rect, RichText, Sense, Ui, vec2};
use fastframe_now_playing as np;
use tokio::sync::Mutex;

use crate::art::Art;
use crate::cache;
use crate::player::{Cmd, Event, Player};
use crate::tidal::{self, Album, Artist, Card, Lyrics, Mix, Playlist, Quality, Results, Shelf, Tidal, Track};

const ACCENT: Color32 = Color32::from_rgb(0x33, 0xff, 0xee);
const GOLD: Color32 = Color32::from_rgb(0xf5, 0xc5, 0x42);
const ROW: f32 = 22.0;

enum Page {
    Home(Vec<Shelf>),
    Mix(Mix, Vec<Track>),
    Search(Results),
    Album(Album, Vec<Track>),
    Artist(Artist, Vec<Track>, Vec<Album>),
    Playlist(Playlist, Vec<Track>),
    Tracks(Vec<Track>),
    Albums(Vec<Album>),
    Artists(Vec<Artist>),
    Playlists(Vec<Playlist>),
    Queue,
    Lyrics,
    Loading,
}

#[derive(Clone, Copy, PartialEq)]
enum Library {
    Tracks,
    Albums,
    Artists,
    Playlists,
}

#[derive(Clone, Copy, PartialEq)]
enum Repeat {
    Off,
    All,
    One,
}

enum Msg {
    Page(Page),
    Ready(u64, cache::Reader),
    SignedIn(Box<Tidal>),
    Favorites(HashSet<u64>),
    Done,
    Error(String),
    Radio(Vec<Track>),
    /// Radio to keep playing after the queue's last track (whose id comes first).
    Continue(u64, Vec<Track>),
    Lyrics(u64, Option<Lyrics>),
    Player(Event),
}

enum Action {
    Search,
    Home,
    Mix(Mix),
    SearchPage,
    Album(u64),
    Artist(u64),
    Playlist(String),
    Library(Library),
    Queue,
    Play(Vec<Track>, usize),
    PlayNext(Track),
    AddToQueue(Track),
    Jump(usize),
    Move(usize, usize),
    Remove(usize),
    Favorite(u64, bool),
    Toggle,
    Next,
    Prev,
    Seek(f64),
    Shuffle,
    ShufflePlay(Vec<Track>),
    TrackRadio(u64),
    ArtistRadio(u64),
    Lyrics,
    Repeat,
    Sort(Sort),
    Back,
    Quality(Quality),
}

/// A sign-in in progress: the client holding the PKCE secret, the URL to open and what the user pasted.
struct Login {
    client: Option<Tidal>,
    url: String,
    pasted: String,
}

pub struct App {
    rt: tokio::runtime::Runtime,
    ctx: egui::Context,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    session: PathBuf,
    cache: PathBuf,
    tidal: Option<Arc<Mutex<Tidal>>>,
    login: Option<Login>,
    busy: bool,
    error: Option<String>,
    player: Player,
    controls: np::NowPlaying,
    shown: np::State,
    page: Page,
    back: Vec<Page>,
    view: View,
    query: String,
    queue: Vec<Track>,
    index: Option<usize>,
    /// The queue's order before shuffling, while shuffle is on.
    unshuffled: Option<Vec<Track>>,
    repeat: Repeat,
    favorites: HashSet<u64>,
    quality: Quality,
    volume: f32,
    dragging: Option<f64>,
    /// Where to resume after reloading the current track in another quality.
    resume_at: Option<f64>,
    /// Lyrics for a track id: None while loading, Some(None) when it has none.
    lyrics: Option<(u64, Option<Option<Lyrics>>)>,
    lyric_line: Option<usize>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, dir: &Path) -> Result<Self> {
        let ctx = cc.egui_ctx.clone();
        ctx.set_visuals(egui::Visuals::dark());
        ctx.style_mut_of(egui::Theme::Dark, |s| s.visuals.hyperlink_color = Color32::from_gray(170));
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
        let cache = dir.join("cache");
        std::fs::create_dir_all(&cache)?;
        cache::evict(&cache, crate::CACHE_BYTES)?;
        fastframe_fonts::FontSetup::default().install(&ctx);
        egui_extras::install_image_loaders(&ctx);
        ctx.add_bytes_loader(Arc::new(Art::new(cache.join("art"), rt.handle().clone())));
        let (tx, rx) = channel();
        let player = Player::start({
            let (tx, ctx) = (tx.clone(), ctx.clone());
            move |event| {
                let _ = tx.send(Msg::Player(event));
                ctx.request_repaint();
            }
        });
        let controls = np::NowPlaying::start(np::App::new("tidalfast", "tidalfast"), {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        });
        let session = tidal::session_path(dir);
        let (quality, volume) = load_settings(&session);
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
            shown: np::State::default(),
            page: Page::Loading,
            back: Vec::new(),
            view: View::default(),
            query: String::new(),
            queue: Vec::new(),
            index: None,
            unshuffled: None,
            repeat: Repeat::Off,
            favorites: HashSet::new(),
            quality,
            volume,
            dragging: None,
            resume_at: None,
            lyrics: None,
            lyric_line: None,
        };
        if app.busy {
            let session = app.session.clone();
            app.spawn(async move { Ok(Msg::SignedIn(Box::new(Tidal::load(&session).await?))) });
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

    fn show(&mut self, page: Page) {
        self.view = View::default();
        let old = std::mem::replace(&mut self.page, page);
        if !matches!(old, Page::Loading) {
            self.back.push(old);
        }
        self.ctx.forget_all_images();
    }

    fn navigate(&mut self, load: impl Future<Output = Result<Page>> + Send + 'static) {
        self.show(Page::Loading);
        self.spawn(async move { Ok(Msg::Page(load.await?)) });
    }

    fn current(&self) -> Option<&Track> {
        self.queue.get(self.index?)
    }

    fn play(&mut self, index: usize) {
        let Some(tidal) = self.tidal.clone() else { return };
        self.index = Some(index);
        self.error = None;
        let (dir, quality) = (self.cache.clone(), self.quality);
        let id = self.queue[index].id;
        let (t, d) = (tidal.clone(), dir.clone());
        self.spawn(async move { Ok(Msg::Ready(id, cache::track(&t, &d, id, quality).await?)) });
        // Download the next track behind this one so it starts instantly.
        if let Some(next) = self.queue.get(index + 1).map(|t| t.id) {
            self.rt.spawn(async move { cache::track(&tidal, &dir, next, quality).await });
        }
    }

    fn next(&mut self) {
        match self.index {
            Some(i) if i + 1 < self.queue.len() => self.play(i + 1),
            Some(_) if self.repeat == Repeat::All => self.play(0),
            Some(i) if let Some(tidal) = self.tidal.clone() => {
                let id = self.queue[i].id;
                self.spawn(async move { Ok(Msg::Continue(id, tidal.lock().await.track_radio(id).await?)) });
            }
            _ => {
                self.index = None;
                self.player.send(Cmd::Stop);
            }
        }
    }

    fn prev(&mut self) {
        match self.index {
            Some(i) if i > 0 && self.player.status.position() < 3.0 => self.play(i - 1),
            Some(_) => self.player.send(Cmd::Seek(0.0)),
            None => {}
        }
    }

    /// Adds to the queue at `at` (also keeping the unshuffled order in step), or starts playing when idle.
    fn enqueue(&mut self, track: Track, at: usize) {
        if self.index.is_none() {
            (self.queue, self.unshuffled) = (vec![track], None);
            return self.play(0);
        }
        if let Some(order) = &mut self.unshuffled {
            order.push(track.clone());
        }
        self.queue.insert(at.min(self.queue.len()), track);
    }

    fn set_shuffle(&mut self, on: bool) {
        let current = self.current().map(|t| t.id);
        if on {
            self.unshuffled = Some(self.queue.clone());
            let from = self.index.map_or(0, |i| i + 1);
            shuffle(&mut self.queue[from..]);
        } else if let Some(order) = self.unshuffled.take() {
            self.queue = order;
            self.index = current.and_then(|id| self.queue.iter().position(|t| t.id == id));
        }
    }

    fn apply(&mut self, action: Action) {
        let Some(tidal) = self.tidal.clone() else { return };
        match action {
            Action::Search => {
                let query = self.query.trim().to_string();
                if !query.is_empty() {
                    self.navigate(async move { Ok(Page::Search(tidal.lock().await.search(&query).await?)) });
                }
            }
            Action::Album(id) => self.navigate(async move {
                let (album, tracks) = tidal.lock().await.album(id).await?;
                Ok(Page::Album(album, tracks))
            }),
            Action::Artist(id) => self.navigate(async move {
                let (artist, top, albums) = tidal.lock().await.artist(id).await?;
                Ok(Page::Artist(artist, top, albums))
            }),
            Action::Playlist(id) => self.navigate(async move {
                let (playlist, tracks) = tidal.lock().await.playlist(&id).await?;
                Ok(Page::Playlist(playlist, tracks))
            }),
            Action::Library(kind) => self.navigate(async move {
                let mut t = tidal.lock().await;
                Ok(match kind {
                    Library::Tracks => Page::Tracks(t.favorite_tracks().await?),
                    Library::Albums => Page::Albums(t.favorite_albums().await?),
                    Library::Artists => Page::Artists(t.favorite_artists().await?),
                    Library::Playlists => Page::Playlists(t.playlists().await?),
                })
            }),
            Action::Queue => self.show(Page::Queue),
            Action::Home => self.navigate(home_page(tidal)),
            Action::Mix(mix) => self.navigate(async move {
                let tracks = tidal.lock().await.mix_tracks(&mix.id).await?;
                Ok(Page::Mix(mix, tracks))
            }),
            Action::Lyrics if matches!(self.page, Page::Lyrics) => self.apply(Action::Back),
            Action::Lyrics => self.show(Page::Lyrics),
            Action::ShufflePlay(mut tracks) => {
                self.unshuffled = Some(tracks.clone());
                shuffle(&mut tracks);
                self.queue = tracks;
                self.play(0);
            }
            Action::TrackRadio(id) => self.spawn(async move { Ok(Msg::Radio(tidal.lock().await.track_radio(id).await?)) }),
            Action::ArtistRadio(id) => self.spawn(async move { Ok(Msg::Radio(tidal.lock().await.artist_radio(id).await?)) }),
            // Sorting by the current column again reverses it, as Tidal does.
            Action::Sort(sort) => {
                let view = &mut self.view;
                view.reverse = view.sort == sort && !view.reverse;
                (view.sort, view.stale) = (sort, true);
            }
            // Back to the last search results rather than searching again.
            Action::SearchPage if !matches!(self.page, Page::Search(_)) => {
                let at = self.back.iter().rposition(|p| matches!(p, Page::Search(_)));
                let page = at.map_or(Page::Search(Results::default()), |at| self.back.remove(at));
                self.show(page);
            }
            Action::SearchPage => {}
            Action::Play(mut tracks, index) => {
                if self.unshuffled.is_some() {
                    // Shuffle stays on: the clicked track first, the rest in random order.
                    self.unshuffled = Some(tracks.clone());
                    let first = tracks.remove(index);
                    shuffle(&mut tracks);
                    tracks.insert(0, first);
                    self.queue = tracks;
                    return self.play(0);
                }
                self.queue = tracks;
                self.play(index);
            }
            Action::PlayNext(track) => {
                let at = self.index.map_or(0, |i| i + 1);
                self.enqueue(track, at);
            }
            Action::AddToQueue(track) => self.enqueue(track, usize::MAX),
            Action::Jump(i) => self.play(i),
            Action::Move(from, to) => {
                let track = self.queue.remove(from);
                self.queue.insert(to, track);
                self.index = self.index.map(|i| match i {
                    i if i == from => to,
                    i if from < i && i <= to => i - 1,
                    i if to <= i && i < from => i + 1,
                    i => i,
                });
            }
            Action::Remove(i) => {
                self.queue.remove(i);
                self.index = self.index.map(|c| if i < c { c - 1 } else { c });
            }
            Action::Favorite(id, on) => {
                if on { self.favorites.insert(id) } else { self.favorites.remove(&id) };
                self.spawn(async move {
                    tidal.lock().await.set_favorite(id, on).await?;
                    Ok(Msg::Done)
                });
            }
            Action::Toggle => self.player.send(Cmd::Toggle),
            Action::Next => self.next(),
            Action::Prev => self.prev(),
            Action::Seek(seconds) => self.player.send(Cmd::Seek(seconds)),
            Action::Shuffle => self.set_shuffle(self.unshuffled.is_none()),
            Action::Repeat => {
                self.repeat = match self.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                }
            }
            Action::Quality(quality) => {
                self.quality = quality;
                save_settings(&self.session, quality, self.volume);
                if let Some(i) = self.index {
                    self.resume_at = Some(self.player.status.position());
                    self.play(i);
                }
            }
            Action::Back => {
                if let Some(page) = self.back.pop() {
                    (self.page, self.view) = (page, View::default());
                    self.ctx.forget_all_images();
                }
            }
        }
    }

    fn receive(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Page(page) => (self.page, self.view) = (page, View::default()),
                Msg::Ready(id, reader) if self.current().is_some_and(|t| t.id == id) => {
                    self.player.send(Cmd::Load(reader));
                    if let Some(seconds) = self.resume_at.take() {
                        self.player.send(Cmd::Seek(seconds));
                    }
                }
                Msg::Ready(..) | Msg::Done => {}
                Msg::Radio(tracks) if !tracks.is_empty() => self.apply(Action::Play(tracks, 0)),
                Msg::Radio(_) => self.error = Some("No radio for this one.".into()),
                Msg::Continue(after, tracks) if self.current().is_some_and(|t| t.id == after) => {
                    let known: HashSet<u64> = self.queue.iter().map(|t| t.id).collect();
                    let before = self.queue.len();
                    self.queue.extend(tracks.into_iter().filter(|t| !known.contains(&t.id)));
                    if self.queue.len() > before {
                        self.play(before);
                    } else {
                        self.index = None;
                        self.player.send(Cmd::Stop);
                    }
                }
                Msg::Continue(..) => {}
                Msg::Lyrics(id, lyrics) => {
                    self.lyrics = Some((id, Some(lyrics)));
                    self.lyric_line = None;
                }
                Msg::SignedIn(tidal) => {
                    let tidal = Arc::new(Mutex::new(*tidal));
                    self.tidal = Some(tidal.clone());
                    (self.busy, self.login) = (false, None);
                    self.spawn(async move { Ok(Msg::Page(home_page(tidal).await?)) });
                    let tidal = self.tidal.clone().expect("just signed in");
                    self.spawn(async move { Ok(Msg::Favorites(tidal.lock().await.favorite_ids().await?)) });
                }
                Msg::Favorites(ids) => self.favorites = ids,
                Msg::Error(e) => {
                    if matches!(self.page, Page::Loading) {
                        self.page = self.back.pop().unwrap_or(Page::Search(Results::default()));
                    }
                    (self.busy, self.error) = (false, Some(e));
                }
                Msg::Player(Event::Ended) if self.repeat == Repeat::One => {
                    if let Some(i) = self.index {
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
        let state = np::State {
            playback: match (self.current(), playing) {
                (None, _) => np::Playback::Stopped,
                (Some(_), true) => np::Playback::Playing,
                (Some(_), false) => np::Playback::Paused,
            },
            track: self.current().map(|t| np::Track {
                id: t.id.to_string(),
                title: t.title.clone(),
                artists: vec![t.artist.clone()],
                album: t.album.clone(),
                duration: Some(Duration::from_secs(t.duration.into())),
                art_url: t.cover.as_deref().map(|c| tidal::image(c, 640)),
                ..Default::default()
            }),
            position: Duration::from_secs_f64(self.player.status.position()),
            ..Default::default()
        };
        if state.playback != self.shown.playback || state.track != self.shown.track {
            self.controls.update(state.clone());
            self.shown = state;
        }
    }

    fn login_ui(&mut self, ui: &mut Ui) {
        let mut finish = None;
        egui::CentralPanel::default().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(140.0);
                ui.heading(RichText::new("tidalfast").size(32.0));
                ui.add_space(24.0);
                if self.busy {
                    ui.spinner();
                } else if let Some(login) = &mut self.login {
                    ui.label("Sign in in your browser, then paste the address of the page you land on:");
                    ui.add(egui::TextEdit::singleline(&mut login.pasted).desired_width(520.0));
                    if ui.button("Continue").clicked() {
                        finish = login.client.take().map(|client| (client, login.pasted.clone()));
                    }
                    ui.hyperlink_to("Open the sign-in page again", &login.url);
                } else if ui.button(RichText::new("Sign in with Tidal").size(18.0)).clicked() {
                    match Tidal::start_login(&self.session) {
                        Ok((client, url)) => {
                            let _ = open::that(&url);
                            self.login = Some(Login { client: Some(client), url, pasted: String::new() });
                        }
                        Err(e) => self.error = Some(format!("{e:#}")),
                    }
                }
                if let Some(e) = &self.error {
                    ui.add_space(12.0);
                    ui.colored_label(Color32::LIGHT_RED, e);
                }
            });
        });
        if let Some((mut client, pasted)) = finish {
            (self.busy, self.error, self.login) = (true, None, None);
            self.spawn(async move {
                client.finish_login(&pasted).await?;
                Ok(Msg::SignedIn(Box::new(client)))
            });
        }
    }

    fn sidebar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        egui::Panel::left("nav").exact_size(180.0).show(ui, |ui| {
            ui.add_space(12.0);
            let mut item = |ui: &mut Ui, selected: bool, text: &str, action: Action| {
                let label = RichText::new(text).size(15.0);
                if ui.add_sized([ui.available_width(), 28.0], egui::Button::selectable(selected, label)).clicked() {
                    actions.push(action);
                }
            };
            item(ui, matches!(self.page, Page::Home(_)), "Home", Action::Home);
            item(ui, matches!(self.page, Page::Search(_)), "Search", Action::SearchPage);
            item(ui, matches!(self.page, Page::Queue), "Queue", Action::Queue);
            ui.add_space(16.0);
            ui.label(RichText::new("YOUR COLLECTION").small().weak());
            item(ui, matches!(self.page, Page::Tracks(_)), "Tracks", Action::Library(Library::Tracks));
            item(ui, matches!(self.page, Page::Albums(_)), "Albums", Action::Library(Library::Albums));
            item(ui, matches!(self.page, Page::Artists(_)), "Artists", Action::Library(Library::Artists));
            item(ui, matches!(self.page, Page::Playlists(_)), "Playlists", Action::Library(Library::Playlists));
        });
    }

    fn player_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let status = self.player.status.clone();
        egui::Panel::bottom("player").exact_size(88.0).show(ui, |ui| {
            ui.columns(3, |cols| {
                let track = self.index.and_then(|i| self.queue.get(i));
                cols[0].horizontal_centered(|ui| {
                    if let Some(t) = track {
                        picture(ui, art(t.cover.as_deref(), 160), 60.0, false);
                        ui.vertical(|ui| {
                            ui.add_space(14.0);
                            ui.horizontal(|ui| {
                                heart(ui, t.id, &self.favorites, actions);
                                ui.add(egui::Label::new(RichText::new(&t.title).strong()).truncate());
                            });
                            if let Some(id) = t.artist_id
                                && ui.link(&t.artist).clicked()
                            {
                                actions.push(Action::Artist(id));
                            }
                        });
                    }
                });
                cols[1].vertical_centered(|ui| {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space((ui.available_width() - 210.0) / 2.0);
                        let text = ui.visuals().text_color();
                        let button = |s: &str, size: f32, on: bool| {
                            let color = if on { ACCENT } else { text };
                            egui::Button::new(RichText::new(s).size(size).color(color)).frame(false)
                        };
                        if ui.add(button("🔀", 16.0, self.unshuffled.is_some())).on_hover_text("Shuffle").clicked() {
                            actions.push(Action::Shuffle);
                        }
                        if ui.add(button("⏮", 22.0, false)).clicked() {
                            actions.push(Action::Prev);
                        }
                        let toggle = if status.playing.load(Relaxed) { "⏸" } else { "▶" };
                        if ui.add(button(toggle, 22.0, false)).clicked() {
                            actions.push(Action::Toggle);
                        }
                        if ui.add(button("⏭", 22.0, false)).clicked() {
                            actions.push(Action::Next);
                        }
                        let repeat = if self.repeat == Repeat::One { "🔂" } else { "🔁" };
                        if ui.add(button(repeat, 16.0, self.repeat != Repeat::Off)).on_hover_text("Repeat").clicked() {
                            actions.push(Action::Repeat);
                        }
                    });
                    let total = track.map_or(0.0, |t| f64::from(t.duration));
                    let mut position = self.dragging.unwrap_or_else(|| status.position()).min(total);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(clock(position)).weak());
                        ui.spacing_mut().slider_width = ui.available_width() - 48.0;
                        let slider = ui.add_enabled(
                            track.is_some(),
                            egui::Slider::new(&mut position, 0.0..=total.max(1.0)).show_value(false),
                        );
                        if slider.dragged() {
                            self.dragging = Some(position);
                        }
                        if slider.drag_stopped() || (slider.clicked() && !slider.dragged()) {
                            self.dragging = None;
                            actions.push(Action::Seek(position));
                        }
                        ui.label(RichText::new(clock(total)).weak());
                    });
                });
                cols[2].with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.spacing_mut().slider_width = 100.0;
                    let volume = ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false));
                    if volume.changed() {
                        status.set_volume(self.volume * self.volume);
                    }
                    if volume.drag_stopped() || (volume.changed() && !volume.dragged()) {
                        save_settings(&self.session, self.quality, self.volume);
                    }
                    ui.label("🔊");
                    let on = matches!(self.page, Page::Lyrics);
                    let lyrics = RichText::new("Lyrics").color(if on { ACCENT } else { ui.visuals().text_color() });
                    if ui.add(egui::Button::new(lyrics).frame(false)).clicked() {
                        actions.push(Action::Lyrics);
                    }
                    let (tier, format) = status.format.lock().unwrap().clone();
                    let (tier, label) = if track.is_some() { (tier, format) } else { (self.quality, self.quality.name().into()) };
                    ui.menu_button(RichText::new(label).color(tier_color(tier)).small(), |ui| {
                        for q in Quality::ALL {
                            let name = RichText::new(q.name()).color(tier_color(q));
                            if ui.radio(self.quality == q, name).clicked() {
                                actions.push(Action::Quality(q));
                                ui.close();
                            }
                        }
                    });
                });
            });
        });
    }

    fn content(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if self.view.rows.is_none() || self.view.stale {
            let rows = match &self.page {
                Page::Tracks(tracks) | Page::Playlist(_, tracks) => arrange(tracks, &self.view),
                Page::Albums(albums) => arrange(albums, &self.view),
                Page::Artists(artists) => arrange(artists, &self.view),
                Page::Playlists(playlists) => arrange(playlists, &self.view),
                _ => Vec::new(),
            };
            (self.view.rows, self.view.stale) = (Some(rows), false);
        }
        let sorted = Some((self.view.sort, self.view.reverse));
        let (playing, favorites) = (self.current().map(|t| t.id), &self.favorites);
        let list = |queue| Rows { playing, favorites, queue };
        egui::CentralPanel::default().show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.back.is_empty(), egui::Button::new("⬅")).clicked() {
                    actions.push(Action::Back);
                }
                let search = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search").desired_width(360.0));
                if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    actions.push(Action::Search);
                }
                if let Some(e) = &self.error {
                    ui.colored_label(Color32::LIGHT_RED, e);
                }
            });
            ui.add_space(8.0);
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match &self.page {
                Page::Loading => {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| ui.spinner());
                }
                Page::Home(shelves) => {
                    for (n, shelf) in shelves.iter().enumerate() {
                        section(ui, &shelf.title);
                        if !shelf.cards.is_empty() {
                            egui::ScrollArea::horizontal().id_salt(("shelf", n)).show(ui, |ui| {
                                ui.horizontal(|ui| actions.extend(shelf.cards.iter().filter_map(|c| home_card(ui, c))));
                            });
                        }
                        if !shelf.tracks.is_empty() {
                            list(false).show(ui, &shelf.tracks, None, true, None, actions);
                        }
                    }
                }
                Page::Mix(mix, tracks) => {
                    if let Some(start) = header(ui, mix.image.clone(), false, &mix.title, &mix.subtitle, false) {
                        actions.push(start.action(tracks.clone(), 0));
                    }
                    list(false).show(ui, tracks, None, true, None, actions);
                }
                Page::Search(r) => search_page(ui, r, &list(false), actions),
                Page::Album(album, tracks) => {
                    let sub = format!("{} · {}", album.artist, album.year);
                    if let Some(start) = header(ui, art(album.cover.as_deref(), 640), false, &album.title, &sub, false) {
                        actions.push(start.action(tracks.clone(), 0));
                    }
                    list(false).show(ui, tracks, None, false, None, actions);
                }
                Page::Artist(artist, top, albums) => {
                    if let Some(start) = header(ui, art(artist.picture.as_deref(), 480), true, &artist.name, "", true) {
                        actions.push(start.action(top.clone(), artist.id));
                    }
                    section(ui, "Top tracks");
                    list(false).show(ui, top, None, true, None, actions);
                    section(ui, "Albums");
                    album_cards(ui, albums, None, actions);
                }
                Page::Playlist(playlist, tracks) => {
                    let sub = format!("{} tracks", playlist.count);
                    if let Some(start) = header(ui, art(playlist.cover.as_deref(), 640), false, &playlist.title, &sub, false) {
                        actions.push(start.action(in_order(tracks, self.view.rows.as_deref()), 0));
                    }
                    filter_box(ui, &mut self.view, "Filter playlist on title, artist or album");
                    list(false).show(ui, tracks, self.view.rows.as_deref(), true, sorted, actions);
                }
                Page::Tracks(tracks) => {
                    if let Some(start) = title_with_play(ui, "Tracks", !tracks.is_empty()) {
                        actions.push(start.action(in_order(tracks, self.view.rows.as_deref()), 0));
                    }
                    filter_box(ui, &mut self.view, "Filter tracks on title, artist or album");
                    list(false).show(ui, tracks, self.view.rows.as_deref(), true, sorted, actions);
                }
                Page::Albums(albums) => {
                    grid_controls(ui, "Albums", &mut self.view, &[Sort::Added, Sort::Title, Sort::Artist, Sort::Year], actions);
                    album_cards(ui, albums, self.view.rows.as_deref(), actions);
                }
                Page::Artists(artists) => {
                    grid_controls(ui, "Artists", &mut self.view, &[Sort::Added, Sort::Title], actions);
                    artist_cards(ui, artists, self.view.rows.as_deref(), actions);
                }
                Page::Playlists(playlists) => {
                    grid_controls(ui, "Playlists", &mut self.view, &[Sort::Added, Sort::Title], actions);
                    playlist_cards(ui, playlists, self.view.rows.as_deref(), actions);
                }
                Page::Lyrics => {
                    let position = self.player.status.position();
                    let track = self.index.and_then(|i| self.queue.get(i));
                    let lyrics = self.lyrics.as_ref().filter(|(id, _)| track.is_some_and(|t| t.id == *id)).map(|l| &l.1);
                    ui.add_space(24.0);
                    ui.vertical_centered(|ui| match (track, lyrics) {
                        (None, _) => {
                            ui.label(RichText::new("Nothing is playing.").weak());
                        }
                        (Some(_), None | Some(None)) => {
                            ui.spinner();
                        }
                        (Some(_), Some(Some(None))) => {
                            ui.label(RichText::new("No lyrics for this track.").weak());
                        }
                        (Some(_), Some(Some(Some(l)))) if l.synced.is_empty() => {
                            ui.label(RichText::new(&l.text).size(20.0));
                        }
                        (Some(_), Some(Some(Some(l)))) => {
                            let now = l.synced.iter().rposition(|(at, _)| *at <= position);
                            for (n, (at, words)) in l.synced.iter().enumerate() {
                                let color = if Some(n) == now { ACCENT } else { Color32::from_gray(120) };
                                let words = if words.is_empty() { "♪" } else { words };
                                let line = ui.add(egui::Label::new(RichText::new(words).size(24.0).color(color)).sense(Sense::click()));
                                if Some(n) == now && self.lyric_line != now {
                                    line.scroll_to_me(Some(Align::Center));
                                }
                                if line.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                                    actions.push(Action::Seek(*at));
                                }
                                ui.add_space(6.0);
                            }
                            self.lyric_line = now;
                        }
                    });
                }
                Page::Queue => {
                    section(ui, "Queue");
                    if self.queue.is_empty() {
                        ui.label(RichText::new("Nothing queued. Right-click a track to add it.").weak());
                    }
                    list(true).show(ui, &self.queue, None, true, None, actions);
                }
            });
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
        if ui.input(|i| i.key_pressed(Key::Space)) && ui.ctx().memory(|m| m.focused().is_none()) {
            actions.push(Action::Toggle);
        }
        if matches!(self.page, Page::Lyrics)
            && let (Some(t), Some(tidal)) = (self.current(), self.tidal.clone())
            && self.lyrics.as_ref().is_none_or(|(id, _)| *id != t.id)
        {
            let id = t.id;
            self.lyrics = Some((id, None));
            self.spawn(async move { Ok(Msg::Lyrics(id, tidal.lock().await.lyrics(id).await?)) });
        }
        self.player_bar(ui, &mut actions);
        self.sidebar(ui, &mut actions);
        self.content(ui, &mut actions);
        for action in actions {
            self.apply(action);
        }
        if self.player.status.playing.load(Relaxed) {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
enum Sort {
    #[default]
    Added,
    Title,
    Artist,
    Album,
    Year,
    Duration,
}

impl Sort {
    fn label(self, reverse: bool) -> &'static str {
        match (self, reverse) {
            (Sort::Added, false) => "Recently added",
            (Sort::Added, true) => "Oldest added",
            (Sort::Title, false) => "A–Z",
            (Sort::Title, true) => "Z–A",
            (Sort::Artist, false) => "Artist A–Z",
            (Sort::Artist, true) => "Artist Z–A",
            (Sort::Album, false) => "Album A–Z",
            (Sort::Album, true) => "Album Z–A",
            (Sort::Year, false) => "Newest",
            (Sort::Year, true) => "Oldest",
            (Sort::Duration, false) => "Shortest",
            (Sort::Duration, true) => "Longest",
        }
    }
}

fn filter_box(ui: &mut Ui, view: &mut View, hint: &str) {
    let filter = egui::TextEdit::singleline(&mut view.filter).hint_text(hint).desired_width(f32::INFINITY);
    if ui.add(filter).changed() {
        view.stale = true;
        ui.ctx().request_repaint();
    }
    ui.add_space(8.0);
}

/// Title, filter and sort dropdown for the card grids; picking the current sort again reverses it.
fn grid_controls(ui: &mut Ui, title: &str, view: &mut View, sorts: &[Sort], actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        section(ui, title);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            egui::ComboBox::from_id_salt("sort").selected_text(view.sort.label(view.reverse)).show_ui(ui, |ui| {
                for &sort in sorts {
                    let selected = view.sort == sort;
                    if ui.selectable_label(selected, sort.label(selected && view.reverse)).clicked() {
                        actions.push(Action::Sort(sort));
                    }
                }
            });
            let filter = egui::TextEdit::singleline(&mut view.filter).hint_text("Filter").desired_width(220.0);
            if ui.add(filter).changed() {
                view.stale = true;
                ui.ctx().request_repaint();
            }
        });
    });
}

/// The filter and sort of a list page, and the resulting row order (None until recomputed).
#[derive(Default)]
struct View {
    filter: String,
    sort: Sort,
    reverse: bool,
    rows: Option<Vec<usize>>,
    /// The filter or sort changed; `rows` still holds the old order until recomputed.
    stale: bool,
}

trait Sortable {
    /// Lowercase text the filter matches.
    fn text(&self) -> String;
    fn key(&self, sort: Sort) -> (u32, String);
}

impl Sortable for Track {
    fn text(&self) -> String {
        format!("{} {} {}", self.title, self.artist, self.album).to_lowercase()
    }

    fn key(&self, sort: Sort) -> (u32, String) {
        match sort {
            Sort::Duration => (self.duration, String::new()),
            Sort::Artist => (0, self.artist.to_lowercase()),
            Sort::Album => (0, self.album.to_lowercase()),
            _ => (0, self.title.to_lowercase()),
        }
    }
}

impl Sortable for Album {
    fn text(&self) -> String {
        format!("{} {}", self.title, self.artist).to_lowercase()
    }

    fn key(&self, sort: Sort) -> (u32, String) {
        match sort {
            Sort::Year => (u32::MAX - self.year.parse().unwrap_or(0), String::new()),
            Sort::Artist => (0, self.artist.to_lowercase()),
            _ => (0, self.title.to_lowercase()),
        }
    }
}

impl Sortable for Artist {
    fn text(&self) -> String {
        self.name.to_lowercase()
    }

    fn key(&self, _: Sort) -> (u32, String) {
        (0, self.name.to_lowercase())
    }
}

impl Sortable for Playlist {
    fn text(&self) -> String {
        self.title.to_lowercase()
    }

    fn key(&self, _: Sort) -> (u32, String) {
        (0, self.title.to_lowercase())
    }
}

fn arrange<T: Sortable>(items: &[T], view: &View) -> Vec<usize> {
    let filter = view.filter.to_lowercase();
    let mut rows: Vec<usize> = (0..items.len()).filter(|&i| filter.is_empty() || items[i].text().contains(&filter)).collect();
    if view.sort != Sort::Added {
        rows.sort_by_cached_key(|&i| items[i].key(view.sort));
    }
    if view.reverse {
        rows.reverse();
    }
    rows
}

/// Items in the given row order, or as they are.
fn ordered<'a, T>(items: &'a [T], order: Option<&'a [usize]>) -> impl Iterator<Item = &'a T> {
    (0..order.map_or(items.len(), <[usize]>::len)).map(move |p| &items[order.map_or(p, |o| o[p])])
}

fn in_order(tracks: &[Track], order: Option<&[usize]>) -> Vec<Track> {
    ordered(tracks, order).cloned().collect()
}

fn shuffle<T>(items: &mut [T]) {
    let mut seed = SystemTime::now().duration_since(UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64) | 1;
    for i in (1..items.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        items.swap(i, (seed % (i as u64 + 1)) as usize);
    }
}

fn tier_color(quality: Quality) -> Color32 {
    match quality {
        Quality::Max => GOLD,
        Quality::High => ACCENT,
        Quality::Low => Color32::from_gray(170),
    }
}

fn settings_path(session: &Path) -> PathBuf {
    session.with_file_name("settings.txt")
}

/// Quality and volume, one per line.
fn load_settings(session: &Path) -> (Quality, f32) {
    let text = std::fs::read_to_string(settings_path(session)).unwrap_or_default();
    let mut lines = text.lines();
    let quality = lines.next().and_then(Quality::parse).unwrap_or(Quality::Max);
    (quality, lines.next().and_then(|v| v.parse().ok()).unwrap_or(1.0))
}

fn save_settings(session: &Path, quality: Quality, volume: f32) {
    let _ = std::fs::write(settings_path(session), format!("{}\n{volume}\n", quality.name()));
}

fn clock(seconds: f64) -> String {
    let s = seconds as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(16.0);
    ui.label(RichText::new(title).size(20.0).strong());
    ui.add_space(6.0);
}

/// How to start a list from its page's buttons.
enum Start {
    Play,
    Shuffle,
    Radio,
}

impl Start {
    /// `id` is the artist whose radio the Radio button starts.
    fn action(self, tracks: Vec<Track>, id: u64) -> Action {
        match self {
            Start::Play => Action::Play(tracks, 0),
            Start::Shuffle => Action::ShufflePlay(tracks),
            Start::Radio => Action::ArtistRadio(id),
        }
    }
}

fn start_buttons(ui: &mut Ui, radio: bool) -> Option<Start> {
    let mut start = None;
    ui.horizontal(|ui| {
        if ui.button(RichText::new("▶  Play").size(16.0)).clicked() {
            start = Some(Start::Play);
        }
        if ui.button(RichText::new("🔀  Shuffle").size(16.0)).clicked() {
            start = Some(Start::Shuffle);
        }
        if radio && ui.button(RichText::new("Artist radio").size(16.0)).clicked() {
            start = Some(Start::Radio);
        }
    });
    start
}

fn title_with_play(ui: &mut Ui, title: &str, can_play: bool) -> Option<Start> {
    let mut start = None;
    ui.horizontal(|ui| {
        section(ui, title);
        if can_play {
            start = start_buttons(ui, false);
        }
    });
    start
}

fn heart(ui: &mut Ui, id: u64, favorites: &HashSet<u64>, actions: &mut Vec<Action>) {
    let on = favorites.contains(&id);
    let color = if on { ACCENT } else { Color32::from_gray(90) };
    let hint = if on { "Remove from your collection" } else { "Add to your collection" };
    if ui.add(egui::Button::new(RichText::new("❤").color(color)).frame(false)).on_hover_text(hint).clicked() {
        actions.push(Action::Favorite(id, !on));
    }
}

/// A Tidal image id as a URL at one of its sizes.
fn art(id: Option<&str>, size: u32) -> Option<String> {
    id.map(|id| tidal::image(id, size))
}

/// Artwork, loaded only once it scrolls into view so long shelves and grids don't fill memory.
fn picture(ui: &mut Ui, url: Option<String>, side: f32, round: bool) -> egui::Response {
    let radius = if round { side / 2.0 } else { 4.0 };
    let (rect, response) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    ui.painter().rect_filled(rect, radius, ui.visuals().faint_bg_color);
    if let Some(url) = url
        && ui.is_rect_visible(rect)
    {
        egui::Image::new(url).corner_radius(radius).paint_at(ui, rect);
    }
    response
}

fn home_card(ui: &mut Ui, card_data: &Card) -> Option<Action> {
    match card_data {
        Card::Album(a) => card(ui, art(a.cover.as_deref(), 320), false, &a.title, &a.artist).then_some(Action::Album(a.id)),
        Card::Artist(a) => card(ui, art(a.picture.as_deref(), 320), true, &a.name, "Artist").then_some(Action::Artist(a.id)),
        Card::Playlist(p) => {
            card(ui, art(p.cover.as_deref(), 320), false, &p.title, &format!("{} tracks", p.count)).then(|| Action::Playlist(p.id.clone()))
        }
        Card::Mix(m) => card(ui, m.image.clone(), false, &m.title, &m.subtitle).then(|| Action::Mix(m.clone())),
    }
}

/// Tidal's home feed, or your playlists if it can't be had.
async fn home_page(tidal: Arc<Mutex<Tidal>>) -> Result<Page> {
    let mut tidal = tidal.lock().await;
    match tidal.home().await {
        Ok(shelves) if !shelves.is_empty() => Ok(Page::Home(shelves)),
        _ => Ok(Page::Playlists(tidal.playlists().await?)),
    }
}

/// Big artwork, title and Play / Shuffle (/ Artist radio) buttons.
fn header(ui: &mut Ui, image: Option<String>, round: bool, title: &str, subtitle: &str, radio: bool) -> Option<Start> {
    let mut start = None;
    ui.horizontal(|ui| {
        picture(ui, image, 200.0, round);
        ui.vertical(|ui| {
            ui.add_space(110.0);
            ui.label(RichText::new(title).size(30.0).strong());
            ui.label(RichText::new(subtitle).weak());
            start = start_buttons(ui, radio);
        });
    });
    ui.add_space(12.0);
    start
}

fn card(ui: &mut Ui, image: Option<String>, round: bool, title: &str, subtitle: &str) -> bool {
    let mut clicked = false;
    ui.allocate_ui(vec2(160.0, 216.0), |ui| {
        ui.vertical(|ui| {
            clicked |= picture(ui, image, 160.0, round).clicked();
            clicked |= ui.add(egui::Label::new(RichText::new(title).strong()).truncate().sense(Sense::click())).clicked();
            ui.add(egui::Label::new(RichText::new(subtitle).weak()).truncate());
        });
    });
    clicked
}

fn album_cards(ui: &mut Ui, albums: &[Album], order: Option<&[usize]>, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        for a in ordered(albums, order) {
            if card(ui, art(a.cover.as_deref(), 320), false, &a.title, &format!("{} · {}", a.artist, a.year)) {
                actions.push(Action::Album(a.id));
            }
        }
    });
}

fn artist_cards(ui: &mut Ui, artists: &[Artist], order: Option<&[usize]>, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        for a in ordered(artists, order) {
            if card(ui, art(a.picture.as_deref(), 320), true, &a.name, "Artist") {
                actions.push(Action::Artist(a.id));
            }
        }
    });
}

fn playlist_cards(ui: &mut Ui, playlists: &[Playlist], order: Option<&[usize]>, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        for p in ordered(playlists, order) {
            if card(ui, art(p.cover.as_deref(), 320), false, &p.title, &format!("{} tracks", p.count)) {
                actions.push(Action::Playlist(p.id.clone()));
            }
        }
    });
}

fn search_page(ui: &mut Ui, r: &Results, rows: &Rows<'_>, actions: &mut Vec<Action>) {
    if r.artists.is_empty() && r.albums.is_empty() && r.tracks.is_empty() && r.playlists.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| ui.label(RichText::new("Search for artists, albums, tracks and playlists.").weak()));
        return;
    }
    if !r.tracks.is_empty() {
        section(ui, "Tracks");
        rows.show(ui, &r.tracks, None, true, None, actions);
    }
    if !r.artists.is_empty() {
        section(ui, "Artists");
        artist_cards(ui, &r.artists, None, actions);
    }
    if !r.albums.is_empty() {
        section(ui, "Albums");
        album_cards(ui, &r.albums, None, actions);
    }
    if !r.playlists.is_empty() {
        section(ui, "Playlists");
        playlist_cards(ui, &r.playlists, None, actions);
    }
}

fn cell(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(vec2(width, ROW), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(width);
        add(ui);
    });
}

/// How track rows behave: what is playing, which are favorites, and whether this list is the queue.
struct Rows<'a> {
    playing: Option<u64>,
    favorites: &'a HashSet<u64>,
    queue: bool,
}

const TIME: f32 = 56.0;
const NUMBER: f32 = 36.0;
const HEART: f32 = 28.0;

impl Rows<'_> {
    /// A table of tracks under clickable column headers when `sorted` is set (sort, reversed).
    /// Click a title to play the list from there; right-click it for more.
    fn show(
        &self,
        ui: &mut Ui,
        tracks: &[Track],
        order: Option<&[usize]>,
        with_album: bool,
        sorted: Option<(Sort, bool)>,
        actions: &mut Vec<Action>,
    ) {
        let with_added = tracks.iter().any(|t| t.added.is_some());
        let spacing = ui.spacing().item_spacing.x;
        let free = ui.available_width() - NUMBER - TIME - HEART - spacing * 6.0;
        let mut columns = vec![(Sort::Title, "TITLE", 0.4), (Sort::Artist, "ARTIST", 0.25)];
        if with_album {
            columns.push((Sort::Album, "ALBUM", 0.22));
        }
        if with_added {
            columns.push((Sort::Added, "DATE ADDED", 0.13));
        }
        let total: f32 = columns.iter().map(|c| c.2).sum();
        let width = |share: f32| free * share / total;
        ui.horizontal(|ui| {
            cell(ui, NUMBER, |ui| {
                ui.label(RichText::new("#").small().weak());
            });
            for &(sort, name, share) in &columns {
                cell(ui, width(share), |ui| column_header(ui, name, sort, sorted, actions));
            }
            cell(ui, TIME, |ui| column_header(ui, "TIME", Sort::Duration, sorted, actions));
        });
        ui.separator();
        let full = ui.available_width();
        for (pos, t) in ordered(tracks, order).enumerate() {
            let row = Rect::from_min_size(ui.cursor().min, vec2(full, ROW));
            // Rows scrolled out of view only take up space, so long playlists stay cheap to draw.
            if !ui.is_rect_visible(row) {
                ui.allocate_space(vec2(free, ROW));
                continue;
            }
            let i = order.map_or(pos, |o| o[pos]);
            let play = || if self.queue { Action::Jump(i) } else { Action::Play(in_order(tracks, order), pos) };
            let hovered = ui.rect_contains_pointer(row);
            if hovered {
                ui.painter().rect_filled(row.expand2(vec2(4.0, 2.0)), 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            ui.horizontal(|ui| {
                let color = if self.playing == Some(t.id) { ACCENT } else { ui.visuals().strong_text_color() };
                cell(ui, NUMBER, |ui| {
                    if hovered {
                        let (rect, button) = ui.allocate_exact_size(vec2(ROW, ROW), Sense::click());
                        let fill = if button.hovered() { ACCENT } else { ui.visuals().strong_text_color() };
                        triangle(ui, rect.center(), 6.0, Direction::Right, fill);
                        if button.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                            actions.push(play());
                        }
                    } else {
                        ui.label(RichText::new((pos + 1).to_string()).weak());
                    }
                });
                cell(ui, width(0.4), |ui| {
                    let title = ui.add(egui::Label::new(RichText::new(&t.title).color(color)).truncate().sense(Sense::click()));
                    if title.clicked() {
                        actions.push(play());
                    }
                    title.context_menu(|ui| self.menu(ui, tracks, i, actions));
                });
                cell(ui, width(0.25), |ui| {
                    if ui.add(egui::Link::new(&t.artist)).clicked()
                        && let Some(id) = t.artist_id
                    {
                        actions.push(Action::Artist(id));
                    }
                });
                if with_album {
                    cell(ui, width(0.22), |ui| {
                        if ui.add(egui::Link::new(&t.album)).clicked()
                            && let Some(id) = t.album_id
                        {
                            actions.push(Action::Album(id));
                        }
                    });
                }
                if with_added {
                    cell(ui, width(0.13), |ui| {
                        ui.label(RichText::new(t.added.as_deref().unwrap_or_default()).weak());
                    });
                }
                cell(ui, TIME, |ui| {
                    ui.label(RichText::new(clock(f64::from(t.duration))).weak());
                });
                heart(ui, t.id, self.favorites, actions);
            });
        }
    }

    fn menu(&self, ui: &mut Ui, tracks: &[Track], i: usize, actions: &mut Vec<Action>) {
        let t = &tracks[i];
        let mut item = |text: &str, action: Action| {
            if ui.button(text).clicked() {
                actions.push(action);
                ui.close();
            }
        };
        if self.queue {
            if i > 0 {
                item("Move up", Action::Move(i, i - 1));
            }
            if i + 1 < tracks.len() {
                item("Move down", Action::Move(i, i + 1));
            }
            if self.playing != Some(t.id) {
                item("Remove from queue", Action::Remove(i));
            }
        } else {
            item("Play next", Action::PlayNext(t.clone()));
            item("Add to queue", Action::AddToQueue(t.clone()));
            item("Track radio", Action::TrackRadio(t.id));
        }
        if let Some(id) = t.album_id {
            item("Go to album", Action::Album(id));
        }
        if let Some(id) = t.artist_id {
            item("Go to artist", Action::Artist(id));
        }
    }
}

/// A column title; clickable with an arrow on the sorted column when the table sorts.
fn column_header(ui: &mut Ui, name: &str, sort: Sort, sorted: Option<(Sort, bool)>, actions: &mut Vec<Action>) {
    let Some((current, reverse)) = sorted else {
        ui.label(RichText::new(name).small().weak());
        return;
    };
    let active = current == sort;
    let text = RichText::new(name).small();
    let text = if active { text.color(ACCENT) } else { text.weak() };
    let header = ui.horizontal(|ui| {
        ui.label(text);
        if active {
            let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            triangle(ui, rect.center(), 4.0, if reverse { Direction::Up } else { Direction::Down }, ACCENT);
        }
    });
    let click = ui.interact(header.response.rect, ui.id().with(name), Sense::click());
    if click.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        actions.push(Action::Sort(sort));
    }
}

enum Direction {
    Up,
    Down,
    Right,
}

/// A filled triangle pointing `direction`, drawn rather than typed so it never depends on font coverage.
fn triangle(ui: &Ui, c: egui::Pos2, size: f32, direction: Direction, fill: Color32) {
    let points = match direction {
        Direction::Down => vec![c + vec2(-size, -size / 2.0), c + vec2(size, -size / 2.0), c + vec2(0.0, size * 0.75)],
        Direction::Up => vec![c + vec2(-size, size / 2.0), c + vec2(size, size / 2.0), c + vec2(0.0, -size * 0.75)],
        Direction::Right => vec![c + vec2(-size * 0.6, -size), c + vec2(-size * 0.6, size), c + vec2(size, 0.0)],
    };
    ui.painter().add(egui::Shape::convex_polygon(points, fill, egui::Stroke::NONE));
}
