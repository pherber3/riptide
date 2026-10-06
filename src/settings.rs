use std::collections::HashMap;
use std::path::Path;

use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};

use fastframe_theme::Catalog;

use crate::app::Action;
use crate::theme::{Palette, bold, p};
use crate::tidal::Quality;
use crate::view::Sort;
use crate::widgets::{section, setting as row, switch, tier_color};

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
    /// The theme: a palette file's name in `data/themes`, or Riptide's own look.
    pub theme: Option<String>,
    /// Waves along the bottom of the lyrics view that move with the music.
    pub tide: bool,
    /// The window's outer position and inner size (x, y, width, height), while not maximized.
    pub window: Option<[f32; 4]>,
    pub maximized: bool,
    /// Each kind of list page's sort, and whether it is reversed.
    pub sorts: HashMap<String, (Sort, bool)>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { quality: Quality::Max, volume: 1.0, normalize: false, close_to_tray: false, device: None, theme: None, tide: true, window: None, maximized: false, sorts: HashMap::new() }
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
pub fn page(ui: &mut Ui, s: &mut Settings, themes: &Catalog<Palette>, lastfm_user: Option<&str>, home: &Path, actions: &mut Vec<Action>) {
    // One column down the middle of the page, however wide the window.
    let width = ui.available_width().min(760.0);
    ui.horizontal(|ui| {
        ui.add_space((ui.available_width() - width) / 2.0);
        ui.vertical(|ui| {
            ui.set_width(width);
            ui.add_space(12.0);
            ui.label(RichText::new("Settings").font(bold(32.0)).color(p().text));
            cards(ui, s, themes, lastfm_user, home, actions);
            ui.add_space(24.0);
        });
    });
}

fn cards(ui: &mut Ui, s: &mut Settings, themes: &Catalog<Palette>, lastfm_user: Option<&str>, home: &Path, actions: &mut Vec<Action>) {
    card(ui, "Appearance", |ui| {
        row(ui, "Theme", "Copy a theme in its folder and edit the colours to make your own", |ui| {
            if ui.button("Open folder").clicked() {
                let _ = open::that(home.join("data").join("themes"));
            }
            let shown = s.theme.as_deref().map_or("Riptide", fastframe_theme::display_name);
            egui::ComboBox::from_id_salt("theme").width(200.0).selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(&mut s.theme, None, "Riptide");
                for theme in themes.picker_themes() {
                    ui.selectable_value(&mut s.theme, Some(theme.filename.clone()), fastframe_theme::display_name(&theme.filename));
                }
            });
        });
        ui.separator();
        row(ui, "Tide", "Waves along the bottom of the lyrics view that rise and roll with the music", |ui| switch(ui, &mut s.tide));
    });
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
        row(ui, "Normalize volume", "Play every track at about the same loudness, as Tidal does", |ui| switch(ui, &mut s.normalize));
    });
    card(ui, "Window", |ui| {
        row(ui, "Close to tray", "Closing the window keeps Riptide playing in the system tray", |ui| switch(ui, &mut s.close_to_tray));
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
    egui::Frame::new().fill(p().surface).corner_radius(10).inner_margin(egui::Margin::symmetric(20, 14)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

