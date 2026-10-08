use egui::{Align, Color32, Layout, Rect, Response, RichText, Sense, Stroke, Ui, Vec2, pos2, vec2};

use crate::app::{Action, Body, Head, Library, Page, Source};
use crate::dialogs::{self, Target};
use crate::theme::{p, GOLD, TEAL, Icon, bold, medium, semibold};
use crate::tidal::{self, Card, Item, Quality, Track};
use crate::view::{Sort, View, in_order, ordered};

const CARD: f32 = 168.0;
const NUMBER: f32 = 36.0;
const TIME: f32 = 56.0;
const HEART: f32 = 32.0;
const THUMB: f32 = 38.0;

/// A playlist row being dragged to a new place, by position.
struct DraggedRow(usize);
/// Menus that hold a text field: a click inside (in the field) mustn't close them.
const KEEP_OPEN: egui::PopupCloseBehavior = egui::PopupCloseBehavior::CloseOnClickOutside;

pub fn tier_color(quality: Quality) -> Color32 {
    match quality {
        Quality::Max => GOLD,
        Quality::High => TEAL,
        Quality::Low => p().secondary,
    }
}

pub fn clock(seconds: f64) -> String {
    let s = seconds as u64;
    match s / 3600 {
        0 => format!("{}:{:02}", s / 60, s % 60),
        h => format!("{h}:{:02}:{:02}", s / 60 % 60, s % 60),
    }
}

pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(28.0);
    ui.label(RichText::new(title).font(bold(22.0)).color(p().text));
    ui.add_space(10.0);
}

pub fn clickable(response: Response) -> Response {
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Text that underlines on hover and clicks like a link, truncated to fit.
pub fn link_text(ui: &mut Ui, text: RichText) -> Response {
    let response = ui.add(egui::Label::new(text).truncate().selectable(false).sense(Sense::click()));
    if response.hovered() {
        ui.painter().hline(response.rect.x_range(), response.rect.bottom() - 1.0, Stroke::new(1.0, p().text));
    }
    clickable(response)
}

/// An icon that brightens to white on hover when it rests grey.
pub fn icon_button(ui: &mut Ui, icon: Icon, size: f32, color: Color32) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size + 10.0), Sense::click());
    let color = if response.hovered() { color.lerp_to_gamma(p().text, 0.5) } else { color };
    icon.image(color, size).paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
    clickable(response)
}

/// A rounded button with an icon: white for a page's main action, grey otherwise.
pub fn pill(ui: &mut Ui, icon: Icon, text: &str, primary: bool) -> Response {
    let galley = ui.painter().layout_no_wrap(text.into(), semibold(14.0), Color32::PLACEHOLDER);
    let (rect, response) = ui.allocate_exact_size(vec2(galley.size().x + 70.0, 40.0), Sense::click());
    let (fill, ink) = match (primary, response.hovered()) {
        (true, false) => (p().text, p().window),
        (true, true) => (p().text.gamma_multiply(0.85), p().window),
        (false, false) => (p().surface, p().text),
        (false, true) => (p().surface_hover, p().text),
    };
    ui.painter().rect_filled(rect, 20.0, fill);
    icon.image(ink, 16.0).paint_at(ui, Rect::from_min_size(rect.left_center() + vec2(22.0, -8.0), Vec2::splat(16.0)));
    ui.painter().galley(pos2(rect.left() + 46.0, rect.center().y - galley.size().y / 2.0), galley, ink);
    clickable(response)
}

/// A rounded text field with a search icon.
pub fn search_field(ui: &mut Ui, text: &mut String, hint: &str, width: f32, id: egui::Id) -> Response {
    let frame = egui::Frame::new().fill(p().surface).corner_radius(18).inner_margin(egui::Margin::symmetric(12, 7));
    frame
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(Icon::Search.image(p().secondary, 15.0));
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
    ui.painter().rect_filled(track, 2.0, p().surface_hover);
    ui.painter().rect_filled(Rect::from_min_max(track.min, pos2(x, track.max.y)), 2.0, if active { p().text } else { p().secondary });
    if active {
        ui.painter().circle_filled(pos2(x, track.center().y), 6.0, p().text);
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
        ui.painter().rect_filled(rect, 8.0, if selected { p().surface_hover } else { p().surface });
    }
    let color = if selected || hovered { p().text } else { p().secondary };
    icon.image(color, 18.0).paint_at(ui, Rect::from_min_size(rect.left_center() + vec2(10.0, -9.0), Vec2::splat(18.0)));
    let text = egui::WidgetText::from(RichText::new(text).font(medium(14.0)));
    let galley = text.into_galley(ui, Some(egui::TextWrapMode::Truncate), rect.width() - 48.0, egui::TextStyle::Body);
    ui.painter().galley(pos2(rect.left() + 40.0, rect.center().y - galley.size().y / 2.0), galley, color);
    clickable(response)
}

