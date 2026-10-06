use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use anyhow::Result;
use egui::{Align, Color32, Key, Layout, RichText, Sense, Ui, vec2};
use fastframe_now_playing as np;
use tokio::sync::Mutex;

use crate::art::Art;
use crate::cache;
use crate::player::{Cmd, Event, Player};
use crate::tidal::{self, Album, Artist, Playlist, Quality, Results, Tidal, Track};

const ACCENT: Color32 = Color32::from_rgb(0x33, 0xff, 0xee);
const GOLD: Color32 = Color32::from_rgb(0xf5, 0xc5, 0x42);
const ROW: f32 = 22.0;

enum Page {
    Search(Results),
    Album(Album, Vec<Track>),
    Artist(Artist, Vec<Track>, Vec<Album>),
    Playlist(Playlist, Vec<Track>),
    Loading,
}

enum Msg {
    Page(Page),
    Ready(u64, cache::Reader),
    SignedIn(Box<Tidal>),
    Error(String),
    Player(Event),
}

enum Action {
    Search,
    Album(u64),
    Artist(u64),
    Playlist(String),
    Play(Vec<Track>, usize),
    Toggle,
    Next,
    Prev,
    Seek(f64),
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
    query: String,
    queue: Vec<Track>,
    index: Option<usize>,
    quality: Quality,
    volume: f32,
    dragging: Option<f64>,
    /// Where to resume after reloading the current track in another quality.
    resume_at: Option<f64>,
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
            page: Page::Search(Results::default()),
            back: Vec::new(),
            query: String::new(),
            queue: Vec::new(),
            index: None,
            quality,
            volume,
            dragging: None,
            resume_at: None,
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

