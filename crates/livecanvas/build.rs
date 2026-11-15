//! Embeds the application manifest into livecanvas.exe.
//!
//! The manifest is functionally required, not cosmetic:
//!  * `supportedOS` for Windows 8+ is what allows `WS_EX_LAYERED` on *child*
//!    windows, which the Windows 11 24H2 desktop hierarchy needs (desktop.rs).
//!  * Per-Monitor-V2 DPI awareness makes every coordinate we handle (monitor
//!    rectangles, low-level mouse hook points) a physical pixel, so forwarded
//!    input lands exactly where the user clicked on any scaling factor.
//!  * `asInvoker` documents that LiveCanvas never needs elevation.

use embed_manifest::{embed_manifest, new_manifest};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_manifest(new_manifest("LiveCanvas.App")).expect("unable to embed the application manifest");
    }
}
