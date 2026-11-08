//! The right sidebar: the shown session's actions and status, or (one click away) the project's GitHub view.
//! It sits under the top bar so the window buttons stay in the corner.

use std::path::PathBuf;

use rune::tok::motion;
use rune::{Color, Cursor, El, Phase, ScrollSpec, Transition, Tx, col, el, row as hbox};

use crate::app::{Act, GitOp, MenuAnchor, MenuItem, SideDrag, Workspace};
use crate::github::model::State;
use crate::panels::icons;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::{button, countdown, icon, icon_button, key_hint, label, meter, section_label, tick};
use crate::platform::Editor;
use crate::store::{Attached, Id, Project, SessionKind, SessionRecord};

/// What the top bar and the right sidebar describe: the shown session and the folder and project it is in.
pub struct Shown {
    pub rec: Option<SessionRecord>,
    pub cwd: Option<PathBuf>,
    pub project: Option<Project>,
}

fn act(ws: &mut Workspace, a: Act, tx: &mut Tx) {
    ws.act(a);
    ws.after_input(tx);
}

impl Workspace {
    pub fn shown(&self) -> Shown {
        let rec = self.active_id().and_then(|id| self.store.session(id)).cloned();
        let project = rec.as_ref().and_then(|r| r.project_id.clone()).and_then(|p| self.store.project(&p)).cloned();
        let cwd = rec.as_ref().map(|r| r.cwd.clone());
        Shown { rec, cwd, project }
    }

