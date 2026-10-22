//! The GPUI-free core of Claudiu: sessions driven through an ACP adapter, an event store, and (later) the
//! guards, firewall and delegation. The UI is a thin client of [`Core`].

pub mod acp;
pub mod bench;
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
pub use core::{Clock, Config, Core, Features, GuardState, Spend};
pub use delegate::Tokens;
pub use guard::GuardConfig;
pub use event::{AccessRequest, Decision, Event, SessionId, SessionRecord, UsageRow, kind};
pub use store::Store;
