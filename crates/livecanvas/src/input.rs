//! Mouse capture and forwarding.
//!
//! The icon view covers the whole desktop and receives all real input, so the
//! wallpaper window never sees a mouse message. We therefore observe the mouse
//! with a low-level hook and replay events into the page when the cursor is
//! over the *bare* desktop.
//!
//! Rules the hook follows so it can never degrade the system:
//!  * it only copies the event into a queue and returns; all decisions and all
//!    cross-process calls happen later, from the message loop;
//!  * it never swallows an event: `CallNextHookEx` is always called, so the
//!    desktop itself keeps working (icon selection, context menu, drag-select).

use crate::{desktop::Attachment, webview::WebView};
use livecanvas_core::Config;
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use windows::{
    core::Result,
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::ScreenToClient,
        System::Variant::{VARIANT, VT_I4},
        UI::{
            Accessibility::{AccessibleObjectFromPoint, IAccessible, ROLE_SYSTEM_LISTITEM},
            Input::KeyboardAndMouse::{GetAsyncKeyState, GetDoubleClickTime, VK_CONTROL, VK_SHIFT},
            WindowsAndMessaging::*,
        },
    },
};

/// Posted to the controller window when the queue becomes non-empty.
pub const WM_APP_INPUT: u32 = WM_APP + 1;
/// Bound on queued events should the UI thread ever stall.
const MAX_QUEUED: usize = 512;

const LEFT: u32 = 1;
const RIGHT: u32 = 2;
const MIDDLE: u32 = 4;

/// One observed mouse event, in physical screen pixels.
#[derive(Clone, Copy)]
pub struct MouseEvent {
    message: u32,
    point: POINT,
    wheel: i32,
    time: u32,
}

thread_local! {
    // The hook procedure runs on the thread that installed it (the UI thread),
    // so thread-local state is sufficient and no locking is needed. Borrows
    // are kept to single statements because the hook can fire while the
    // window procedure is inside a nested message loop.
    static QUEUE: RefCell<VecDeque<MouseEvent>> = const { RefCell::new(VecDeque::new()) };
    static NOTIFY: Cell<HWND> = const { Cell::new(HWND(std::ptr::null_mut())) };
}

/// Uninstalls the hook when dropped.
pub struct Hook(HHOOK);

impl Drop for Hook {
    fn drop(&mut self) {
        // SAFETY: the handle came from SetWindowsHookExW and is unhooked once.
        let _ = unsafe { UnhookWindowsHookEx(self.0) };
    }
}

/// Installs the low-level mouse hook; events wake `notify` via [`WM_APP_INPUT`].
pub fn install(notify: HWND) -> Result<Hook> {
    NOTIFY.with(|n| n.set(notify));
    // SAFETY: `mouse_proc` has the required signature and lives for the whole
    // process. WH_MOUSE_LL hooks are called in the installing thread's context
    // (no DLL is injected anywhere), so a null module handle is correct.
    unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), None, 0) }.map(Hook)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for WH_MOUSE_LL with HC_ACTION the system guarantees that
        // `lparam` points to a valid MSLLHOOKSTRUCT for the duration of the call.
        let info = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let event = MouseEvent {
            message: wparam.0 as u32,
            point: info.pt,
            // High word of mouseData is the signed wheel delta.
            wheel: i32::from((info.mouseData >> 16) as u16 as i16),
            time: info.time,
        };
        let was_empty = QUEUE.with(|queue| {
            let mut queue = queue.borrow_mut();
            let was_empty = queue.is_empty();
            // Coalesce consecutive moves: only the latest position matters.
            match queue.back_mut() {
                Some(last) if last.message == WM_MOUSEMOVE && event.message == WM_MOUSEMOVE => *last = event,
                _ => {
                    if queue.len() >= MAX_QUEUED {
                        queue.pop_front();
                    }
                    queue.push_back(event);
                }
            }
            was_empty
        });
        if was_empty {
            // SAFETY: posting to our own window; failure (e.g. during
            // shutdown) only means the events are dropped, which is fine.
            let _ = unsafe { PostMessageW(Some(NOTIFY.with(Cell::get)), WM_APP_INPUT, WPARAM(0), LPARAM(0)) };
        }
    }
    // SAFETY: passing the unmodified hook arguments down the chain.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Removes and returns all queued events.
pub fn take_events() -> Vec<MouseEvent> {
    QUEUE.with(|queue| queue.borrow_mut().drain(..).collect())
}

/// True if the point is over a desktop icon. Uses MSAA, the documented
/// accessibility API, instead of reading explorer.exe's memory.
fn over_icon(point: POINT) -> bool {
    let mut object: Option<IAccessible> = None;
    let mut child = VARIANT::default();
    // SAFETY: both out-pointers reference live locals. COM is initialised on
    // this thread (app::run). `get_accRole` returns an owned VARIANT; reading
    // `lVal` is valid because we check the discriminant is VT_I4 first.
    unsafe {
        if AccessibleObjectFromPoint(point, &mut object, &mut child).is_err() {
            return false;
        }
        let Some(object) = object else { return false };
        match object.get_accRole(&child) {
            Ok(role) => {
                let inner = &role.Anonymous.Anonymous;
                inner.vt == VT_I4 && inner.Anonymous.lVal as u32 == ROLE_SYSTEM_LISTITEM
            }
            Err(_) => false,
        }
    }
}