    /// The right sidebar. It slides open and shut, and its width springs between the menu's and GitHub's; the two
    /// views cross-fade.
    pub fn render_right(&self) -> El {
        let open = !self.store.data.layout.right_hidden;
        let github = self.right_gh.as_ref().and_then(|id| self.gh_pane(id));
        let width = self.right_width_of(github.is_some(), self.window_size.0);
        let body = match github {
            Some(p) => col()
                .key(format!("right-gh-{}", p.id))
                .w(width)
                .h_full()
                .transition(Transition::opacity())
                .child(
                    hbox()
                        .h(32.0)
                        .w_full()
                        .px(6.0)
                        .gap(6.0)
                        .center_y()
                        .border_sides([0.0, 0.0, 1.0, 0.0], rgb(theme::BORDER))
                        .child(icon_button(&icons::ARROW_LEFT).key("right-back").help("Back").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| {
                            ws.right_gh = None;
                            ws.focus_active();
                            ws.after_input(tx);
                        }))
                        .child(label(format!("GitHub · {}", p.project), 12.5, theme::TEXT).truncate().shrink()),
                )
                .child(col().grow(1.0).shrink().min_h(0.0).w_full().child(p.view.render(width))),
            None => self.render_right_menu().key("right-menu").w(width).h_full().transition(Transition::opacity()),
        };
        // Both views can be resized by their left edge; a small grip sits in the gutter.
        let handle = hbox()
            .key("right-resize")
            .pin(Some(-(theme::PANEL_GAP / 2.0 + 3.0)), Some(0.0), None, Some(0.0))
            .w(6.0)
            .center()
            .cursor(Cursor::ResizeColumn)
            .hover_group()
            .on_drag(|ws: &mut Workspace, d: &mut rune::Drag, tx: &mut Tx| {
                match d.phase {
                    Phase::Down => {
                        let wide = ws.right_gh.is_some();
                        ws.right_drag = Some(SideDrag { size: ws.right_width_of(wide, ws.window_size.0) });
                    }
                    Phase::Move => ws.drag_right(d.dx),
                    Phase::Up => ws.right_drag = None,
                }
                ws.after_input(tx);
            })
            .child(el().w(2.0).h(28.0).radius(1.0).fill(rgb(0x2b2b30)).group_color(rgb(0x4a4a52)));
        let panel = col()
            .key("right-panel")
            .w(width)
            .h_full()
            .radius(theme::PANEL_RADIUS)
            .clip_rounded(theme::PANEL_RADIUS)
            .border(1.0, rgb(theme::PANEL_BORDER))
            .fill(rgb(theme::SIDEBAR_BG))
            .motion(motion::GLIDE)
            .child(body);
        let mut outer = hbox()
            .key("right-sidebar")
            .h_full()
            .w(if open { width + theme::PANEL_GAP } else { 0.0 })
            .clip_rounded(0.0)
            .justify_end()
            .motion(motion::GLIDE)
            .child(col().key("right-inner").h_full().pl(theme::PANEL_GAP).opacity(if open { 1.0 } else { 0.0 }).motion(motion::GLIDE).child(panel).child(handle));
        if self.right_drag.is_some() {
            outer = outer.instant_changes();
        }
        outer
    }

    fn render_right_menu(&self) -> El {
        let shown = self.shown();
        let id = self.active_id().cloned();
        let running = id.as_ref().is_some_and(|id| self.is_running(id));
        let mut menu = col().w_full().gap(10.0).padding(6.0);
        if let Some((from, to_name)) = id.as_deref().and_then(|i| self.session_away(i)) {
            // Fades in when the session turns out to be on another account.
            let body = col()
                .key("handoff")
                .gap(6.0)
                .px(8.0)
                .transition(Transition::opacity())
                .child(label(format!("This session runs on {from}."), 12.0, theme::WARN).wrap())
                .child(button(format!("Hand off to {to_name}"), true).key("handoff-session").on_press_tx(|ws: &mut Workspace, tx: &mut Tx| act(ws, Act::Handoff, tx)));
            menu = menu.child(group("ACCOUNT", body));
        }
        menu = menu.child(group("ACTIONS", self.action_rows(&shown, id.as_ref(), running)));
        let attached = id.as_deref().map(|i| self.attached_of(i)).unwrap_or_default();
        if !attached.is_empty() {
            menu = menu.child(group("GITHUB", self.attached_rows(id.as_ref(), attached)));
        }
        let links = id.as_deref().map(|i| self.links_of(i)).unwrap_or_default();
        if !links.is_empty() {
            menu = menu.child(group("LINKED", self.linked_rows(id.as_ref(), links)));
        }
        menu = menu.child(group("STATUS", self.status_rows(&shown, id.as_ref(), running)));
        col().scroll(ScrollSpec { stick_bottom: false }).child(menu)
    }

    /// Open in the editor, the folder, git, the shell and the GitHub view.
    fn action_rows(&self, shown: &Shown, id: Option<&Id>, running: bool) -> El {
        let claude = shown.rec.as_ref().is_some_and(|r| r.kind == SessionKind::Claude);
        let target = shown.project.as_ref().map(|p| p.path.clone()).or_else(|| shown.cwd.clone());
        let repo = shown.project.as_ref().filter(|p| p.git.is_some()).and(shown.cwd.clone());
        let busy = self.git_busy;
        let hints = self.shortcuts_shown;

        let mut actions = col().w_full().gap(1.0);
        if let Some(target) = &target {
            // "Open in <editor>" plus a ▾ to pick another installed editor (remembered, see Act::OpenEditor).
            if let Some(editor) = self.preferred_editor() {
                let (t, pick_target) = (target.clone(), target.clone());
                actions = actions.child(
                    hbox()
                        .w_full()
                        .center_y()
                        .child(row("open-in", &icons::EXTERNAL_LINK, format!("Open in {}", editor_name(editor))).grow(1.0).shrink().on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, Act::OpenEditor(editor, t.clone()), tx)))
                        .child(icon_button(&icons::CHEVRON_DOWN).key("open-in-pick").help("Open in…").on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
                            let current = ws.preferred_editor();
                            let items = ws.editors.iter().filter(|(_, found)| *found).map(|(e, _)| MenuItem::new(format!("{}{}", tick(current == Some(*e)), e.label()), Act::OpenEditor(*e, pick_target.clone()))).collect();
                            ws.open_menu(MenuAnchor::Key("open-in-pick".into()), 0.0, items);
                            ws.after_input(tx);
                        })),
                );
            }
            let t = target.clone();
            actions = actions.child(row("reveal", &icons::FOLDER_OPEN, "Open folder").on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, Act::Reveal(t.clone()), tx)));
        }
        if let Some(repo) = repo {
            // One git operation at a time: while one runs, both rows are disabled and the running one turns.
            let git_row = |key: &'static str, glyph: &'static rune::Icon, text: &'static str, doing: &'static str, op: GitOp, make: Box<dyn Fn(PathBuf) -> Act>| {
                if busy == Some(op) {
                    dim_row(key, glyph, doing).child(crate::panels::widgets::spinner(12.0, theme::TEXT_FAINT))
                } else if busy.is_some() {
                    dim_row(key, glyph, text)
                } else {
                    let repo = repo.clone();
                    row(key, glyph, text).on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, make(repo.clone()), tx))
                }
            };
            // The commit message is written through the running session's mod, so it needs one.
            // Until it has been read, assume there is work.
            let nothing_to_do = self.pending.as_ref().is_some_and(|(dir, p)| *dir == repo && !p.any());
            let commit = match id.filter(|_| running && claude) {
                Some(_) if nothing_to_do && busy.is_none() => dim_row("commit-push", &icons::GIT_COMMIT_HORIZONTAL, "Commit & Push · nothing to commit or push"),
                Some(id) => {
                    let id = id.clone();
                    git_row("commit-push", &icons::GIT_COMMIT_HORIZONTAL, "Commit & Push", "Committing…", GitOp::CommitPush, Box::new(move |cwd| Act::CommitPush(id.clone(), cwd)))
                }
                None => dim_row("commit-push", &icons::GIT_COMMIT_HORIZONTAL, "Commit & Push · needs a running session"),
            };
            actions = actions.child(commit).child(git_row("pull", &icons::ARROW_DOWN_TO_LINE, "Pull", "Pulling…", GitOp::Pull, Box::new(Act::Pull)));
        }
        if let (Some(id), true, true) = (id.cloned(), claude, running) {
            let open = self.shells.get(&id).is_some_and(|p| p.open);
            let mut r = row("shell", &icons::TERMINAL, if open { "Hide shell" } else { "Show shell" });
            if hints {
                r = r.child(key_hint("J").key("hint-shell-row"));
            }
            actions = actions.child(r.on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, Act::ToggleShell(id.clone()), tx)));
        }
        let cwd = self.active_cwd();
        actions.child(if id.is_some() {
            let mut r = row("github", &icons::GIT_PULL_REQUEST, "GitHub");
            if hints {
                r = r.child(key_hint("Shift+G").key("hint-github"));
            }
            r.on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| act(ws, Act::OpenGithub { cwd: cwd.clone() }, tx))
        } else {
            dim_row("github", &icons::GIT_PULL_REQUEST, "GitHub")
        })
    }

    /// Issues and pull requests attached to the session: click to open, × to take off.
    fn attached_rows(&self, id: Option<&Id>, attached: Vec<Attached>) -> El {
        let mut rows = col().w_full().gap(1.0);
        for a in attached {
            let (pull, number) = (a.pull, a.number);
            let owner = id.cloned();
            let key = format!("attached-{pull}-{number}");
            let title = match (a.title, a.state) {
                (None, _) => pending_title(number),
                (Some(t), Some(State::Closed)) => label(format!("#{number}  {t} (closed)"), 12.5, theme::TEXT_DIM).truncate().shrink(),
                (Some(t), Some(State::Merged)) => label(format!("#{number}  {t} (merged)"), 12.5, theme::TEXT_DIM).truncate().shrink(),
                (Some(t), _) => label(format!("#{number}  {t}"), 12.5, theme::TEXT).truncate().shrink(),
            };
            rows = rows.child(removable_row(
                key,
                if pull { &icons::GIT_PULL_REQUEST } else { &icons::CIRCLE_DOT },
                title,
                move |ws| ws.open_attached(pull, number),
                move |ws| {
                    if let Some(id) = &owner {
                        ws.detach(id, pull, number);
                    }
                },
            ));
        }
        rows
    }

    /// Sessions linked with this one: click to go there, × to unlink.
    fn linked_rows(&self, id: Option<&Id>, links: Vec<(Id, String)>) -> El {
        let mut rows = col().w_full().gap(1.0);
        for (other, title) in links {
            let (me, go, gone) = (id.cloned(), other.clone(), other.clone());
            rows = rows.child(removable_row(
                format!("linked-{other}"),
                &icons::BRACKETS,
                label(title, 12.5, theme::TEXT).truncate().shrink(),
                move |ws| ws.act(Act::Select(go.clone())),
                move |ws| {
                    if let Some(me) = &me {
                        ws.unlink(me, &gone);
                    }
                },
            ));
        }
        rows
    }

    /// Context and cache meters, and the settings Claude Code keeps (menus while the session runs).
    fn status_rows(&self, shown: &Shown, id: Option<&Id>, running: bool) -> El {
        let (ctx_fraction, ctx_caption) = match self.selected_context() {
            Some((f, tokens, true, Some(full))) => (Some(f), format!("{}k of {}k tokens", tokens / 1000, full / 1000)),
            Some((f, tokens, true, None)) => (Some(f), format!("{}k tokens", tokens / 1000)),
            Some((f, tokens, false, _)) => (Some(f), format!("~{}k tokens (estimate)", tokens / 1000)),
            None => (None, "no session".to_string()),
        };
        // Fills with time left; goes red in the last fifth. Expired means the next message re-reads the whole context at full price.
        let cache = match self.selected_cache() {
            Some((_, 0, _)) => meter("meter-cache", "CACHE", Some(0.0), "expired".into(), None),
            Some((f, left, ttl)) => {
                let tone = rgb(if f < 0.2 { theme::DANGER } else { theme::ACCENT });
                let window = if ttl >= 3600 { "1h" } else { "5m" };
                let left = if left < 60 { "<1m".to_string() } else { countdown(left) };
                meter("meter-cache", "CACHE", Some(f), format!("{left} left of {window}"), Some(tone))
            }
            None => meter("meter-cache", "CACHE", None, "no session".into(), None),
        };

        // The figures come from the snapshot the session's mod reports.
        let meta = shown.rec.as_ref().filter(|r| r.kind == SessionKind::Claude).and_then(|r| r.claude.as_ref());
        let (compact, precompute, advisor, active, chrome, known) = match meta {
            Some(m) => (m.auto_compact, m.precompute_compact, m.advisor.clone(), m.advisor_active.clone(), m.chrome, true),
            None => (None, None, None, None, false, false),
        };
        let advisor = advisor.map(|a| a.to_lowercase());
        let active = active.map(|a| a.to_lowercase());
        let compact_text = compact.map_or("—", |v| if v { "On" } else { "Off" }).to_string();
        let advisor_text = if known { advisor_display(advisor.as_deref(), active.as_deref()) } else { "—".into() };
        let settable = id.filter(|_| meta.is_some() && running).cloned();

        let compact_row = match settable.clone() {
            Some(sid) => pick_row("set-auto-compact", "Auto-compact", compact_text, move || auto_compact_menu(&sid, compact)),
            None => kv("Auto-compact", compact_text),
        };
        let mut settings = col().w_full().gap(2.0).child(compact_row);
        // Only while Claude Code offers the setting; without its feature flag the setting does nothing.
        if let Some(on) = precompute {
            let t = if on { "On" } else { "Off" }.to_string();
            settings = settings.child(match settable.clone() {
                Some(sid) => pick_row("set-precompute-compact", "Background compaction", t, move || precompute_compact_menu(&sid, on)),
                None => kv("Background compaction", t),
            });
        }
        let advisor_row = match settable.clone() {
            Some(sid) => pick_row("set-advisor", "Advisor", advisor_text, move || advisor_menu(&sid, advisor.as_deref())),
            None => kv("Advisor", advisor_text),
        };
        let chrome_row = match settable {
            Some(sid) => pick_row("set-chrome", "Claude in Chrome", if chrome { "On" } else { "Off" }.into(), move || chrome_menu(&sid, chrome)),
            None => kv("Claude in Chrome", if chrome { "On" } else { "Off" }.into()),
        };
        col()
            .w_full()
            .gap(10.0)
            .px(8.0)
            .child(meter("meter-context", "CONTEXT", ctx_fraction, ctx_caption, None))
            .child(cache)
            .child(settings.child(advisor_row).child(chrome_row))
    }
}