/// A menu entry that does `action`.
pub fn menu_item(ui: &mut Ui, actions: &mut Vec<Action>, text: impl Into<egui::WidgetText>, action: Action) {
    if ui.button(text).clicked() {
        actions.push(action);
        ui.close();
    }
}

pub fn heart(ui: &mut Ui, id: u64, library: &Library, visible: bool, actions: &mut Vec<Action>) {
    let item = Item::Track(id);
    let on = library.saved.contains(&item);
    if !on && !visible {
        ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
        return;
    }
    let (icon, color, hint) = if on { (Icon::HeartFilled, p().accent, "Remove from your collection") } else { (Icon::Heart, p().secondary, "Add to your collection") };
    if icon_button(ui, icon, 16.0, color).on_hover_text(hint).clicked() {
        actions.push(Action::Save(item, !on));
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
    ui.painter().rect_filled(rect, radius, p().surface);
    if let Some(url) = url
        && ui.is_rect_visible(rect)
    {
        crate::art::drawn(ui.ctx(), &url);
        // Artwork that can't be had leaves the plain tile rather than egui's error mark.
        let image = egui::Image::new(url).corner_radius(radius);
        if image.load_for_size(ui.ctx(), rect.size()).is_ok() {
            image.paint_at(ui, rect);
        }
    }
}

/// The tide along the bottom of the lyrics view: three layers of water drifting across each other,
/// standing higher and rolling faster as the music gets louder.
#[derive(Default)]
pub struct Tide {
    /// How high it stands, 0 to 1.
    swell: f32,
    /// How far the water has rolled.
    travel: f64,
}

impl Tide {
    /// Moves on `dt` seconds toward the music's loudness (RMS `level`). Returns whether it is still
    /// settling.
    pub fn step(&mut self, level: f32, dt: f32) -> bool {
        let target = (level * 3.0).min(1.0);
        // Slow both ways, so it follows verses and choruses rather than each drum hit.
        let rate = if target > self.swell { 2.5 } else { 0.8 };
        self.swell += (target - self.swell) * (1.0 - (-rate * dt).exp());
        self.travel += f64::from(dt * (0.6 + 1.4 * self.swell));
        self.swell > 0.005
    }

    pub fn paint(&self, ui: &Ui, rect: Rect) {
        const STEPS: u32 = 96;
        let (swell, travel) = (self.swell, self.travel);
        let painter = ui.painter_at(rect);
        // Back to front: fainter, higher and slower behind; each layer drifts its own way.
        for (layer, (alpha, scale, speed, offset)) in [(10u8, 0.7, 0.21, 0.0), (16, 0.85, -0.29, 2.1), (24, 1.0, 0.37, 4.2)].into_iter().enumerate() {
            let base = rect.bottom() - rect.height() * (0.55 - 0.15 * layer as f32);
            let height = (4.0 + 30.0 * swell) * scale;
            let k = std::f32::consts::TAU / (rect.width() * (0.9 - 0.2 * layer as f32)).max(1.0);
            let turn = |rate: f64| (travel * speed * rate).rem_euclid(std::f64::consts::TAU) as f32 + offset;
            let (long, short) = (turn(1.0), turn(-1.4));
            let (crest, deep) = (Color32::from_white_alpha(alpha), Color32::from_white_alpha(alpha / 4));
            let mut water = egui::Mesh::default();
            water.reserve_vertices(2 * STEPS as usize + 2);
            water.reserve_triangles(2 * STEPS as usize);
            for i in 0..=STEPS {
                let x = rect.left() + rect.width() * i as f32 / STEPS as f32;
                let u = k * (x - rect.left());
                let y = base - height * (0.65 * (u + long).sin() + 0.35 * (2.3 * u + short).sin());
                water.colored_vertex(pos2(x, y), crest);
                water.colored_vertex(pos2(x, rect.bottom()), deep);
                if i > 0 {
                    let v = 2 * i;
                    water.add_triangle(v - 2, v - 1, v);
                    water.add_triangle(v - 1, v, v + 1);
                }
            }
            painter.add(water);
        }
    }
}

/// The sorted column's small arrow, pointing up or down.
fn arrow(ui: &Ui, c: egui::Pos2, up: bool) {
    let (tip, base) = if up { (-3.0, 2.0) } else { (3.0, -2.0) };
    let points = vec![c + vec2(-4.0, base), c + vec2(4.0, base), c + vec2(0.0, tip)];
    ui.painter().add(egui::Shape::convex_polygon(points, p().accent, Stroke::NONE));
}

/// The white round play (or pause) button, a little bigger while hovered.
pub fn play_disc(ui: &Ui, center: egui::Pos2, radius: f32, hovered: bool, icon: Icon) {
    let (radius, fill) = if hovered { (radius + 1.0, p().text.gamma_multiply(0.85)) } else { (radius, p().text) };
    ui.painter().circle_filled(center, radius, fill);
    let nudge = if icon == Icon::Play { 1.0 } else { 0.0 };
    icon.image(p().window, 16.0).paint_at(ui, Rect::from_center_size(center + vec2(nudge, 0.0), Vec2::splat(16.0)));
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
        Card::Album(a) if a.year.is_empty() => (tidal::image(a.cover.as_deref(), 320), false, a.title.clone(), a.artist.clone()),
        Card::Album(a) => (tidal::image(a.cover.as_deref(), 320), false, a.title.clone(), format!("{} · {}", a.artist, a.year)),
        Card::Artist(a) => (tidal::image(a.picture.as_deref(), 320), true, a.name.clone(), "Artist".into()),
        Card::Playlist(p) => (tidal::image(p.cover.as_deref(), 320), false, p.title.clone(), format!("{} tracks", p.count)),
        Card::Mix(m) => (m.image.clone(), false, m.title.clone(), m.subtitle.clone()),
        Card::Folder { name, count, .. } => (None, false, name.clone(), format!("Folder · {count} playlists")),
    };
    let hovered = response.hovered();
    let painter = ui.painter();
    if hovered {
        painter.rect_filled(rect, 10.0, p().surface);
    }
    let cover = Rect::from_min_size(rect.min + vec2(pad, pad), Vec2::splat(CARD));
    paint_picture(ui, image, cover, round);
    if let Card::Folder { .. } = card {
        Icon::Folder.image(p().dim, 56.0).paint_at(ui, Rect::from_center_size(cover.center(), Vec2::splat(56.0)));
    }
    let line = |text: RichText| egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), CARD, egui::TextStyle::Body);
    painter.galley(cover.left_bottom() + vec2(0.0, 8.0), line(RichText::new(title).font(semibold(14.0))), p().text);
    painter.galley(cover.left_bottom() + vec2(0.0, 27.0), line(RichText::new(subtitle).size(13.0)), p().secondary);
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
enum Start {
    Play,
    Shuffle,
    Radio,
}

