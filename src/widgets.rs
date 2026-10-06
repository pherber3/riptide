use std::collections::HashSet;

use egui::{Align, Color32, Layout, Rect, Response, RichText, Sense, Stroke, Ui, Vec2, pos2, vec2};

use crate::app::{Action, Body, Head, Page, Source};
use crate::theme::{ACCENT, DANGER, DIM, GOLD, HOVER, Icon, LINE, SECONDARY, SURFACE, TEXT, bold, medium, semibold};
use crate::tidal::{self, Card, Item, Playlist, Quality, Track};
use crate::view::{Sort, View, in_order, ordered};

const CARD: f32 = 168.0;
const NUMBER: f32 = 36.0;
const TIME: f32 = 56.0;
const HEART: f32 = 32.0;
const THUMB: f32 = 38.0;
/// Menus that hold a text field: a click inside (in the field) mustn't close them.
const KEEP_OPEN: egui::PopupCloseBehavior = egui::PopupCloseBehavior::CloseOnClickOutside;

pub fn tier_color(quality: Quality) -> Color32 {
    match quality {
        Quality::Max => GOLD,
        Quality::High => ACCENT,
        Quality::Low => SECONDARY,
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
    ui.add_space(28.0);
    ui.label(RichText::new(title).font(bold(22.0)).color(TEXT));
    ui.add_space(10.0);
}

pub fn clickable(response: Response) -> Response {
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Text that underlines on hover and clicks like a link, truncated to fit.
pub fn link_text(ui: &mut Ui, text: RichText) -> Response {
    let response = ui.add(egui::Label::new(text).truncate().selectable(false).sense(Sense::click()));
    if response.hovered() {
        ui.painter().hline(response.rect.x_range(), response.rect.bottom() - 1.0, Stroke::new(1.0, TEXT));
    }
    clickable(response)
}

/// An icon that brightens to white on hover when it rests grey.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size + 10.0), Sense::click());
    let color = if response.hovered() { color.lerp_to_gamma(Color32::WHITE, 0.5) } else { color };
    icon.image(color, size).paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
    clickable(response)
}

/// A rounded button with an icon: white for a page's main action, grey otherwise.
pub fn pill(ui: &mut Ui, icon: Icon, text: &str, primary: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.into(), semibold(14.0), Color32::PLACEHOLDER);
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x + 70.0, 40.0), Sense::click());
    let (fill, ink) = match (primary, response.hovered()) {
        (true, false) => (TEXT, Color32::BLACK),
        (true, true) => (Color32::from_gray(214), Color32::BLACK),
        (false, false) => (SURFACE, TEXT),
        (false, true) => (HOVER, TEXT),
    };
    ui.painter().rect_filled(rect, 20.0, fill);
    icon.image(ink, 16.0).paint_at(ui, Rect::from_min_size(rect.left_center() + vec2(22.0, -8.0), Vec2::splat(16.0)));
    ui.painter().galley(pos2(rect.left() + 46.0, rect.center().y - galley.size().y / 2.0), galley, ink);
    clickable(response)
}

/// A rounded text field with a search icon.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32, id: egui::Id) -> Response {
    let frame = egui::Frame::new().fill(SURFACE).corner_radius(18).inner_margin(egui::Margin::symmetric(12, 7));
    frame
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(Icon::Search.image(SECONDARY, 15.0));
                ui.add(egui::TextEdit::singleline(text).id(id).hint_text(hint).frame(egui::Frame::NONE).desired_width(width - 23.0))
            })
            .inner
        })
        .inner
}

/// A thin slider drawn like a progress bar, with a knob while hovered or dragged.
pub fn bar(ui: &mut Ui, value: &mut f64, max: f64, width: f32, enabled: bool) -> Response {
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, mut response) = ui.allocate_exact_size(vec2(width, 16.0), sense);
    if let Some(p) = response.interact_pointer_pos() {
        *value = f64::from(((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0)) * max;
        response.mark_changed();
    }
    let active = enabled && (response.hovered() || response.dragged());
    let track = Rect::from_center_size(rect.center(), vec2(width, 4.0));
    let x = track.left() + track.width() * (*value / max.max(1e-9)).clamp(0.0, 1.0) as f32;
    ui.painter().rect_filled(track, 2.0, HOVER);
    ui.painter().rect_filled(Rect::from_min_max(track.min, pos2(x, track.max.y)), 2.0, if active { TEXT } else { SECONDARY });
    if active {
        ui.painter().circle_filled(pos2(x, track.center().y), 6.0, TEXT);
    }
    if enabled { clickable(response) } else { response }
}