fn auto_compact_menu(sid: &Id, current: Option<bool>) -> Vec<MenuItem> {
    [(true, "On"), (false, "Off")].into_iter().map(|(on, name)| MenuItem::new(format!("{}{name}", tick(current == Some(on))), Act::SetAutoCompact(sid.clone(), on))).collect()
}

fn precompute_compact_menu(sid: &Id, current: bool) -> Vec<MenuItem> {
    [(true, "On"), (false, "Off")].into_iter().map(|(on, name)| MenuItem::new(format!("{}{name}", tick(current == on)), Act::SetPrecomputeCompact(sid.clone(), on))).collect()
}

/// Off, the two aliases, and "Other…" (ticked for any other model) which asks for a name.
fn chrome_menu(sid: &Id, current: bool) -> Vec<MenuItem> {
    [(true, "On"), (false, "Off")].into_iter().map(|(on, name)| MenuItem::new(format!("{}{name}", tick(current == on)), Act::SetChrome(sid.clone(), on))).collect()
}

fn advisor_menu(sid: &Id, current: Option<&str>) -> Vec<MenuItem> {
    const ALIASES: [(&str, &str); 2] = [("opus", "Opus"), ("sonnet", "Sonnet")];
    let current = current.and_then(advisor_alias);
    let other = current.is_some_and(|a| !ALIASES.iter().any(|(k, _)| *k == a));
    let mut items = vec![MenuItem::new(format!("{}Off", tick(current.is_none())), Act::SetAdvisor(sid.clone(), "off".into()))];
    items.extend(ALIASES.iter().map(|(alias, name)| MenuItem::new(format!("{}{name}", tick(current == Some(*alias))), Act::SetAdvisor(sid.clone(), (*alias).into()))));
    items.push(MenuItem::new(format!("{}Other…", tick(other)), Act::AskAdvisor(sid.clone())));
    items
}

