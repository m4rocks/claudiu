//! m4code's own component kit, modeled on shadcn/ui and built on gpui-base's headless behavior where it fits.
//!
//! Each component copies shadcn's visual structure (spacing, radius, border, ring and the hover, highlighted,
//! selected, disabled and focus-visible states) and takes every color from [`token`], which maps the kit's semantic
//! roles onto `theme`. A theme change only has to touch [`token`].
//!
//! - [`command`]: the list inside a picker or menu (shadcn Command / Combobox). Keyboard highlight and pointer hover
//!   are one state; the current value keeps its own check and outline while another row is highlighted.
//! - [`menu`]: context and dropdown menus (shadcn ContextMenu / DropdownMenu) on the same list.
//! - [`toggle_group`]: a segmented control for a two- or three-way choice (shadcn ToggleGroup / Tabs).
//! - [`button`], [`badge`], [`kbd`], [`separator`]: the small pieces.

#[allow(dead_code)] // Not used yet; the next migration (widgets::button and key_badge) adopts it.
pub mod badge;
#[allow(dead_code)] // Not used yet; the next migration (widgets::button and key_badge) adopts it.
pub mod button;
pub mod command;
#[allow(dead_code)] // Not used yet; the next migration (widgets::button and key_badge) adopts it.
pub mod kbd;
pub mod menu;
#[allow(dead_code)] // Not used yet; the next migration (widgets::button and key_badge) adopts it.
pub mod separator;
pub mod toggle_group;

/// The kit's semantic colors. Every kit component paints with these and nothing else.
#[allow(dead_code)] // The button roles wait on `button` coming into use.
pub mod token {
    use gpui::Hsla;

    use crate::panels::theme::{self, hsla};

    /// A floating surface: menus, pickers, popovers.
    pub fn popover() -> Hsla {
        hsla(theme::SURFACE_ELEVATED)
    }

    /// The outline of a floating surface or a control.
    pub fn border() -> Hsla {
        hsla(theme::PANEL_BORDER)
    }

    /// The divider between groups of rows.
    pub fn separator() -> Hsla {
        hsla(theme::BORDER)
    }

    /// The highlighted row of a list (keyboard or pointer) and a ghost control under the pointer. shadcn's `accent`.
    pub fn accent() -> Hsla {
        hsla(theme::CONTROL_HOVER)
    }

    /// The outline of a list's current value, strong enough to read under the highlight.
    pub fn selected_border() -> Hsla {
        hsla(theme::FOCUS_BORDER)
    }

    /// The ring of whatever has the keyboard.
    pub fn ring() -> Hsla {
        hsla(theme::FOCUS_BORDER)
    }

    /// The sunken track of a segmented control or an input.
    pub fn muted() -> Hsla {
        hsla(theme::PANEL_BG)
    }

    /// The raised segment of a segmented control, and a secondary button.
    pub fn raised() -> Hsla {
        hsla(theme::CONTROL_HOVER)
    }

    pub fn secondary() -> Hsla {
        hsla(theme::CONTROL)
    }

    pub fn secondary_hover() -> Hsla {
        hsla(theme::CONTROL_HOVER)
    }

    /// The loudest button, and its text.
    pub fn primary() -> Hsla {
        hsla(theme::ACCENT)
    }

    pub fn primary_foreground() -> Hsla {
        hsla(theme::PANEL_BG)
    }

    pub fn foreground() -> Hsla {
        hsla(theme::TEXT)
    }

    /// Secondary text: details, inactive segments.
    pub fn muted_foreground() -> Hsla {
        hsla(theme::TEXT_DIM)
    }

    /// Hints, shortcuts and disabled text.
    pub fn faint_foreground() -> Hsla {
        hsla(theme::TEXT_FAINT)
    }

    pub fn destructive() -> Hsla {
        hsla(theme::DANGER)
    }
}