/// A page's header, as in Tidal: the artwork with the title, a line about it and the length beside
/// it, and the buttons in a row underneath.
fn header(ui: &mut Ui, head: &Head, can_play: bool, length: Option<String>, library: &Library, actions: &mut Vec<Action>) -> Option<Start> {
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
            if let Some(item) = &head.item
                && icon_button(ui, Icon::Copy, 20.0, p().secondary).on_hover_text("Copy link").clicked()
            {
                actions.push(Action::CopyLink(item.link()));
            }
            match &head.item {
                Some(Item::Playlist(id)) if library.mine(id) => playlist_menu(ui, id, &head.title, &library.folders, actions),
                // An artist is followed with a heart; an album or playlist is added to its list.
                Some(item) => {
                    let on = library.saved.contains(item);
                    let (list, saved, unsaved) = match item {
                        Item::Album(_) => ("Albums", Icon::Check, Icon::Plus),
                        Item::Playlist(_) => ("Playlists", Icon::Check, Icon::Plus),
                        Item::Artist(_) | Item::Track(_) => ("Artists", Icon::HeartFilled, Icon::Heart),
                    };
                    let icon = if on { saved } else { unsaved };
                    let hint = if on { format!("Remove from your {list}") } else { format!("Add to your {list}") };
                    if icon_button(ui, icon, 22.0, if on { p().accent } else { p().secondary }).on_hover_text(hint).clicked() {
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
            let side = 256.0;
            ui.horizontal(|ui| {
                picture(ui, image.clone(), side, *round);
                ui.add_space(32.0);
                // The lines are laid out first so the block can sit centred beside the artwork.
                let width = ui.available_width();
                let lines: Vec<_> = [
                    Some(RichText::new(&head.title).font(bold(40.0)).color(p().text)),
                    (!head.subtitle.is_empty()).then(|| RichText::new(&head.subtitle).size(15.0).color(p().secondary)),
                    length.map(|length| RichText::new(length).font(semibold(12.0)).color(p().secondary)),
                ]
                .into_iter()
                .flatten()
                .map(|text| egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), width, egui::TextStyle::Body))
                .collect();
                let gap = 8.0;
                let height = lines.iter().map(|line| line.size().y + gap).sum::<f32>() - gap;
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = gap;
                    ui.add_space(((side - height) / 2.0).max(0.0));
                    for line in lines {
                        ui.label(line);
                    }
                });
            });
            ui.add_space(24.0);
            buttons(ui);
        }
        None => {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&head.title).font(bold(32.0)).color(p().text));
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
    let more = egui::Button::image(Icon::More.image(p().secondary, 22.0)).frame(false);
    egui::containers::menu::MenuButton::from_button(more).ui(ui, |ui| playlist_actions(ui, id, title, folders, actions));
}