/// Claude Code may store either an alias (`opus`) or the resolved model id (`claude-opus-5-5`); the menu and the
/// value both speak aliases, so map the id back to the family it belongs to.
fn advisor_alias(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("off") {
        return None;
    }
    let lower = value.to_ascii_lowercase();
    if lower == "opus" || lower.contains("opus") {
        Some("opus")
    } else if lower == "sonnet" || lower.contains("sonnet") {
        Some("sonnet")
    } else {
        Some(value)
    }
}

/// `Opus`, or `Opus → Sonnet` while the saved setting waits for `/clear` or `/compact`.
fn advisor_display(saved: Option<&str>, active: Option<&str>) -> String {
    let Some(saved) = saved.and_then(advisor_alias) else { return "Off".into() };
    let active = active.and_then(advisor_alias).unwrap_or(saved);
    if active == saved { advisor_name(active) } else { format!("{} → {}", advisor_name(active), advisor_name(saved)) }
}

fn advisor_name(alias: &str) -> String {
    match alias {
        "opus" => "Opus".into(),
        "sonnet" => "Sonnet".into(),
        other => other.to_string(),
    }
}

fn group(title: &str, body: El) -> El {
    col().w_full().gap(4.0).child(el().px(8.0).child(section_label(title.to_string()))).child(body)
}

