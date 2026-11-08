//! The status icon at the start of a session row: one Lucide circle per state, turning while Claude works. When the
//! state changes, the old icon shrinks and fades away while the new one grows in (each state's icon is its own keyed
//! element, so Rune's enter and exit transitions do the morph).

use rune::{El, Icon, Transition, el, row};

use crate::panels::icons;
use crate::panels::theme::{self, rgb};
use crate::panels::widgets::icon;

/// What a session row's status icon shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionDot {
    /// Claude Code is waiting for the user: a question or a permission.
    Waiting,
    /// Claude finished while the row was not being watched.
    Attention,
    /// The process is running and streaming output.
    Working,
    /// Running and idle, with the user up to date.
    Active,
    /// The session is not running.
    Inactive,
}

/// The icon's square, and how long one turn of the working icon takes (seconds).
const SLOT: f32 = 14.0;
const SPIN: f64 = 1.6;

/// One state's icon and color. `None` is the not-running ring, which is drawn rather than loaded.
fn icon_of(state: SessionDot) -> Option<(&'static Icon, u32)> {
    match state {
        SessionDot::Waiting => Some((&icons::CIRCLE_QUESTION_MARK, theme::WARN)),
        SessionDot::Attention => Some((&icons::CIRCLE_CHECK, theme::ACCENT)),
        SessionDot::Working => Some((&icons::CIRCLE_DOT_DASHED, 0xc8c8c8)),
        SessionDot::Active => Some((&icons::CIRCLE_SMALL, theme::TEXT_DIM)),
        SessionDot::Inactive => None,
    }
}

fn name(state: SessionDot) -> &'static str {
    match state {
        SessionDot::Waiting => "waiting",
        SessionDot::Attention => "attention",
        SessionDot::Working => "working",
        SessionDot::Active => "active",
        SessionDot::Inactive => "inactive",
    }
}

/// A row's status icon. `key` must be the same on every frame for the row and differ between rows. `spin` is false
/// while nobody can see the window, so a working icon stops asking for frames.
pub fn status_icon(key: &str, state: SessionDot, spin: bool) -> El {
    let drawn = match icon_of(state) {
        Some((glyph, color)) => icon(glyph, SLOT, color),
        None => el().size(7.0, 7.0).radius(3.5).border(1.0, rgb(0x3d3d43)),
    };
    let morph = Transition::scale(0.4).combined(Transition::opacity());
    let mut shown = row().key(format!("{key}-{}", name(state))).absolute(0.0, 0.0).size(SLOT, SLOT).center().transition(morph).child(drawn);
    if state == SessionDot::Working && spin {
        shown = shown.looping(rune::LoopSpec::rotation(SPIN));
    }
    row().key(key.to_string()).size(SLOT, SLOT).center().child(shown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_session_that_is_not_running_draws_a_ring_instead_of_an_icon() {
        for state in [SessionDot::Waiting, SessionDot::Attention, SessionDot::Working, SessionDot::Active] {
            assert!(icon_of(state).is_some(), "{state:?}");
        }
        assert!(icon_of(SessionDot::Inactive).is_none());
    }

    fn view(state: &(SessionDot, bool), _: &mut rune::Cx<(SessionDot, bool)>) -> El {
        row().padding(20.0).child(status_icon("dot", state.0, state.1))
    }

    #[test]
    fn a_change_of_state_crossfades_the_icons_and_ends_on_the_new_one() {
        let mut rt = rune::Runtime::new((SessionDot::Active, true), view, (100.0, 100.0), rune::Env::new(true, 2.0));
        rt.frame(1.0 / 60.0);
        assert!(rt.find("dot-active").is_some());
        rt.update(|s, _| s.0 = SessionDot::Waiting);
        rt.frame(1.0 / 60.0);
        assert!(rt.find("dot-waiting").is_some(), "the new icon is in at once");
        assert!(rt.presented("dot-active").is_some(), "the old one is still fading out");
        rt.settle_all();
        rt.frame(1.0 / 60.0);
        assert!(rt.presented("dot-active").is_none(), "the old icon is gone once the morph ends");
        assert!(rt.find("dot-waiting").is_some());
    }

    #[test]
    fn the_working_icon_turns_only_while_it_may_spin() {
        for spin in [true, false] {
            let mut rt = rune::Runtime::new((SessionDot::Working, spin), view, (100.0, 100.0), rune::Env::new(true, 2.0));
            let mut busy = false;
            for _ in 0..60 {
                busy = rt.frame(1.0 / 60.0);
            }
            assert_eq!(busy, spin, "asks for frames only while it spins (spin = {spin})");
        }
    }
}