/// A sidebar entry, filled when selected or hovered.
pub fn nav_item(ui: &mut Ui, icon: Icon, text: &str, selected: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::click_and_drag());
    if !ui.is_rect_visible(rect) {
        return response;
    }
    let hovered = response.hovered();
    if selected || hovered {
        ui.painter().rect_filled(rect, 8.0, if selected { HOVER } else { SURFACE });
    }
    let color = if selected || hovered { TEXT } else { SECONDARY };
    icon.image(color, 18.0).paint_at(ui, Rect::from_min_size(rect.left_center() + vec2(10.0, -9.0), Vec2::splat(18.0)));
    let text = egui::WidgetText::from(RichText::new(text).font(medium(14.0)));
    let galley = text.into_galley(ui, Some(egui::TextWrapMode::Truncate), rect.width() - 48.0, egui::TextStyle::Body);
    ui.painter().galley(pos2(rect.left() + 40.0, rect.center().y - galley.size().y / 2.0), galley, color);
    clickable(response)
}

pub fn heart(ui: &mut Ui, id: u64, favorites: &HashSet<u64>, visible: bool, actions: &mut Vec<Action>) {
    let on = favorites.contains(&id);
    if !on && !visible {
        ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
        return;
    }
    let (icon, color, hint) = if on { (Icon::HeartFilled, ACCENT, "Remove from your collection") } else { (Icon::Heart, SECONDARY, "Add to your collection") };
    if icon_button(ui, icon, 16.0, color).on_hover_text(hint).clicked() {
        actions.push(Action::Favorite(id, !on));
    }
}

pub fn picture(ui: &mut Ui, url: Option<String>, side: f32, round: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    paint_picture(ui, url, rect, round);
    response
}

/// Artwork, loaded only once it scrolls into view so long shelves and grids don't fill memory.
fn paint_picture(ui: &Ui, url: Option<String>, rect: Rect, round: bool) {
    let radius = if round { rect.width() / 2.0 } else { (rect.width() / 28.0).clamp(4.0, 8.0) };
    ui.painter().rect_filled(rect, radius, SURFACE);
    if let Some(url) = url
        && ui.is_rect_visible(rect)
    {
        // Artwork that can't be had leaves the plain tile rather than egui's error mark.
        let image = egui::Image::new(url).corner_radius(radius);
        if image.load_for_size(ui.ctx(), rect.size()).is_ok() {
            image.paint_at(ui, rect);
        }
    }
}

/// The sorted column's small arrow, pointing up or down.
fn arrow(ui: &Ui, c: egui::Pos2, up: bool) {
    let (tip, base) = if up { (-3.0, 2.0) } else { (3.0, -2.0) };
    let points = vec![c + vec2(-4.0, base), c + vec2(4.0, base), c + vec2(0.0, tip)];
    ui.painter().add(egui::Shape::convex_polygon(points, ACCENT, Stroke::NONE));
}

/// The white round play (or pause) button, a little bigger while hovered.
pub fn play_disc(ui: &Ui, center: egui::Pos2, radius: f32, hovered: bool, icon: Icon) {
    let (radius, fill) = if hovered { (radius + 1.0, Color32::WHITE) } else { (radius, TEXT) };
    ui.painter().circle_filled(center, radius, fill);
    let nudge = if icon == Icon::Play { 1.0 } else { 0.0 };
    icon.image(Color32::BLACK, 16.0).paint_at(ui, Rect::from_center_size(center + vec2(nudge, 0.0), Vec2::splat(16.0)));
}

