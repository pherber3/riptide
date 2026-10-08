use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use egui::ViewportCommand as Command;
use fastframe_tray::{Event, MenuItem, Tray};

use super::{Action, App};

/// A later launch asked for the window (see `main`), and the context to wake for it.
static SURFACE: AtomicBool = AtomicBool::new(false);
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();

/// Answers a later launch: bring this copy's window up.
pub fn surface_request(request: &str) -> Option<String> {
    (request == "show").then(|| {
        SURFACE.store(true, Relaxed);
        CONTEXT.get().map(egui::Context::request_repaint);
        "ok".into()
    })
}

/// The tray item, with playback controls in its menu.
pub(super) fn tray(ctx: &egui::Context) -> Option<Tray> {
    let _ = CONTEXT.set(ctx.clone());
    let menu = [("show", "Show Riptide"), ("play", "Play / Pause"), ("next", "Next"), ("previous", "Previous"), ("quit", "Quit")];
    let config = fastframe_tray::Config {
        id: "riptide",
        title: "Riptide".into(),
        icon: crate::theme::logo,
        template_icon: None,
        themed_icon: false,
        menu_on_click: false,
        menu: menu.into_iter().map(|(id, label)| MenuItem::action(id, label)).collect(),
    };
    let ctx = ctx.clone();
    Tray::spawn(config, move || ctx.request_repaint())
}

impl App {
    /// The tray's menu, a later launch asking for the window, and closing the window (which hides
    /// it in the tray instead, when that is turned on).
    pub(super) fn window(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        let mut show = SURFACE.swap(false, Relaxed);
        let mut hide = false;
        for event in self.tray.as_ref().map(Tray::events).unwrap_or_default() {
            match event {
                Event::Show => show = true,
                Event::Toggle | Event::Menu("show") => (show, hide) = (self.hidden, !self.hidden),
                Event::Menu("play") => actions.push(Action::Toggle),
                Event::Menu("next") => actions.push(Action::Next),
                Event::Menu("previous") => actions.push(Action::Prev),
                Event::Menu("quit") => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(Command::Close);
                }
                Event::Menu(_) => {}
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.settings.close_to_tray && !self.quitting && self.tray.is_some() {
            ctx.send_viewport_cmd(Command::CancelClose);
            hide = true;
        }
        if hide {
            self.hidden = true;
            self.save_state();
            ctx.send_viewport_cmd(Command::Visible(false));
        }
        if show {
            self.hidden = false;
            for command in [Command::Visible(true), Command::Minimized(false), Command::Focus] {
                ctx.send_viewport_cmd(command);
            }
        }
        // Where the window is, to open there next time.
        ctx.input(|i| {
            let v = i.viewport();
            self.settings.maximized = v.maximized.unwrap_or(false);
            if !self.settings.maximized
                && v.minimized != Some(true)
                && let (Some(outer), Some(inner)) = (v.outer_rect, v.inner_rect)
            {
                self.settings.window = Some([outer.min.x, outer.min.y, inner.width(), inner.height()]);
            }
        });
    }
}
