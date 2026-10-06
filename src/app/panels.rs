use std::sync::atomic::Ordering::Relaxed;

use egui::{Align, Color32, Key, Layout, RichText, Sense, Ui, Vec2, vec2};

use super::{Action, App, Body, SEARCH_PAUSE, Source, then};
use crate::dialogs::{self, Target};
use crate::queue::Repeat;
use crate::theme::{self, ACCENT, BAR, DANGER, DIM, Icon, LINE, SECONDARY, SIDEBAR, TEXT, bold, semibold};
use crate::tidal::{self, Card, Item, Quality};
use crate::widgets::{Rows, art, bar, clickable, clock, heart, icon_button, link_text, menu_item, nav_item, picture, pill, play_disc, playlist_actions, search_field, section, tier_color};

/// A playlist being dragged in the sidebar: id and title.
struct Dragged(String, String);

impl App {
    pub(super) fn login_ui(&mut self, ui: &mut Ui) {
        let mut finish = None;
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::BG)).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.3);
                ui.label(RichText::new("riptide").font(bold(44.0)).color(TEXT));
                ui.label(RichText::new("Your Tidal library, light and fast").size(15.0).color(SECONDARY));
                ui.add_space(32.0);
                if self.busy {
                    ui.spinner();
                } else if let Some((flow, pasted)) = &mut self.login {
                    ui.label(RichText::new("Sign in in your browser, then paste the address of the page you land on:").color(SECONDARY));
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
                    ui.colored_label(DANGER, e);
                }
            });
        });
        if let Some((flow, pasted)) = finish {
            (self.busy, self.message) = (true, None);
            let session = tidal::session_path(&self.data);
            self.spawn(async move {
                let tidal = flow.finish(&pasted, &session).await?;
                Ok(then(move |app| app.signed_in(tidal)))
            });
        }
    }

    pub(super) fn sidebar(&self, ui: &mut Ui, actions: &mut Vec<Action>) {
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
            let top = [
                (Icon::Home, "Home", open == Some(&Source::Home), Action::Open(Source::Home)),
                (Icon::Explore, "Explore", open == Some(&Source::explore()), Action::Open(Source::explore())),
                (Icon::Search, "Search", matches!(open, Some(Source::Search(_))), Action::FocusSearch),
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
            egui::Sides::new().show(
                ui,
                |ui| {
                    ui.add_space(10.0);
                    ui.label(RichText::new("PLAYLISTS").font(semibold(11.0)).color(DIM));
                },
                |ui| {
                    let plus = egui::Button::image(Icon::Plus.image(SECONDARY, 16.0)).frame(false);
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
                                ui.painter().rect_stroke(response.rect, 8.0, egui::Stroke::new(1.5, ACCENT), egui::StrokeKind::Inside);
                            }
                            if let Some(dragged) = response.dnd_release_payload::<Dragged>() {
                                actions.push(Action::MovePlaylist(dragged.0.clone(), id.clone()));
                            }
                            response.context_menu(|ui| {
                                menu_item(ui, actions, "Rename", dialogs::form(Target::RenameFolder(id.clone()), text.as_str()));
                                let delete = dialogs::confirm(format!("Delete {text}?"), "The folder goes; the playlists in it move to the top level.", Action::DeleteFolder(id.clone()));
                                menu_item(ui, actions, RichText::new("Delete folder").color(DANGER), delete);
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
        let galley = painter.layout_no_wrap(dragged.1.clone(), semibold(13.0), TEXT);
        let rect = egui::Rect::from_min_size(at + vec2(14.0, 6.0), galley.size() + vec2(20.0, 12.0));
        painter.rect_filled(rect, 8.0, theme::HOVER);
        painter.galley(rect.min + vec2(10.0, 6.0), galley, TEXT);
    }

    pub(super) fn player_bar(&mut self, ui: &mut Ui, mood: Option<Color32>, actions: &mut Vec<Action>) {
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
                    heart(ui, t.id, &self.library, true, actions);
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
                    let mut volume = f64::from(self.settings.volume);
                    let slider = bar(ui, &mut volume, 1.0, 96.0, true);
                    if slider.changed() {
                        self.settings.volume = volume as f32;
                        status.set_volume(self.settings.volume * self.settings.volume);
                    }
                    save = slider.drag_stopped() || slider.clicked();
                    ui.add(if self.settings.volume > 0.0 { Icon::Volume } else { Icon::Muted }.image(SECONDARY, 18.0));
                    ui.add_space(6.0);
                    for (open, icon, hint, action) in [(self.queue_open, Icon::Queue, "Queue", Action::Queue), (self.lyrics_open, Icon::Lyrics, "Lyrics", Action::Lyrics)] {
                        if icon_button(ui, icon, 18.0, if open { ACCENT } else { SECONDARY }).on_hover_text(hint).clicked() {
                            actions.push(action);
                        }
                    }
                    ui.add_space(6.0);
                    let (tier, format) = status.format.lock().unwrap().clone();
                    let (tier, format) = if track.is_some() { (tier, format) } else { (self.settings.quality, String::new()) };
                    let color = tier_color(tier);
                    let badge = egui::Button::new(RichText::new(tier.name().to_uppercase()).font(bold(11.0)).color(color)).fill(color.gamma_multiply(0.14)).corner_radius(4.0);
                    egui::containers::menu::MenuButton::from_button(badge).ui(ui, |ui| {
                        for q in Quality::ALL {
                            if ui.radio(self.settings.quality == q, RichText::new(q.name()).color(tier_color(q))).clicked() {
                                actions.push(Action::Quality(q));
                            }
                        }
                    });
                    ui.label(RichText::new(format).size(11.0).color(mood.map_or(DIM, |_| Color32::from_white_alpha(150))));
                });
            });
        });
        if save {
            self.settings.save(&self.data);
        }
    }

    /// The queue, beside the page.
    pub(super) fn queue_panel(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        if !self.queue_open {
            return;
        }
        let rows = Rows { playing: self.queue.current().map(|t| t.id), library: &self.library, editing: None, queue: true };
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
    pub(super) fn now_playing(&mut self, ui: &mut Ui, mood: Option<Color32>, actions: &mut Vec<Action>) {
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
                ui.vertical(|ui| {
                    scroll.show(ui, |ui| {
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
                    })
                });
            });
        });
    }

    pub(super) fn content(&mut self, ui: &mut Ui, actions: &mut Vec<Action>) {
        let frame = egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin { left: 28, right: 16, top: 14, bottom: 0 });
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
                    if let Some((text, error)) = &self.message {
                        ui.add(egui::Label::new(RichText::new(text).color(if *error { DANGER } else { SECONDARY })).truncate());
                    }
                });
            });
            ui.add_space(4.0);
            let editing = match self.page.as_ref().map(|p| &p.source) {
                Some(Source::Playlist(id)) if self.library.mine(id) => Some(id.clone()),
                _ => None,
            };
            let lastfm_user = self.lastfm.as_ref().and_then(|l| l.session.as_ref()).map(|(_, user)| user.clone());
            let Some(page) = &mut self.page else { return };
            let rows = Rows { playing: self.queue.current().map(|t| t.id), library: &self.library, editing: editing.as_deref(), queue: false };
            let (settings, home) = (&mut self.settings, self.data.parent().unwrap_or(&self.data));
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                crate::widgets::page(ui, page, &rows, actions);
                if matches!(page.body, Body::Settings) {
                    crate::settings::page(ui, settings, lastfm_user.as_deref(), home, actions);
                }
            });
        });
    }
}