/// Rename, move or delete one of the user's own playlists: its ⋯ menu, or right-click in the sidebar.
pub fn playlist_actions(ui: &mut Ui, id: &str, title: &str, folders: &[Card], actions: &mut Vec<Action>) {
    ui.set_min_width(220.0);
    menu_item(ui, actions, "Rename", dialogs::form(Target::Rename(id.into()), title));
    ui.menu_button("Move to folder", |ui| {
        let named = folders.iter().filter_map(|c| match c {
            Card::Folder { id, name, .. } => Some((id.as_str(), name.as_str())),
            _ => None,
        });
        for (folder, name) in std::iter::once((tidal::ROOT, "Top level")).chain(named) {
            menu_item(ui, actions, name, Action::MovePlaylist(id.into(), folder.into()));
        }
    });
    let text = "This deletes the playlist from your Tidal account. It can't be undone.";
    let delete = dialogs::confirm(format!("Delete {title}?"), text, Action::DeletePlaylist(id.into()));
    menu_item(ui, actions, RichText::new("Delete playlist").color(p().danger), delete);
}

/// An on/off switch.
pub fn switch(ui: &mut Ui, on: &mut bool) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(40.0, 22.0), egui::Sense::click());
    if clickable(response.clone()).clicked() {
        *on = !*on;
    }
    let t = ui.ctx().animate_bool(response.id, *on);
    ui.painter().rect_filled(rect, 11.0, p().surface_hover.lerp_to_gamma(p().accent, t));
    let x = egui::lerp(rect.left() + 11.0..=rect.right() - 11.0, t);
    ui.painter().circle_filled(egui::pos2(x, rect.center().y), 8.0, p().text);
}

/// A setting: its name and a line about it on the left, its control on the right.
pub fn setting(ui: &mut Ui, title: &str, detail: &str, control: impl FnOnce(&mut Ui)) {
    // The words make room for the control: they wrap rather than run under it.
    egui::Sides::new().shrink_left().show(
        ui,
        |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).font(medium(15.0)).color(p().text));
                ui.label(RichText::new(detail).size(13.0).color(p().secondary));
            });
        },
        control,
    );
}

