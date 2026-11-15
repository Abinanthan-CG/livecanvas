//! Paths, logging and small Win32 conveniences shared by the other modules.

use livecanvas_core::APP_NAME;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};
use windows::{
    core::PCWSTR,
    Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MB_OK},
};

/// Registry key (under HKCU, so no elevation is needed) for per-user autostart.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The log is truncated once it grows beyond this size.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// Encodes a string as NUL-terminated UTF-16 for Win32 `W` functions.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// `%LOCALAPPDATA%\LiveCanvas` - config, log, demo page and the WebView2 profile.
pub fn data_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(APP_NAME)
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.toml")
}

/// The WebView2 user-data folder. Cookies, logins, `localStorage` and caches
/// live here, which is what makes sign-ins survive restarts.
pub fn profile_dir() -> PathBuf {
    data_dir().join("WebView2")
}

pub fn log_path() -> PathBuf {
    data_dir().join("livecanvas.log")
}

/// Folder the built-in demo wallpaper is unpacked into.
pub fn demo_dir() -> PathBuf {
    data_dir().join("demo")
}

/// Appends a line to the log file. Logging must never take the app down, so
/// I/O errors are deliberately ignored here (and only here).
pub fn log(message: &str) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let too_big = fs::metadata(&path).map(|m| m.len() > MAX_LOG_BYTES).unwrap_or(false);
    let file = OpenOptions::new().create(true).write(true).append(!too_big).truncate(too_big).open(&path);
    if let Ok(mut file) = file {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(file, "[{secs}] {message}");
    }
}

/// Logs the error of a fallible call that is not worth aborting for.
pub fn log_err<T, E: std::fmt::Display>(context: &str, result: Result<T, E>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(e) => {
            log(&format!("{context}: {e}"));
            None
        }
    }
}

/// Shows a modal message box (the GUI subsystem has no console to print to).
pub fn message_box(text: &str, error: bool) {
    let text = wide(text);
    let title = wide(APP_NAME);
    let icon = if error { MB_ICONERROR } else { MB_ICONINFORMATION };
    // SAFETY: both pointers reference NUL-terminated UTF-16 buffers that
    // outlive the call; a null owner window is explicitly allowed.
    unsafe {
        MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), MB_OK | icon);
    }
}

/// Adds or removes the per-user "start with Windows" registry value.
pub fn set_autostart(enable: bool) -> Result<(), String> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY).map_err(|e| e.to_string())?;
    if enable {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        key.set_string(APP_NAME, format!("\"{}\"", exe.display())).map_err(|e| e.to_string())
    } else {
        match key.remove_value(APP_NAME) {
            Ok(()) => Ok(()),
            // Already absent is the desired end state, not a failure.
            Err(_) if key.get_string(APP_NAME).is_err() => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Whether the autostart value currently exists.
pub fn autostart_enabled() -> bool {
    windows_registry::CURRENT_USER
        .open(RUN_KEY)
        .and_then(|key| key.get_string(APP_NAME))
        .is_ok()
}
