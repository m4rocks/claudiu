//! Universal anchored dropdown picker shared across m4code (model, project, branch & worktree, permission mode,
//! effort, skills scope, and editor selection). Uses the compact model picker as its visual base and stems directly
//! from the button that opened it without dimming the rest of the window. The rows inside are `ui::command`'s.

use gpui::{
    Anchor as Corner, AnyElement, ClickEvent, Context, Div, ElementId, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Pixels,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, point, prelude::FluentBuilder, px,
};
use gpui_kit::component::input::{Input, InputState};

use crate::panels::scale;
use crate::panels::theme::theme;
use crate::panels::ui::command::Choices;
use crate::panels::widgets::{Dropdown, fade_in};
use crate::panels::ui::kbd::kbd;
use crate::panels::ui::label::section_label;
use crate::panels::ui::popover;

/// Default width of every universal picker dropdown.
pub const WIDTH: f32 = 348.0;

/// Anchored dropdown popover stemming directly from the trigger button, with a transparent click-outside dismissal
/// layer (no dimmed backdrop), uniform header (`TITLE` + `Esc close`), and Escape handling even while searching.
/// Automatically flips above/below the trigger if the preferred side lacks room, so it never covers its button.
#[allow(clippy::too_many_arguments)]
pub fn dropdown_overlay<V: 'static>(
    (id, title): (&'static str, impl Into<SharedString>),
    at: Dropdown,
    priority: usize,
    body: AnyElement,
    choices: Option<Choices<V>>,
    arrow_nav: popover::ArrowNav,
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
        .flex_none()
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
                .child(kbd("Esc"))
                .child(div().text_size(scale::px(10.5)).text_color(theme().subtle_foreground).child("close")),
        );

    let hit = at.hit;
    let card = popover::content(id, arrow_nav)
        .w(px(WIDTH).max(at.min_w))
        .when_some(max_card_h, |d, h| d.max_h(h).overflow_hidden())
        .p(px(11.0))
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
        }));
    let nav_choices = if arrow_nav.is_enabled() { choices } else { None };
    let card = keyboard(card, nav_choices, move |v, window, cx| close_key(v, window, cx), cx)
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

/// The keys of a dropdown `frame`: Esc runs `on_close`, and Up, Down and Enter move through and choose `choices`.
/// The frame holds the list's focus, so the keys arrive when the dropdown has no search box.
pub fn keyboard<V: 'static>(
    frame: Stateful<Div>,
    choices: Option<Choices<V>>,
    on_close: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    frame.when_some(choices.as_ref().map(|c| c.nav().focus_handle().clone()), |d, focus| d.track_focus(&focus)).capture_key_down(cx.listener(
        move |v, ev: &KeyDownEvent, window, cx| {
            let plain = !ev.keystroke.modifiers.modified();
            if ev.keystroke.key == "escape" {
                on_close(v, window, cx);
                cx.stop_propagation();
                cx.notify();
            } else if plain && choices.as_ref().is_some_and(|c| c.key(&ev.keystroke.key, v, window, cx)) {
                cx.stop_propagation();
            }
        },
    ))
}

/// Uniform search input at the top of a filterable picker dialog.
pub fn search_input(input: &gpui::Entity<InputState>) -> impl IntoElement {
    div().flex_none().child(Input::new(input).h(px(28.0)).text_size(scale::px(12.0)))
}

/// Horizontal divider between sections inside a picker dialog.
pub fn divider() -> Div {
    div().flex_none().h(px(1.0)).w_full().bg(theme().border)
}

/// Sub-section header inside a multi-section picker dialog (e.g. WORKTREES / BRANCHES).
pub fn section_header(title: impl Into<SharedString>, trailing: Option<AnyElement>) -> Div {
    div()
        .flex_none()
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
        .bg(theme().background)
        .border_1()
        .border_color(theme().panel_border)
        .hover(|s| s.bg(theme().accent).text_color(theme().foreground))
        .flex()
        .items_center()
        .cursor_pointer()
        .text_size(scale::px(11.0))
        .text_color(theme().muted_foreground)
        .child(label.into())
        .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| on_click(v, window, cx)))
}

/// Consistent empty-state text inside a picker list.
pub fn empty_note(text: impl Into<SharedString>) -> Div {
    div()
        .px(px(8.0))
        .py(px(8.0))
        .text_size(scale::px(11.5))
        .text_color(theme().subtle_foreground)
        .child(text.into())
}