fn filter_box(ui: &mut Ui, view: &mut View) {
    search_field(ui, &mut view.filter, "Filter", 240.0, egui::Id::new(("filter", view.key)));
}

/// One page: header, then its track table, card grid or shelves.
pub fn page(ui: &mut Ui, page: &mut Page, rows: &Rows, actions: &mut Vec<Action>) {
    let Page { head, body, view, .. } = page;
    // A list of tracks says how many and how long, as "86 TRACKS (5:29:07)".
    let length = match &*body {
        Body::Tracks { tracks, .. } if !tracks.is_empty() => {
            let seconds = tracks.iter().map(|t| f64::from(t.duration)).sum();
            Some(format!("{} TRACK{} ({})", tracks.len(), if tracks.len() == 1 { "" } else { "S" }, clock(seconds)))
        }
        _ => None,
    };
    let start = head.as_ref().and_then(|head| header(ui, head, !body.tracks().is_empty(), length, rows.library, actions));
    let mut order = None;
    match body {
        Body::Tracks { tracks, album_column } => {
            let sorted = view.as_ref().map(|v| (v.sort, v.reverse));
            if let Some(view) = view {
                ui.horizontal(|ui| {
                    filter_box(ui, view);
                    // A sorted list says how to get back to its own order (a playlist's can be dragged).
                    if view.sort != Sort::Added || view.reverse {
                        let own = if view.key == "playlist" { "Playlist order" } else { "Recently added" };
                        ui.add_space(8.0);
                        if link_text(ui, RichText::new(format!("Back to {}", own.to_lowercase())).color(p().secondary)).on_hover_text(own).clicked() {
                            actions.push(Action::ResetSort);
                        }
                    }
                });
                ui.add_space(12.0);
                order = Some(view.rows(tracks));
            }
            rows.show(ui, tracks, order, *album_column, sorted, actions);
        }
        Body::Grid { cards, sorts } => {
            let order = view.as_mut().map(|view| {
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
                view.rows(cards)
            });
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                ordered(cards, order).for_each(|c| card(ui, c, actions));
            });
        }
        Body::Shelves(shelves) => {
            for (n, shelf) in shelves.iter().enumerate() {
                shelf_title(ui, &shelf.title, shelf.more.as_deref(), actions);
                if !shelf.links.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for (title, path) in &shelf.links {
                            if chip(ui, title).clicked() {
                                actions.push(Action::Open(Source::Page(path.clone())));
                            }
                        }
                    });
                }
                if !shelf.text.is_empty() {
                    about(ui, &shelf.text);
                }
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
        (Some(Start::Play), _) => actions.push(Action::PlayTracks(tracks(), 0, false)),
        (Some(Start::Shuffle), _) => {
            let tracks = tracks();
            let first = crate::queue::random(tracks.len());
            actions.push(Action::PlayTracks(tracks, first, true));
        }
        (Some(Start::Radio), Some(head)) => actions.extend(head.radio.clone().map(Action::Play)),
        _ => {}
    }
}

/// A shelf's title, with "View all" on the right when there is more of it.
fn shelf_title(ui: &mut Ui, title: &str, more: Option<&str>, actions: &mut Vec<Action>) {
    if title.is_empty() && more.is_none() {
        return ui.add_space(16.0);
    }
    ui.add_space(28.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).font(bold(22.0)).color(p().text));
        if let Some(more) = more {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if link_text(ui, RichText::new("View all").font(semibold(13.0)).color(p().secondary)).clicked() {
                    actions.push(Action::Open(Source::Page(more.into())));
                }
            });
        }
    });
    ui.add_space(10.0);
}

/// A rounded link to another page, such as a genre.
fn chip(ui: &mut Ui, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.into(), medium(14.0), Color32::PLACEHOLDER);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(32.0, 18.0), Sense::click());
    ui.painter().rect_filled(rect, rect.height() / 2.0, if response.hovered() { p().surface_hover } else { p().surface });
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, p().text);
    clickable(response)
}

