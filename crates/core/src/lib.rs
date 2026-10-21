//! The GPUI-free core of Claudiu: sessions driven through an ACP adapter, an event store, and (later) the
//! guards, firewall and delegation. The UI is a thin client of [`Core`].

pub mod acp;
mod core;
mod delegate;
mod event;
mod firewall;
mod guard;
mod hooks;
mod http;
mod index;
mod mcp;
mod policy;
mod shell;
mod store;

pub use acp::AdapterCommand;
pub use core::{Clock, Config, Core, GuardState};
pub use guard::GuardConfig;
pub use event::{AccessRequest, Decision, Event, SessionId, SessionRecord, UsageRow, kind};
pub use store::Store;
