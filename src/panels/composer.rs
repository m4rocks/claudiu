//! The message box, as the Welcome screen and the chat both draw it: where you type on top, a row of chips (model,
//! mode, project, branch) and actions under it.

use gpui::{AnyElement, Div, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, div, prelude::FluentBuilder, px};
use gpui_kit::assets::IconName;
use gpui_kit::base::Button;
use gpui_kit::component::input::InputEvent;

use crate::harness::Harness;
use crate::panels::{logos, scale};
use crate::panels::theme::theme;
use crate::panels::widgets::{Anchor, button_colored, button_disabled_leading, button_leading, focusable, icon};

pub fn can_send(text: &str) -> bool {
    !text.trim().is_empty()
}

/// Whether a key in the message box sends it. The textarea reports Shift+Enter as a press of Enter too, after it has
/// put the new line in, so only an Enter without Shift sends.
pub fn sends(event: &InputEvent) -> bool {
    matches!(event, InputEvent::PressEnter { shift: false, .. })
}

pub fn send_button(id: impl Into<SharedString>, enabled: bool) -> Button {
    let id = id.into();
    let button = if enabled {
        button_leading(id, IconName::ArrowUp, "Send", true)
    } else {
        button_disabled_leading(id, Some(IconName::ArrowUp), "Send")
    };
    button.h(px(26.0)).px(px(12.0))
}

/// Review changes action button in the composer footer: same height and padding as the Send button, with a clear
/// visual contrast between enabled (emerald-accented active control) and disabled (dimmed flat surface).
pub fn review_button(id: impl Into<SharedString>, enabled: bool) -> Button {
    let id = id.into();
    let button = if enabled {
        button_colored(
            id,
            Some(IconName::FileDiff),
            "Review changes",
            theme().success_subtle,
            theme().success_subtle_foreground,
            theme().success_subtle_hover,
            Some(theme().success.opacity(0.55)),
            false,
        )
        .font_weight(FontWeight::MEDIUM)
    } else {
        button_disabled_leading(id, Some(IconName::FileDiff), "Review changes")
            .bg(theme().background)
            .opacity(0.42)
    };
    button.h(px(26.0)).px(px(12.0))
}

/// The single shared message box used by both the Welcome screen and the Chat view:
/// optional `top` (a waiting question), `input` area, left footer (`model`, `permission`, `git`, plus any
/// context-specific `extra_chips`), and right footer (`extra_actions` + `send`).
pub struct Composer {
    pub lit: bool,
    pub top: Option<AnyElement>,
    pub input: AnyElement,
    pub model: AnyElement,
    pub permission: Option<AnyElement>,
    pub project: Option<AnyElement>,
    pub git: Option<AnyElement>,
    pub extra_chips: Vec<AnyElement>,
    pub extra_actions: Vec<AnyElement>,
    pub send: AnyElement,
}

pub fn render(parts: Composer) -> Div {
    let mut chips = Vec::with_capacity(4 + parts.extra_chips.len());
    chips.push(parts.model);
    if let Some(permission) = parts.permission {
        chips.push(permission);
    }
    if let Some(project) = parts.project {
        chips.push(project);
    }
    if let Some(git) = parts.git {
        chips.push(git);
    }
    chips.extend(parts.extra_chips);

    let mut actions = parts.extra_actions;
    actions.push(parts.send);

    frame(parts.lit, parts.top, parts.input, chips, actions)
}

/// The box: `top` (a question waiting for an answer) above `input`, then `chips` on the left of the footer and
/// `actions` on its right. `lit` draws a brighter border, for focus or a waiting question.
pub fn frame(lit: bool, top: Option<AnyElement>, input: AnyElement, chips: Vec<AnyElement>, actions: Vec<AnyElement>) -> Div {
    div()
        .w_full()
        .rounded(px(14.0))
        .bg(theme().card)
        .border_1()
        .border_color(if lit { theme().ring } else { theme().panel_border })
        .overflow_hidden()
        .flex()
        .flex_col()
        .children(top)
        .child(input)
        .child(
            div()
                .flex()
                .items_end()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(8.0))
                .border_t_1()
                .border_color(theme().border)
                .child(div().flex_1().min_w_0().flex().flex_wrap().items_center().gap(px(6.0)).children(chips))
                .child(div().flex_none().flex().items_center().gap(px(6.0)).children(actions)),
        )
}