/// Artwork with a title and subtitle. The whole card highlights on hover and opens on click; a play
/// button on the artwork plays it without opening.
fn card(ui: &mut Ui, card: &Card, actions: &mut Vec<Action>) {
    let pad = 10.0;
    let (rect, response) = ui.allocate_exact_size(vec2(CARD + 2.0 * pad, CARD + 2.0 * pad + 44.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let (image, round, title, subtitle) = match card {
        Card::Album(a) if a.year.is_empty() => (art(a.cover.as_deref(), 320), false, a.title.clone(), a.artist.clone()),
        Card::Album(a) => (art(a.cover.as_deref(), 320), false, a.title.clone(), format!("{} · {}", a.artist, a.year)),
        Card::Artist(a) => (art(a.picture.as_deref(), 320), true, a.name.clone(), "Artist".into()),
        Card::Playlist(p) => (art(p.cover.as_deref(), 320), false, p.title.clone(), format!("{} tracks", p.count)),
        Card::Mix(m) => (m.image.clone(), false, m.title.clone(), m.subtitle.clone()),
        Card::Folder { name, count, .. } => (None, false, name.clone(), format!("Folder · {count} playlists")),
    };
    let hovered = response.hovered();
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(rect, 10.0, SURFACE);
    }
    let cover = Rect::from_min_size(rect.min + vec2(pad, pad), Vec2::splat(CARD));
    paint_picture(ui, image, cover, round);
    if let Card::Folder { .. } = card {
        Icon::Folder.image(DIM, 56.0).paint_at(ui, Rect::from_center_size(cover.center(), Vec2::splat(56.0)));
    }
    let line = |text: RichText| egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), CARD, egui::TextStyle::Body);
    painter.galley(cover.left_bottom() + vec2(0.0, 8.0), line(RichText::new(title).font(semibold(14.0))), TEXT);
    painter.galley(cover.left_bottom() + vec2(0.0, 27.0), line(RichText::new(subtitle).size(13.0)), SECONDARY);
    let play = Source::play(card);
    let button = cover.right_bottom() - vec2(30.0, 30.0);
    let on_button = play.is_some() && response.hover_pos().is_some_and(|p| p.distance(button) <= 20.0);
    if hovered && play.is_some() {
        painter.circle_filled(button + vec2(0.0, 2.0), 21.0, Color32::from_black_alpha(90));
        play_disc(ui, button, 20.0, on_button, Icon::Play);
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
fn header(ui: &mut Ui, head: &Head, can_play: bool, rows: &Rows, actions: &mut Vec<Action>) -> Option<Start> {
    let mut start = None;
    let mut buttons = |ui: &mut Ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if can_play && pill(ui, Icon::Play, "Play", true).clicked() {
                start = Some(Start::Play);
            }
            if can_play && pill(ui, Icon::Shuffle, "Shuffle", false).clicked() {
                start = Some(Start::Shuffle);
            }
            if head.radio.is_some() && pill(ui, Icon::Radio, "Radio", false).clicked() {
                start = Some(Start::Radio);
            }
            match &head.item {
                Some(Item::Playlist(id)) if rows.mine(id) => playlist_menu(ui, id, &head.title, rows.folders, actions),
                // An artist is followed with a heart; an album or playlist is added to its list.
                Some(item) => {
                    let on = rows.saved.contains(item);
                    let list = match item {
                        Item::Album(_) => "Albums",
                        Item::Playlist(_) => "Playlists",
                        Item::Artist(_) => "Artists",
                    };
                    let icon = match (item, on) {
                        (Item::Artist(_), true) => Icon::HeartFilled,
                        (Item::Artist(_), false) => Icon::Heart,
                        (_, true) => Icon::Check,
                        (_, false) => Icon::Plus,
                    };
                    let hint = if on { format!("Remove from your {list}") } else { format!("Add to your {list}") };
                    if icon_button(ui, icon, 22.0, if on { ACCENT } else { SECONDARY }).on_hover_text(hint).clicked() {
                        actions.push(Action::Save(item.clone(), !on));
                    }
                }
                None => {}
            }
        });
    };
    ui.add_space(12.0);
    match &head.art {
        Some((image, round)) => {
            let side = 232.0;
            ui.horizontal(|ui| {
                picture(ui, image.clone(), side, *round);
                ui.add_space(20.0);
                ui.allocate_ui_with_layout(vec2(ui.available_width(), side), Layout::bottom_up(Align::Min), |ui| {
                    buttons(ui);
                    ui.add_space(12.0);
                    if !head.subtitle.is_empty() {
                        ui.add(egui::Label::new(RichText::new(&head.subtitle).size(15.0).color(SECONDARY)).truncate());
                    }
                    ui.add(egui::Label::new(RichText::new(&head.title).font(bold(40.0)).color(TEXT)).truncate());
                    ui.label(RichText::new(head.kind).font(semibold(12.0)).color(SECONDARY));
                });
            });
        }
        None => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&head.title).font(bold(32.0)).color(TEXT));
                ui.add_space(16.0);
                buttons(ui);
            });
        }
    }
    ui.add_space(16.0);
    start
}

