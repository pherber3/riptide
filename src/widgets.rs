use std::collections::HashSet;

use egui::{Align, Color32, Layout, Rect, RichText, Sense, Ui, vec2};

use crate::app::{Action, Body, Head, Page, Source};
use crate::tidal::{self, Card, Quality, Track};
use crate::view::{Sort, View, in_order, ordered};

pub const ACCENT: Color32 = Color32::from_rgb(0x33, 0xff, 0xee);
const GOLD: Color32 = Color32::from_rgb(0xf5, 0xc5, 0x42);
const ROW: f32 = 22.0;
const CARD: f32 = 160.0;
const NUMBER: f32 = 36.0;
const TIME: f32 = 56.0;
const HEART: f32 = 28.0;

pub fn tier_color(quality: Quality) -> Color32 {
    match quality {
        Quality::Max => GOLD,
        Quality::High => ACCENT,
        Quality::Low => Color32::from_gray(170),
    }
}

/// A Tidal image id as a URL at one of its sizes.
pub fn art(id: Option<&str>, size: u32) -> Option<String> {
    id.map(|id| tidal::image(id, size))
}

pub fn clock(seconds: f64) -> String {
    let s = seconds as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(16.0);
    ui.label(RichText::new(title).size(20.0).strong());
    ui.add_space(6.0);
}