/// Padding around the text area inside `frame`.
pub fn input_area() -> Div {
    div().px(px(14.0)).pt(px(12.0)).pb(px(8.0))
}

/// One chip in the footer; `open` while its dropdown is up.
pub fn chip(id: impl Into<ElementId>, open: bool) -> Button {
    focusable(id)
        .h(px(26.0))
        .px(px(8.0))
        .rounded(px(6.0))
        .gap(px(5.0))
        .text_size(scale::px(11.5))
        .bg(if open { theme().row_active } else { theme().background })
        .border_color(if open { theme().ring } else { theme().panel_border })
        .hover(|s| s.bg(theme().accent))
        .cursor_pointer()
}

/// Model + effort chip shared by both composers.
pub fn model_chip(id: impl Into<ElementId>, open: bool, anchor: &Anchor, harness: Harness, label: impl Into<SharedString>, effort: Option<String>) -> Button {
    chip(id, open)
        .relative()
        .child(anchor.probe())
        .child(logos::draw(logos::for_harness(harness), 11.0, theme().muted_foreground))
        .child(div().max_w(px(165.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().line_height(gpui::relative(1.35)).text_color(theme().foreground).child(label.into()))
        .when_some(effort, |d, badge| {
            d.child(div().px(px(5.0)).py(px(1.0)).rounded(px(4.0)).bg(theme().muted).text_size(scale::px(10.0)).line_height(gpui::relative(1.25)).text_color(theme().muted_foreground).child(badge))
        })
        .child(icon(IconName::ChevronDown, 10.5).text_color(theme().subtle_foreground))
}

/// Permission mode chip shared by both composers.
pub fn permission_chip(id: impl Into<ElementId>, open: bool, label: impl Into<SharedString>) -> Button {
    chip(id, open)
        .child(icon(IconName::Lock, 11.0).text_color(theme().muted_foreground))
        .child(div().line_height(gpui::relative(1.35)).text_color(theme().foreground).child(label.into()))
        .child(icon(IconName::ChevronDown, 10.5).text_color(theme().subtle_foreground))
}

/// What the branch chip shows: the branch (or worktree) and, in a linked worktree, its name; a spinner `tail` while
/// git works.
pub fn git_chip(id: impl Into<ElementId>, open: bool, primary: impl Into<SharedString>, linked: Option<String>, tail: Option<AnyElement>) -> Button {
    chip(id, open)
        .text_color(theme().foreground)
        .child(icon(IconName::GitBranch, 12.0).text_color(theme().muted_foreground))
        .child(div().max_w(px(150.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().line_height(gpui::relative(1.35)).child(primary.into()))
        .when_some(linked, |d, wt| {
            d.child(div().text_color(theme().subtle_foreground).child("·"))
                .child(icon(IconName::FolderGit2, 11.0).text_color(theme().subtle_foreground))
                .child(div().max_w(px(100.0)).overflow_hidden().whitespace_nowrap().text_ellipsis().line_height(gpui::relative(1.35)).text_color(theme().muted_foreground).child(wt))
        })
        .child(tail.unwrap_or_else(|| icon(IconName::ChevronDown, 10.5).text_color(theme().subtle_foreground).into_any_element()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_sends_and_shift_enter_does_not() {
        assert!(sends(&InputEvent::PressEnter { secondary: false, shift: false }));
        assert!(sends(&InputEvent::PressEnter { secondary: true, shift: false }), "Cmd/Ctrl+Enter sends too");
        assert!(!sends(&InputEvent::PressEnter { secondary: false, shift: true }), "a new line");
        assert!(!sends(&InputEvent::Change));
    }
}
