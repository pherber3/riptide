use std::path::Path;

use egui::{Align, Layout, RichText, Ui};

use crate::app::Action;
use crate::theme::{SECONDARY, SURFACE, TEXT, medium};
use crate::tidal::Quality;
use crate::widgets::{section, switch, tier_color};

/// What the settings page shows.
pub struct State<'a> {
    pub quality: Quality,
    pub device: Option<&'a str>,
    pub normalize: bool,
    pub lastfm_user: Option<&'a str>,
    pub data: &'a Path,
}

/// The settings page: one card per section, a row per setting with its control on the right.
pub fn page(ui: &mut Ui, state: &State, actions: &mut Vec<Action>) {
    ui.set_max_width(760.0);
    card(ui, "Playback", |ui| {
        let detail = match state.quality {
            Quality::Max => "Up to 24-bit, 192 kHz FLAC",
            Quality::High => "16-bit, 44.1 kHz FLAC (CD quality)",
            Quality::Low => "AAC, 320 kbps",
        };
        row(ui, "Streaming quality", detail, |ui| {
            for q in Quality::ALL.into_iter().rev() {
                let text = RichText::new(q.name()).color(tier_color(q));
                if ui.add(egui::Button::selectable(state.quality == q, text)).clicked() {
                    actions.push(Action::Quality(q));
                }
            }
        });
        ui.separator();
        row(ui, "Output device", "Where Riptide plays, whatever the system default is", |ui| {
            egui::ComboBox::from_id_salt("device").width(260.0).selected_text(state.device.unwrap_or("System default")).show_ui(ui, |ui| {
                if ui.selectable_label(state.device.is_none(), "System default").clicked() {
                    actions.push(Action::Device(None));
                }
                // Listed only while the menu is open; nothing is opened to list them.
                for name in fastframe_audio::output_device_names().unwrap_or_default() {
                    if ui.selectable_label(state.device == Some(name.as_str()), &name).clicked() {
                        actions.push(Action::Device(Some(name)));
                    }
                }
            });
        });
        ui.separator();
        row(ui, "Normalize volume", "Play every track at about the same loudness, as Tidal does", |ui| {
            if switch(ui, state.normalize).clicked() {
                actions.push(Action::Normalize(!state.normalize));
            }
        });
    });
    card(ui, "Connections", |ui| {
        let detail = match state.lastfm_user {
            Some(user) => format!("Scrobbling as {user}"),
            None => "Scrobble what you play to your Last.fm profile".into(),
        };
        row(ui, "Last.fm", &detail, |ui| match state.lastfm_user {
            Some(_) if ui.button("Disconnect").clicked() => actions.push(Action::DisconnectLastFm),
            None if ui.button("Connect").clicked() => actions.push(Action::ConnectLastFm),
            _ => {}
        });
    });
    card(ui, "About", |ui| {
        row(ui, concat!("Riptide ", env!("CARGO_PKG_VERSION")), &format!("Settings, sign-in and cache live in {}", state.data.display()), |ui| {
            if ui.button("Open folder").clicked() {
                let _ = open::that(state.data);
            }
        });
    });
}

fn card(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    section(ui, title);
    egui::Frame::new().fill(SURFACE).corner_radius(10).inner_margin(egui::Margin::symmetric(20, 14)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

/// A setting: its name and a line about it on the left, its control on the right.
fn row(ui: &mut Ui, title: &str, detail: &str, control: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(RichText::new(title).font(medium(15.0)).color(TEXT));
            ui.label(RichText::new(detail).size(13.0).color(SECONDARY));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), control);
    });
}
