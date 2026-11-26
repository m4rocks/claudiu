//! Context and dropdown menus after shadcn's ContextMenu and DropdownMenu: a popover surface with a border and a
//! shadow, rows inset 4px from its edge, the picker's highlight ([`command`]), right-aligned shortcut hints,
//! separators and destructive rows. Up/Down skip disabled rows, Enter runs the highlighted one and Esc closes.
//!
//! Build the rows as [`Entry`]s, draw them with [`menu`], and float the result with [`popup`], which flips it at the
//! window's edges and closes it on a click outside.

use std::rc::Rc;

use gpui::{
    Anchor as Corner, AnyElement, Context, Div, ElementId, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Point,
    SharedString, Stateful, StatefulInteractiveElement, Styled, Window, anchored, deferred, div, prelude::FluentBuilder, px,
};
use gpui_kit::assets::IconName;

use super::command::{self, Choices, Nav};
use crate::panels::theme::theme;
use crate::panels::{picker, scale, widgets::{fade_in, icon}};

type Select<V> = Rc<dyn Fn(&mut V, &mut Window, &mut Context<V>)>;

/// One row of a menu.
pub struct Entry<V> {
    id: Option<&'static str>,
    label: SharedString,
    detail: Option<SharedString>,
    icon: Option<IconName>,
    shortcut: Option<SharedString>,
    trailing: Option<AnyElement>,
    destructive: bool,
    disabled: bool,
    separator_before: bool,
    on_select: Select<V>,
}

impl<V: 'static> Entry<V> {
    pub fn new(label: impl Into<SharedString>, on_select: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static) -> Self {
        Self {
            id: None,
            label: label.into(),
            detail: None,
            icon: None,
            shortcut: None,
            trailing: None,
            destructive: false,
            disabled: false,
            separator_before: false,
            on_select: Rc::new(on_select),
        }
    }

    /// Names the row `<menu id>-<id>` instead of by its position, so tests can find it.
    pub fn id(mut self, id: &'static str) -> Self {
        self.id = Some(id);
        self
    }

    /// A faint second line under the label.
    pub fn detail(mut self, detail: Option<impl Into<SharedString>>) -> Self {
        self.detail = detail.map(Into::into);
        self
    }

    /// An icon before the label.
    pub fn icon(mut self, icon: Option<IconName>) -> Self {
        self.icon = icon;
        self
    }

    /// Faint text at the right edge that is never cut: the row's shortcut, or a date.
    pub fn shortcut(mut self, shortcut: Option<impl Into<SharedString>>) -> Self {
        self.shortcut = shortcut.map(Into::into);
        self
    }

    /// Content after the shortcut, such as a remove button that shows under the pointer.
    pub fn trailing(mut self, trailing: Option<AnyElement>) -> Self {
        self.trailing = trailing;
        self
    }

    /// Red, for something that destroys work.
    pub fn destructive(mut self, on: bool) -> Self {
        self.destructive = on;
        self
    }

    /// Greyed out; the keyboard skips it and a click does nothing.
    pub fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }

    /// A rule above this row, to start a group.
    pub fn separator_before(mut self, on: bool) -> Self {
        self.separator_before = on;
        self
    }
}

/// The popover surface menus and dropdowns sit on.
pub fn content(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .occlude()
        .cursor_default()
        .min_w(px(128.0))
        .p(px(4.0))
        .rounded(px(8.0))
        .bg(theme().popover)
        .border_1()
        .border_color(theme().panel_border)
        .shadow_lg()
        .flex()
        .flex_col()
}

/// A rule between groups of rows, edge to edge like shadcn's `-mx-1 my-1`.
pub fn separator() -> Div {
    div().flex_none().h(px(1.0)).mx(px(-4.0)).my(px(4.0)).bg(theme().border)
}

/// `entries` on a [`content`] surface `w` wide, keyed by `nav`: Up/Down/Enter move and choose, Esc runs `on_close`.
/// Choosing a row runs its action only; the caller's action closes the menu.
pub fn menu<V: 'static>(
    id: &str,
    w: Pixels,
    nav: &Nav,
    entries: Vec<Entry<V>>,
    on_close: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    window: &Window,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    let mut choices = Choices::new(nav, "");
    let max_h = window.viewport_size().height - px(16.0);
    let mut card = content(SharedString::from(id.to_string())).w(w).max_h(max_h).overflow_y_scroll();
    for (i, e) in entries.into_iter().enumerate() {
        if e.separator_before && i > 0 {
            card = card.child(separator());
        }
        let row_id = SharedString::from(match e.id {
            Some(name) => format!("{id}-{name}"),
            None => format!("{id}-{i}"),
        });
        let selector = row_id.clone();
        let color = if e.disabled {
            theme().subtle_foreground
        } else if e.destructive {
            theme().destructive
        } else {
            theme().foreground
        };
        let icon_color = if e.destructive && !e.disabled { color } else { theme().subtle_foreground };
        let row = command::item(row_id.clone())
            .group(row_id)
            .debug_selector(move || selector.to_string())
            .text_color(color)
            .when(!e.disabled, |d| d.cursor_pointer())
            .children(e.icon.map(|g| icon(g, 13.0).text_color(icon_color)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(e.label))
                    .children(e.detail.map(|d| div().overflow_hidden().whitespace_nowrap().text_ellipsis().text_size(scale::px(11.0)).text_color(theme().subtle_foreground).child(d))),
            )
            .children(e.shortcut.map(|s| div().flex_none().pl(px(12.0)).text_size(scale::px(11.0)).text_color(theme().subtle_foreground).child(s)))
            .children(e.trailing);
        card = card.child(if e.disabled {
            row.into_any_element()
        } else {
            let select = e.on_select;
            choices.add(row, move |v, window, cx| select(v, window, cx), cx)
        });
    }
    picker::keyboard(card, Some(choices), on_close, cx)
}

/// Float `menu` with its top-left corner at `pos`, over everything below `priority`. It flips to the other side of
/// `pos` where it would leave the window, and any click outside it runs `on_dismiss`.
pub fn popup<V: 'static>(
    pos: Point<Pixels>,
    menu: Stateful<Div>,
    priority: usize,
    on_dismiss: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
    cx: &mut Context<V>,
) -> AnyElement {
    let dismiss = Rc::new(on_dismiss);
    let (left, right, out) = (dismiss.clone(), dismiss.clone(), dismiss);
    deferred(
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .on_mouse_down(MouseButton::Left, cx.listener(move |v, _: &MouseDownEvent, window, cx| left(v, window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |v, _: &MouseDownEvent, window, cx| right(v, window, cx)))
            .child(
                anchored()
                    .anchor(Corner::TopLeft)
                    .position(pos)
                    .child(fade_in(
                        menu.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
                            // The catcher behind covers only the parent's area; this closes on a click anywhere else.
                            .on_mouse_down_out(cx.listener(move |v, _: &MouseDownEvent, window, cx| out(v, window, cx))),
                        "menu-popup-in",
                        110,
                        0.0,
                    )),
            ),
    )
    .with_priority(priority)
    .into_any_element()
}
