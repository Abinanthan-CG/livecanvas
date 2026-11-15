//! LiveCanvas - an interactive HTML live wallpaper for Windows 11.
//!
//! Module map:
//!  * `desktop`  - finds the shell's wallpaper layer and parents our window into it
//!  * `webview`  - WebView2 environment + composition controller on DirectComposition
//!  * `input`    - low-level mouse hook and forwarding into the page
//!  * `tray`     - notification-area icon and menu
//!  * `app`      - application state, window procedure, message loop
//!  * `instance` - single-instance guard and command hand-off
//!  * `util`     - paths, logging and small Win32 helpers

// GUI subsystem: no console window flashes up when started from Explorer.
#![cfg_attr(windows, windows_subsystem = "windows")]
// Enforced by `cargo clippy`: every `unsafe` block must carry a SAFETY comment,
// and `unsafe fn` bodies get no implicit unsafe context.
#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

#[cfg(windows)]
mod app;
#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod dialogs;
#[cfg(windows)]
mod input;
#[cfg(windows)]
mod instance;
#[cfg(windows)]
mod tray;
#[cfg(windows)]
mod util;
#[cfg(windows)]
mod webview;

#[cfg(windows)]
fn main() {
    use livecanvas_core::{command::USAGE, Command};

    let command = match Command::parse(std::env::args().skip(1)) {
        Ok(command) => command,
        Err(message) => {
            util::message_box(&format!("{message}\n\n{USAGE}"), true);
            std::process::exit(2);
        }
    };

    let code = match command {
        Command::Help => {
            util::message_box(USAGE, false);
            0
        }
        Command::Version => {
            util::message_box(concat!("LiveCanvas ", env!("CARGO_PKG_VERSION")), false);
            0
        }
        Command::Autostart(enable) => match util::set_autostart(enable) {
            Ok(()) => 0,
            Err(e) => {
                util::message_box(&format!("Could not change the autostart setting:\n{e}"), true);
                1
            }
        },
        command => match instance::acquire() {
            // We are the first instance: run the wallpaper.
            Some(guard) => {
                let code = match app::run(command) {
                    Ok(()) => 0,
                    Err(e) => {
                        util::log(&format!("fatal: {e}"));
                        util::message_box(&format!("LiveCanvas could not start:\n{e}"), true);
                        1
                    }
                };
                drop(guard);
                code
            }
            // Already running: hand the command over and exit.
            None => match instance::forward(&command) {
                Ok(()) => 0,
                Err(e) => {
                    util::message_box(&format!("Could not reach the running LiveCanvas:\n{e}"), true);
                    1
                }
            },
        },
    };
    std::process::exit(code);
}

#[cfg(not(windows))]
fn main() {
    eprintln!("LiveCanvas is a Windows application; build it for x86_64-pc-windows-msvc.");
    std::process::exit(1);
}
