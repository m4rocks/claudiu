//! Universal anchored dropdown picker shared across Claudiu (model, project, branch & worktree, permission mode,
//! effort, skills scope, and editor selection). Uses the compact model picker as its visual base and stems directly
//! from the button that opened it without dimming the rest of the window.

use gpui::{
    Anchor as Corner, AnyElement, ClickEvent, Context, Div, ElementId, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Pixels, SharedString,
    Stateful, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, point, prelude::FluentBuilder, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputState};

use crate::panels::scale;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{Dropdown, fade_in, icon, key_badge, section_label};

/// Default width of every universal picker dropdown.
pub const WIDTH: f32 = 348.0;

/// Anchored dropdown popover stemming directly from the trigger button, with a transparent click-outside dismissal
/// layer (no dimmed backdrop), uniform header (`TITLE` + `Esc close`), and Escape handling even while searching.
/// Automatically flips above/below the trigger if the preferred side lacks room, so it never covers its button.
pub fn dropdown_overlay<V: 'static>(
    (id, title): (&'static str, impl Into<SharedString>),
    at: Dropdown,
    priority: usize,
    body: AnyElement,
    on_close: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    window: &Window,
    cx: &mut Context<V>,
) -> AnyElement {
    let on_close = std::rc::Rc::new(on_close);
    let close_key = on_close.clone();
    let close_out = on_close;

    let viewport = window.viewport_size();
    let mut pos = at.pos;
    let mut anchor = at.anchor;
    let mut max_card_h: Option<Pixels> = None;
    if at.trigger.size.height > px(0.0) {
        let gap = px(4.0);
        let margin = px(10.0);
        let space_below = (viewport.height - at.trigger.bottom() - gap - margin).max(px(120.0));
        let space_above = (at.trigger.top() - gap - margin).max(px(120.0));
        let opens_below = matches!(anchor, Corner::TopLeft | Corner::TopRight);
        if opens_below {
            if space_below < px(260.0) && space_above > space_below {
                anchor = match anchor {
                    Corner::TopRight => Corner::BottomRight,
                    _ => Corner::BottomLeft,
                };
                pos = match anchor {
                    Corner::BottomRight => point(at.trigger.right(), at.trigger.top() - gap),
                    _ => point(at.trigger.left(), at.trigger.top() - gap),
                };
                max_card_h = Some(space_above);
            } else {
                max_card_h = Some(space_below);
            }
        } else if space_above < px(260.0) && space_below > space_above {
            anchor = match anchor {
                Corner::BottomRight => Corner::TopRight,
                _ => Corner::TopLeft,
            };
            pos = match anchor {
                Corner::TopRight => point(at.trigger.right(), at.trigger.bottom() + gap),
                _ => point(at.trigger.left(), at.trigger.bottom() + gap),
            };
            max_card_h = Some(space_below);
        } else {
            max_card_h = Some(space_above);
        }
    }

    let header = div()
        .px(px(2.0))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .child(section_label(title))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(4.0))
                .child(key_badge("Esc"))
                .child(div().text_size(scale::px(10.5)).text_color(hsla(theme::TEXT_FAINT)).child("close")),
        );

    let hit = at.hit;
    let card = div()
        .id(id)
        .occlude()
        .cursor_default()
        .w(px(WIDTH).max(at.min_w))
        .when_some(max_card_h, |d, h| d.max_h(h).overflow_hidden())
        .p(px(11.0))
        .rounded(px(10.0))
        .bg(hsla(theme::SURFACE_ELEVATED))
        .border_1()
        .border_color(hsla(theme::PANEL_BORDER))
        .shadow_lg()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_down_out(cx.listener(move |v, ev: &MouseDownEvent, window, cx| {
            close_out(v, window, cx);
            // Clicking the trigger that owns this dropdown toggles it closed instead of reopening in bubble phase;
            // clicking any other button lets the event reach that button so its dropdown opens immediately.
            if hit.contains(&ev.position) {
                cx.stop_propagation();
            }
            cx.notify();
        }))
        .capture_key_down(cx.listener(move |v, ev: &KeyDownEvent, window, cx| {
            if ev.keystroke.key == "escape" {
                close_key(v, window, cx);
                cx.stop_propagation();
                cx.notify();
            }
        }))
        .child(header)
        .child(body);

    deferred(
        anchored()
            .anchor(anchor)
            .position(pos)
            .snap_to_window_with_margin(px(8.0))
            .child(fade_in(card, format!("{id}-in"), 110, 0.0)),
    )
    .with_priority(priority)
    .into_any_element()
}

/// Uniform search input at the top of a filterable picker dialog.
pub fn search_input(input: &gpui::Entity<InputState>) -> impl IntoElement {
    Input::new(input).h(px(28.0)).text_size(scale::px(12.0))
}

/// Horizontal divider between sections inside a picker dialog.
pub fn divider() -> Div {
    div().h(px(1.0)).w_full().bg(hsla(theme::BORDER))
}

/// Sub-section header inside a multi-section picker dialog (e.g. WORKTREES / BRANCHES).
pub fn section_header(title: impl Into<SharedString>, trailing: Option<AnyElement>) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .child(section_label(title))
        .children(trailing)
}

