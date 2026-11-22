//! The status icon at the start of a session row: one Lucide circle per state, spinning while Claude works. When the
//! state changes, the old icon turns, shrinks and fades away while the new one does the same in reverse.

use std::f32::consts::{FRAC_PI_2, TAU};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gpui::{Animation, AnimationExt, AnyElement, App, Hsla, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, ease_out_quint, px};
use gpui_kit::assets::IconName;

use crate::panels::theme::{self, hsla};
use crate::panels::widgets::icon;

/// What a session row's status icon shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// The icon's square, and how long one turn of the working icon takes.
const SLOT: f32 = 14.0;
const SPIN: Duration = Duration::from_millis(1600);
/// How often the working icon is redrawn (see `Workspace::start_spin`). A gpui animation would redraw the whole
/// window every frame for as long as any session works.
pub const SPIN_STEP: Duration = Duration::from_millis(40);
/// How long one icon takes to turn into the next.
const MORPH: Duration = Duration::from_millis(280);

/// How far the working icon has turned `elapsed` into its spin, in radians; repeats every `SPIN`.
fn spin_angle(elapsed: Duration) -> f32 {
    (elapsed.as_millis() % SPIN.as_millis()) as f32 / SPIN.as_millis() as f32 * TAU
}

/// How one icon is drawn: its size and opacity as a fraction of the resting look, and an extra turn.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    scale: f32,
    opacity: f32,
    turn: f32,
}

impl Look {
    const REST: Look = Look { scale: 1.0, opacity: 1.0, turn: 0.0 };
}

/// The old and the new icon `t` (0 to 1) of the way through a morph. The new one ends at `Look::REST`, so nothing
/// jumps when the morph is over and the plain icon takes its place.
fn morph_frame(t: f32) -> (Look, Look) {
    let old = Look { scale: 1.0 - 0.6 * t, opacity: 1.0 - t, turn: t * FRAC_PI_2 };
    let new = Look { scale: 0.4 + 0.6 * t, opacity: t, turn: (t - 1.0) * FRAC_PI_2 };
    (old, new)
}

/// One state's icon and color. `None` is the not-running ring, which is drawn rather than loaded.
fn icon_of(state: SessionDot) -> Option<(IconName, u32)> {
    match state {
        SessionDot::Waiting => Some((IconName::CircleQuestionMark, theme::WARN)),
        SessionDot::Attention => Some((IconName::CircleCheck, theme::ACCENT)),
        SessionDot::Working => Some((IconName::CircleDotDashed, 0xc8c8c8)),
        SessionDot::Active => Some((IconName::CircleSmall, theme::TEXT_DIM)),
        SessionDot::Inactive => None,
    }
}

/// `state` drawn with `look`, centered in the slot.
fn glyph(state: SessionDot, look: Look) -> AnyElement {
    static START: OnceLock<Instant> = OnceLock::new();
    let drawn = match icon_of(state) {
        Some((name, color)) => {
            let spin = if state == SessionDot::Working { spin_angle(START.get_or_init(Instant::now).elapsed()) } else { 0.0 };
            let color: Hsla = hsla(color).opacity(look.opacity);
            icon(name, SLOT * look.scale).text_color(color).rotate(gpui::radians(spin + look.turn)).into_any_element()
        }
        None => div().size(px(7.0 * look.scale)).rounded_full().border_1().border_color(hsla(0x3d3d43).opacity(look.opacity)).into_any_element(),
    };
    div().absolute().inset_0().flex().items_center().justify_center().child(drawn).into_any_element()
}

/// What the icon showed last, kept across redraws so a change of state can be told from a redraw.
struct Memory {
    shown: SessionDot,
    /// The state before the last change, until `MORPH` after it.
    from: Option<SessionDot>,
    changed: Instant,
    /// Counts the changes, so each morph is a new animation.
    epoch: u64,
}

/// A row's status icon. `id` must be the same on every redraw of the row and differ between rows.
#[derive(IntoElement)]
pub struct StatusIcon {
    id: SharedString,
    state: SessionDot,
}

impl StatusIcon {
    pub fn new(id: impl Into<SharedString>, state: SessionDot) -> Self {
        Self { id: id.into(), state }
    }
}

impl RenderOnce for StatusIcon {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, state } = self;
        let memory = window.use_keyed_state(id.clone(), cx, move |_, _| Memory { shown: state, from: None, changed: Instant::now(), epoch: 0 });
        let (from, epoch) = memory.update(cx, |m, _| {
            if m.shown != state {
                m.from = Some(m.shown);
                m.shown = state;
                m.changed = Instant::now();
                m.epoch += 1;
            }
            (m.from.filter(|_| m.changed.elapsed() < MORPH), m.epoch)
        });
        let slot = div().relative().flex_none().size(px(SLOT));
        match from {
            None => slot.child(glyph(state, Look::REST)).into_any_element(),
            Some(from) => slot
                .with_animation(
                    SharedString::from(format!("{id}-morph-{epoch}")),
                    Animation::new(MORPH).with_easing(ease_out_quint()),
                    move |slot, t| {
                        let (old, new) = morph_frame(t);
                        slot.child(glyph(from, old)).child(glyph(state, new))
                    },
                )
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_morph_starts_on_the_old_icon_and_ends_on_the_new_one_at_rest() {
        let (old, new) = morph_frame(0.0);
        assert_eq!(old, Look::REST);
        assert_eq!(new.opacity, 0.0);
        let (old, new) = morph_frame(1.0);
        assert_eq!(old.opacity, 0.0);
        assert_eq!(new, Look::REST);
    }

    #[test]
    fn halfway_through_both_icons_are_partly_visible() {
        let (old, new) = morph_frame(0.5);
        assert!(old.opacity > 0.0 && new.opacity > 0.0);
        assert!(old.scale < 1.0 && new.scale < 1.0);
    }

    #[test]
    fn the_working_icon_turns_once_per_spin_and_repeats() {
        assert_eq!(spin_angle(Duration::ZERO), 0.0);
        let quarter = spin_angle(SPIN / 4);
        assert!((quarter - FRAC_PI_2).abs() < 1e-3, "a quarter of the spin is {quarter} radians");
        let (early, later) = (spin_angle(Duration::from_millis(300)), spin_angle(Duration::from_millis(300) + SPIN * 3));
        assert!((early - later).abs() < 1e-4);
    }

    #[test]
    fn only_a_session_that_is_not_running_draws_a_ring_instead_of_an_icon() {
        for state in [SessionDot::Waiting, SessionDot::Attention, SessionDot::Working, SessionDot::Active] {
            assert!(icon_of(state).is_some(), "{state:?}");
        }
        assert!(icon_of(SessionDot::Inactive).is_none());
    }
}
