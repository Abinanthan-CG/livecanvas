//! Single-instance guard and the `WM_COPYDATA` hand-off between instances.

use crate::util::{log, wide};
use livecanvas_core::Command;
use std::{ffi::c_void, thread, time::Duration};
use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, LPARAM, WPARAM},
        System::{DataExchange::COPYDATASTRUCT, Threading::CreateMutexW},
        UI::WindowsAndMessaging::{FindWindowW, SendMessageTimeoutW, SMTO_ABORTIFHUNG, WM_COPYDATA},
    },
};

/// Window class of the hidden controller window; also how instances find each other.
pub const CONTROLLER_CLASS: PCWSTR = w!("LiveCanvas.Controller");
/// `COPYDATASTRUCT::dwData` tag identifying our protocol ("LCV1").
const COPYDATA_MAGIC: usize = 0x4C43_5631;
/// Upper bound on an accepted payload; real commands are a few hundred bytes.
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Holds the named mutex for the lifetime of the first instance.
pub struct Guard(Option<HANDLE>);

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            // SAFETY: `handle` came from CreateMutexW, is owned exclusively by
            // this guard and is closed exactly once.
            let _ = unsafe { CloseHandle(handle) };
        }
    }
}

/// Returns a guard if this is the first instance in the session, else `None`.
pub fn acquire() -> Option<Guard> {
    // SAFETY: the name is a valid NUL-terminated literal; default security
    // attributes (None) are allowed. `Local\` scopes the mutex to the session.
    let created = unsafe { CreateMutexW(None, false, w!("Local\\LiveCanvas.SingleInstance")) };
    match created {
        Ok(handle) => {
            // SAFETY: GetLastError only reads thread-local state. CreateMutexW
            // succeeds with ERROR_ALREADY_EXISTS when the mutex already existed.
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                drop(Guard(Some(handle)));
                None
            } else {
                Some(Guard(Some(handle)))
            }
        }
        Err(e) => {
            // Without a mutex we cannot detect other instances; running is the
            // more useful failure mode than refusing to start.
            log(&format!("single-instance mutex unavailable: {e}"));
            Some(Guard(None))
        }
    }
}

/// Sends `command` to the running instance.
pub fn forward(command: &Command) -> Result<(), String> {
    let Some(wire) = command.to_wire() else {
        return Ok(());
    };
    // The other instance may still be starting up, so retry briefly.
    let mut target = None;
    for _ in 0..30 {
        // SAFETY: the class name is a valid literal; a null title is allowed.
        if let Ok(hwnd) = unsafe { FindWindowW(CONTROLLER_CLASS, PCWSTR::null()) } {
            target = Some(hwnd);
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let hwnd = target.ok_or("the running instance has no controller window")?;

    let payload = wide(&wire);
    let data = COPYDATASTRUCT {
        dwData: COPYDATA_MAGIC,
        cbData: u32::try_from(std::mem::size_of_val(payload.as_slice())).map_err(|e| e.to_string())?,
        lpData: payload.as_ptr() as *mut c_void,
    };
    // SAFETY: `data` and `payload` outlive this synchronous call; the system
    // marshals the buffer into the receiver, which only reads it.
    let accepted = unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_COPYDATA,
            WPARAM(0),
            LPARAM(&data as *const COPYDATASTRUCT as isize),
            SMTO_ABORTIFHUNG,
            5000,
            None,
        )
    };
    if accepted.0 == 0 {
        return Err("the running instance did not accept the command".into());
    }
    Ok(())
}

/// Decodes a `WM_COPYDATA` message. Any process in the session can send one,
/// so everything is validated and unknown input yields `None`.
pub fn decode(lparam: LPARAM) -> Option<Command> {
    let data = lparam.0 as *const COPYDATASTRUCT;
    if data.is_null() {
        return None;
    }
    // SAFETY: for WM_COPYDATA the system guarantees `lparam` points to a
    // readable COPYDATASTRUCT for the duration of the message.
    let data = unsafe { data.read_unaligned() };
    let len = data.cbData as usize;
    if data.dwData != COPYDATA_MAGIC || data.lpData.is_null() || len == 0 || len % 2 != 0 || len > MAX_PAYLOAD_BYTES {
        return None;
    }
    // SAFETY: the system guarantees `lpData` points to `cbData` readable bytes
    // while the message is being processed. We read bytes (alignment 1) and
    // copy them out before returning.
    let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, len) };
    let units: Vec<u16> = bytes.chunks_exact(2).map(|pair| u16::from_le_bytes([pair[0], pair[1]])).collect();
    let text = String::from_utf16(&units).ok()?;
    Command::from_wire(text.trim_end_matches('\0'))
}
