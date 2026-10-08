use std::sync::RwLock;

use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle, vec2};
use fastframe_fonts::Weight;

const fn rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// The quality tiers' colours, as Tidal has them: the same in every theme.
pub const GOLD: Color32 = rgb(0xf5c542);
pub const TEAL: Color32 = rgb(0x33ffee);

/// The interface colours, by the names palette files use (see `fastframe_theme::BASE_COLORS`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// The page behind everything.
    pub window: Color32,
    /// The sidebar, side panels and player bar.
    pub panel: Color32,
    /// Fields, cards and highlighted rows.
    pub surface: Color32,
    pub surface_hover: Color32,
    pub surface_active: Color32,
    /// Hairlines between things.
    pub outline: Color32,
    pub text: Color32,
    pub secondary: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    pub on_accent: Color32,
    pub danger: Color32,
    pub warning: Color32,
    /// Menus and popups.
    pub overlay: Color32,
    pub shadow: Color32,
}

/// Riptide's own look: near-black, with Tidal's teal.
pub const DARK: Palette = Palette {
    dark: true,
    window: rgb(0x0b0b0d),
    panel: rgb(0x121215),
    surface: rgb(0x1e1e22),
    surface_hover: rgb(0x2a2a30),
    surface_active: rgb(0x34343b),
    outline: rgb(0x242429),
    text: rgb(0xf5f5f7),
    secondary: rgb(0xa1a1a6),
    dim: rgb(0x6b6b70),
    accent: TEAL,
    accent_hover: rgb(0x80fff5),
    on_accent: rgb(0x000000),
    danger: rgb(0xff6b6b),
    warning: GOLD,
    overlay: rgb(0x1e1e22),
    shadow: Color32::from_black_alpha(160),
};

const LIGHT: Palette = Palette {
    dark: false,
    window: rgb(0xffffff),
    panel: rgb(0xf5f5f7),
    surface: rgb(0xececf0),
    surface_hover: rgb(0xe2e2e7),
    surface_active: rgb(0xd6d6dc),
    outline: rgb(0xe0e0e5),
    text: rgb(0x1d1d1f),
    secondary: rgb(0x6e6e73),
    dim: rgb(0xa1a1a6),
    accent: rgb(0x0e9f94),
    accent_hover: rgb(0x0b857c),
    on_accent: rgb(0xffffff),
    danger: rgb(0xd63b4c),
    warning: rgb(0xb8860b),
    overlay: rgb(0xffffff),
    shadow: Color32::from_black_alpha(50),
};

impl fastframe_theme::Palette for Palette {
    fn base(base: fastframe_theme::Base) -> Self {
        match base {
            fastframe_theme::Base::Dark => DARK,
            fastframe_theme::Base::Light => LIGHT,
        }
    }

    fn set(&mut self, name: &str, color: Color32) -> bool {
        let slot = match name {
            "window" => &mut self.window,
            "panel" => &mut self.panel,
            "surface" => &mut self.surface,
            "surface_hover" => &mut self.surface_hover,
            "surface_active" => &mut self.surface_active,
            "outline" => &mut self.outline,
            "text" => &mut self.text,
            "secondary" => &mut self.secondary,
            "dim" => &mut self.dim,
            "accent" => &mut self.accent,
            "accent_hover" => &mut self.accent_hover,
            "on_accent" => &mut self.on_accent,
            "danger" => &mut self.danger,
            "warning" => &mut self.warning,
            "overlay" => &mut self.overlay,
            "shadow" => &mut self.shadow,
            _ => return false,
        };
        *slot = color;
        true
    }
}

static PALETTE: RwLock<Palette> = RwLock::new(DARK);

/// The palette in use.
pub fn p() -> Palette {
    *PALETTE.read().unwrap()
}

pub fn medium(size: f32) -> FontId {
    Weight::Medium.font_id(size)
}

pub fn semibold(size: f32) -> FontId {
    Weight::SemiBold.font_id(size)
}

pub fn bold(size: f32) -> FontId {
    Weight::Bold.font_id(size)
}