/// The ⋯ menu of one of the user's own playlists.
fn playlist_menu(ui: &mut Ui, id: &str, title: &str, folders: &[Card], actions: &mut Vec<Action>) {
    let more = egui::Button::image(Icon::More.image(SECONDARY, 22.0)).frame(false);
    egui::containers::menu::MenuButton::from_button(more).ui(ui, |ui| playlist_actions(ui, id, title, folders, actions));
}

/// Rename, move or delete one of the user's own playlists: its ⋯ menu, or right-click in the sidebar.
pub fn playlist_actions(ui: &mut Ui, id: &str, title: &str, folders: &[Card], actions: &mut Vec<Action>) {
    {
        ui.set_min_width(220.0);
        ui.spacing_mut().button_padding = vec2(12.0, 7.0);
        if ui.button("Rename").clicked() {
            actions.push(Action::PlaylistForm(Target::Rename(id.into()), title.into()));
            ui.close();
        }
        ui.menu_button("Move to folder", |ui| {
            let named = folders.iter().filter_map(|c| match c {
                Card::Folder { id, name, .. } => Some((id.as_str(), name.as_str())),
                _ => None,
            });
            for (folder, name) in std::iter::once((crate::app::ROOT, "Top level")).chain(named) {
                if ui.button(name).clicked() {
                    actions.push(Action::MovePlaylist(id.into(), folder.into()));
                    ui.close();
                }
            }
        });
        ui.menu_button("Delete playlist", |ui| {
            ui.label(RichText::new("This can't be undone.").color(SECONDARY));
            if ui.button(RichText::new("Delete").color(DANGER)).clicked() {
                actions.push(Action::DeletePlaylist(id.into()));
                ui.close();
            }
        });
    }
}

/// An on/off switch.
pub fn switch(ui: &mut Ui, on: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(40.0, 22.0), egui::Sense::click());
    let t = ui.ctx().animate_bool(response.id, on);
    ui.painter().rect_filled(rect, 11.0, HOVER.lerp_to_gamma(ACCENT, t));
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, TEXT);
    clickable(response)
}

/// What the playlist dialog is for: a new playlist (with a track to put in it), or renaming one.
pub enum Target {
    Create(Option<u64>),
    Rename(String),
}

pub struct PlaylistForm {
    pub target: Target,
    pub title: String,
    pub description: String,
    pub public: bool,
}

/// The Create playlist (or Rename playlist) dialog, as Tidal has it. Some(true) when saved,
/// Some(false) when closed.
pub fn playlist_dialog(ctx: &egui::Context, form: &mut PlaylistForm) -> Option<bool> {
    let creating = matches!(form.target, Target::Create(_));
    let frame = egui::Frame::new().fill(SURFACE).corner_radius(14).inner_margin(24);
    let modal = egui::Modal::new(egui::Id::new("playlist dialog")).frame(frame).show(ctx, |ui| {
        ui.set_width(460.0);
        let mut close = false;
        ui.horizontal(|ui| {
            ui.label(RichText::new(if creating { "Create playlist" } else { "Rename playlist" }).font(bold(22.0)).color(TEXT));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| close = icon_button(ui, Icon::Close, 18.0, SECONDARY).clicked());
        });
        ui.add_space(16.0);
        let title = ui.add(egui::TextEdit::singleline(&mut form.title).hint_text("Title").font(medium(16.0)).margin(vec2(14.0, 12.0)).desired_width(f32::INFINITY));
        if ui.ctx().memory(|m| m.focused().is_none()) {
            title.request_focus();
        }
        let enter = title.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if creating {
            ui.add_space(10.0);
            let description = egui::TextEdit::multiline(&mut form.description).hint_text("Write a description").char_limit(500).desired_rows(4);
            ui.add(description.margin(vec2(14.0, 12.0)).desired_width(f32::INFINITY));
            ui.label(RichText::new(format!("{}/500 characters", form.description.chars().count())).size(12.0).color(DIM));
            ui.add_space(12.0);
            egui::Frame::new().fill(HOVER).corner_radius(10).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Make it public").font(semibold(15.0)).color(TEXT));
                        ui.label(RichText::new("Your playlist will be visible on your profile and to anyone.").size(13.0).color(SECONDARY));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if switch(ui, form.public).clicked() {
                            form.public = !form.public;
                        }
                    });
                });
            });
        }
        ui.add_space(16.0);
        let ready = !form.title.trim().is_empty();
        let save = ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let button = egui::Button::new(RichText::new("Save").font(semibold(14.0)).color(Color32::BLACK)).fill(TEXT).corner_radius(20).min_size(vec2(88.0, 40.0));
            ui.add_enabled(ready, button).clicked()
        });
        (close, ready && (save.inner || enter))
    });
    let (close, save) = modal.inner;
    if save {
        Some(true)
    } else if close || modal.should_close() {
        Some(false)
    } else {
        None
    }
}

