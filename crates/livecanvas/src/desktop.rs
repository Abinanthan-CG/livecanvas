//! Places a window between the static wallpaper and the desktop icons.
//!
//! The technique uses only documented window APIs plus one long-standing
//! (undocumented but stable since Windows 8) shell message. No code is injected
//! into explorer.exe and no elevation is required.
//!
//! Sending `0x052C` to `Progman` makes the shell split the desktop into layers.
//! Two hierarchies exist in the wild:
//!
//! ```text
//! Classic (Windows 8 .. Windows 11 23H2)      Raised desktop (Windows 11 24H2+)
//!   WorkerW                                     Progman  (WS_EX_NOREDIRECTIONBITMAP)
//!     SHELLDLL_DefView  <- icons                  SHELLDLL_DefView  <- icons
//!       SysListView32                               SysListView32
//!   WorkerW  <- wallpaper; WE GO HERE             [LiveCanvas host]  <- WE GO HERE
//!   Progman                                       WorkerW  <- static wallpaper
//! ```
//!
//! In the raised layout Progman has no redirection surface and its children
//! are composed individually, so our host must be a *layered* child window
//! (enabled by the manifest, see build.rs). WebView2 content is not drawn into
//! the layered host itself but into a plain child ("surface") that carries the
//! DirectComposition target, mirroring how Chromium hosts its own D3D window.

