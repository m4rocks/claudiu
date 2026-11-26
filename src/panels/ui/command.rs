//! The list inside every picker and menu, after shadcn's Command and Combobox (cmdk).
//!
//! - The highlighted row, where Up/Down/Enter act, gets the [`token::accent`] background. Pointer hover moves the
//!   highlight, so the two never disagree.
//! - The current value gets a check and a [`token::selected_border`] outline, which stay while another row is
//!   highlighted.
//! - A disabled row is faint and is left out of the keyboard's rows.
//!
//! [`Nav`] is the list's state, kept by whoever owns the list. [`Choices`] collects the rows each frame and answers
//! the keys; [`row`] draws the usual row and [`item`] is the bare row for lists that lay theirs out by hand.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, Div, ElementId, FocusHandle, FontWeight, InteractiveElement, IntoElement, MouseMoveEvent,
    ParentElement, Pixels, SharedString, Stateful, StatefulInteractiveElement, Styled, Window, canvas, div, prelude::FluentBuilder, px,
};
use gpui_kit::assets::IconName;

use super::token;
use crate::panels::{glide, scale, widgets::icon};

/// Up/Down/Enter for a list, kept by whoever owns the list and passed to [`Choices`] each frame.
#[derive(Clone)]
pub struct Nav {
    /// Held by the list's frame. A list with no search box takes it on opening, so its keys reach the list.
    focus: FocusHandle,
    highlight: Rc<Cell<usize>>,
    /// The search text the highlight was set under; new text starts it over at the first row.
    query: Rc<RefCell<String>>,
    /// Where each row was drawn last frame, to scroll the highlighted one into view.
    rows: Rc<RefCell<Vec<Bounds<Pixels>>>>,
    scroll: glide::Smooth,
}

impl Nav {
    pub fn new(cx: &mut App) -> Self {
        Self { focus: cx.focus_handle(), highlight: Default::default(), query: Default::default(), rows: Default::default(), scroll: Default::default() }
    }

    /// Start a newly opened list on its first row.
    pub fn reset(&self) {
        self.reset_to(0);
    }

    /// Start a newly opened list on row `ix`, such as the current value, scrolled near it.
    pub fn reset_to(&self, ix: usize) {
        self.highlight.set(ix);
        self.query.borrow_mut().clear();
        self.scroll.scroll_to_item(ix);
    }

    /// The highlighted row, as an index into the rows added to [`Choices`].
    #[allow(dead_code)]
    pub fn highlighted(&self) -> usize {
        self.highlight.get()
    }

    /// Give the keyboard to the list, for one with no search box to type in.
    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }
}

/// What choosing a row does.
type Choose<V> = Rc<dyn Fn(&mut V, &mut Window, &mut Context<V>)>;

/// The rows a list can choose from, in order, as they are drawn. A click on a row and Enter on the highlighted row
/// run the same `choose`.
pub struct Choices<V> {
    nav: Nav,
    actions: Vec<Choose<V>>,
}

impl<V: 'static> Choices<V> {
    pub fn new(nav: &Nav, query: &str) -> Self {
        if *nav.query.borrow() != query {
            *nav.query.borrow_mut() = query.to_string();
            nav.highlight.set(0);
        }
        Self { nav: nav.clone(), actions: Vec::new() }
    }

    pub fn nav(&self) -> &Nav {
        &self.nav
    }

    /// `row`, highlighted when the keyboard or pointer is on it, and choosable by click or Enter.
    pub fn add(&mut self, row: Stateful<Div>, choose: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static, cx: &mut Context<V>) -> AnyElement {
        let ix = self.actions.len();
        let choose: Choose<V> = Rc::new(choose);
        self.actions.push(choose.clone());
        let place = track_row(self.nav.rows.clone(), ix);
        let highlight = self.nav.highlight.clone();
        highlighted(row.relative(), self.nav.highlight.get() == ix)
            .child(place)
            .on_mouse_move(cx.listener(move |_, _: &MouseMoveEvent, _, cx| {
                if highlight.replace(ix) != ix {
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |v, _: &ClickEvent, window, cx| choose(v, window, cx)))
            .into_any_element()
    }

    /// The scrolling list the rows go in, so the highlight can be kept in view.
    pub fn list(&self, id: impl Into<ElementId>, window: &mut Window) -> Stateful<Div> {
        self.nav.scroll.wire(div().id(id), window).flex().flex_col().gap(px(2.0))
    }

    /// Handle Up, Down or Enter; false for any other key.
    pub fn key(&self, key: &str, v: &mut V, window: &mut Window, cx: &mut Context<V>) -> bool {
        let Some(last) = self.actions.len().checked_sub(1) else { return false };
        let at = self.nav.highlight.get().min(last);
        match key {
            "up" | "down" => {
                let to = step(at, last, key == "down");
                self.nav.highlight.set(to);
                reveal_row(&self.nav.scroll, &self.nav.rows.borrow(), to);
                cx.notify();
            }
            "enter" => (self.actions[at])(v, window, cx),
            _ => return false,
        }
        true
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
pub fn reveal_row(scroll: &glide::Smooth, rows: &[Bounds<Pixels>], ix: usize) {
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

/// A bare row: shadcn's `CommandItem` geometry (8px across, 5px down, small radius, 8px between its parts) with a
/// transparent outline so the current value's outline doesn't move anything.
pub fn item(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .px(px(8.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(gpui::transparent_black())
        .flex()
        .items_center()
        .gap(px(8.0))
        .text_size(scale::px(12.5))
        .text_color(token::foreground())
}

/// `el` with the highlighted row's background when `on`. For lists that keep their own highlight; [`Choices::add`]
/// does this itself.
pub fn highlighted<E: Styled>(el: E, on: bool) -> E {
    if on { el.bg(token::accent()) } else { el }
}

/// `el` marked as the current value: its outline. [`row`] adds the check too.
pub fn current<E: Styled>(el: E, on: bool) -> E {
    if on { el.border_color(token::selected_border()) } else { el }
}

/// The usual row: an optional leading icon, a label with an optional faint second line, optional trailing content,
/// and the check of the current value. A `disabled` row is faint and should be left out of [`Choices`].
pub fn row(
    id: impl Into<ElementId>,
    leading: Option<AnyElement>,
    label: impl Into<SharedString>,
    detail: Option<String>,
    trailing: Option<AnyElement>,
    selected: bool,
    disabled: bool,
) -> Stateful<Div> {
    let text = if disabled { token::faint_foreground() } else { token::foreground() };
    let title = div()
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
        .children(detail.map(|d| div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(scale::px(11.0)).text_color(token::faint_foreground()).child(d)));
    current(item(id), selected)
        .text_color(text)
        .when(!disabled, |d| d.cursor_pointer())
        .children(leading)
        .child(title)
        .children(trailing)
        .when(selected, |d| d.child(icon(IconName::Check, 12.0).text_color(token::foreground())))
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
