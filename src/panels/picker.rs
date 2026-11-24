//! Universal anchored dropdown picker shared across m4code (model, project, branch & worktree, permission mode,
//! effort, skills scope, and editor selection). Uses the compact model picker as its visual base and stems directly
//! from the button that opened it without dimming the rest of the window.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    Anchor as Corner, AnyElement, App, Bounds, ClickEvent, Context, Div, ElementId, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Pixels,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window, anchored, canvas, deferred, div, point, prelude::FluentBuilder, px,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Input, InputState};

use crate::panels::scale;
use crate::panels::theme::{self, hsla};
use crate::panels::widgets::{Dropdown, dropdown_card, fade_in, icon, key_badge, section_label};

/// Default width of every universal picker dropdown.
pub const WIDTH: f32 = 348.0;

/// Up/Down/Enter for a dropdown's list, kept by whoever owns the dropdown and passed to `Choices` each frame.
#[derive(Clone)]
pub struct Nav {
    /// Held by the dropdown's frame. A dropdown with no search box takes it on opening, so its keys reach the list.
    focus: FocusHandle,
    highlight: Rc<Cell<usize>>,
    /// The search text the highlight was set under; new text starts it over at the first row.
    query: Rc<RefCell<String>>,
    /// Where each row was drawn last frame, to scroll the highlighted one into view.
    rows: Rc<RefCell<Vec<Bounds<Pixels>>>>,
    scroll: super::glide::Smooth,
}

impl Nav {
    pub fn new(cx: &mut App) -> Self {
        Self { focus: cx.focus_handle(), highlight: Default::default(), query: Default::default(), rows: Default::default(), scroll: Default::default() }
    }

    /// Start a newly opened dropdown on its first row.
    pub fn reset(&self) {
        self.highlight.set(0);
        self.query.borrow_mut().clear();
    }

    /// Give the keyboard to the dropdown, for one with no search box to type in.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }
}

/// What choosing a row does.
type Choose<V> = Rc<dyn Fn(&mut V, &mut Window, &mut Context<V>)>;

/// The rows a dropdown can choose from, in order, as they are drawn. A click on a row and Enter on the highlighted row
/// run the same `choose`.
pub struct Choices<V> {
    nav: Nav,
    actions: Vec<Choose<V>>,
}

/// Invisible overlay that records a row's last painted bounds into `rows[ix]`, so `reveal_row` can scroll it into view.
pub fn track_row(rows: Rc<RefCell<Vec<Bounds<Pixels>>>>, ix: usize) -> AnyElement {
    canvas(
        move |bounds, _, _| {
            let mut rows = rows.borrow_mut();
            if rows.len() <= ix {
                rows.resize(ix + 1, Bounds::default());
            }
            rows[ix] = bounds;
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// Scroll `scroll` by the smallest amount needed to bring `rows[ix]` fully inside its viewport.
pub fn reveal_row(scroll: &super::glide::Smooth, rows: &[Bounds<Pixels>], ix: usize) {
    let Some(row) = rows.get(ix).copied() else { return };
    if row.size.height <= px(0.0) {
        return;
    }
    let view = scroll.handle().bounds();
    if view.size.height <= px(0.0) {
        return;
    }
    let mut offset = scroll.handle().offset();
    if row.top() < view.top() {
        offset.y += view.top() - row.top();
    } else if row.bottom() > view.bottom() {
        offset.y -= row.bottom() - view.bottom();
    }
    scroll.scroll_to(offset);
}

impl<V: 'static> Choices<V> {
    pub fn new(nav: &Nav, query: &str) -> Self {
        if *nav.query.borrow() != query {
            *nav.query.borrow_mut() = query.to_string();
            nav.highlight.set(0);
        }
        Self { nav: nav.clone(), actions: Vec::new() }
    }

    /// `row`, highlighted when the keyboard is on it, and choosable by click or Enter.
    pub fn add(&mut self, row: Stateful<Div>, choose: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static, cx: &mut Context<V>) -> AnyElement {
        let ix = self.actions.len();
        let choose: Choose<V> = Rc::new(choose);
        self.actions.push(choose.clone());
        let place = track_row(self.nav.rows.clone(), ix);
        row.relative()
            .when(self.nav.highlight.get() == ix, |d| d.bg(hsla(theme::SURFACE_HOVER)))
            .child(place)
            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| choose(v, window, cx)))
            .into_any_element()
    }

    /// The scrolling list the rows go in, so the highlight can be kept in view.
    pub fn list(&self, id: impl Into<ElementId>, window: &mut Window) -> Stateful<Div> {
        self.nav.scroll.wire(div().id(id), window).flex().flex_col().gap(px(2.0))
    }

    /// Handle Up, Down or Enter; false for any other key.
    fn key(&self, key: &str, v: &mut V, window: &mut Window, cx: &mut Context<V>) -> bool {
        let Some(last) = self.actions.len().checked_sub(1) else { return false };
        let at = self.nav.highlight.get().min(last);
        match key {
            "up" | "down" => {
                let to = step(at, last, key == "down");
                self.nav.highlight.set(to);
                self.reveal(to);
                cx.notify();
            }
            "enter" => (self.actions[at])(v, window, cx),
            _ => return false,
        }
        true
    }

    fn reveal(&self, ix: usize) {
        reveal_row(&self.nav.scroll, &self.nav.rows.borrow(), ix);
    }
}

/// The row Up or Down moves to from `at`, wrapping around a list whose last row is `last`.
fn step(at: usize, last: usize, down: bool) -> usize {
    match (down, at) {
        (true, at) if at >= last => 0,
        (true, at) => at + 1,
        (false, 0) => last,
        (false, at) => at - 1,
    }
}

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
    let card = dropdown_card(id)
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
    let card = keyboard(card, choices, move |v, window, cx| close_key(v, window, cx), cx)
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
    frame.when_some(choices.as_ref().map(|c| c.nav.focus.clone()), |d, focus| d.track_focus(&focus)).capture_key_down(cx.listener(
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

#[cfg(test)]
mod tests {
    use super::step;

    #[test]
    fn up_and_down_move_one_row_and_wrap_at_the_ends() {
        assert_eq!(step(0, 3, true), 1);
        assert_eq!(step(3, 3, true), 0, "down from the last row goes to the first");
        assert_eq!(step(0, 3, false), 3, "up from the first row goes to the last");
        assert_eq!(step(2, 3, false), 1);
        assert_eq!(step(0, 0, true), 0, "one row stays put");
    }
}