    fn navigate(&mut self, load: impl Future<Output = Result<Page>> + Send + 'static) {
        let old = std::mem::replace(&mut self.page, Page::Loading);
        if !matches!(old, Page::Loading) {
            self.back.push(old);
        }
        self.ctx.forget_all_images();
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
            Action::Play(tracks, index) => {
                self.queue = tracks;
                self.play(index);
            }
            Action::Toggle => self.player.send(Cmd::Toggle),
            Action::Next => self.next(),
            Action::Prev => self.prev(),
            Action::Seek(seconds) => self.player.send(Cmd::Seek(seconds)),
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
                    self.page = page;
                    self.ctx.forget_all_images();
                }
            }
        }
    }

    fn receive(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Page(page) => self.page = page,
                Msg::Ready(id, reader) if self.current().is_some_and(|t| t.id == id) => {
                    self.player.send(Cmd::Load(reader));
                    if let Some(seconds) = self.resume_at.take() {
                        self.player.send(Cmd::Seek(seconds));
                    }
                }
                Msg::Ready(..) => {}
                Msg::SignedIn(tidal) => {
                    self.tidal = Some(Arc::new(Mutex::new(*tidal)));
                    (self.busy, self.login) = (false, None);
                }
                Msg::Error(e) => {
                    if matches!(self.page, Page::Loading) {
                        self.page = self.back.pop().unwrap_or(Page::Search(Results::default()));
                    }
                    (self.busy, self.error) = (false, Some(e));
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

    fn player_bar(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let status = self.player.status.clone();
        egui::Panel::bottom("player").exact_size(88.0).show(ui, |ui| {
            ui.columns(3, |cols| {
                let track = self.index.and_then(|i| self.queue.get(i));
                cols[0].horizontal_centered(|ui| {
                    if let Some(t) = track {
                        picture(ui, t.cover.as_deref(), 160, 60.0, false);
                        ui.vertical(|ui| {
                            ui.add_space(14.0);
                            ui.add(egui::Label::new(RichText::new(&t.title).strong()).truncate());
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
                        ui.add_space((ui.available_width() - 130.0) / 2.0);
                        let big = |s: &str| egui::Button::new(RichText::new(s).size(22.0)).frame(false);
                        if ui.add(big("⏮")).clicked() {
                            actions.push(Action::Prev);
                        }
                        let toggle = if status.playing.load(Relaxed) { "⏸" } else { "▶" };
                        if ui.add(big(toggle)).clicked() {
                            actions.push(Action::Toggle);
                        }
                        if ui.add(big("⏭")).clicked() {
                            actions.push(Action::Next);
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
        let playing = self.current().map(|t| t.id);
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
                Page::Search(r) => search_page(ui, r, playing, actions),
                Page::Album(album, tracks) => {
                    let sub = format!("{} · {}", album.artist, album.year);
                    if header(ui, album.cover.as_deref(), 640, false, &album.title, &sub) {
                        actions.push(Action::Play(tracks.clone(), 0));
                    }
                    track_list(ui, tracks, playing, false, actions);
                }
                Page::Artist(artist, top, albums) => {
                    if header(ui, artist.picture.as_deref(), 480, true, &artist.name, "") {
                        actions.push(Action::Play(top.clone(), 0));
                    }
                    section(ui, "Top tracks");
                    track_list(ui, top, playing, true, actions);
                    section(ui, "Albums");
                    album_cards(ui, albums, actions);
                }
                Page::Playlist(playlist, tracks) => {
                    let sub = format!("{} tracks", playlist.count);
                    if header(ui, playlist.cover.as_deref(), 640, false, &playlist.title, &sub) {
                        actions.push(Action::Play(tracks.clone(), 0));
                    }
                    track_list(ui, tracks, playing, true, actions);
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
        self.player_bar(ui, &mut actions);
        self.content(ui, &mut actions);
        for action in actions {
            self.apply(action);
        }
        if self.player.status.playing.load(Relaxed) {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
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

fn picture(ui: &mut Ui, image: Option<&str>, size: u32, side: f32, round: bool) -> egui::Response {
    let radius = if round { side / 2.0 } else { 4.0 };
    match image {
        Some(id) => ui.add(
            egui::Image::new(tidal::image(id, size))
                .fit_to_exact_size(vec2(side, side))
                .corner_radius(radius)
                .sense(Sense::click()),
        ),
        None => {
            let (rect, response) = ui.allocate_exact_size(vec2(side, side), Sense::click());
            ui.painter().rect_filled(rect, radius, ui.visuals().faint_bg_color);
            response
        }
    }
}

/// Big artwork, title and a play button; true when play is clicked.
fn header(ui: &mut Ui, image: Option<&str>, size: u32, round: bool, title: &str, subtitle: &str) -> bool {
    let mut play = false;
    ui.horizontal(|ui| {
        picture(ui, image, size, 200.0, round);
        ui.vertical(|ui| {
            ui.add_space(110.0);
            ui.label(RichText::new(title).size(30.0).strong());
            ui.label(RichText::new(subtitle).weak());
            play = ui.button(RichText::new("▶  Play").size(16.0)).clicked();
        });
    });
    ui.add_space(12.0);
    play
}

fn card(ui: &mut Ui, image: Option<&str>, round: bool, title: &str, subtitle: &str) -> bool {
    let mut clicked = false;
    ui.allocate_ui(vec2(160.0, 216.0), |ui| {
        ui.vertical(|ui| {
            clicked |= picture(ui, image, 320, 160.0, round).clicked();
            clicked |= ui.add(egui::Label::new(RichText::new(title).strong()).truncate().sense(Sense::click())).clicked();
            ui.add(egui::Label::new(RichText::new(subtitle).weak()).truncate());
        });
    });
    clicked
}

fn album_cards(ui: &mut Ui, albums: &[Album], actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        for a in albums {
            if card(ui, a.cover.as_deref(), false, &a.title, &format!("{} · {}", a.artist, a.year)) {
                actions.push(Action::Album(a.id));
            }
        }
    });
}

fn search_page(ui: &mut Ui, r: &Results, playing: Option<u64>, actions: &mut Vec<Action>) {
    if r.artists.is_empty() && r.albums.is_empty() && r.tracks.is_empty() && r.playlists.is_empty() {
        ui.add_space(40.0);
        ui.vertical_centered(|ui| ui.label(RichText::new("Search for artists, albums, tracks and playlists.").weak()));
        return;
    }
    if !r.tracks.is_empty() {
        section(ui, "Tracks");
        track_list(ui, &r.tracks, playing, true, actions);
    }
    if !r.artists.is_empty() {
        section(ui, "Artists");
        ui.horizontal_wrapped(|ui| {
            for a in &r.artists {
                if card(ui, a.picture.as_deref(), true, &a.name, "Artist") {
                    actions.push(Action::Artist(a.id));
                }
            }
        });
    }
    if !r.albums.is_empty() {
        section(ui, "Albums");
        album_cards(ui, &r.albums, actions);
    }
    if !r.playlists.is_empty() {
        section(ui, "Playlists");
        ui.horizontal_wrapped(|ui| {
            for p in &r.playlists {
                if card(ui, p.cover.as_deref(), false, &p.title, &format!("{} tracks", p.count)) {
                    actions.push(Action::Playlist(p.id.clone()));
                }
            }
        });
    }
}

fn cell(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(vec2(width, ROW), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(width);
        add(ui);
    });
}

/// Click a title to play the list from there.
fn track_list(ui: &mut Ui, tracks: &[Track], playing: Option<u64>, with_album: bool, actions: &mut Vec<Action>) {
    let width = ui.available_width() - 100.0;
    for (i, t) in tracks.iter().enumerate() {
        ui.horizontal(|ui| {
            let color = if playing == Some(t.id) { ACCENT } else { ui.visuals().strong_text_color() };
            cell(ui, 32.0, |ui| {
                ui.label(RichText::new((i + 1).to_string()).weak());
            });
            cell(ui, width * if with_album { 0.45 } else { 0.65 }, |ui| {
                let title = egui::Label::new(RichText::new(&t.title).color(color)).truncate().sense(Sense::click());
                if ui.add(title).clicked() {
                    actions.push(Action::Play(tracks.to_vec(), i));
                }
            });
            cell(ui, width * 0.3, |ui| {
                let link = egui::Link::new(&t.artist);
                if ui.add(link).clicked()
                    && let Some(id) = t.artist_id
                {
                    actions.push(Action::Artist(id));
                }
            });
            if with_album {
                cell(ui, width * 0.2, |ui| {
                    if ui.add(egui::Link::new(&t.album)).clicked()
                        && let Some(id) = t.album_id
                    {
                        actions.push(Action::Album(id));
                    }
                });
            }
            ui.label(RichText::new(clock(f64::from(t.duration))).weak());
        });
    }
}
