//! Chat: Claude Code in a native conversation view instead of a terminal, through the Agent Client Protocol.
//! `acp` runs the adapter, `model` turns its updates into what is drawn (no GPUI), `store` keeps the history list,
//! `view` is the pane.

pub mod acp;
pub mod model;
pub mod store;
pub mod ui;
pub mod view;
