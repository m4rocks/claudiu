//! What Claudiu draws around the terminals. `workspace_view` is the root view (main area, overlays, toasts), `sidebar`
//! and `inspector` are the two side panels, `widgets` holds the small shared pieces. `theme` and `scale` set the look,
//! and `glide` does the smooth scrolling.

pub mod glide;
pub mod inspector;
pub mod scale;
pub mod sidebar;
pub mod theme;
pub mod widgets;
pub mod workspace_view;