fn filter_box(ui: &mut Ui, view: &mut View) {
    search_field(ui, &mut view.filter, "Filter", 240.0, egui::Id::new(("filter", view.key)));
}

/// One page: header, then its track table, card grid or shelves.
pub fn page(ui: &mut Ui, page: &mut Page, rows: &Rows, actions: &mut Vec<Action>) {
    let Page { head, body, view, .. } = page;
    let start = head.as_ref().and_then(|head| header(ui, head, !body.tracks().is_empty(), rows, actions));
    let mut order = None;
    match body {
        Body::Tracks { tracks, album_column } => {
            let sorted = view.as_ref().map(|v| (v.sort, v.reverse));
            if let Some(view) = view {
                filter_box(ui, view);
                ui.add_space(12.0);
                order = Some(view.rows(tracks));
            }
            rows.show(ui, tracks, order, *album_column, sorted, actions);
        }
        Body::Grid { cards, sorts } => {
            if let Some(view) = view {
                ui.horizontal(|ui| {
                    filter_box(ui, view);
                    egui::ComboBox::from_id_salt("sort").selected_text(view.sort.label(view.reverse)).show_ui(ui, |ui| {
                        for &sort in *sorts {
                            let selected = view.sort == sort;
                            if ui.selectable_label(selected, sort.label(selected && view.reverse)).clicked() {
                                actions.push(Action::Sort(sort));
                            }
                        }
                    });
                });
                ui.add_space(8.0);
                order = Some(view.rows(cards));
            }
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                ordered(cards, order).for_each(|c| card(ui, c, actions));
            });
            order = None;
        }
        Body::Settings => {}
        Body::Shelves(shelves) => {
            for (n, shelf) in shelves.iter().enumerate() {
                section(ui, &shelf.title);
                if !shelf.cards.is_empty() {
                    egui::ScrollArea::horizontal().id_salt(("shelf", n)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            shelf.cards.iter().for_each(|c| card(ui, c, actions));
                        });
                    });
                }
                if !shelf.tracks.is_empty() {
                    rows.show(ui, &shelf.tracks, None, true, None, actions);
                }
            }
        }
    }
    ui.add_space(24.0);
    let tracks = || in_order(body.tracks(), order);
    match (start, head) {
        (Some(Start::Play), _) => actions.push(Action::PlayTracks(tracks(), 0)),
        (Some(Start::Shuffle), _) => actions.push(Action::ShuffleTracks(tracks())),
        (Some(Start::Radio), Some(head)) => actions.extend(head.radio.clone().map(Action::Play)),
        _ => {}
    }
}

fn cell(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(vec2(width, height), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(width);
        add(ui);
    });
}

/// How track rows behave: what is playing, which are favorites, and whether this list is the queue.
pub struct Rows<'a> {
    pub playing: Option<u64>,
    pub favorites: &'a HashSet<u64>,
    pub playlists: &'a [Playlist],
    pub saved: &'a HashSet<Item>,
    /// Top-level folders and playlists, to move a playlist into.
    pub folders: &'a [Card],
    /// The id of the user's own playlist these rows are, whose tracks can be removed.
    pub editing: Option<&'a str>,
    pub queue: bool,
}