fastframe_icons::icons! {
    pub enum Icon {
        prefix: "riptide-icon-",
        directory: "../assets/icons/",
        Home => "house",
        Search => lucide "search",
        Music => "music",
        Disc => "disc-3",
        Artists => lucide "users",
        Playlists => "list-music",
        Folder => "folder",
        Queue => "list-end",
        Lyrics => lucide "mic",
        Back => lucide "chevron-left",
        Forward => lucide "chevron-right",
        Down => lucide "chevron-down",
        Plus => lucide "plus",
        More => lucide "ellipsis",
        Check => lucide "check",
        Close => lucide "x",
        Copy => lucide "copy",
        Explore => "compass",
        Settings => lucide "settings",
        Heart => "heart",
        HeartFilled => "heart-filled",
        Play => "play-filled",
        Pause => "pause-filled",
        Prev => "skip-back-filled",
        Next => "skip-forward-filled",
        Shuffle => "shuffle",
        Repeat => "repeat",
        RepeatOne => "repeat-1",
        Radio => "radio",
        Playing => "audio-lines",
        Volume => lucide "volume-2",
        Muted => lucide "volume-x",
    }
}

/// The app's logo at `size` pixels square, as RGBA: the window, taskbar and tray icon.
/// Where the theme files are kept, in the data folder.
pub fn folder(data: &std::path::Path) -> std::path::PathBuf {
    data.join("themes")
}

pub fn logo(size: usize) -> Vec<u8> {
    let image = image::load_from_memory(include_bytes!("../assets/riptide.png")).expect("bundled icon");
    let size = size as u32;
    let image = if image.width() == size { image } else { image.resize_exact(size, size, image::imageops::FilterType::Lanczos3) };
    image.to_rgba8().into_raw()
}

pub fn install(ctx: &egui::Context, palette: Palette) {
    egui_extras::install_image_loaders(ctx);
    fastframe_icons::install::<Icon>(ctx);
    apply(ctx, palette);
}

/// Uses `c` for everything: the app's own drawing reads it through `p()`, and egui's widgets get
/// borderless rounded looks in its colours, with Inter at real weights.
pub fn apply(ctx: &egui::Context, c: Palette) {
    *PALETTE.write().unwrap() = c;
    let theme = if c.dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(theme);
    ctx.style_mut_of(theme, |s| {
        let v = &mut s.visuals;
        *v = if c.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        v.panel_fill = c.window;
        v.window_fill = c.overlay;
        v.extreme_bg_color = c.surface;
        v.faint_bg_color = c.surface;
        v.weak_text_color = Some(c.secondary);
        v.hyperlink_color = c.secondary;
        v.selection.bg_fill = c.accent.gamma_multiply(0.3);
        v.selection.stroke = Stroke::new(1.0, c.accent);
        v.text_cursor.stroke = Stroke::new(2.0, c.accent);
        v.window_stroke = Stroke::new(1.0, c.outline);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.window_shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: c.shadow };
        v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: c.shadow };
        v.striped = false;
        let w = &mut v.widgets;
        for (state, fill, text) in [
            (&mut w.noninteractive, c.window, c.text),
            (&mut w.inactive, c.surface, c.secondary),
            (&mut w.hovered, c.surface_hover, c.text),
            (&mut w.active, c.surface_active, c.text),
            (&mut w.open, c.surface_hover, c.text),
        ] {
            state.corner_radius = CornerRadius::same(6);
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke = Stroke::new(1.5, text);
            state.expansion = 0.0;
        }
        w.noninteractive.bg_stroke = Stroke::new(1.0, c.outline);
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 7.0);
        s.spacing.interact_size.y = 28.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        // A slim, quiet scrollbar in its own gutter, so it never covers what is at the right edge; a
        // little wider and brighter only while in use.
        s.spacing.scroll = egui::style::ScrollStyle {
            floating_allocated_width: 12.0,
            bar_width: 6.0,
            floating_width: 3.0,
            foreground_color: false,
            active_handle_opacity: 0.5,
            interact_handle_opacity: 0.8,
            ..egui::style::ScrollStyle::floating()
        };
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, medium(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(TextStyle::Heading, bold(24.0));
    });
}
