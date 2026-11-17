//! The window a second Claudiu shows when another one already uses its data folder, before it quits.

use rune::{Cx, El, col, row};

use crate::panels::theme;
use crate::panels::widgets::{button, label};
use crate::platform;

const SIZE: (f32, f32) = (420.0, 170.0);

pub struct AlreadyRunning;

fn render(_: &AlreadyRunning, _cx: &mut Cx<AlreadyRunning>) -> El {
    col()
        .w_full()
        .h_full()
        .fill(theme::rgb(theme::APP_BG))
        .padding(24.0)
        .gap(10.0)
        .child(label("Claudiu is already running", 15.0, theme::TEXT))
        .child(label("Another Claudiu window uses the same data folder. Switch to it instead.", 13.0, theme::TEXT_DIM).wrap())
        .child(rune::el().grow(1.0))
        .child(row().w_full().child(rune::el().grow(1.0)).child(button("OK", true).key("already-running-ok").on_press_tx(|_: &mut AlreadyRunning, tx: &mut rune::Tx| tx.quit())))
}

/// Shows the notice until it is closed, then returns.
pub fn show() {
    let spec = rune::AppSpec {
        min_size: Some(SIZE),
        families: (platform::ui_font(), platform::monospace_font()),
        ..rune::AppSpec::new("Claudiu", SIZE, || AlreadyRunning, render)
    };
    let app = rune::App::new(spec).on_close(|_: &mut AlreadyRunning, tx: &mut rune::Tx| tx.quit());
    rune::run_windows(move |_| vec![rune::WindowSpec::from_app("already-running", app).options(rune::WindowOptions { resizable: false, min_size: Some(SIZE), ..Default::default() })]);
}