/// A long paragraph (an artist's bio): the start, and the rest behind "Read more".
fn about(ui: &mut Ui, text: &str) {
    let id = ui.id().with("read more");
    let open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let cut = text.char_indices().nth(600).map(|(at, _)| at).filter(|_| !open);
    ui.scope(|ui| {
        ui.set_max_width(820.0);
        let shown = cut.map_or(text.to_string(), |at| format!("{}…", text[..at].trim_end()));
        ui.label(RichText::new(shown).size(15.0).color(p().secondary));
    });
    if cut.is_some() && link_text(ui, RichText::new("Read more").font(semibold(13.0)).color(p().text)).clicked() {
        ui.data_mut(|d| d.insert_temp(id, true));
    }
}

/// A fixed-width cell of a track row, its content centred vertically.
fn cell(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui)) {
    ui.allocate_ui_with_layout(vec2(width, height), Layout::left_to_right(Align::Center), |ui| {
        ui.set_width(width);
        add(ui);
    });
}

/// How track rows behave: what is playing, what is saved, and whether this list is the queue or
/// one of the user's playlists (by id), whose tracks can be removed and rearranged.
pub struct Rows<'a> {
    pub playing: Option<u64>,
    pub library: &'a Library,
    pub list: List<'a>,
}

/// What a track table lists, which decides what its rows can do.
#[derive(Clone, Copy)]
pub enum List<'a> {
    /// The play queue: rows jump within it, and move or leave it.
    Queue,
    /// One of the user's own playlists, by id: rows can be dragged into place or removed.
    Playlist(&'a str),
    /// Any other tracks: playing one plays the list from there.
    Tracks,
}

