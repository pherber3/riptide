use std::sync::atomic::Ordering::Relaxed;

use egui::{Align, Color32, Key, Layout, RichText, Sense, Ui, Vec2, vec2};

use super::views::{List, Rows, heart, link_to, menu_item, playlist_actions};
use super::{Action, App, SEARCH_PAUSE, Source};
use crate::dialogs::{self, Target};
use crate::queue::Repeat;
use crate::theme::{Icon, bold, p, semibold};
use crate::tidal::{self, Card, Item, Lyrics, Quality};
use crate::widgets::{bar, clickable, clock, icon_button, link_text, nav_item, picture, pill, play_disc, search_field, section, tier_badge, tier_color};

/// A playlist being dragged in the sidebar: id and title.
struct Dragged(String, String);

impl App {
    pub(super) fn login_ui(&mut self, ui: &mut Ui) {
        let mut finish = None;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(p().window)).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.label(RichText::new("riptide").font(bold(44.0)).color(p().text));
                ui.label(RichText::new("Your Tidal library, light and fast").size(15.0).color(p().secondary));
                ui.add_space(32.0);
                if self.busy {
                    ui.spinner();
                } else if let Some((flow, pasted)) = &mut self.login {
                    ui.label(RichText::new("Sign in in your browser, then paste the address of the page you land on:").color(p().secondary));
                    ui.add_space(8.0);
                    ui.add(egui::TextEdit::singleline(pasted).desired_width(520.0).margin(vec2(12.0, 8.0)));
                    ui.hyperlink_to("Open the sign-in page again", &flow.url);
                    ui.add_space(12.0);
                    finish = pill(ui, Icon::Forward, "Continue", true).clicked().then(|| self.login.take()).flatten();
                } else if pill(ui, Icon::Music, "Sign in with Tidal", true).clicked() {
                    let flow = tidal::Login::start();
                    let _ = open::that(&flow.url);
                    self.login = Some((flow, String::new()));
                }
                if let Some((e, true)) = &self.message {
                    ui.add_space(12.0);
                    ui.colored_label(p().danger, e);
                }
            });
        });
        if let Some((flow, pasted)) = finish {
            let session = tidal::session_path(&self.data);
            self.sign_in(async move { flow.finish(&pasted, &session).await });
        }
    }

    pub(super) fn sidebar(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let open = self.page.as_ref().map(|p| &p.source);
        let frame = egui::Frame::new().fill(p().panel).inner_margin(egui::Margin { left: 12, right: 12, top: 20, bottom: 8 });
        egui::Panel::left("nav").exact_size(232.0).frame(frame).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            // The small capitals over each group of links.
            let heading = |ui: &mut Ui, text: &str| {
                ui.add_space(10.0);
                ui.label(RichText::new(text).font(semibold(11.0)).color(p().dim));
            };
            ui.horizontal(|ui| {
                ui.add_space(10.0);
                ui.label(RichText::new("riptide").font(bold(20.0)).color(p().text));
            });
            ui.add_space(16.0);
            for (icon, text, source) in [(Icon::Home, "Home", Source::Home), (Icon::Explore, "Explore", Source::explore())] {
                if nav_item(ui, icon, text, open == Some(&source)).clicked() {
                    actions.push(Action::Open(source));
                }
            }
            ui.add_space(22.0);
            ui.horizontal(|ui| heading(ui, "COLLECTION"));
            ui.add_space(4.0);
            for (icon, text, source) in [
                (Icon::Music, "Tracks", Source::Tracks),
                (Icon::Disc, "Albums", Source::Albums),
                (Icon::Artists, "Artists", Source::Artists),
                (Icon::Playlists, "Playlists", Source::playlists()),
            ] {
                if nav_item(ui, icon, text, open == Some(&source)).clicked() {
                    actions.push(Action::Open(source));
                }
            }
            ui.add_space(22.0);
            egui::Sides::new().show(
                ui,
                |ui| heading(ui, "PLAYLISTS"),
                |ui| {
                    let plus = egui::Button::image(Icon::Plus.image(p().secondary, 16.0)).frame(false);
                    egui::containers::menu::MenuButton::from_button(plus).ui(ui, |ui| {
                        menu_item(ui, actions, "New playlist", dialogs::form(Target::Create(None), ""));
                        menu_item(ui, actions, "New folder", dialogs::form(Target::CreateFolder, ""));
                    });
                },
            );
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                for card in &self.library.folders {
                    let (icon, text, selected) = match card {
                        Card::Folder { id, name, .. } => (Icon::Folder, name, matches!(open, Some(Source::Folder(o, _)) if o == id)),
                        Card::Playlist(p) => (Icon::Playlists, &p.title, matches!(open, Some(Source::Playlist(o)) if *o == p.id)),
                        _ => continue,
                    };
                    let response = nav_item(ui, icon, text, selected);
                    match card {
                        // A playlist dragged onto a folder moves into it.
                        Card::Folder { id, .. } => {
                            if response.dnd_hover_payload::<Dragged>().is_some() {
                                ui.painter().rect_stroke(response.rect, 8.0, egui::Stroke::new(1.5, p().accent), egui::StrokeKind::Inside);
                            }
                            if let Some(dragged) = response.dnd_release_payload::<Dragged>() {
                                actions.push(Action::MovePlaylist(dragged.0.clone(), id.clone()));
                            }
                            response.context_menu(|ui| {
                                menu_item(ui, actions, "Rename", dialogs::form(Target::RenameFolder(id.clone()), text.as_str()));
                                let delete = dialogs::confirm(
                                    format!("Delete {text}?"),
                                    "The folder goes; the playlists in it move to the top level.",
                                    Action::DeleteFolder(id.clone()),
                                );
                                menu_item(ui, actions, RichText::new("Delete folder").color(p().danger), delete);
                            });
                        }
                        Card::Playlist(p) => {
                            if response.drag_started() {
                                egui::DragAndDrop::set_payload(ui.ctx(), Dragged(p.id.clone(), p.title.clone()));
                            }
                            response.context_menu(|ui| match self.library.mine(&p.id) {
                                true => playlist_actions(ui, &p.id, &p.title, &self.library.folders, actions),
                                false => menu_item(ui, actions, "Remove from your Playlists", Action::Save(Item::Playlist(p.id.clone()), false)),
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
    pub(super) fn drag_label(&self, ctx: &egui::Context) {
        let (Some(dragged), Some(at)) = (egui::DragAndDrop::payload::<Dragged>(ctx), ctx.pointer_interact_pos()) else { return };
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("dragged playlist")));
        let galley = painter.layout_no_wrap(dragged.1.clone(), semibold(13.0), p().text);
        let rect = egui::Rect::from_min_size(at + vec2(14.0, 6.0), galley.size() + vec2(20.0, 12.0));
        painter.rect_filled(rect, 8.0, p().surface_hover);
        painter.galley(rect.min + vec2(10.0, 6.0), galley, p().text);
    }

    /// The bar along the bottom: what is playing, the transport, and volume and quality.
    pub(super) fn player_bar(&mut self, ui: &mut Ui, mood: Option<Color32>, actions: &mut Vec<Action>) {
        let frame = egui::Frame::new().fill(mood.map_or(p().panel, |c| c.lerp_to_gamma(Color32::BLACK, 0.25))).inner_margin(egui::Margin::symmetric(16, 0));
        egui::Panel::bottom("player").exact_size(84.0).frame(frame).show(ui, |ui| {
            let edge = ui.clip_rect();
            ui.painter().hline(edge.x_range(), edge.top(), egui::Stroke::new(1.0, p().outline));
            ui.columns(3, |cols| {
                cols[0].horizontal_centered(|ui| self.playing_now(ui, actions));
                cols[1].vertical_centered(|ui| self.transport(ui, actions));
                cols[2].with_layout(Layout::right_to_left(Align::Center), |ui| self.output(ui, mood, actions));
            });
        });
    }

    /// The playing track's cover, title, artist and where it is playing from, each a link.
    fn playing_now(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let Some(t) = self.queue.current() else { return };
        let cover = picture(ui, tidal::image(t.cover.as_deref(), 160), 56.0, false);
        if cover.hovered() {
            ui.painter().rect_filled(cover.rect, 6.0, Color32::from_black_alpha(90));
        }
        let mut album = clickable(cover).clicked();
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width() - 40.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add_space(if self.queue.from.is_some() { 14.0 } else { 23.0 });
            album |= link_text(ui, RichText::new(&t.title).font(semibold(14.0)).color(p().text)).clicked();
            link_to(ui, RichText::new(&t.artist).size(13.0), t.artist_id.map(Source::Artist), actions);
            // Where it is playing from, as in Tidal: a way back to that playlist or album.
            if let Some((source, name)) = &self.queue.from {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 5.0;
                    let icon = match source {
                        Source::Album(_) => Icon::Disc,
                        Source::Artist(_) | Source::ArtistRadio(_) => Icon::Artists,
                        _ => Icon::Playlists,
                    };
                    ui.add(icon.image(p().secondary, 13.0));
                    link_to(ui, RichText::new(name).size(12.0), Some(source.clone()), actions);
                });
            }
        });
        heart(ui, t.id, &self.library, true, actions);
        if album && let Some(id) = t.album_id {
            actions.push(Action::Open(Source::Album(id)));
        }
    }

    /// Shuffle, previous, play, next and repeat, over the seek bar.
    fn transport(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let status = self.player.status.clone();
        ui.add_space(12.0);
        // A row as tall as the play button from the start, so every button centres on the same line.
        let row = Layout::left_to_right(Align::Center);
        ui.allocate_ui_with_layout(vec2(ui.available_width(), 36.0), row, |ui| {
            ui.spacing_mut().item_spacing.x = 14.0;
            ui.add_space((ui.available_width() - 200.0) / 2.0);
            let on = |on: bool| if on { p().accent } else { p().secondary };
            let toggle = if status.playing.load(Relaxed) { Icon::Pause } else { Icon::Play };
            let repeat = if self.queue.repeat == Repeat::One { Icon::RepeatOne } else { Icon::Repeat };
            let buttons = [
                (Icon::Shuffle, 16.0, on(self.queue.shuffled()), Action::Shuffle),
                (Icon::Prev, 18.0, p().text, Action::Prev),
                (toggle, 18.0, p().text, Action::Toggle),
                (Icon::Next, 18.0, p().text, Action::Next),
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
        let total = self.queue.current().map_or(0.0, |t| f64::from(t.duration));
        let mut position = self.dragging.or(self.restored).unwrap_or_else(|| status.position()).min(total);
        ui.horizontal(|ui| {
            let time = |text: String| RichText::new(text).size(11.0).color(p().secondary);
            ui.label(time(clock(position)));
            let seek = bar(ui, &mut position, total, ui.available_width() - 40.0, self.queue.current().is_some());
            if seek.dragged() {
                self.dragging = Some(position);
            }
            if seek.drag_stopped() || seek.clicked() {
                self.dragging = None;
                actions.push(Action::Seek(position));
            }
            ui.label(time(clock(total)));
        });
    }

    /// Volume, the queue and lyrics buttons, and the quality playing and chosen.
    fn output(&mut self, ui: &mut Ui, mood: Option<Color32>, actions: &mut Vec<Action>) {
        let status = self.player.status.clone();
        let mut volume = f64::from(self.settings.volume);
        let slider = bar(ui, &mut volume, 1.0, 96.0, true);
        if slider.changed() {
            self.settings.volume = volume as f32;
            status.set_volume(self.settings.volume);
        }
        let mut save = slider.drag_stopped() || slider.clicked();
        let muted = self.settings.volume == 0.0;
        if icon_button(ui, if muted { Icon::Muted } else { Icon::Volume }, 18.0, p().secondary).on_hover_text(if muted { "Unmute" } else { "Mute" }).clicked() {
            self.settings.volume = if muted {
                self.unmuted.take().unwrap_or(0.5)
            } else {
                self.unmuted = Some(self.settings.volume);
                0.0
            };
            status.set_volume(self.settings.volume);
            save = true;
        }
        ui.add_space(6.0);
        for (open, icon, hint, action) in [(self.queue_open, Icon::Queue, "Queue", Action::Queue), (self.lyrics_open, Icon::Lyrics, "Lyrics", Action::Lyrics)] {
            if icon_button(ui, icon, 18.0, if open { p().accent } else { p().secondary }).on_hover_text(hint).clicked() {
                actions.push(action);
            }
        }
        ui.add_space(6.0);
        let (tier, format) = if self.queue.current().is_some() { status.format.lock().unwrap().clone() } else { (self.settings.quality, String::new()) };
        egui::containers::menu::MenuButton::from_button(tier_badge(tier)).ui(ui, |ui| {
            for q in Quality::ALL {
                if ui.radio(self.settings.quality == q, RichText::new(q.name()).color(tier_color(q))).clicked() {
                    actions.push(Action::Quality(q));
                }
            }
        });
        ui.label(RichText::new(format).size(11.0).color(mood.map_or(p().dim, |_| Color32::from_white_alpha(150))));
        if save {
            self.settings.save(&self.data);
        }
    }

    /// The queue, beside the page.
    pub(super) fn queue_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if !self.queue_open {
            return;
        }
        let rows = Rows { playing: self.queue.current().map(|t| t.id), library: &self.library, list: List::Queue };
        let frame = egui::Frame::new().fill(p().panel).inner_margin(egui::Margin::symmetric(20, 0));
        egui::Panel::right("side").default_size(420.0).resizable(true).frame(frame).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                section(ui, "Queue", |_| {});
                if self.queue.tracks.is_empty() {
                    ui.label(RichText::new("Nothing queued. Right-click a track to add it.").color(p().secondary));
                }
                rows.show(ui, &self.queue.tracks, None, false, None, actions);
            });
        });
    }

    /// The playing track's artwork and lyrics over the whole window, in the artwork's colour.
    pub(super) fn now_playing(&mut self, ui: &mut Ui, mood: Option<Color32>, actions: &mut Vec<Action>) {
        let soft = Color32::from_white_alpha(180);
        let frame = egui::Frame::new().fill(mood.unwrap_or(p().panel)).inner_margin(egui::Margin { left: 56, right: 40, top: 16, bottom: 0 });
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            if self.settings.tide {
                self.paint_tide(ui);
            }
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
            // The cover and its title sit centred; the lyrics start level with the cover's top.
            let top = ((height - side - 70.0) / 2.0).max(0.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(side);
                    ui.add_space(top);
                    picture(ui, tidal::image(t.cover.as_deref(), 640), side, false);
                    ui.add_space(16.0);
                    ui.add(egui::Label::new(RichText::new(&t.title).font(semibold(20.0)).color(Color32::WHITE)).truncate());
                    ui.add(egui::Label::new(RichText::new(&t.artist).size(15.0).color(soft)).truncate());
                });
                ui.add_space(64.0);
                let lyrics = self.lyrics.as_ref().filter(|(id, _)| *id == t.id).and_then(|(_, l)| l.as_ref());
                ui.vertical(|ui| {
                    ui.add_space(top);
                    lyric_column(ui, lyrics, self.player.status.position(), &mut self.lyric_line, side, actions);
                });
            });
        });
    }

    /// The tide along the bottom of the lyrics view. It moves while the music plays and settles
    /// once it stops.
    fn paint_tide(&mut self, ui: &mut Ui) {
        let playing = self.player.status.playing.load(Relaxed);
        let level = if playing { self.player.status.level() } else { 0.0 };
        let settling = self.tide.step(level, ui.input(|i| i.stable_dt).min(0.1));
        let full = ui.clip_rect();
        let depth = (full.height() * 0.2).clamp(80.0, 180.0);
        self.tide.paint(ui, egui::Rect::from_min_max(egui::pos2(full.left(), full.bottom() - depth), full.max));
        if (playing || settling) && !self.out_of_sight() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
        }
    }

    pub(super) fn content(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let frame = egui::Frame::new().fill(p().window).inner_margin(egui::Margin { left: 28, right: 16, top: 14, bottom: 0 });
        egui::CentralPanel::default().frame(frame).show(ui, |ui| {
            // The page's artwork colour glows down from the top and scrolls away with the page. It is
            // drawn first, behind everything, once the scroll position is known.
            let glow = ui.painter().add(egui::Shape::Noop);
            let cover = self.page.as_ref().filter(|_| !self.settings_open).and_then(|p| p.head.as_ref()?.art.as_ref()?.0.as_deref());
            let tint = cover.and_then(crate::art::tint);
            let full = ui.clip_rect();
            ui.horizontal(|ui| {
                let back = self.settings_open || self.history.can_step(true);
                for (icon, enabled, back) in [(Icon::Back, back, true), (Icon::Forward, self.history.can_step(false), false)] {
                    if ui.add_enabled_ui(enabled, |ui| icon_button(ui, icon, 20.0, p().secondary)).inner.clicked() {
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
                    let (icon, hint) = if self.settings_open { (Icon::Close, "Close settings") } else { (Icon::Settings, "Settings") };
                    if icon_button(ui, icon, 20.0, p().secondary).on_hover_text(hint).clicked() {
                        actions.push(Action::Settings);
                    }
                    ui.add_space(8.0);
                    if self.loading {
                        ui.spinner();
                    }
                    if let Some((text, error)) = &self.message {
                        ui.add(egui::Label::new(RichText::new(text).color(if *error { p().danger } else { p().secondary })).truncate());
                    }
                });
            });
            ui.add_space(4.0);
            let editing = match self.page.as_ref().map(|p| &p.source) {
                Some(Source::Playlist(id)) if self.library.mine(id) => Some(id.clone()),
                _ => None,
            };
            if self.settings_open {
                let lastfm_user = self.lastfm.as_ref().and_then(|l| l.session.as_ref()).map(|(_, user)| user.as_str());
                egui::ScrollArea::vertical()
                    .id_salt("settings")
                    .auto_shrink(false)
                    .show(ui, |ui| crate::settings::page(ui, &mut self.settings, &self.themes, lastfm_user, &self.data, actions));
                return;
            }
            let mut area = egui::ScrollArea::vertical().id_salt("page").auto_shrink(false);
            if let Some(y) = self.scroll_to.take() {
                area = area.vertical_scroll_offset(y);
            }
            let Some(page) = &mut self.page else { return };
            let list = editing.as_deref().map_or(List::Tracks, List::Playlist);
            let rows = Rows { playing: self.queue.current().map(|t| t.id), library: &self.library, list };
            let scrolled = area.show(ui, |ui| super::views::page(ui, page, &rows, actions));
            page.scroll = scrolled.state.offset.y;
            if let Some(color) = tint {
                let rect = egui::Rect::from_min_size(full.min - vec2(0.0, scrolled.state.offset.y), vec2(full.width(), 460.0));
                ui.painter().set(glow, egui::Shape::gradient_rect(rect, egui::Direction::TopDown, [color.gamma_multiply(0.6), Color32::TRANSPARENT]));
            }
        });
    }
}