/// Compact inline action pill in a picker section header or footer (e.g. `+ New branch`, `+ Add project`).
pub fn action_pill<V: 'static>(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    on_click: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    div()
        .id(id)
        .px(px(7.0))
        .h(px(20.0))
        .rounded(px(5.0))
        .bg(hsla(theme::PANEL_BG))
        .border_1()
        .border_color(hsla(theme::PANEL_BORDER))
        .hover(|s| s.bg(hsla(theme::SURFACE_HOVER)).text_color(hsla(theme::TEXT)))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(scale::px(11.0))
        .text_color(hsla(theme::TEXT_DIM))
        .child(label.into())
        .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| on_click(v, window, cx)))
}

/// Uniform selectable row inside a picker dialog.
pub fn row(
    id: impl Into<ElementId>,
    leading: Option<AnyElement>,
    label: impl Into<SharedString>,
    detail: Option<String>,
    trailing: Option<AnyElement>,
    selected: bool,
    disabled: bool,
) -> Stateful<Div> {
    let accent = hsla(theme::ACCENT);
    let text_color = if selected {
        accent
    } else if disabled {
        hsla(theme::TEXT_FAINT)
    } else {
        hsla(theme::TEXT)
    };
    let title_col = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(1.0))
        .child(
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .font_weight(if selected { FontWeight::MEDIUM } else { FontWeight::NORMAL })
                .child(label.into()),
        )
        .children(detail.map(|d| {
            div()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(scale::px(11.0))
                .text_color(hsla(theme::TEXT_FAINT))
                .child(d)
        }));

    let mut el = div()
        .id(id)
        .px(px(8.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .gap(px(8.0))
        .bg(hsla(if selected { theme::ROW_ACTIVE } else { theme::SURFACE_ELEVATED }))
        .text_size(scale::px(12.5))
        .text_color(text_color)
        .children(leading)
        .child(title_col)
        .children(trailing)
        .when(selected, |d| d.child(icon(IconName::Check, 12.0).text_color(accent)));
    if !disabled {
        el = el.cursor_pointer().hover(|s| s.bg(hsla(theme::SURFACE_HOVER)));
    }
    el
}

/// Consistent empty-state text inside a picker list.
pub fn empty_note(text: impl Into<SharedString>) -> Div {
    div()
        .px(px(8.0))
        .py(px(8.0))
        .text_size(scale::px(11.5))
        .text_color(hsla(theme::TEXT_FAINT))
        .child(text.into())
}

/// Plain-English display name for any harness permission mode id/name (converts camelCase / snake_case / kebab-case).
pub fn format_mode_name(id: &str, raw_name: &str) -> String {
    match id {
        "default" | "manual" => return "Ask Before Editing".into(),
        "acceptEdits" | "accept_edits" | "auto_edit" | "autoEdit" => return "Accept Edits".into(),
        "auto" => return "Auto".into(),
        "dontAsk" | "dont_ask" => return "Don't Ask".into(),
        "plan" => return "Plan Mode".into(),
        "bypassPermissions" | "bypass_permissions" | "yolo" | "fullAccess" | "full_access" | "full-access" => return "Full Access".into(),
        _ => {}
    }
    let src = if raw_name.trim().is_empty() { id } else { raw_name };
    humanize_identifier(src)
}

/// Turn `dontAsk`, `read_only`, `auto-edit`, or `auto` into `"Dont Ask"`, `"Read Only"`, `"Auto Edit"`, `"Auto"`.
fn humanize_identifier(s: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c.is_whitespace() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let prev_lower = i > 0 && chars[i - 1].is_lowercase();
        let next_lower = i + 1 < chars.len() && chars[i + 1].is_lowercase();
        let prev_upper = i > 0 && chars[i - 1].is_uppercase();
        if c.is_uppercase() && (prev_lower || (prev_upper && next_lower)) && !cur.is_empty() {
            words.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
        .into_iter()
        .map(|w| {
            if w.eq_ignore_ascii_case("dont") {
                return "Don't".to_string();
            }
            let mut it = w.chars();
            match it.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(it.flat_map(char::to_lowercase)).collect(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// One-line explanation for a permission mode id (shared by in-chat and Settings permission pickers).
pub fn permission_description(id: &str) -> &'static str {
    match id {
        "default" | "manual" | "ask" | "review" => "Ask before running commands or editing files",
        "acceptEdits" | "accept_edits" | "auto_edit" | "autoEdit" => "Auto-accept file edits; ask before commands",
        "plan" | "read_only" | "readOnly" | "read-only" => "Read-only planning; propose changes first",
        "auto" => "Approve safe edits and commands automatically",
        "dontAsk" | "dont_ask" => "Deny tools not explicitly allowed",
        "bypassPermissions" | "bypass_permissions" | "yolo" | "fullAccess" | "full_access" | "full-access" => "Run all commands and edits without asking",
        _ => "Agent permission mode",
    }
}

#[cfg(test)]
mod tests {
    use super::format_mode_name;

    #[test]
    fn formats_permission_modes_in_plain_english() {
        assert_eq!(format_mode_name("dontAsk", "dontAsk"), "Don't Ask");
        assert_eq!(format_mode_name("auto", "auto"), "Auto");
        assert_eq!(format_mode_name("acceptEdits", "acceptEdits"), "Accept Edits");
        assert_eq!(format_mode_name("bypassPermissions", "bypassPermissions"), "Full Access");
        assert_eq!(format_mode_name("read_only", "read_only"), "Read Only");
        assert_eq!(format_mode_name("agentDecides", "agentDecides"), "Agent Decides");
    }
}