impl Rows<'_> {
    /// A table of tracks under column headers, clickable to sort when `sorted` is set (sort, reversed).
    /// Click a title or a hovered number, or double-click a row, to play the list from there;
    /// right-click a row for more. Lists that span albums show each track's cover.
    pub fn show(&self, ui: &mut Ui, tracks: &[Track], order: Option<&[usize]>, album: bool, sorted: Option<(Sort, bool)>, actions: &mut Vec<Action>) {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let height = if album { 52.0 } else { 40.0 };
            // The user's own playlist, in its own order and unfiltered, can be rearranged by dragging.
            let reorder = match self.list {
                List::Playlist(id) if sorted == Some((Sort::Added, false)) && order.is_none_or(|o| o.len() == tracks.len()) => Some(id),
                _ => None,
            };
            let added = tracks.first().is_some_and(|t| t.added.is_some());
            let columns = [(Sort::Title, "TITLE", 0.4, true), (Sort::Artist, "ARTIST", 0.25, true), (Sort::Album, "ALBUM", 0.22, album), (Sort::Added, "DATE ADDED", 0.13, added)];
            let columns: Vec<_> = columns.into_iter().filter(|c| c.3).collect();
            let free = ui.available_width() - NUMBER - TIME - HEART - ui.spacing().item_spacing.x * 6.0;
            let total: f32 = columns.iter().map(|c| c.2).sum();
            let width = |share: f32| free * share / total;
            ui.horizontal(|ui| {
                cell(ui, NUMBER, 28.0, |ui| {
                    ui.label(RichText::new("#").font(medium(11.0)).color(p().dim));
                });
                for &(sort, name, share, _) in &columns {
                    cell(ui, width(share), 28.0, |ui| column_header(ui, name, sort, sorted, actions));
                }
                cell(ui, TIME, 28.0, |ui| column_header(ui, "TIME", Sort::Duration, sorted, actions));
            });
            let full = ui.available_width();
            let left = ui.cursor().left();
            ui.painter().hline(left..=left + full, ui.cursor().top(), Stroke::new(1.0, p().outline));
            ui.add_space(6.0);
            for (pos, t) in ordered(tracks, order).enumerate() {
                let rect = Rect::from_min_size(ui.cursor().min, vec2(full, height));
                // Rows scrolled out of view only take up space, so long playlists stay cheap to draw.
                if !ui.is_rect_visible(rect) {
                    ui.allocate_space(vec2(free, height));
                    continue;
                }
                let i = order.map_or(pos, |o| o[pos]);
                let play = || match self.list {
                    List::Queue => Action::Jump(i),
                    _ => Action::PlayTracks(in_order(tracks, order), pos, false),
                };
                let row = ui.interact(rect, ui.id().with(("row", pos)), if reorder.is_some() { Sense::click_and_drag() } else { Sense::click() });
                if let Some(playlist) = reorder {
                    self.drag_row(ui, row.clone(), rect, playlist, pos, actions);
                }
                let hovered = ui.rect_contains_pointer(rect);
                if hovered {
                    ui.painter().rect_filled(rect, 6.0, p().surface);
                }
                let playing = self.playing == Some(t.id);
                ui.horizontal(|ui| {
                    cell(ui, NUMBER, height, |ui| {
                        if hovered {
                            if icon_button(ui, Icon::Play, 14.0, p().text).clicked() {
                                actions.push(play());
                            }
                        } else if playing {
                            ui.add(Icon::Playing.image(p().accent, 16.0));
                        } else {
                            ui.label(RichText::new((pos + 1).to_string()).color(p().dim));
                        }
                    });
                    for &(sort, _, share, _) in &columns {
                        cell(ui, width(share), height, |ui| match sort {
                            Sort::Title => {
                                if album {
                                    picture(ui, tidal::image(t.cover.as_deref(), 80), THUMB, false);
                                    ui.add_space(4.0);
                                }
                                let color = if playing { p().accent } else { p().text };
                                let title = ui.add(egui::Label::new(RichText::new(&t.title).font(medium(14.0)).color(color)).truncate().selectable(false).sense(Sense::click()));
                                if title.clicked() {
                                    actions.push(play());
                                }
                            }
                            Sort::Artist => link_to(ui, &t.artist, t.artist_id.map(Source::Artist), actions),
                            Sort::Album => link_to(ui, &t.album, t.album_id.map(Source::Album), actions),
                            _ => {
                                ui.label(RichText::new(t.added.as_deref().unwrap_or_default()).color(p().secondary));
                            }
                        });
                    }
                    cell(ui, TIME, height, |ui| {
                        ui.label(RichText::new(clock(f64::from(t.duration))).color(p().secondary));
                    });
                    heart(ui, t.id, self.library, hovered, actions);
                });
                if row.double_clicked() {
                    actions.push(play());
                }
                egui::Popup::context_menu(&row).close_behavior(KEEP_OPEN).show(|ui| self.menu(ui, tracks, i, actions));
            }
        });
    }

    /// Dragging a row of the user's playlist: a line shows where it will land, and letting go
    /// there moves it.
    fn drag_row(&self, ui: &Ui, row: Response, rect: Rect, playlist: &str, pos: usize, actions: &mut Vec<Action>) {
        if row.drag_started() {
            egui::DragAndDrop::set_payload(ui.ctx(), DraggedRow(pos));
        }
        let (Some(dragged), Some(at)) = (egui::DragAndDrop::payload::<DraggedRow>(ui.ctx()), ui.ctx().pointer_interact_pos()) else { return };
        if !rect.contains(at) {
            return;
        }
        // The gap above or below this row, and where the dragged track ends up once taken out.
        let gap = if at.y < rect.center().y { pos } else { pos + 1 };
        let y = if gap == pos { rect.top() } else { rect.bottom() };
        ui.painter().hline(rect.x_range(), y, Stroke::new(2.0, p().accent));
        let to = if gap > dragged.0 { gap - 1 } else { gap };
        if ui.input(|i| i.pointer.any_released()) && to != dragged.0 {
            actions.push(Action::MoveInPlaylist(playlist.into(), dragged.0, to));
        }
    }

    /// A track's right-click menu, laid out like Tidal's: the track, then what to do with it.
    fn menu(&self, ui: &mut Ui, tracks: &[Track], i: usize, actions: &mut Vec<Action>) {
        let t = &tracks[i];
        ui.set_min_width(240.0);
        ui.horizontal(|ui| {
            picture(ui, tidal::image(t.cover.as_deref(), 80), 40.0, false);
            ui.vertical(|ui| {
                ui.add(egui::Label::new(RichText::new(&t.title).font(semibold(14.0)).color(p().text)).truncate());
                ui.add(egui::Label::new(RichText::new(&t.artist).size(13.0).color(p().secondary)).truncate());
            });
        });
        ui.separator();
        if let List::Queue = self.list {
            if i > 0 {
                menu_item(ui, actions, "Move up", Action::Move(i, i - 1));
            }
            if i + 1 < tracks.len() {
                menu_item(ui, actions, "Move down", Action::Move(i, i + 1));
            }
            if self.playing != Some(t.id) {
                menu_item(ui, actions, "Remove from queue", Action::Remove(i));
            }
        } else {
            menu_item(ui, actions, "Play next", Action::Enqueue(t.clone(), true));
            menu_item(ui, actions, "Add to queue", Action::Enqueue(t.clone(), false));
        }
        if let List::Playlist(playlist) = self.list {
            menu_item(ui, actions, "Remove from this playlist", Action::RemoveFromPlaylist(playlist.into(), i));
        }
        let config = egui::containers::menu::MenuConfig::new().close_behavior(KEEP_OPEN);
        egui::containers::menu::SubMenuButton::new("Add to playlist").config(config).ui(ui, |ui| self.add_to_playlist(ui, t.id, actions));
        let saved = self.library.saved.contains(&Item::Track(t.id));
        let collection = if saved { "Remove from My Collection" } else { "Add to My Collection" };
        menu_item(ui, actions, collection, Action::Save(Item::Track(t.id), !saved));
        menu_item(ui, actions, "Go to track radio", Action::Play(Source::TrackRadio(t.id)));
        menu_item(ui, actions, "Credits", Action::Credits(t.id, t.title.clone()));
        menu_item(ui, actions, "Copy link", Action::CopyLink(Item::Track(t.id).link()));
        if let Some(id) = t.album_id {
            menu_item(ui, actions, "Go to album", Action::Open(Source::Album(id)));
        }
        if let Some(id) = t.artist_id {
            menu_item(ui, actions, "Go to artist", Action::Open(Source::Artist(id)));
        }
    }

    /// The "Add to playlist" submenu: make a new one, or pick from the recent ones or any by name.
    fn add_to_playlist(&self, ui: &mut Ui, track: u64, actions: &mut Vec<Action>) {
        ui.set_min_width(240.0);
        let filter_id = egui::Id::new("playlist filter");
        if ui.add(egui::Button::image_and_text(Icon::Plus.image(p().text, 16.0), "Create new playlist")).clicked() {
            actions.push(dialogs::form(Target::Create(Some(track)), ""));
            ui.close();
        }
        ui.separator();
        let mut filter: String = ui.data(|d| d.get_temp(filter_id)).unwrap_or_default();
        search_field(ui, &mut filter, "Find a playlist", 216.0, filter_id.with("field"));
        let needle = filter.to_lowercase();
        ui.label(RichText::new(if needle.is_empty() { "RECENT" } else { "MATCHING" }).font(semibold(11.0)).color(p().dim));
        let shown = if needle.is_empty() { 10 } else { usize::MAX };
        egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
            for p in self.library.playlists.iter().filter(|p| p.title.to_lowercase().contains(&needle)).take(shown) {
                menu_item(ui, actions, &p.title, Action::AddToPlaylist(p.id.clone(), track));
            }
        });
        ui.data_mut(|d| d.insert_temp(filter_id, filter));
    }
}

/// Secondary text that opens a page, when it has one.
pub fn link_to(ui: &mut Ui, text: impl Into<RichText>, to: Option<Source>, actions: &mut Vec<Action>) {
    if link_text(ui, text.into().color(p().secondary)).clicked()
        && let Some(source) = to
    {
        actions.push(Action::Open(source));
    }
}

/// A column title; clickable with an arrow on the sorted column when the table sorts.
fn column_header(ui: &mut Ui, name: &str, sort: Sort, sorted: Option<(Sort, bool)>, actions: &mut Vec<Action>) {
    let text = RichText::new(name).font(medium(11.0));
    let Some((current, reverse)) = sorted else {
        ui.label(text.color(p().dim));
        return;
    };
    let active = current == sort;
    let header = ui.horizontal(|ui| {
        ui.label(text.color(if active { p().text } else { p().dim }));
        if active {
            let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
            arrow(ui, rect.center(), reverse);
        }
    });
    if clickable(ui.interact(header.response.rect, ui.id().with(name), Sense::click())).clicked() {
        actions.push(Action::Sort(sort));
    }
}