/// The lyrics beside the cover, from its top to its bottom, the playing line level with its middle:
/// they fade toward both ends (the top fade slides in as they scroll, so the first line starts in
/// full), and a click on a line plays from there.
fn lyric_column(ui: &mut Ui, lyrics: Option<&Lyrics>, position: f64, shown: &mut Option<usize>, side: f32, actions: &mut Vec<Action>) {
    let (soft, faint) = (Color32::from_white_alpha(180), Color32::from_white_alpha(110));
    let scroll = egui::ScrollArea::vertical().auto_shrink(false).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden);
    scroll.max_height(side).show(ui, |ui| {
        let from = ui.clip_rect().top();
        let fade = side * 0.22;
        let unscrolled = (fade - (from - ui.cursor().top())).max(0.0);
        // Laid out without a colour and painted in the one for where it lands, so the
        // fade never re-shapes the text.
        let lyric = |ui: &mut Ui, text: RichText, color: Color32| {
            let text = egui::WidgetText::from(text.color(Color32::PLACEHOLDER));
            let galley = text.into_galley(ui, Some(egui::TextWrapMode::Wrap), ui.available_width(), egui::TextStyle::Body);
            let (rect, response) = ui.allocate_exact_size(galley.size(), Sense::click());
            if ui.is_rect_visible(rect) {
                let y = rect.center().y;
                let f = ((y - from + unscrolled) / fade).min((from + side - y) / fade).clamp(0.0, 1.0);
                ui.painter().galley(rect.min, galley, color.gamma_multiply(f * f * (3.0 - 2.0 * f)));
            }
            response
        };
        match lyrics {
            None => {
                ui.spinner();
            }
            Some(l) if l.synced.is_empty() && l.text.is_empty() => {
                ui.label(RichText::new("No lyrics for this track.").font(bold(34.0)).color(soft));
            }
            Some(l) if l.synced.is_empty() => {
                for words in l.text.lines() {
                    lyric(ui, RichText::new(words).font(semibold(24.0)), Color32::WHITE);
                }
            }
            Some(l) => {
                let now = l.synced.iter().rposition(|(at, _)| *at <= position);
                for (n, (at, words)) in l.synced.iter().enumerate() {
                    let color = if Some(n) == now { Color32::WHITE } else { faint };
                    let words = if words.is_empty() { "♪" } else { words };
                    let response = lyric(ui, RichText::new(words).font(bold(34.0)), color);
                    if Some(n) == now && *shown != now {
                        response.scroll_to_me(Some(Align::Center));
                    }
                    if clickable(response).clicked() {
                        actions.push(Action::Seek(*at));
                    }
                    ui.add_space(18.0);
                }
                *shown = now;
            }
        }
        ui.add_space(side / 2.0);
    });
}