/// `#12` with a grey bar where the title will be, until GitHub answers.
fn pending_title(number: u32) -> El {
    hbox().center_y().gap(8.0).child(label(format!("#{number}"), 12.5, theme::TEXT)).child(el().h(8.0).w(96.0).radius(4.0).fill(rgb(0x1f1f21)))
}

fn base_row(key: &str, glyph: &'static rune::Icon, title: impl Into<RowLabel>, color: u32) -> El {
    let title = match title.into() {
        RowLabel::Text(t) => label(t, 12.5, color).truncate().shrink(),
        RowLabel::El(e) => *e,
    };
    hbox()
        .key(key.to_string())
        .h(26.0)
        .w_full()
        .px(8.0)
        .gap(8.0)
        .center_y()
        .radius(6.0)
        .child(icon(glyph, 14.0, theme::TEXT_DIM))
        .child(hbox().grow(1.0).shrink().min_w(0.0).child(title))
}

/// What a row says: plain text, or a drawn label (a pending title).
enum RowLabel {
    Text(String),
    El(Box<El>),
}

impl From<&str> for RowLabel {
    fn from(s: &str) -> Self {
        RowLabel::Text(s.to_string())
    }
}

impl From<String> for RowLabel {
    fn from(s: String) -> Self {
        RowLabel::Text(s)
    }
}

impl From<El> for RowLabel {
    fn from(e: El) -> Self {
        RowLabel::El(Box::new(e))
    }
}

