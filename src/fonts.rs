use std::sync::Arc;

use egui::FontData;

/// Windows fonts for the scripts Inter lacks: Chinese, Japanese, Korean, Indic, Thai, other
/// African and Asian scripts, and symbols. The first that has a glyph draws it.
#[cfg(windows)]
const FALLBACKS: &[&str] = &[
    "msyh.ttc",
    "YuGothR.ttc",
    "malgun.ttf",
    "Nirmala.ttc",
    "LeelawUI.ttf",
    "ebrima.ttf",
    "gadugi.ttf",
    "mmrtext.ttf",
    "himalaya.ttf",
    "taile.ttf",
    "seguisym.ttf",
    "segoeui.ttf",
];

#[cfg(not(windows))]
const FALLBACKS: &[&str] = &[];

/// Inter, then the system fallbacks memory-mapped rather than read in: a CJK font is 10–20 MB,
/// and mapped, only the glyphs actually drawn are ever paged in.
pub fn install(ctx: &egui::Context) {
    let mut fonts = fastframe_fonts::FontSetup::default().system_fallbacks(false).definitions();
    let dir = std::path::Path::new(&std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into())).join("Fonts");
    for file in FALLBACKS {
        let Ok(handle) = std::fs::File::open(dir.join(file)) else { continue };
        // SAFETY: installed system fonts are not modified while the app runs.
        let Ok(map) = (unsafe { memmap2::Mmap::map(&handle) }) else { continue };
        let bytes: &'static [u8] = Box::leak(Box::new(map));
        fonts.font_data.insert(file.to_string(), Arc::new(FontData::from_static(bytes)));
        for family in fonts.families.values_mut() {
            family.push(file.to_string());
        }
    }
    ctx.set_fonts(fonts);
}
