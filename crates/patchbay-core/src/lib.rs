//! Patchbay core: everything that talks SSH, independent of any GUI.
//!
//! The Tauri app (and potentially a future headless daemon) drives this crate
//! through [`session::SessionManager`], [`fs::FileSystem`],
//! [`transfer::TransferManager`], [`search::SearchManager`],
//! [`terminal::TerminalManager`], [`forward::ForwardManager`],
//! [`edit::EditManager`], [`dragout::DragOutCache`] and the
//! host/snippet/settings stores, and receives
//! asynchronous notifications as [`model::CoreEvent`]s.

pub mod dragout;
pub mod edit;
pub mod error;
pub mod forward;
pub mod fs;
pub mod hosts;
pub mod known_hosts;
pub mod model;
mod persist;
pub mod search;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod snippets;
pub mod ssh_config;
pub mod tailscale;
pub mod terminal;
pub mod transfer;

pub use error::{Error, Result};
