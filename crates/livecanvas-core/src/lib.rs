//! Platform-independent logic for LiveCanvas.
//!
//! Everything in this crate is free of `unsafe` and of Windows APIs, so it can
//! be unit-tested on any host with `cargo test -p livecanvas-core`.

#![forbid(unsafe_code)]

pub mod command;
pub mod config;
pub mod source;

pub use command::Command;
pub use config::{Config, MonitorTarget};
pub use source::{resolve, FolderMapping, Navigation, VIRTUAL_HOST};

/// Product name used for window classes, registry values and data folders.
pub const APP_NAME: &str = "LiveCanvas";