pub fn clickable(response: egui::Response) -> egui::Response {
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Text that underlines on hover and clicks like a link, truncated to fit.
pub fn link_text(ui: &mut Ui, text: RichText) -> egui::Response {
    let response = ui.add(egui::Label::new(text).truncate().selectable(false).sense(Sense::click()));
    if response.hovered() {
        let stroke = egui::Stroke::new(1.0, ui.visuals().strong_text_color());
        ui.painter().hline(response.rect.x_range(), response.rect.bottom() - 1.0, stroke);
    }
    clickable(response)
}

pub fn heart(ui: &mut Ui, id: u64, favorites: &HashSet<u64>, actions: &mut Vec<Action>) {
    let on = favorites.contains(&id);
    let color = if on { ACCENT } else { Color32::from_gray(90) };
    let hint = if on { "Remove from your collection" } else { "Add to your collection" };
    if ui.add(egui::Button::new(RichText::new("❤").color(color)).frame(false)).on_hover_text(hint).clicked() {
        actions.push(Action::Favorite(id, !on));
    }
}

pub fn picture(ui: &mut Ui, url: Option<String>, side: f32, round: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    paint_picture(ui, url, rect, round);
    response
}

/// Artwork, loaded only once it scrolls into view so long shelves and grids don't fill memory.
fn paint_picture(ui: &Ui, url: Option<String>, rect: Rect, round: bool) {
    let radius = if round { rect.width() / 2.0 } else { 4.0 };
    ui.painter().rect_filled(rect, radius, ui.visuals().faint_bg_color);
    if let Some(url) = url
        && ui.is_rect_visible(rect)
    {
        egui::Image::new(url).corner_radius(radius).paint_at(ui, rect);
    }
}

pub enum Direction {
    Up,
    Down,
    Right,
}

/// A filled triangle pointing `direction`, drawn rather than typed so it never depends on font coverage.
pub fn triangle(ui: &Ui, c: egui::Pos2, size: f32, direction: Direction, fill: Color32) {
    let points = match direction {
        Direction::Down => vec![c + vec2(-size, -size / 2.0), c + vec2(size, -size / 2.0), c + vec2(0.0, size * 0.75)],
        Direction::Up => vec![c + vec2(-size, size / 2.0), c + vec2(size, size / 2.0), c + vec2(0.0, -size * 0.75)],
        Direction::Right => vec![c + vec2(-size * 0.6, -size), c + vec2(-size * 0.6, size), c + vec2(size, 0.0)],
    };
    ui.painter().add(egui::Shape::convex_polygon(points, fill, egui::Stroke::NONE));
}

/// Artwork with a title and subtitle. The whole card highlights on hover and opens on click; a play
/// button on the artwork plays it without opening.
fn card(ui: &mut Ui, card: &Card, actions: &mut Vec<Action>) {
    let pad = 8.0;
    let (rect, response) = ui.allocate_exact_size(vec2(CARD + 2.0 * pad, CARD + 2.0 * pad + 40.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let (image, round, title, subtitle) = match card {
        Card::Album(a) if a.year.is_empty() => (art(a.cover.as_deref(), 320), false, a.title.clone(), a.artist.clone()),
        Card::Album(a) => (art(a.cover.as_deref(), 320), false, a.title.clone(), format!("{} · {}", a.artist, a.year)),
        Card::Artist(a) => (art(a.picture.as_deref(), 320), true, a.name.clone(), "Artist".into()),
        Card::Playlist(p) => (art(p.cover.as_deref(), 320), false, p.title.clone(), format!("{} tracks", p.count)),
        Card::Mix(m) => (m.image.clone(), false, m.title.clone(), m.subtitle.clone()),
        Card::Folder { name, count, .. } => (None, false, format!("📁 {name}"), format!("Folder · {count} playlists")),
    };
    let hovered = response.hovered();
    if hovered {
        ui.painter().rect_filled(rect, 6.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let cover = Rect::from_min_size(rect.min + vec2(pad, pad), vec2(CARD, CARD));
    paint_picture(ui, image, cover, round);
    let line = |text: RichText| egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), CARD, egui::TextStyle::Body);
    let color = ui.visuals().text_color();
    ui.painter().galley(cover.left_bottom() + vec2(0.0, 6.0), line(RichText::new(title).strong()), color);
    ui.painter().galley(cover.left_bottom() + vec2(0.0, 24.0), line(RichText::new(subtitle).weak()), color);
    let play = Source::play(card);
    let button = cover.right_bottom() - vec2(24.0, 24.0);
    let on_button = play.is_some() && response.hover_pos().is_some_and(|p| p.distance(button) <= 18.0);
    if hovered && play.is_some() {
        ui.painter().circle_filled(button, 18.0, if on_button { ACCENT } else { Color32::from_gray(235) });
        triangle(ui, button + vec2(1.5, 0.0), 7.0, Direction::Right, Color32::BLACK);
    }
    if clickable(response).clicked() {
        actions.push(match play {
            Some(source) if on_button => Action::Play(source),
            _ => Action::Open(Source::open(card)),
        });
    }
}

/// How to start a page's tracks from its header buttons.
pub enum Start {
    Play,
    Shuffle,
    Radio,
}

/// The page title, with big artwork when it has some, and Play / Shuffle (/ Radio) buttons.
fn header(ui: &mut Ui, head: &Head, can_play: bool) -> Option<Start> {
    let mut start = None;
    let mut buttons = |ui: &mut Ui| {
        ui.horizontal(|ui| {
            if can_play && ui.button(RichText::new("▶  Play").size(16.0)).clicked() {
                start = Some(Start::Play);
            }
            if can_play && ui.button(RichText::new("🔀  Shuffle").size(16.0)).clicked() {
                start = Some(Start::Shuffle);
            }
            if head.radio.is_some() && ui.button(RichText::new("Radio").size(16.0)).clicked() {
                start = Some(Start::Radio);
            }
        });
    };
    match &head.art {
        Some((image, round)) => {
            ui.horizontal(|ui| {
                picture(ui, image.clone(), 200.0, *round);
                ui.vertical(|ui| {
                    ui.add_space(110.0);
                    ui.label(RichText::new(&head.title).size(30.0).strong());
                    ui.label(RichText::new(&head.subtitle).weak());
                    buttons(ui);
                });
            });
            ui.add_space(12.0);
        }
        None => {
            ui.horizontal(|ui| {
                section(ui, &head.title);
                buttons(ui);
            });
        }
    }
    start
}

fn filter_box(ui: &mut Ui, view: &mut View, hint: &str, width: f32) {
    ui.add(egui::TextEdit::singleline(&mut view.filter).hint_text(hint).desired_width(width));
}

/// One page: header, then its track table, card grid or shelves.
pub fn page(ui: &mut Ui, page: &mut Page, rows: &Rows, actions: &mut Vec<Action>) {
    let Page { head, body, view, .. } = page;
    let start = head.as_ref().and_then(|head| header(ui, head, !body.tracks().is_empty()));
    let mut order = None;
    match body {
        Body::Tracks { tracks, album_column } => {
            let sorted = view.as_ref().map(|v| (v.sort, v.reverse));
            if let Some(view) = view {
                filter_box(ui, view, "Filter on title, artist or album", f32::INFINITY);
                ui.add_space(8.0);
                order = Some(view.rows(tracks));
            }
            rows.show(ui, tracks, order, *album_column, sorted, actions);
        }
        Body::Grid { cards, sorts } => {
            if let Some(view) = view {
                ui.horizontal(|ui| {
                    filter_box(ui, view, "Filter", 220.0);
                    egui::ComboBox::from_id_salt("sort").selected_text(view.sort.label(view.reverse)).show_ui(ui, |ui| {
                        for &sort in *sorts {
                            let selected = view.sort == sort;
                            if ui.selectable_label(selected, sort.label(selected && view.reverse)).clicked() {
                                actions.push(Action::Sort(sort));
                            }
                        }
                    });
                });
                order = Some(view.rows(cards));
            }
            ui.horizontal_wrapped(|ui| ordered(cards, order).for_each(|c| card(ui, c, actions)));
            order = None;
        }
        Body::Shelves(shelves) => {
            for (n, shelf) in shelves.iter().enumerate() {
                section(ui, &shelf.title);
                if !shelf.cards.is_empty() {
                    egui::ScrollArea::horizontal().id_salt(("shelf", n)).show(ui, |ui| {
                        ui.horizontal(|ui| shelf.cards.iter().for_each(|c| card(ui, c, actions)));
                    });
                }
                if !shelf.tracks.is_empty() {
                    rows.show(ui, &shelf.tracks, None, true, None, actions);
                }
            }
        }
    }
    let tracks = || in_order(body.tracks(), order);
    match (start, head) {
        (Some(Start::Play), _) => actions.push(Action::PlayTracks(tracks(), 0)),
        (Some(Start::Shuffle), _) => actions.push(Action::ShuffleTracks(tracks())),
        (Some(Start::Radio), Some(head)) => actions.extend(head.radio.clone().map(Action::Play)),
        _ => {}
    }
}

fn cell(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(vec2(width, ROW), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(width);
        add(ui);
    });
}

/// How track rows behave: what is playing, which are favorites, and whether this list is the queue.
pub struct Rows<'a> {
    pub playing: Option<u64>,
    pub favorites: &'a HashSet<u64>,
    pub queue: bool,
}

impl Rows<'_> {
    /// A table of tracks under column headers, clickable to sort when `sorted` is set (sort, reversed).
    /// Click a title or a hovered number to play the list from there; right-click a title for more.
    pub fn show(&self, ui: &mut Ui, tracks: &[Track], order: Option<&[usize]>, album: bool, sorted: Option<(Sort, bool)>, actions: &mut Vec<Action>) {
        let added = tracks.first().is_some_and(|t| t.added.is_some());
        let columns = [(Sort::Title, "TITLE", 0.4, true), (Sort::Artist, "ARTIST", 0.25, true), (Sort::Album, "ALBUM", 0.22, album), (Sort::Added, "DATE ADDED", 0.13, added)];
        let columns: Vec<_> = columns.into_iter().filter(|c| c.3).collect();
        let free = ui.available_width() - NUMBER - TIME - HEART - ui.spacing().item_spacing.x * 6.0;
        let total: f32 = columns.iter().map(|c| c.2).sum();
        let width = |share: f32| free * share / total;
        ui.horizontal(|ui| {
            cell(ui, NUMBER, |ui| {
                ui.label(RichText::new("#").small().weak());
            });
            for &(sort, name, share, _) in &columns {
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
            let play = || if self.queue { Action::Jump(i) } else { Action::PlayTracks(in_order(tracks, order), pos) };
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
                        if clickable(button).clicked() {
                            actions.push(play());
                        }
                    } else {
                        ui.label(RichText::new((pos + 1).to_string()).weak());
                    }
                });
                for &(sort, _, share, _) in &columns {
                    cell(ui, width(share), |ui| match sort {
                        Sort::Title => {
                            let title = ui.add(egui::Label::new(RichText::new(&t.title).color(color)).truncate().sense(Sense::click()));
                            if title.clicked() {
                                actions.push(play());
                            }
                            title.context_menu(|ui| self.menu(ui, tracks, i, actions));
                        }
                        Sort::Artist => link(ui, &t.artist, t.artist_id.map(Source::Artist), actions),
                        Sort::Album => link(ui, &t.album, t.album_id.map(Source::Album), actions),
                        _ => {
                            ui.label(RichText::new(t.added.as_deref().unwrap_or_default()).weak());
                        }
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
            item("Play next", Action::Enqueue(t.clone(), true));
            item("Add to queue", Action::Enqueue(t.clone(), false));
            item("Track radio", Action::Play(Source::TrackRadio(t.id)));
        }
        if let Some(id) = t.album_id {
            item("Go to album", Action::Open(Source::Album(id)));
        }
        if let Some(id) = t.artist_id {
            item("Go to artist", Action::Open(Source::Artist(id)));
        }
    }
}

fn link(ui: &mut Ui, text: &str, to: Option<Source>, actions: &mut Vec<Action>) {
    if ui.add(egui::Link::new(text)).clicked()
        && let Some(source) = to
    {
        actions.push(Action::Open(source));
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
    if clickable(ui.interact(header.response.rect, ui.id().with(name), Sense::click())).clicked() {
        actions.push(Action::Sort(sort));
    }
}
