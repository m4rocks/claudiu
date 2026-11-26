//! The window a second m4code shows when another one already uses its data folder, before it quits.

use gpui::{App, AppContext, Bounds, ClickEvent, Context, IntoElement, ParentElement, Render, Styled, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, size};

use crate::panels::theme::theme;
use crate::panels::widgets::button;

struct AlreadyRunning;

impl Render for AlreadyRunning {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .p(px(24.0))
            .bg(theme().window)
            .child(div().text_size(px(15.0)).text_color(theme().foreground).child("m4code is already running"))
            .child(div().text_size(px(13.0)).text_color(theme().muted_foreground).child("Another m4code window uses the same data folder. Switch to it instead."))
            .child(div().flex_1())
            .child(div().flex().justify_end().child(button("already-running-ok", "OK", true).on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.quit()))))
    }
}

/// Shows the notice until it is closed, then returns.
pub fn show() {
    gpui_kit::application().run(|cx: &mut App| {
        gpui_kit::init(cx);
        let bounds = size(px(420.0), px(170.0));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, bounds, cx))),
            window_min_size: Some(bounds),
            is_resizable: false,
            app_id: Some(crate::platform::APP_ID.into()),
            titlebar: Some(TitlebarOptions { title: Some("m4code".into()), ..Default::default() }),
            ..Default::default()
        };
        if cx.open_window(options, |_, cx| cx.new(|_| AlreadyRunning)).is_err() {
            cx.quit();
        }
        cx.on_window_closed(|cx, _| cx.quit()).detach();
        cx.activate(true);
    });
}
