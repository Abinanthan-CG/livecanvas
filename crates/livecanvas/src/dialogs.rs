//! Standard system pickers and clipboard access used by the tray menu.

use crate::util::wide;
use std::ffi::c_void;
use windows::{
    core::{w, PCWSTR, PWSTR},
    Win32::{
        Foundation::{HGLOBAL, HWND, MAX_PATH},
        System::{
            Com::CoTaskMemFree,
            DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard},
            Memory::{GlobalLock, GlobalSize, GlobalUnlock},
            Ole::CF_UNICODETEXT,
        },
        UI::{
            Controls::Dialogs::{
                GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST,
                OPENFILENAMEW,
            },
            Shell::{SHBrowseForFolderW, SHGetPathFromIDListW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS, BROWSEINFOW},
        },
    },
};

/// Clipboard text longer than this is not a URL or path we want to act on.
const MAX_CLIPBOARD_UNITS: usize = 4096;

fn until_nul(buffer: &[u16]) -> String {
    let len = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// Shows the "open file" dialog filtered to HTML pages.
pub fn pick_file(owner: HWND) -> Option<String> {
    let mut path = [0u16; 2048];
    // A filter is a list of NUL-separated pairs, ended by a double NUL.
    let filter = wide("HTML pages\0*.html;*.htm\0All files\0*.*\0");
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrFile: PWSTR(path.as_mut_ptr()),
        nMaxFile: path.len() as u32,
        lpstrTitle: w!("LiveCanvas - choose an HTML page"),
        // NOCHANGEDIR: never let the dialog change our working directory.
        Flags: OFN_EXPLORER | OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: `dialog` is fully initialised; `filter` and `path` outlive the
    // modal call and `nMaxFile` matches the length of `path`.
    let chosen = unsafe { GetOpenFileNameW(&mut dialog) }.as_bool();
    chosen.then(|| until_nul(&path))
}

/// Shows the "browse for folder" dialog.
pub fn pick_folder(owner: HWND) -> Option<String> {
    let info = BROWSEINFOW {
        hwndOwner: owner,
        lpszTitle: w!("Choose a folder that contains index.html"),
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        ..Default::default()
    };
    let mut path = [0u16; MAX_PATH as usize];
    // SAFETY: `info` is fully initialised and outlives the modal call. The
    // returned item-id list is owned by us and freed with CoTaskMemFree exactly
    // once. SHGetPathFromIDListW requires a buffer of at least MAX_PATH units,
    // which `path` is.
    unsafe {
        let id_list = SHBrowseForFolderW(&info);
        if id_list.is_null() {
            return None;
        }
        let ok = SHGetPathFromIDListW(id_list, &mut path).as_bool();
        CoTaskMemFree(Some(id_list as *const c_void));
        ok.then(|| until_nul(&path))
    }
}

/// Reads Unicode text from the clipboard, trimmed; `None` if there is none.
pub fn clipboard_text(owner: HWND) -> Option<String> {
    // SAFETY: the clipboard is opened and closed in this block. The handle
    // returned by GetClipboardData is owned by the clipboard; we only lock it,
    // copy at most `GlobalSize` bytes out, and unlock it again before closing.
    unsafe {
        OpenClipboard(Some(owner)).ok()?;
        let text = (|| {
            let handle = GetClipboardData(u32::from(CF_UNICODETEXT.0)).ok()?;
            let global = HGLOBAL(handle.0);
            let units = (GlobalSize(global) / 2).min(MAX_CLIPBOARD_UNITS);
            let data = GlobalLock(global) as *const u16;
            if data.is_null() || units == 0 {
                return None;
            }
            let text = until_nul(std::slice::from_raw_parts(data, units));
            let _ = GlobalUnlock(global);
            Some(text.trim().to_owned())
        })();
        let _ = CloseClipboard();
        text.filter(|t| !t.is_empty())
    }
}