use crate::util::log;
use livecanvas_core::MonitorTarget;
use windows::{
    core::{w, Error, Result, BOOL, PCWSTR},
    Win32::{
        Foundation::{COLORREF, E_FAIL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, GetStockObject, MapWindowPoints, BLACK_BRUSH, HBRUSH, HDC,
            HMONITOR, MONITORINFO,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
};

const HOST_CLASS: PCWSTR = w!("LiveCanvas.Host");
/// Undocumented shell message that makes Progman create the wallpaper layer.
const WM_SPAWN_WORKER: u32 = 0x052C;

/// Our windows inside the shell's desktop hierarchy.
#[derive(Clone, Copy)]
pub struct Attachment {
    /// Child of the shell's wallpaper layer; positions the wallpaper.
    pub host: HWND,
    /// Child of `host`; owns the DirectComposition target for WebView2.
    pub surface: HWND,
    /// Screen rectangle covered, in physical pixels.
    pub rect: RECT,
    /// Shell windows that count as "the bare desktop" for input hit-testing.
    shell_windows: [HWND; 5],
}

impl Attachment {
    /// True if `hwnd` is the desktop (icon view, wallpaper layer) or one of ours.
    pub fn is_desktop(&self, hwnd: HWND) -> bool {
        hwnd == self.host || hwnd == self.surface || self.shell_windows.contains(&hwnd)
    }

    /// False once explorer.exe restarted and took our child windows with it.
    pub fn is_alive(&self) -> bool {
        // SAFETY: IsWindow accepts any handle value, including stale ones.
        unsafe { IsWindow(Some(self.host)).as_bool() && IsWindow(Some(self.surface)).as_bool() }
    }

    pub fn width(&self) -> i32 {
        self.rect.right - self.rect.left
    }

    pub fn height(&self) -> i32 {
        self.rect.bottom - self.rect.top
    }
}

/// The relevant shell windows, as discovered by [`find_shell`].
struct Shell {
    /// Window our host becomes a child of.
    parent: HWND,
    /// The icon view; in the raised layout we must sit directly below it.
    defview: HWND,
    /// Raised layout only: the static-wallpaper WorkerW that must stay below us.
    wallpaper: Option<HWND>,
    raised: bool,
    progman: HWND,
    /// Top-level window that hosts the icon view (and is "the desktop" when focused).
    icon_host: HWND,
}

fn not_found(what: &str) -> Error {
    Error::new(E_FAIL, format!("desktop shell window not found: {what}"))
}

/// `EnumWindows` callback: finds the top-level window that hosts the icon view.
unsafe extern "system" fn find_icon_host(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Option<(HWND, HWND)>` that `find_shell`
    // passes to EnumWindows; it is valid and unaliased for the whole call.
    let found = unsafe { &mut *(lparam.0 as *mut Option<(HWND, HWND)>) };
    // SAFETY: plain window lookup; all handles come from the system.
    match unsafe { FindWindowExW(Some(hwnd), None, w!("SHELLDLL_DefView"), PCWSTR::null()) } {
        Ok(defview) => {
            *found = Some((hwnd, defview));
            false.into() // stop enumerating
        }
        Err(_) => true.into(),
    }
}

fn find_shell() -> Result<Shell> {
    // SAFETY: every call below is a window lookup or a message send that only
    // passes system-provided handles and valid literals. SendMessageTimeoutW
    // is bounded so a hung shell cannot hang us.
    unsafe {
        let progman = FindWindowW(w!("Progman"), PCWSTR::null()).map_err(|_| not_found("Progman"))?;
        SendMessageTimeoutW(progman, WM_SPAWN_WORKER, WPARAM(0xD), LPARAM(0x1), SMTO_NORMAL, 1000, None);

        // Raised desktop: the icon view and a WorkerW are direct children of Progman.
        let child_defview = FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), PCWSTR::null()).ok();
        let child_worker = FindWindowExW(Some(progman), None, w!("WorkerW"), PCWSTR::null()).ok();
        if let (Some(defview), Some(worker)) = (child_defview, child_worker) {
            return Ok(Shell { parent: progman, defview, wallpaper: Some(worker), raised: true, progman, icon_host: progman });
        }

        // Classic: the icon view lives in a top-level WorkerW, and the wallpaper
        // layer is the next top-level WorkerW after it in z-order.
        let mut found: Option<(HWND, HWND)> = None;
        // EnumWindows reports an error when the callback stops it early, which
        // is our success path, so the result is intentionally not inspected.
        let _ = EnumWindows(Some(find_icon_host), LPARAM(&mut found as *mut _ as isize));
        let (icon_host, defview) = found.ok_or_else(|| not_found("SHELLDLL_DefView"))?;
        let worker = FindWindowExW(None, Some(icon_host), w!("WorkerW"), PCWSTR::null())
            .map_err(|_| not_found("wallpaper WorkerW"))?;
        Ok(Shell { parent: worker, defview, wallpaper: None, raised: false, progman, icon_host })
    }
}

/// `EnumDisplayMonitors` callback: collects `(rect, is_primary)` per monitor.
unsafe extern "system" fn collect_monitor(monitor: HMONITOR, _: HDC, _: *mut RECT, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Vec<(RECT, bool)>` passed by `monitors`,
    // valid and unaliased for the duration of the enumeration.
    let list = unsafe { &mut *(lparam.0 as *mut Vec<(RECT, bool)>) };
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `info` is a correctly sized, writable MONITORINFO.
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        list.push((info.rcMonitor, info.dwFlags & MONITORINFOF_PRIMARY != 0));
    }
    true.into()
}

fn monitors() -> Vec<(RECT, bool)> {
    let mut list: Vec<(RECT, bool)> = Vec::new();
    // SAFETY: the callback only dereferences `lparam` as the Vec passed here,
    // which outlives this synchronous call.
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(&mut list as *mut _ as isize)) };
    list
}

/// Screen rectangle (physical pixels) for the configured monitor target.
pub fn target_rect(target: MonitorTarget) -> RECT {
    let all = monitors();
    let primary = all.iter().find(|(_, primary)| *primary).or(all.first()).map(|(rect, _)| *rect);
    let chosen = match target {
        MonitorTarget::Span => None,
        MonitorTarget::Primary => primary,
        // An index that no longer exists (monitor unplugged) falls back to primary.
        MonitorTarget::Index(i) => all.get(i).map(|(rect, _)| *rect).or(primary),
    };
    chosen.unwrap_or_else(|| {
        // SAFETY: GetSystemMetrics has no preconditions.
        let (x, y, w, h) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        RECT { left: x, top: y, right: x + w, bottom: y + h }
    })
}

/// Window procedure for the host and surface windows. They are pure render
/// targets: the shell's icon view sits above them and receives all input.
unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: forwarding the unmodified arguments of this very message.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn instance() -> Result<HINSTANCE> {
    // SAFETY: a null module name returns the handle of the current executable.
    Ok(unsafe { GetModuleHandleW(None) }?.into())
}

