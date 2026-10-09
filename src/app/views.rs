//! The pieces of a page: its header, card grids, shelves and track tables, and the menus on them.

use egui::{Align, Color32, Layout, Rect, Response, RichText, Sense, Stroke, Ui, Vec2, vec2};

use super::page::Editions;
use super::{Action, Body, Head, Library, Page, Source};
use crate::dialogs::{self, Target};
use crate::theme::{Icon, bold, medium, p, semibold};
use crate::tidal::{self, Card, Item, Track};
use crate::view::{Sort, View, in_order, ordered};
use crate::widgets::{
    arrow, chip, clickable, clock, fitted, icon_button, link_text, paint_picture, picture, pill, play_disc, search_field, section, tier_badge, tier_color,
};

const CARD: f32 = 168.0;
const NUMBER: f32 = 36.0;
const TIME: f32 = 56.0;
const HEART: f32 = 32.0;
const THUMB: f32 = 38.0;

/// A playlist row being dragged to a new place, by position.
struct DraggedRow(usize);

/// Menus that hold a text field: a click inside (in the field) mustn't close them.
const KEEP_OPEN: egui::PopupCloseBehavior = egui::PopupCloseBehavior::CloseOnClickOutside;

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
    let (icon, color, hint) =
        if on { (Icon::HeartFilled, p().accent, "Remove from your collection") } else { (Icon::Heart, p().secondary, "Add to your collection") };
    if icon_button(ui, icon, 16.0, color).on_hover_text(hint).clicked() {
        actions.push(Action::Save(item, !on));
    }
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
        Card::Album(a) if a.year().is_empty() => (tidal::image(a.cover.as_deref(), 320), false, a.title.clone(), a.artist.clone()),
        Card::Album(a) => (tidal::image(a.cover.as_deref(), 320), false, a.title.clone(), format!("{} · {}", a.artist, a.year())),
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
    painter.galley(cover.left_bottom() + vec2(0.0, 8.0), fitted(ui, RichText::new(title).font(semibold(14.0)), CARD), p().text);
    painter.galley(cover.left_bottom() + vec2(0.0, 27.0), fitted(ui, RichText::new(subtitle).size(13.0), CARD), p().secondary);
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
    let (mut start, mut go, mut read) = (None, None, false);
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
            // An album's other editions, to switch to.
            if let Some(editions) = head.editions.as_ref().filter(|e| !e.others.is_empty()) {
                let versions = egui::Button::new(RichText::new("Other versions").font(semibold(14.0)).color(p().text))
                    .fill(p().surface)
                    .corner_radius(20.0)
                    .min_size(vec2(0.0, 40.0));
                egui::containers::menu::MenuButton::from_button(versions).ui(ui, |ui| {
                    for (edition, to) in &editions.others {
                        let name = RichText::new(edition.name(editions.explicit_varies)).color(tier_color(edition.quality));
                        menu_item(ui, actions, name, Action::Replace(to.clone()));
                    }
                });
            }
            if let Some(item) = &head.item
                && icon_button(ui, Icon::Copy, 20.0, p().secondary).on_hover_text("Copy link").clicked()
            {
                actions.push(Action::CopyLink(item.link()));
            }
            match &head.item {
                Some(Item::Playlist(id)) if library.mine(id) => {
                    let more = egui::Button::image(Icon::More.image(p().secondary, 22.0)).frame(false);
                    egui::containers::menu::MenuButton::from_button(more).ui(ui, |ui| playlist_actions(ui, id, &head.title, &library.folders, actions));
                }
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
                let byline = match (&head.by, head.subtitle.as_str()) {
                    (Some((name, _)), "") => name.clone(),
                    (Some((name, _)), subtitle) => format!("{name} · {subtitle}"),
                    (None, subtitle) => subtitle.into(),
                };
                let line = |text: RichText| fitted(ui, text, width);
                let title = line(RichText::new(&head.title).font(bold(40.0)).color(p().text));
                let subtitle = (!byline.is_empty()).then(|| line(RichText::new(&byline).size(15.0).color(p().secondary)));
                let length = length.map(|length| line(RichText::new(length).font(semibold(12.0)).color(p().secondary)));
                // The start of the paragraph about it; "Read more" opens all of it.
                let about = (!head.about.is_empty()).then(|| {
                    let mut job = egui::text::LayoutJob::simple(head.about.clone(), egui::FontId::proportional(14.0), p().secondary, width.min(640.0));
                    job.wrap.max_rows = 3;
                    job.sections[0].format.line_height = Some(20.0);
                    ui.ctx().fonts_mut(|f| f.layout_job(job))
                });
                let read_more = RichText::new("Read more").font(semibold(13.0)).color(p().text);
                let more = about.as_ref().filter(|a| a.elided).map(|_| line(read_more.clone()));
                let gap = 8.0;
                let lines = [Some(&title), subtitle.as_ref(), length.as_ref(), about.as_ref(), more.as_ref()];
                let height = lines.into_iter().flatten().map(|l| l.size().y + gap).sum::<f32>() - gap;
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = gap;
                    ui.add_space(((side - height) / 2.0).max(0.0));
                    ui.label(title);
                    match (&head.by, subtitle) {
                        // The maker's name opens their page.
                        (Some((name, to)), Some(_)) => {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                if link_text(ui, RichText::new(name).size(15.0).color(p().secondary)).clicked() {
                                    go = Some(to.clone());
                                }
                                if !head.subtitle.is_empty() {
                                    ui.label(RichText::new(format!(" · {}", head.subtitle)).size(15.0).color(p().secondary));
                                }
                            });
                        }
                        (_, Some(subtitle)) => {
                            ui.label(subtitle);
                        }
                        (_, None) => {}
                    }
                    // The edition's tier follows the length, in the badge the player shows it with.
                    if length.is_some() || head.editions.is_some() {
                        ui.horizontal(|ui| {
                            if let Some(length) = length {
                                ui.label(length);
                            }
                            if let Some(Editions { this, explicit_varies, .. }) = &head.editions {
                                ui.add(tier_badge(this.quality).sense(Sense::hover()));
                                if *explicit_varies {
                                    let explicit = if this.explicit { "EXPLICIT" } else { "CLEAN" };
                                    ui.label(RichText::new(explicit).font(semibold(12.0)).color(p().secondary));
                                }
                            }
                        });
                    }
                    if let Some(about) = about {
                        ui.label(about);
                    }
                    read = more.is_some() && link_text(ui, read_more).clicked();
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
    actions.extend(go.map(Action::Open));
    if read {
        actions.push(Action::Dialog(Box::new(dialogs::Dialog::About(head.title.clone(), head.about.clone()))));
    }
    start
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
                if !shelf.cards.is_empty() {
                    egui::ScrollArea::horizontal().id_salt(("shelf", n)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            shelf.cards.iter().for_each(|c| card(ui, c, actions));
                        });
                    });
                }
                if !shelf.tracks.is_empty() {
                    ui.push_id(("shelf", n), |ui| rows.show(ui, &shelf.tracks, None, true, None, actions));
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
    section(ui, title, |ui| {
        if let Some(more) = more
            && link_text(ui, RichText::new("View all").font(semibold(13.0)).color(p().secondary)).clicked()
        {
            actions.push(Action::Open(Source::Page(more.into())));
        }
    });
}

