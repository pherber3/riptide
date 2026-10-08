use egui::{Color32, Context, RichText, Ui, vec2};

use crate::app::Action;
use crate::theme::{Icon, bold, medium, p, semibold};
use crate::widgets::{icon_button, setting, switch};

/// The one dialog that can be open at a time.
pub enum Dialog {
    /// "Are you sure?" before something that can't be undone, then the action.
    Confirm {
        title: String,
        text: &'static str,
        then: Action,
    },
    /// A track's credits: its title, then each role and its names.
    Credits(String, Vec<(String, String)>),
    Form(PlaylistForm),
}

/// What the playlist dialog is for: a new playlist (with a track to put in it), renaming one, or the
/// same for a folder.
pub enum Target {
    Create(Option<u64>),
    Rename(String),
    CreateFolder,
    RenameFolder(String),
}

pub struct PlaylistForm {
    pub target: Target,
    pub title: String,
    pub description: String,
    pub public: bool,
}

/// Opens the playlist (or folder) dialog with `title` filled in.
pub fn form(target: Target, title: impl Into<String>) -> Action {
    Action::Dialog(Box::new(Dialog::Form(PlaylistForm { target, title: title.into(), description: String::new(), public: false })))
}

/// Asks before doing `then`.
pub fn confirm(title: String, text: &'static str, then: Action) -> Action {
    Action::Dialog(Box::new(Dialog::Confirm { title, text, then }))
}

/// Shows the open dialog. None while it stays open; once it closes, whether it was accepted
/// (confirmed or saved) rather than dismissed.
pub fn show(ctx: &Context, dialog: &mut Dialog) -> Option<bool> {
    let (accepted, dismissed) = match dialog {
        Dialog::Confirm { title, text, .. } => modal(ctx, 400.0, title, |ui| {
            ui.label(RichText::new(*text).size(14.0).color(p().secondary));
            ui.add_space(20.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let yes = ui.add(button("Delete", Color32::WHITE, p().danger)).clicked();
                (yes, ui.add(button("Cancel", p().text, p().surface_hover)).clicked())
            })
            .inner
        }),
        Dialog::Credits(title, credits) => modal(ctx, 440.0, &format!("Credits · {title}"), |ui| {
            egui::ScrollArea::vertical().max_height(480.0).show(ui, |ui| {
                if credits.is_empty() {
                    ui.label(RichText::new("Tidal has no credits for this track.").color(p().secondary));
                }
                for (role, names) in credits.iter() {
                    ui.label(RichText::new(role.to_uppercase()).font(semibold(11.0)).color(p().dim));
                    ui.label(RichText::new(names).size(15.0).color(p().text));
                    ui.add_space(10.0);
                }
            });
            (false, false)
        }),
        Dialog::Form(form) => {
            let heading = match form.target {
                Target::Create(_) => "Create playlist",
                Target::Rename(_) => "Rename playlist",
                Target::CreateFolder => "Create folder",
                Target::RenameFolder(_) => "Rename folder",
            };
            modal(ctx, 460.0, heading, |ui| (playlist_form(ui, form), false))
        }
    };
    (accepted || dismissed).then_some(accepted)
}

/// The Create playlist dialog's fields, as Tidal has them (just the title for the others). Whether
/// it was saved.
fn playlist_form(ui: &mut Ui, form: &mut PlaylistForm) -> bool {
    let title = ui.add(egui::TextEdit::singleline(&mut form.title).hint_text("Title").font(medium(16.0)).margin(vec2(14.0, 12.0)).desired_width(f32::INFINITY));
    if ui.ctx().memory(|m| m.focused().is_none()) {
        title.request_focus();
    }
    let enter = title.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    if let Target::Create(_) = form.target {
        ui.add_space(10.0);
        let description = egui::TextEdit::multiline(&mut form.description).hint_text("Write a description").char_limit(500).desired_rows(4);
        ui.add(description.margin(vec2(14.0, 12.0)).desired_width(f32::INFINITY));
        ui.label(RichText::new(format!("{}/500 characters", form.description.chars().count())).size(12.0).color(p().dim));
        ui.add_space(12.0);
        egui::Frame::new().fill(p().surface_hover).corner_radius(10).inner_margin(egui::Margin::symmetric(16, 12)).show(ui, |ui| {
            setting(ui, "Make it public", "Your playlist will be visible on your profile and to anyone.", |ui| switch(ui, &mut form.public));
        });
    }
    ui.add_space(16.0);
    let ready = !form.title.trim().is_empty();
    let save = ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| ui.add_enabled(ready, button("Save", p().window, p().text)).clicked());
    ready && (save.inner || enter)
}

/// A dialog: a heading with a close button over `body`, which says whether it was accepted.
/// Returns that, and whether the dialog was dismissed (the close button, Esc, or a click outside).
fn modal(ctx: &Context, width: f32, heading: &str, body: impl FnOnce(&mut Ui) -> (bool, bool)) -> (bool, bool) {
    let frame = egui::Frame::new().fill(p().surface).corner_radius(14).inner_margin(24);
    let modal = egui::Modal::new(egui::Id::new("dialog")).frame(frame).show(ctx, |ui| {
        ui.set_width(width);
        let (_, close) = egui::Sides::new().shrink_left().show(
            ui,
            |ui| ui.add(egui::Label::new(RichText::new(heading).font(bold(20.0)).color(p().text)).truncate()),
            |ui| icon_button(ui, Icon::Close, 18.0, p().secondary).clicked(),
        );
        ui.add_space(14.0);
        let (accepted, cancelled) = body(ui);
        (accepted, cancelled || close)
    });
    let closed = modal.should_close();
    let (accepted, dismissed) = modal.inner;
    (accepted, dismissed || closed)
}

fn button(text: &str, ink: Color32, fill: Color32) -> egui::Button<'_> {
    egui::Button::new(RichText::new(text).font(semibold(14.0)).color(ink)).fill(fill).corner_radius(20).min_size(vec2(88.0, 38.0))
}
