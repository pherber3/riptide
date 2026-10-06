use egui::epaint::Shadow;
use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle, vec2};
use fastframe_fonts::Weight;

const fn rgb(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

pub const BG: Color32 = rgb(0x0b0b0d);
pub const SIDEBAR: Color32 = rgb(0x111114);
pub const BAR: Color32 = rgb(0x151518);
pub const SURFACE: Color32 = rgb(0x1e1e22);
pub const HOVER: Color32 = rgb(0x2a2a30);
pub const LINE: Color32 = rgb(0x242429);
pub const TEXT: Color32 = rgb(0xf5f5f7);
pub const SECONDARY: Color32 = rgb(0xa1a1a6);
pub const DIM: Color32 = rgb(0x6b6b70);
pub const ACCENT: Color32 = rgb(0x33ffee);
pub const GOLD: Color32 = rgb(0xf5c542);
pub const DANGER: Color32 = rgb(0xff6b6b);

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

/// The one dark look: near-black panels, borderless rounded widgets, Inter at real weights.
pub fn install(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
    fastframe_icons::install::<Icon>(ctx);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        let v = &mut s.visuals;
        v.panel_fill = BG;
        v.window_fill = SURFACE;
        v.extreme_bg_color = SURFACE;
        v.faint_bg_color = SURFACE;
        v.weak_text_color = Some(SECONDARY);
        v.hyperlink_color = SECONDARY;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.3);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
        v.window_stroke = Stroke::new(1.0, LINE);
        v.window_corner_radius = CornerRadius::same(10);
        v.menu_corner_radius = CornerRadius::same(8);
        v.window_shadow = Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(160) };
        v.popup_shadow = Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(160) };
        v.striped = false;
        let w = &mut v.widgets;
        for (state, fill, text) in [(&mut w.noninteractive, BG, TEXT), (&mut w.inactive, SURFACE, SECONDARY), (&mut w.hovered, HOVER, TEXT), (&mut w.active, HOVER, TEXT), (&mut w.open, HOVER, TEXT)] {
            state.corner_radius = CornerRadius::same(6);
            state.bg_fill = fill;
            state.weak_bg_fill = fill;
            state.bg_stroke = Stroke::NONE;
            state.fg_stroke = Stroke::new(1.5, text);
            state.expansion = 0.0;
        }
        w.noninteractive.bg_stroke = Stroke::new(1.0, LINE);
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.interact_size.y = 28.0;
        s.spacing.menu_margin = egui::Margin::same(6);
        s.spacing.scroll = egui::style::ScrollStyle::floating();
        s.text_styles.insert(TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(TextStyle::Button, medium(14.0));
        s.text_styles.insert(TextStyle::Small, FontId::proportional(12.0));
        s.text_styles.insert(TextStyle::Heading, bold(24.0));
    });
}
