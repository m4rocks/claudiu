//! What Claudiu draws around the terminals. `workspace_view` is the root view (top bar, main area, shell panel),
//! `sidebar` and `inspector` are the two side panels, `editor_pane` editor mode's files and tabs, `overlays` the menu, modal and toast above them, and
//! `settings_view` the Settings dialog's pages. `widgets` holds the small shared pieces, `status_icon` the icon at
//! the start of a session row, `icons` the Lucide icons and `markdown` Claudiu's markdown style. `theme` and `scale`
//! set the look, and `glide` does the terminal's smooth scrolling.

pub mod editor_pane;
pub mod glide;
pub mod icons;
pub mod inspector;
pub mod markdown;
pub mod overlays;
pub mod scale;
pub mod settings_view;
pub mod sidebar;
pub mod status_icon;
pub mod theme;
pub mod widgets;
pub mod workspace_view;
