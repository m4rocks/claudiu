//! What Claudiu draws around the terminals. `workspace_view` is the root view (main area, overlays, toasts), `sidebar`
//! and `inspector` are the two side panels, `editor_pane` is editor mode's files and editor, `widgets` holds the small
//! shared pieces. `theme` and `scale` set the look, and `glide` does the smooth scrolling.

pub mod editor_pane;
pub mod glide;
pub mod inspector;
pub mod scale;
pub mod sidebar;
pub mod theme;
pub mod widgets;
pub mod workspace_view;