/// A fixed-width cell of a track row, its content centred vertically.
fn cell<R>(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui) -> R) {
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
    /// Click a row to play the list from there; right-click it for more. Lists that span albums show each track's cover.
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
            let columns: Vec<_> = [
                (Sort::Title, "TITLE", 0.4, true),
                (Sort::Artist, "ARTIST", 0.25, true),
                (Sort::Album, "ALBUM", 0.22, album),
                (Sort::Added, "DATE ADDED", 0.13, added),
            ]
            .into_iter()
            .filter(|c| c.3)
            .collect();
            let free = ui.available_width() - NUMBER - TIME - HEART - ui.spacing().item_spacing.x * 6.0;
            let total: f32 = columns.iter().map(|c| c.2).sum();
            let width = |share: f32| free * share / total;
            ui.horizontal(|ui| {
                cell(ui, NUMBER, 28.0, |ui| ui.label(RichText::new("#").font(medium(11.0)).color(p().dim)));
                for &(sort, name, share, _) in &columns {
                    cell(ui, width(share), 28.0, |ui| column_header(ui, name, sort, sorted, actions));
                }
                cell(ui, TIME, 28.0, |ui| column_header(ui, "TIME", Sort::Duration, sorted, actions));
            });
            let full = ui.available_width();
            let left = ui.cursor().left();
            ui.painter().hline(left..=left + full, ui.cursor().top(), Stroke::new(1.0, p().outline));
            ui.add_space(6.0);
            // Only the rows in view are drawn; the rest take up their space in one block above and
            // one below, so a long playlist costs no more to draw than a short one.
            let count = order.map_or(tracks.len(), <[usize]>::len);
            let (top, view) = (ui.cursor().top(), ui.clip_rect());
            let row_at = |y: f32| (((y - top) / height).max(0.0) as usize).min(count);
            let (first, end) = (row_at(view.top()), (row_at(view.bottom()) + 1).min(count));
            ui.allocate_space(vec2(free, first as f32 * height));
            for (pos, t) in ordered(tracks, order).enumerate().take(end).skip(first) {
                let rect = Rect::from_min_size(ui.cursor().min, vec2(full, height));
                let i = order.map_or(pos, |o| o[pos]);
                let play = || match self.list {
                    List::Queue => Action::Jump(i),
                    _ => Action::PlayTracks(in_order(tracks, order), pos, false),
                };
                let row = clickable(ui.interact(rect, ui.id().with(("row", pos)), if reorder.is_some() { Sense::click_and_drag() } else { Sense::click() }));
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
                            ui.add(Icon::Play.image(p().text, 14.0));
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
                                let title = RichText::new(&t.title).font(medium(14.0)).color(if playing { p().accent } else { p().text });
                                ui.add(egui::Label::new(title).truncate().selectable(false));
                            }
                            Sort::Artist => link_to(ui, &t.artist, t.artist_id.map(Source::Artist), actions),
                            Sort::Album => link_to(ui, &t.album, t.album_id.map(Source::Album), actions),
                            _ => {
                                ui.label(RichText::new(t.added.as_deref().unwrap_or_default()).color(p().secondary));
                            }
                        });
                    }
                    cell(ui, TIME, height, |ui| ui.label(RichText::new(clock(f64::from(t.duration))).color(p().secondary)));
                    heart(ui, t.id, self.library, hovered, actions);
                });
                if row.clicked() {
                    actions.push(play());
                }
                egui::Popup::context_menu(&row).close_behavior(KEEP_OPEN).show(|ui| self.menu(ui, tracks, i, actions));
            }
            ui.allocate_space(vec2(free, (count - end) as f32 * height));
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
