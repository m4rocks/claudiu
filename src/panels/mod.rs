//! What m4code draws around the terminals. `workspace_view` is the root view, with the Welcome screen in `home`, the
//! menu, modal and toast in `overlays`, the workspace's dropdowns in `workspace_pickers` and the Settings modal in
//! `settings_view`; `sidebar`
//! and `inspector` are the two side panels, `editor_pane` is editor mode's files and editor, `widgets` holds the small
//! shared pieces, `status_icon` the icon at the start of a session row. `theme` and `scale` set the look, and `glide` does the smooth scrolling.

pub mod attachments;
pub mod comment_box;
pub mod composer;
pub mod editor_pane;
pub mod glide;
mod home;
pub mod inspector;
pub mod logos;
mod overlays;
mod palette;
pub mod picker;
pub mod scale;
pub mod settings_pages;
mod settings_view;
mod skills_page;
pub mod sidebar;
pub mod status_icon;
pub mod theme;
pub mod widgets;
pub mod workspace_view;
mod workspace_pickers;
