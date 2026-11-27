//! m4code's own component kit, modeled on shadcn/ui and built on gpui-base's headless behavior where it fits.
//!
//! Each component copies shadcn's visual structure (spacing, radius, border, ring and the hover, highlighted,
//! selected, disabled and focus-visible states) and takes every color from [`theme()`](crate::panels::theme::theme).
//!
//! - [`command`]: the list inside a picker or menu (shadcn Command / Combobox). Keyboard highlight and pointer hover
//!   are one state; the current value keeps its own check and outline while another row is highlighted.
//! - [`menu`]: context and dropdown menus (shadcn ContextMenu / DropdownMenu) on the same list.
//! - [`toggle_group`] and [`tabs`]: segmented controls and tab rows (shadcn ToggleGroup / Tabs).
//! - [`card`], [`popover`], [`alert`], [`empty`], [`skeleton`]: surfaces and states (Card, Popover/Dialog, Alert,
//!   Empty, Skeleton).
//! - [`button`], [`badge`], [`kbd`], [`separator`], [`switch`], [`checkbox`], [`label`], [`progress`], [`spinner`], [`avatar`],
//!   [`breadcrumb`], [`tooltip`]: the small pieces.

pub mod alert;
pub mod avatar;
pub mod badge;
pub mod breadcrumb;
pub mod button;
pub mod card;
pub mod checkbox;
pub mod command;
pub mod empty;
pub mod kbd;
pub mod label;
pub mod item;
pub mod toast;
pub mod menu;
pub mod notice;
pub mod popover;
pub mod progress;
pub mod separator;
pub mod skeleton;
pub mod spinner;
pub mod switch;
pub mod tabs;
pub mod toggle_group;
pub mod tooltip;
