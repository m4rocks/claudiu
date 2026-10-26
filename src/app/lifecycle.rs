//! Ending and starting Claudiu: the quit question, remembering what was open, and bringing it back.

use super::*;
use crate::claude::{ask, mcp, usage};
use crate::store::WindowGeom;
use gpui::{Bounds, WindowBounds, point, px, size};

/// The ids of the sessions that were open, in order.
fn running_file() -> PathBuf {
    datadir::data_dir().join("running.json")
}

/// Start-up housekeeping: delete the old per-session files each folder's owner no longer reads. Nothing here touches
/// Claude Code's own data.
pub fn tidy_leftovers() {
    let data = datadir::data_dir();
    let [mcp_configs, titles, names, attached] = mcp::cleanup_dirs();
    datadir::prune(names, 7 * 86_400);
    datadir::prune(attached, 7 * 86_400);
    datadir::prune(mcp_configs, 7 * 86_400);
    datadir::prune(crate::claude::trim::settings_dir(), 7 * 86_400);
    // Titles are copied into the store on the next tick, so an old file is only a duplicate.
    datadir::prune(titles, 7 * 86_400);
    datadir::prune(usage::snapshot_dir(), 30 * 86_400);
    datadir::prune(usage::ctl_dir(), 7 * 86_400);
    datadir::prune(ask::dir(), 7 * 86_400);
    datadir::prune(crate::visual::dir(), 7 * 86_400);
    datadir::prune(data.join("screens"), 30 * 86_400);
    datadir::prune(data.join("pasted"), 7 * 86_400);
    datadir::prune(data.join("github-cache"), 30 * 86_400);
    // The status-line helper's per-session settings, gone since the mod reports usage; sweep what older builds left.
    let _ = std::fs::remove_dir_all(data.join("statusline"));
}

/// The window bounds saved last time, when they still land on a connected display.
pub fn saved_window_bounds(cx: &App) -> Option<WindowBounds> {
    let geom = Store::load(Store::default_path()).data.layout.window?;
    let displays: Vec<_> = cx
        .displays()
        .iter()
        .map(|d| {
            let b = d.bounds();
            (f32::from(b.origin.x), f32::from(b.origin.y), f32::from(b.size.width), f32::from(b.size.height))
        })
        .collect();
    if !geom.on_screen(&displays) {
        return None;
    }
    let bounds = Bounds { origin: point(px(geom.x), px(geom.y)), size: size(px(geom.w), px(geom.h)) };
    Some(if geom.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) })
}

impl Workspace {
    /// Keep the window's size and position in the layout so the next start opens it there; saved with the next tick.
    pub fn remember_window(&mut self, window: &Window) {
        let (bounds, maximized) = match window.window_bounds() {
            WindowBounds::Windowed(b) | WindowBounds::Fullscreen(b) => (b, false),
            WindowBounds::Maximized(b) => (b, true),
        };
        let geom = WindowGeom {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            w: f32::from(bounds.size.width),
            h: f32::from(bounds.size.height),
            maximized,
        };
        if self.store.data.layout.window != Some(geom) {
            self.store.data.layout.window = Some(geom);
            self.store.mark_dirty();
        }
    }

    /// Called by the window's close handler. Returns true when it is fine to close right now: asks first only
    /// while a Claude session is mid-turn; idle sessions just end with the app.
    pub fn request_quit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.quitting {
            return true;
        }
        // Moving a window raises no render, so its last position is read here, before the layout is saved.
        self.remember_window(window);
        let working = self.live.iter().filter(|l| {
            let v = l.view.read(cx);
            v.is_running() && v.is_working() && self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude)
        });
        let working = working.count();
        let unsaved = self.unsaved(None);
        if working == 0 && unsaved == 0 {
            self.finalize(cx);
            return true;
        }
        let mut body = Vec::new();
        if working > 0 {
            body.push(format!(
                "{working} Claude session{} still working. Quitting interrupts {} — sessions stay in your history and can be resumed.",
                if working == 1 { " is" } else { "s are" },
                if working == 1 { "it" } else { "them" }
            ));
        }
        if unsaved > 0 {
            body.push(format!("{unsaved} file{} in the editor {} unsaved changes that will be lost.", super::editing::plural(unsaved), if unsaved == 1 { "has" } else { "have" }));
        }
        self.modal = Some(Modal::Confirm {
            title: "Quit Claudiu?".into(),
            body: body.join(" "),
            confirm: "Quit anyway".into(),
            danger: true,
            act: Act::ConfirmQuit,
        });
        window.focus(&self.modal_focus, cx);
        cx.notify();
        false
    }

    /// The sessions that are open now, in order.
    fn open_ids(&self, cx: &App) -> Vec<Id> {
        self.live.iter().filter(|l| self.store.session(&l.id).is_some_and(|r| r.kind == SessionKind::Claude) && self.is_running(&l.id, cx)).map(|l| l.id.clone()).collect()
    }

    /// Remember what is open, so the next start brings it back (also while running, in case Claudiu does not get to
    /// quit). It has its own file: another Claudiu sharing the data folder rewrites `state.json` without knowing this
    /// list.
    pub(super) fn remember_open(&mut self, cx: &App) {
        if !self.restored || self.quitting {
            return;
        }
        let ids = self.open_ids(cx);
        if self.remembered != ids {
            let _ = datadir::write_atomic(&running_file(), &serde_json::json!(ids).to_string());
            self.remembered = ids;
        }
    }

    /// Bring back what was open when Claudiu last ended, then show what was on screen. (Older versions also listed
    /// chats here; those ids match no session and are skipped.)
    pub(super) fn restore_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved: Vec<Id> = std::fs::read_to_string(running_file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let shown = self.selected.clone();
        let mut back = vec![];
        for id in saved {
            if self.store.session(&id).is_some_and(|s| s.kind == SessionKind::Claude && !s.hidden) {
                self.resume(&id, window, cx);
                back.push(id);
            }
        }
        if let Some(id) = shown.filter(|s| back.contains(s)) {
            self.show_session(&id, window, cx);
        }
        self.restored = true;
        self.remember_open(cx);
    }

    pub fn finalize(&mut self, cx: &App) {
        self.remember_open(cx);
        self.quitting = true;
        let t = now();
        let running: Vec<Id> = self.live.iter().map(|l| l.id.clone()).collect();
        for id in running {
            if let Some(rec) = self.store.session_mut(&id)
                && rec.exit_code.is_none() {
                    rec.exit_code = Some(-1);
                    rec.last_active = t;
                }
        }
        // Terminal sessions are never persisted, nor are ephemeral ones.
        let ephemeral = &self.ephemeral;
        self.store
            .data
            .sessions
            .retain(|s| s.kind != SessionKind::Shell && !ephemeral.contains(&s.id));
        self.store.save();
    }
}
