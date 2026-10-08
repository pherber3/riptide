//! Riptide's own controls, drawn with egui: buttons, bars, pictures and the like. They know nothing
//! of pages or the library; `app::views` builds those from them.

use egui::{Align, Color32, Layout, Rect, Response, RichText, Sense, Stroke, Ui, Vec2, pos2, vec2};

use crate::theme::{GOLD, Icon, TEAL, bold, medium, p, semibold};
use crate::tidal::Quality;

/// A tier's badge, as the player shows the quality playing: "MAX" in gold, "HIGH" in teal.
pub fn tier_badge(quality: Quality) -> egui::Button<'static> {
    let color = tier_color(quality);
    egui::Button::new(RichText::new(quality.name().to_uppercase()).font(bold(11.0)).color(color)).fill(color.gamma_multiply(0.14)).corner_radius(4.0)
}

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

/// A section's title, with whatever `aside` adds at its right.
pub fn section(ui: &mut Ui, title: &str, aside: impl FnOnce(&mut Ui)) {
    ui.add_space(28.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).font(bold(22.0)).color(p().text));
        ui.with_layout(Layout::right_to_left(Align::Center), aside);
    });
    ui.add_space(10.0);
}

/// Text laid out on one line no wider than `width`, cut short with an ellipsis, to paint.
pub fn fitted(ui: &Ui, text: impl Into<egui::WidgetText>, width: f32) -> std::sync::Arc<egui::Galley> {
    text.into().into_galley(ui, Some(egui::TextWrapMode::Truncate), width, egui::TextStyle::Body)
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
    let lit = selected || response.hovered();
    if lit {
        ui.painter().rect_filled(rect, 8.0, if selected { p().surface_hover } else { p().surface });
    }
    let color = if lit { p().text } else { p().secondary };
    icon.image(color, 18.0).paint_at(ui, Rect::from_min_size(rect.left_center() + vec2(10.0, -9.0), Vec2::splat(18.0)));
    let galley = fitted(ui, RichText::new(text).font(medium(14.0)), rect.width() - 48.0);
    ui.painter().galley(pos2(rect.left() + 40.0, rect.center().y - galley.size().y / 2.0), galley, color);
    clickable(response)
}

pub fn picture(ui: &mut Ui, url: Option<String>, side: f32, round: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    paint_picture(ui, url, rect, round);
    response
}

/// Artwork, loaded only once it scrolls into view so long shelves and grids don't fill memory.
pub fn paint_picture(ui: &Ui, url: Option<String>, rect: Rect, round: bool) {
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
pub fn arrow(ui: &Ui, c: egui::Pos2, up: bool) {
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

/// A rounded link to another page, such as a genre.
pub fn chip(ui: &mut Ui, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.into(), medium(14.0), Color32::PLACEHOLDER);
    let (rect, response) = ui.allocate_exact_size(galley.size() + vec2(32.0, 18.0), Sense::click());
    ui.painter().rect_filled(rect, rect.height() / 2.0, if response.hovered() { p().surface_hover } else { p().surface });
    ui.painter().galley(rect.center() - galley.size() / 2.0, galley, p().text);
    clickable(response)
}