fn keys(buttons: u32) -> COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS {
    let mut bits = 0;
    let mut add = |on: bool, key: COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS| {
        if on {
            bits |= key.0;
        }
    };
    // SAFETY: GetAsyncKeyState has no preconditions; a negative value means "down".
    let (shift, control) = unsafe { (GetAsyncKeyState(i32::from(VK_SHIFT.0)) < 0, GetAsyncKeyState(i32::from(VK_CONTROL.0)) < 0) };
    add(buttons & LEFT != 0, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_LEFT_BUTTON);
    add(buttons & RIGHT != 0, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_RIGHT_BUTTON);
    add(buttons & MIDDLE != 0, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_MIDDLE_BUTTON);
    add(shift, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_SHIFT);
    add(control, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_CONTROL);
    COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS(bits)
}

/// Decides which observed events are replayed into the page.
#[derive(Default)]
pub struct Forwarder {
    /// Buttons whose press we forwarded; their release must follow, wherever it happens.
    captured: Cell<u32>,
    /// Whether the page currently believes the cursor is over it.
    inside: Cell<bool>,
    /// Time and position of the last forwarded left press, for double-clicks.
    last_press: Cell<Option<(u32, POINT)>>,
}

impl Forwarder {
    pub fn process(&self, event: MouseEvent, attachment: &Attachment, view: &WebView, config: &Config) {
        let rect = attachment.rect;
        let p = event.point;
        let in_rect = p.x >= rect.left && p.x < rect.right && p.y >= rect.top && p.y < rect.bottom;
        // SAFETY: WindowFromPoint has no preconditions. Anything covering the
        // desktop at this point (an app window, the taskbar, a menu) is
        // returned instead of a desktop window, which disables forwarding.
        let on_desktop = in_rect && attachment.is_desktop(unsafe { WindowFromPoint(p) });
        let mut client = p;
        // SAFETY: `surface` is a live window and `client` a live local.
        let _ = unsafe { ScreenToClient(attachment.surface, &mut client) };
        let held = self.captured.get();

        match event.message {
            WM_MOUSEMOVE => {
                if on_desktop || held != 0 {
                    self.inside.set(true);
                    view.send_mouse(COREWEBVIEW2_MOUSE_EVENT_KIND_MOVE, keys(held), 0, client);
                } else {
                    self.leave(view);
                }
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
                let (button, mut kind) = match event.message {
                    WM_LBUTTONDOWN => (LEFT, COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOWN),
                    WM_RBUTTONDOWN => (RIGHT, COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_DOWN),
                    _ => (MIDDLE, COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_DOWN),
                };
                let blocked = !on_desktop || (button == RIGHT && !config.forward_right_click) || over_icon(p);
                if blocked {
                    self.last_press.set(None);
                    return;
                }
                if button == LEFT {
                    // The hook reports raw presses only; synthesise double-clicks
                    // with the user's system thresholds, as USER32 would.
                    if self.is_double_click(&event) {
                        kind = COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_DOUBLE_CLICK;
                        self.last_press.set(None);
                    } else {
                        self.last_press.set(Some((event.time, p)));
                    }
                }
                self.captured.set(held | button);
                self.inside.set(true);
                view.send_mouse(kind, keys(held | button), 0, client);
                if config.keyboard && button == LEFT {
                    view.focus();
                }
            }
            WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
                let (button, kind) = match event.message {
                    WM_LBUTTONUP => (LEFT, COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP),
                    WM_RBUTTONUP => (RIGHT, COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP),
                    _ => (MIDDLE, COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP),
                };
                // Only release what we pressed, so the page never sees a stray "up".
                if held & button != 0 {
                    self.captured.set(held & !button);
                    view.send_mouse(kind, keys(held & !button), 0, client);
                }
            }
            WM_MOUSEWHEEL | WM_MOUSEHWHEEL if on_desktop => {
                let kind = if event.message == WM_MOUSEWHEEL {
                    COREWEBVIEW2_MOUSE_EVENT_KIND_WHEEL
                } else {
                    COREWEBVIEW2_MOUSE_EVENT_KIND_HORIZONTAL_WHEEL
                };
                // mouseData carries the signed delta, reinterpreted as u32 by the API.
                view.send_mouse(kind, keys(held), event.wheel as u32, client);
            }
            _ => {}
        }
    }

    fn is_double_click(&self, event: &MouseEvent) -> bool {
        let Some((time, at)) = self.last_press.get() else { return false };
        // SAFETY: both functions only read system settings.
        let (max_ms, max_dx, max_dy) =
            unsafe { (GetDoubleClickTime(), GetSystemMetrics(SM_CXDOUBLECLK), GetSystemMetrics(SM_CYDOUBLECLK)) };
        event.time.wrapping_sub(time) <= max_ms
            && (event.point.x - at.x).abs() <= max_dx / 2
            && (event.point.y - at.y).abs() <= max_dy / 2
    }

    /// Tells the page the cursor left, so hover states do not stick.
    fn leave(&self, view: &WebView) {
        if self.inside.replace(false) {
            view.send_mouse(
                COREWEBVIEW2_MOUSE_EVENT_KIND_LEAVE,
                COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE,
                0,
                POINT::default(),
            );
        }
    }

    /// Releases held buttons and hover state, e.g. when forwarding is switched off.
    pub fn reset(&self, view: Option<&WebView>) {
        let held = self.captured.replace(0);
        self.last_press.set(None);
        let Some(view) = view else {
            self.inside.set(false);
            return;
        };
        let ups = [
            (LEFT, COREWEBVIEW2_MOUSE_EVENT_KIND_LEFT_BUTTON_UP),
            (RIGHT, COREWEBVIEW2_MOUSE_EVENT_KIND_RIGHT_BUTTON_UP),
            (MIDDLE, COREWEBVIEW2_MOUSE_EVENT_KIND_MIDDLE_BUTTON_UP),
        ];
        for (button, kind) in ups {
            if held & button != 0 {
                view.send_mouse(kind, COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS_NONE, 0, POINT::default());
            }
        }
        self.leave(view);
    }
}