impl Rows<'_> {
    /// Whether the user made this playlist (and so can change it).
    fn mine(&self, id: &str) -> bool {
        self.playlists.iter().any(|p| p.id == id)
    }

    /// A table of tracks under column headers, clickable to sort when `sorted` is set (sort, reversed).
    /// Click a title or a hovered number, or double-click a row, to play the list from there;
    /// right-click a row for more. Lists that span albums show each track's cover.
    pub fn show(&self, ui: &mut Ui, tracks: &[Track], order: Option<&[usize]>, album: bool, sorted: Option<(Sort, bool)>, actions: &mut Vec<Action>) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let height = if album { 52.0 } else { 40.0 };
            let added = tracks.first().is_some_and(|t| t.added.is_some());
            let columns = [(Sort::Title, "TITLE", 0.4, true), (Sort::Artist, "ARTIST", 0.25, true), (Sort::Album, "ALBUM", 0.22, album), (Sort::Added, "DATE ADDED", 0.13, added)];
            let columns: Vec<_> = columns.into_iter().filter(|c| c.3).collect();
            let free = ui.available_width() - NUMBER - TIME - HEART - ui.spacing().item_spacing.x * 6.0;
            let total: f32 = columns.iter().map(|c| c.2).sum();
            let width = |share: f32| free * share / total;
            ui.horizontal(|ui| {
                cell(ui, NUMBER, 28.0, |ui| {
                    ui.label(RichText::new("#").font(medium(11.0)).color(DIM));
                });
                for &(sort, name, share, _) in &columns {
                    cell(ui, width(share), 28.0, |ui| column_header(ui, name, sort, sorted, actions));
                }
                cell(ui, TIME, 28.0, |ui| column_header(ui, "TIME", Sort::Duration, sorted, actions));
            });
            let full = ui.available_width();
            let left = ui.cursor().left();
            ui.painter().hline(left..=left + full, ui.cursor().top(), Stroke::new(1.0, LINE));
            ui.add_space(6.0);
            for (pos, t) in ordered(tracks, order).enumerate() {
                let rect = Rect::from_min_size(ui.cursor().min, vec2(full, height));
                // Rows scrolled out of view only take up space, so long playlists stay cheap to draw.
                if !ui.is_rect_visible(rect) {
                    ui.allocate_space(vec2(free, height));
                    continue;
                }
                let i = order.map_or(pos, |o| o[pos]);
                let play = || if self.queue { Action::Jump(i) } else { Action::PlayTracks(in_order(tracks, order), pos) };
                let row = ui.interact(rect, ui.id().with(("row", pos)), Sense::click());
                let hovered = ui.rect_contains_pointer(rect);
                if hovered {
                    ui.painter().rect_filled(rect, 6.0, SURFACE);
                }
                let playing = self.playing == Some(t.id);
                ui.horizontal(|ui| {
                    cell(ui, NUMBER, height, |ui| {
                        if hovered {
                            if icon_button(ui, Icon::Play, 14.0, TEXT).clicked() {
                                actions.push(play());
                            }
                        } else if playing {
                            ui.add(Icon::Playing.image(ACCENT, 16.0));
                        } else {
                            ui.label(RichText::new((pos + 1).to_string()).color(DIM));
                        }
                    });
                    for &(sort, _, share, _) in &columns {
                        cell(ui, width(share), height, |ui| match sort {
                            Sort::Title => {
                                if album {
                                    picture(ui, art(t.cover.as_deref(), 80), THUMB, false);
                                    ui.add_space(4.0);
                                }
                                let color = if playing { ACCENT } else { TEXT };
                                let title = ui.add(egui::Label::new(RichText::new(&t.title).font(medium(14.0)).color(color)).truncate().selectable(false).sense(Sense::click()));
                                if title.clicked() {
                                    actions.push(play());
                                }
                            }
                            Sort::Artist => link(ui, &t.artist, t.artist_id.map(Source::Artist), actions),
                            Sort::Album => link(ui, &t.album, t.album_id.map(Source::Album), actions),
                            _ => {
                                ui.label(RichText::new(t.added.as_deref().unwrap_or_default()).color(SECONDARY));
                            }
                        });
                    }
                    cell(ui, TIME, height, |ui| {
                        ui.label(RichText::new(clock(f64::from(t.duration))).color(SECONDARY));
                    });
                    heart(ui, t.id, self.favorites, hovered, actions);
                });
                if row.double_clicked() {
                    actions.push(play());
                }
                egui::Popup::context_menu(&row).close_behavior(KEEP_OPEN).show(|ui| self.menu(ui, tracks, i, actions));
            }
        });
    }

    /// A track's right-click menu, laid out like Tidal's: the track, then what to do with it.
    fn menu(&self, ui: &mut Ui, tracks: &[Track], i: usize, actions: &mut Vec<Action>) {
        let t = &tracks[i];
        ui.set_min_width(240.0);
        ui.spacing_mut().button_padding = vec2(12.0, 7.0);
        ui.horizontal(|ui| {
            picture(ui, art(t.cover.as_deref(), 80), 40.0, false);
            ui.vertical(|ui| {
                ui.add(egui::Label::new(RichText::new(&t.title).font(semibold(14.0)).color(TEXT)).truncate());
                ui.add(egui::Label::new(RichText::new(&t.artist).size(13.0).color(SECONDARY)).truncate());
            });
        });
        ui.separator();
        let item = |ui: &mut Ui, actions: &mut Vec<Action>, text: &str, action: Action| {
            if ui.button(text).clicked() {
                actions.push(action);
                ui.close();
            }
        };
        if self.queue {
            if i > 0 {
                item(ui, actions, "Move up", Action::Move(i, i - 1));
            }
            if i + 1 < tracks.len() {
                item(ui, actions, "Move down", Action::Move(i, i + 1));
            }
            if self.playing != Some(t.id) {
                item(ui, actions, "Remove from queue", Action::Remove(i));
            }
        } else {
            item(ui, actions, "Play next", Action::Enqueue(t.clone(), true));
            item(ui, actions, "Add to queue", Action::Enqueue(t.clone(), false));
        }
        if let Some(playlist) = self.editing {
            item(ui, actions, "Remove from this playlist", Action::RemoveFromPlaylist(playlist.into(), i));
        }
        let config = egui::containers::menu::MenuConfig::new().close_behavior(KEEP_OPEN);
        egui::containers::menu::SubMenuButton::new("Add to playlist").config(config).ui(ui, |ui| self.add_to_playlist(ui, t.id, actions));
        let saved = self.favorites.contains(&t.id);
        let collection = if saved { "Remove from My Collection" } else { "Add to My Collection" };
        item(ui, actions, collection, Action::Favorite(t.id, !saved));
        item(ui, actions, "Go to track radio", Action::Play(Source::TrackRadio(t.id)));
        if let Some(id) = t.album_id {
            item(ui, actions, "Go to album", Action::Open(Source::Album(id)));
        }
        if let Some(id) = t.artist_id {
            item(ui, actions, "Go to artist", Action::Open(Source::Artist(id)));
        }
    }

    /// The "Add to playlist" submenu: make a new one, or pick from the recent ones or any by name.
    fn add_to_playlist(&self, ui: &mut Ui, track: u64, actions: &mut Vec<Action>) {
        ui.set_min_width(240.0);
        ui.spacing_mut().button_padding = vec2(12.0, 7.0);
        let filter_id = egui::Id::new("playlist filter");
        if ui.add(egui::Button::image_and_text(Icon::Plus.image(TEXT, 16.0), "Create new playlist")).clicked() {
            actions.push(Action::PlaylistForm(Target::Create(Some(track)), String::new()));
            ui.close();
        }
        ui.separator();
        let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
        ui.add(egui::TextEdit::singleline(&mut filter).hint_text("Find a playlist").desired_width(216.0));
        let needle = filter.to_lowercase();
        ui.label(RichText::new(if needle.is_empty() { "RECENT" } else { "MATCHING" }).font(semibold(11.0)).color(DIM));
        let shown = if needle.is_empty() { 10 } else { usize::MAX };
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for p in self.playlists.iter().filter(|p| p.title.to_lowercase().contains(&needle)).take(shown) {
                if ui.button(&p.title).clicked() {
                    actions.push(Action::AddToPlaylist(p.id.clone(), track));
                    ui.close();
                }
            }
        });
        ui.data_mut(|d| d.insert_temp(filter_id, filter));
    }
}

fn link(ui: &mut Ui, text: &str, to: Option<Source>, actions: &mut Vec<Action>) {
    if link_text(ui, RichText::new(text).color(SECONDARY)).clicked()
        && let Some(source) = to
    {
        actions.push(Action::Open(source));
    }
}

/// A column title; clickable with an arrow on the sorted column when the table sorts.
fn column_header(ui: &mut Ui, name: &str, sort: Sort, sorted: Option<(Sort, bool)>, actions: &mut Vec<Action>) {
    let text = RichText::new(name).font(medium(11.0));
    let Some((current, reverse)) = sorted else {
        ui.label(text.color(DIM));
        return;
    };
    let active = current == sort;
    let header = ui.horizontal(|ui| {
        ui.label(text.color(if active { TEXT } else { DIM }));
        if active {
            let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            arrow(ui, rect.center(), reverse);
        }
    });
    if clickable(ui.interact(header.response.rect, ui.id().with(name), Sense::click())).clicked() {
        actions.push(Action::Sort(sort));
    }
}
