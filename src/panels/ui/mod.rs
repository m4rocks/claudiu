//! m4code's own component kit, modeled on shadcn/ui and built on gpui-base's headless behavior where it fits.
//!
//! Each component copies shadcn's visual structure (spacing, radius, border, ring and the hover, highlighted,
//! selected, disabled and focus-visible states) and takes every color from [`theme()`](crate::panels::theme::theme).
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