/// Registers the window class. Call once at startup.
pub fn register_class() -> Result<()> {
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(host_proc),
        hInstance: instance()?,
        // SAFETY: stock objects are process-global and never need freeing.
        hbrBackground: HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0),
        lpszClassName: HOST_CLASS,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised and its pointers are 'static.
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(Error::from_thread());
    }
    Ok(())
}

/// Creates the host and surface windows inside the shell's wallpaper layer.
pub fn attach(target: MonitorTarget) -> Result<Attachment> {
    let shell = find_shell()?;
    let rect = target_rect(target);
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    let hinstance = instance()?;
    log(&format!("attaching: raised={} rect=({},{} {}x{})", shell.raised, rect.left, rect.top, width, height));

    // Child coordinates are relative to the parent's client area, whose origin
    // is not the screen origin when a monitor sits left of / above the primary.
    let mut origin = [POINT { x: rect.left, y: rect.top }];
    // SAFETY: all calls pass handles we just obtained from the system and
    // pointers to live locals. Creating a child of another process's window is
    // permitted by USER32 and is the documented behaviour of WS_CHILD.
    unsafe {
        MapWindowPoints(None, Some(shell.parent), &mut origin);
        let ex_style = if shell.raised { WS_EX_LAYERED } else { WINDOW_EX_STYLE(0) };
        let host = CreateWindowExW(
            ex_style,
            HOST_CLASS,
            w!("LiveCanvas"),
            WS_CHILD | WS_CLIPCHILDREN,
            origin[0].x,
            origin[0].y,
            width,
            height,
            Some(shell.parent),
            None,
            Some(hinstance),
            None,
        )?;
        let finish = || -> Result<HWND> {
            if shell.raised {
                SetLayeredWindowAttributes(host, COLORREF(0), 255, LWA_ALPHA)?;
            }
            let surface = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                HOST_CLASS,
                w!("LiveCanvas.Surface"),
                WS_CHILD | WS_VISIBLE,
                0,
                0,
                width,
                height,
                Some(host),
                None,
                Some(hinstance),
                None,
            )?;
            let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
            if shell.raised {
                // Directly below the icons, and keep the static wallpaper below us.
                SetWindowPos(host, Some(shell.defview), 0, 0, 0, 0, flags)?;
                if let Some(wallpaper) = shell.wallpaper {
                    SetWindowPos(wallpaper, Some(host), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE)?;
                }
            } else {
                SetWindowPos(host, Some(HWND_TOP), 0, 0, 0, 0, flags)?;
            }
            Ok(surface)
        };
        match finish() {
            Ok(surface) => {
                let list_view = FindWindowExW(Some(shell.defview), None, w!("SysListView32"), PCWSTR::null());
                let shell_windows = [
                    list_view.unwrap_or(shell.defview),
                    shell.defview,
                    shell.wallpaper.unwrap_or(shell.parent),
                    shell.progman,
                    shell.icon_host,
                ];
                Ok(Attachment { host, surface, rect, shell_windows })
            }
            Err(e) => {
                // Do not leave a half-initialised window inside the shell.
                let _ = DestroyWindow(host);
                Err(e)
            }
        }
    }
}

/// Removes our windows from the shell hierarchy.
pub fn detach(attachment: &Attachment) {
    // SAFETY: IsWindow accepts stale handles; DestroyWindow is only called on
    // a window this thread created, and also destroys the surface child.
    unsafe {
        if IsWindow(Some(attachment.host)).as_bool() {
            let _ = DestroyWindow(attachment.host);
        }
    }
}

/// Asks the shell to repaint the user's normal wallpaper (used on exit so no
/// stale frame of the page is left behind on the wallpaper layer).
pub fn refresh_wallpaper() {
    // SAFETY: no buffer is passed; a null path with SPI_SETDESKWALLPAPER
    // re-applies the wallpaper that is already configured for the user.
    let _ = unsafe { SystemParametersInfoW(SPI_SETDESKWALLPAPER, 0, None, SPIF_UPDATEINIFILE) };
}
