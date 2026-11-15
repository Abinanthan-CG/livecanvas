//! Notification-area icon and its context menu.

use crate::util::log_err;
use std::ffi::c_void;
use windows::{
    core::{w, Error, Result, PCWSTR},
    Win32::{
        Foundation::{HWND, LPARAM, POINT, WPARAM},
        Graphics::Gdi::{CreateBitmap, DeleteObject},
        UI::{
            Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW},
            WindowsAndMessaging::*,
        },
    },
};

/// Callback message the shell sends for mouse activity on the icon.
pub const WM_APP_TRAY: u32 = WM_APP + 2;
const ICON_SIZE: i32 = 32;

/// Menu entries. The discriminants double as Win32 menu command ids (non-zero).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuCommand {
    Interactive = 1,
    Keyboard,
    Mute,
    Reload,
    OpenFile,
    OpenFolder,
    PasteUrl,
    Demo,
    DevTools,
    EditConfig,
    ReloadConfig,
    Autostart,
    Exit,
}

impl MenuCommand {
    const ALL: [Self; 13] = [
        Self::Interactive,
        Self::Keyboard,
        Self::Mute,
        Self::Reload,
        Self::OpenFile,
        Self::OpenFolder,
        Self::PasteUrl,
        Self::Demo,
        Self::DevTools,
        Self::EditConfig,
        Self::ReloadConfig,
        Self::Autostart,
        Self::Exit,
    ];

    fn from_id(id: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|command| *command as i32 == id)
    }
}

/// Check-mark state shown in the menu.
#[derive(Clone, Copy)]
pub struct MenuState {
    pub interactive: bool,
    pub keyboard: bool,
    pub muted: bool,
    pub autostart: bool,
    pub devtools: bool,
}

/// Owns the tray icon; removes it on drop.
pub struct Tray {
    hwnd: HWND,
    icon: HICON,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Result<Self> {
        let tray = Self { hwnd, icon: make_icon()? };
        tray.add();
        Ok(tray)
    }

    fn data(&self) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_APP_TRAY,
            hIcon: self.icon,
            ..Default::default()
        };
        for (slot, unit) in data.szTip.iter_mut().zip("LiveCanvas".encode_utf16()) {
            *slot = unit;
        }
        data
    }

    /// (Re-)registers the icon. Must be repeated after explorer.exe restarts.
    pub fn add(&self) {
        // SAFETY: `data` is a fully initialised NOTIFYICONDATAW with the right cbSize.
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &self.data()) };
        if !added.as_bool() {
            crate::util::log("could not add the tray icon (is the shell running?)");
        }
    }

    /// Shows the menu at the cursor and returns the chosen entry, if any.
    pub fn show_menu(&self, state: MenuState) -> Option<MenuCommand> {
        let check = |on: bool| if on { MF_STRING | MF_CHECKED } else { MF_STRING };
        let gray = |on: bool| if on { MF_STRING } else { MF_STRING | MF_GRAYED };
        let entries: [(Option<MenuCommand>, PCWSTR, MENU_ITEM_FLAGS); 17] = [
            (Some(MenuCommand::Interactive), w!("Interactive (forward mouse)\tCtrl+Alt+I"), check(state.interactive)),
            (Some(MenuCommand::Keyboard), w!("Keyboard focus on click"), check(state.keyboard)),
            (Some(MenuCommand::Mute), w!("Mute audio"), check(state.muted)),
            (None, PCWSTR::null(), MF_SEPARATOR),
            (Some(MenuCommand::OpenFile), w!("Use HTML file..."), MF_STRING),
            (Some(MenuCommand::OpenFolder), w!("Use folder with index.html..."), MF_STRING),
            (Some(MenuCommand::PasteUrl), w!("Use URL or path from clipboard"), MF_STRING),
            (Some(MenuCommand::Demo), w!("Use built-in demo"), MF_STRING),
            (None, PCWSTR::null(), MF_SEPARATOR),
            (Some(MenuCommand::Reload), w!("Reload page"), MF_STRING),
            (Some(MenuCommand::DevTools), w!("Open DevTools"), gray(state.devtools)),
            (None, PCWSTR::null(), MF_SEPARATOR),
            (Some(MenuCommand::EditConfig), w!("Edit config.toml"), MF_STRING),
            (Some(MenuCommand::ReloadConfig), w!("Apply config.toml"), MF_STRING),
            (Some(MenuCommand::Autostart), w!("Start with Windows"), check(state.autostart)),
            (None, PCWSTR::null(), MF_SEPARATOR),
            (Some(MenuCommand::Exit), w!("Exit"), MF_STRING),
        ];
        // SAFETY: the menu handle is created, used and destroyed within this
        // block; all strings are 'static literals; `point` is a live local.
        unsafe {
            let menu = log_err("create menu", CreatePopupMenu())?;
            for (command, text, flags) in entries {
                let id = command.map_or(0, |c| c as usize);
                log_err("append menu item", AppendMenuW(menu, flags, id, text));
            }
            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            // Required by USER32: without a foreground owner the menu would
            // not close when the user clicks somewhere else.
            let _ = SetForegroundWindow(self.hwnd);
            let flags = TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON;
            let chosen = TrackPopupMenu(menu, flags, point.x, point.y, None, self.hwnd, None);
            // Documented tray-menu idiom: posting a benign message forces the task
            // switch that lets the next menu be dismissed by clicking elsewhere.
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            MenuCommand::from_id(chosen.0)
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // SAFETY: removes the icon registered in `add` and frees the icon
        // handle created in `make_icon`; both happen exactly once.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data());
            let _ = DestroyIcon(self.icon);
        }
    }
}

/// Draws the application icon at runtime: a rounded teal tile with a white
/// "play" triangle. Generating it in code keeps the build free of resource
/// compilers and binary assets.
fn make_icon() -> Result<HICON> {
    let size = ICON_SIZE;
    let mut pixels = vec![0u32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            // Rounded-rectangle test: distance to the nearest corner centre.
            let radius = 7;
            let cx = x.clamp(radius, size - 1 - radius);
            let cy = y.clamp(radius, size - 1 - radius);
            let inside_tile = (x - cx).pow(2) + (y - cy).pow(2) <= radius * radius;
            let in_triangle = (12..=23).contains(&x) && (y - 16).abs() * 11 <= (23 - x) * 7;
            let shade = (y * 70 / size) as u32;
            let argb = if in_triangle {
                0xFFFF_FFFF
            } else if inside_tile {
                0xFF00_0000 | ((20 - shade / 5) << 16) | ((184 - shade) << 8) | (166 - shade)
            } else {
                0 // fully transparent (premultiplied alpha requires colour 0)
            };
            pixels[(y * size + x) as usize] = argb;
        }
    }
    let mask_bits = [0u8; (ICON_SIZE * ICON_SIZE / 8) as usize];
    // SAFETY: both buffers are large enough for the bitmap dimensions passed
    // (32x32 at 32 bpp, and 32x32 at 1 bpp with word-aligned rows) and outlive
    // the calls, which copy the bits. The bitmaps are deleted after
    // CreateIconIndirect has copied them into the icon.
    unsafe {
        let color = CreateBitmap(size, size, 1, 32, Some(pixels.as_ptr() as *const c_void));
        let mask = CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const c_void));
        let info = ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color };
        let icon = CreateIconIndirect(&info);
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon.map_err(|_| Error::from_thread())
    }
}