fn row(key: &str, glyph: &'static rune::Icon, title: impl Into<RowLabel>) -> El {
    base_row(key, glyph, title, theme::TEXT).cursor(Cursor::Pointer).role("button").hover(Color::WHITE.alpha(0.04)).press_scale(0.985)
}

/// The row as it looks while its action is unavailable: no hover, no pointer (give it no click handler either).
fn dim_row(key: &str, glyph: &'static rune::Icon, title: impl Into<RowLabel>) -> El {
    base_row(key, glyph, title, theme::TEXT_FAINT).disabled(true)
}

/// A list row that does something when clicked and has a × that removes the entry. It fades in when it arrives and
/// out when removed, while the rows below slide up.
fn removable_row(key: String, glyph: &'static rune::Icon, title: El, open: impl Fn(&mut Workspace) + 'static, remove: impl Fn(&mut Workspace) + 'static) -> El {
    let x = format!("remove-{key}");
    row(&key, glyph, title)
        .hover_group()
        .motion(motion::REFLOW)
        .transition(Transition::opacity().combined(Transition::offset(10.0, 0.0)))
        .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            open(ws);
            ws.after_input(tx);
        })
        .child(icon_button(&icons::X).key(x).group_opacity(0.0, 1.0).help("Remove").on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            remove(ws);
            ws.after_input(tx);
        }))
}

/// A setting row that opens a menu (built when it is clicked): the value with a chevron, lit on hover.
fn pick_row(key: &'static str, title: &str, value: String, menu: impl Fn() -> Vec<MenuItem> + 'static) -> El {
    hbox()
        .key(key)
        .w_full()
        .h(22.0)
        .mx(-6.0)
        .px(6.0)
        .gap(10.0)
        .center_y()
        .justify_between()
        .radius(5.0)
        .cursor(Cursor::Pointer)
        .role("button")
        .hover(Color::WHITE.alpha(0.04))
        .child(label(title.to_string(), 12.0, theme::TEXT_DIM))
        .child(hbox().center_y().gap(4.0).shrink().child(label(value, 12.0, theme::TEXT).truncate().shrink()).child(icon(&icons::CHEVRON_DOWN, 12.0, theme::TEXT_FAINT)))
        .on_press_tx(move |ws: &mut Workspace, tx: &mut Tx| {
            ws.open_menu(MenuAnchor::Key(key.into()), 0.0, menu());
            ws.after_input(tx);
        })
}

fn kv(title: &str, value: String) -> El {
    hbox().w_full().h(22.0).gap(10.0).center_y().justify_between().child(label(title.to_string(), 12.0, theme::TEXT_DIM)).child(label(value, 12.0, theme::TEXT).truncate().shrink())
}

fn editor_name(editor: Editor) -> &'static str {
    match editor {
        Editor::VsCode => "VS Code",
        Editor::Zed => "Zed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(items: &[MenuItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn the_setting_menus_tick_the_current_value() {
        let sid = "s".to_string();
        assert_eq!(labels(&auto_compact_menu(&sid, Some(false))), ["    On", "✓  Off"]);
        assert_eq!(labels(&auto_compact_menu(&sid, None)), ["    On", "    Off"]);
        assert_eq!(labels(&precompute_compact_menu(&sid, true)), ["✓  On", "    Off"]);
        assert_eq!(labels(&advisor_menu(&sid, None)), ["✓  Off", "    Opus", "    Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("sonnet"))), ["    Off", "    Opus", "✓  Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("claude-opus-5-5"))), ["    Off", "✓  Opus", "    Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("claude-sonnet-4-5-20250929"))), ["    Off", "    Opus", "✓  Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("off"))), ["✓  Off", "    Opus", "    Sonnet", "    Other…"]);
        assert_eq!(labels(&advisor_menu(&sid, Some("my-model"))), ["    Off", "    Opus", "    Sonnet", "✓  Other…"]);
        assert_eq!(advisor_display(None, None), "Off");
        assert_eq!(advisor_display(Some("sonnet"), Some("sonnet")), "Sonnet");
        assert_eq!(advisor_display(Some("sonnet"), Some("claude-opus-5-5")), "Opus → Sonnet");
        assert_eq!(advisor_display(Some("opus"), None), "Opus");
    }
}
