//! The GPUI-free core of Claudiu: sessions driven through an ACP adapter, an event store, and (later) the
//! guards, firewall and delegation. The UI is a thin client of [`Core`].

pub mod acp;
mod core;
mod event;
mod store;

pub use acp::AdapterCommand;
pub use core::{Clock, Config, Core};
pub use event::{Event, SessionId, SessionRecord, UsageRow, kind};
pub use store::Store;
