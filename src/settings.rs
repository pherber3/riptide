use std::collections::HashMap;
use std::path::Path;

use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::app::Action;
use crate::theme::{SECONDARY, SURFACE, TEXT, medium};
use crate::tidal::Quality;
use crate::view::Sort;
use crate::widgets::{section, switch, tier_color};

/// What Riptide remembers between runs, besides the sign-in and the queue: `data/settings.json`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub quality: Quality,
    pub volume: f32,
    /// Scale each track to Tidal's reference loudness.
    pub normalize: bool,
    /// Closing the window hides it in the tray rather than quitting.
    pub close_to_tray: bool,
    /// The output device by name, or the system default.
    pub device: Option<String>,
    /// The window's outer position and inner size (x, y, width, height), while not maximized.
    pub window: Option<[f32; 4]>,
    pub maximized: bool,
    /// Each kind of list page's sort, and whether it is reversed.
    pub sorts: HashMap<String, (Sort, bool)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { quality: Quality::Max, volume: 1.0, normalize: false, close_to_tray: false, device: None, window: None, maximized: false, sorts: HashMap::new() }
    }
}

impl Settings {
    pub fn load(data: &Path) -> Self {
        std::fs::read(data.join("settings.json")).ok().and_then(|json| serde_json::from_slice(&json).ok()).unwrap_or_default()
    }

    pub fn save(&self, data: &Path) {
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(data.join("settings.json"), json);
        }
    }
}

/// The settings page: one card per section, a row per setting with its control on the right. It
/// edits `s` directly; quality goes through an action, since it reloads the playing track.
pub fn page(ui: &mut Ui, s: &mut Settings, lastfm_user: Option<&str>, home: &Path, actions: &mut Vec<Action>) {
    ui.set_max_width(760.0);
    card(ui, "Playback", |ui| {
        let detail = match s.quality {
            Quality::Max => "Up to 24-bit, 192 kHz FLAC",
            Quality::High => "16-bit, 44.1 kHz FLAC (CD quality)",
            Quality::Low => "AAC, 320 kbps",
        };
        row(ui, "Streaming quality", detail, |ui| {
            for q in Quality::ALL.into_iter().rev() {
                if ui.add(egui::Button::selectable(s.quality == q, RichText::new(q.name()).color(tier_color(q)))).clicked() {
                    actions.push(Action::Quality(q));
                }
            }
        });
        ui.separator();
        row(ui, "Output device", "Where Riptide plays, whatever the system default is", |ui| {
            // The devices are listed once each time the menu opens; nothing is opened to list them.
            let listed = egui::Id::new("devices");
            let combo = egui::ComboBox::from_id_salt("device").width(260.0).selected_text(s.device.as_deref().unwrap_or("System default")).show_ui(ui, |ui| {
                let names = ui.data_mut(|d| d.get_temp_mut_or_insert_with(listed, || fastframe_audio::output_device_names().unwrap_or_default()).clone());
                ui.selectable_value(&mut s.device, None, "System default");
                for name in names {
                    ui.selectable_value(&mut s.device, Some(name.clone()), name);
                }
            });
            if combo.inner.is_none() {
                ui.data_mut(|d| d.remove::<Vec<String>>(listed));
            }
        });
        ui.separator();
        row(ui, "Normalize volume", "Play every track at about the same loudness, as Tidal does", |ui| {
            if switch(ui, s.normalize).clicked() {
                s.normalize = !s.normalize;
            }
        });
    });
    card(ui, "Window", |ui| {
        row(ui, "Close to tray", "Closing the window keeps Riptide playing in the system tray", |ui| {
            if switch(ui, s.close_to_tray).clicked() {
                s.close_to_tray = !s.close_to_tray;
            }
        });
    });
    card(ui, "Connections", |ui| {
        let detail = lastfm_user.map_or("Scrobble what you play to your Last.fm profile".into(), |user| format!("Scrobbling as {user}"));
        row(ui, "Last.fm", &detail, |ui| match lastfm_user {
            Some(_) if ui.button("Disconnect").clicked() => actions.push(Action::DisconnectLastFm),
            None if ui.button("Connect").clicked() => actions.push(Action::ConnectLastFm),
            _ => {}
        });
    });
    card(ui, "About", |ui| {
        row(ui, concat!("Riptide ", env!("CARGO_PKG_VERSION")), &format!("Settings, sign-in and cache live in {}", home.display()), |ui| {
            if ui.button("Open folder").clicked() {
                let _ = open::that(home);
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
    egui::Sides::new().show(
        ui,
        |ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(title).font(medium(15.0)).color(TEXT));
                ui.label(RichText::new(detail).size(13.0).color(SECONDARY));
            });
        },
        control,
    );
}
