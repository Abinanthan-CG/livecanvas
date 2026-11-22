//! Application state, the hidden controller window and the message loop.
//!
//! Everything runs on one UI thread. Window procedures and COM callbacks are
//! re-entrant (a cross-process COM call or a modal menu pumps messages), so
//! state lives in `Cell`/`RefCell` fields whose borrows never span a call that
//! can pump: values are copied or cloned out first.

use crate::{
    desktop::{self, Attachment},
    dialogs,
    input::{self, Forwarder, WM_APP_INPUT},
    instance::{self, CONTROLLER_CLASS},
    tray::{MenuCommand, MenuState, Tray, WM_APP_TRAY},
    util::{autostart_enabled, config_path, demo_dir, log, log_err, message_box, set_autostart, wide},
    webview::{self, Crashed, Ready, WebView},
};
use livecanvas_core::{resolve, Command, Config, Navigation};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    fmt::Display,
    fs,
    rc::Rc,
    time::{Duration, Instant},
};
use windows::{
    core::{w, Error, PCWSTR},
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        System::{
            Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED},
            LibraryLoader::GetModuleHandleW,
        },
        UI::{
            Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT},
            Shell::ShellExecuteW,
            WindowsAndMessaging::*,
        },
    },
};

const WM_APP_REBUILD: u32 = WM_APP + 3;
const WM_APP_COMMAND: u32 = WM_APP + 4;
const TIMER_WATCHDOG: usize = 1;
const WATCHDOG_MS: u32 = 1000;
/// After a failed attach, wait this many watchdog ticks before retrying.
const RETRY_TICKS: u32 = 5;
/// Crashes within this window count toward exponential rebuild backoff.
const CRASH_WINDOW: Duration = Duration::from_secs(60);
/// Maximum delay (in watchdog ticks) between crash-triggered rebuilds.
const MAX_CRASH_BACKOFF_TICKS: u32 = 30;
const HOTKEY_TOGGLE: i32 = 1;
/// The built-in wallpaper shown when no source is configured.
const DEMO_HTML: &str = include_str!("../assets/demo/index.html");

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

fn app() -> Option<Rc<App>> {
    APP.with(|slot| slot.borrow().clone())
}

fn text<E: Display>(error: E) -> String {
    error.to_string()
}

struct App {
    /// Hidden top-level window: tray, timers, hotkey, IPC and hook wake-ups.
    hwnd: HWND,
    config: RefCell<Config>,
    attachment: Cell<Option<Attachment>>,
    view: RefCell<Option<Rc<WebView>>>,
    tray: RefCell<Option<Rc<Tray>>>,
    forwarder: Forwarder,
    /// Commands received from other instances, executed from the message loop.
    commands: RefCell<VecDeque<Command>>,
    /// Bumped on every rebuild so late async completions can be recognised.
    generation: Cell<u32>,
    rebuild_pending: Cell<bool>,
    pumping_input: Cell<bool>,
    /// A full-screen window currently hides the wallpaper.
    covered: Cell<bool>,
    retry_in: Cell<u32>,
    crash_count: Cell<u32>,
    last_crash: Cell<Option<Instant>>,
    /// A WebView2 start-up failure was already shown to the user.
    reported: Cell<bool>,
    /// Broadcast by the shell when explorer.exe (re)creates the taskbar.
    taskbar_created: u32,
}

/// Runs LiveCanvas until the user exits. `command` is what this process was started with.
pub fn run(command: Command) -> Result<(), String> {
    let Command::Run { source } = command else {
        // Remote-control commands (--reload, --quit, ...) with no running
        // instance have nothing to act on; that is not an error.
        return Ok(());
    };
    let path = config_path();
    let mut config = Config::load(&path).map_err(|e| format!("{} is invalid:\n{e}", path.display()))?;
    if let Some(source) = source {
        resolve(&source)?;
        config.source = source;
    }
    // SAFETY: initialises COM (single-threaded apartment, as WebView2 requires)
    // for this thread; balanced by CoUninitialize after all COM objects are gone.
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.ok().map_err(text)?;
    let result = run_loop(config);
    // SAFETY: `run_loop` has dropped every COM object before returning.
    unsafe { CoUninitialize() };
    result
}

fn run_loop(config: Config) -> Result<(), String> {
    desktop::register_class().map_err(text)?;
    let hwnd = create_controller().map_err(text)?;
    let hook = input::install(hwnd).map_err(text)?;
    let app = Rc::new(App {
        hwnd,
        config: RefCell::new(config),
        attachment: Cell::new(None),
        view: RefCell::new(None),
        tray: RefCell::new(log_err("tray icon", Tray::new(hwnd)).map(Rc::new)),
        forwarder: Forwarder::default(),
        commands: RefCell::new(VecDeque::new()),
        generation: Cell::new(0),
        rebuild_pending: Cell::new(false),
        pumping_input: Cell::new(false),
        covered: Cell::new(false),
        retry_in: Cell::new(0),
        crash_count: Cell::new(0),
        last_crash: Cell::new(None),
        reported: Cell::new(false),
        // SAFETY: registers (or looks up) a system-wide message id by name.
        taskbar_created: unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) },
    });
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));
    app.save_config();
    app.apply_hotkey();
    app.rebuild();

    // SAFETY: standard message loop; `message` is a live local. GetMessageW
    // returns 0 on WM_QUIT and -1 on failure, both of which end the loop.
    unsafe {
        SetTimer(Some(hwnd), TIMER_WATCHDOG, WATCHDOG_MS, None);
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }

    // Orderly shutdown: WebView first, then our shell windows, then the rest.
    app.teardown();
    desktop::refresh_wallpaper();
    app.tray.borrow_mut().take();
    APP.with(|slot| slot.borrow_mut().take());
    drop(hook);
    Ok(())
}

fn create_controller() -> windows::core::Result<HWND> {
    // SAFETY: registers a class whose procedure and name are 'static, then
    // creates a never-shown top-level window (not message-only, so that it
    // receives broadcasts such as TaskbarCreated and WM_DISPLAYCHANGE).
    unsafe {
        let hinstance: HINSTANCE = GetModuleHandleW(None)?.into();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(controller_proc),
            hInstance: hinstance,
            lpszClassName: CONTROLLER_CLASS,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err(Error::from_thread());
        }
        CreateWindowExW(
            WS_EX_TOOLWINDOW,
            CONTROLLER_CLASS,
            w!("LiveCanvas"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            None,
        )
    }
}

unsafe extern "system" fn controller_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(app) = app() else {
        // SAFETY: forwarding the unmodified arguments of this very message.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    match msg {
        WM_APP_INPUT => app.pump_input(),
        WM_APP_REBUILD => {
            app.rebuild_pending.set(false);
            app.rebuild();
        }
        WM_APP_COMMAND => loop {
            // The queue borrow ends before the command runs.
            let next = app.commands.borrow_mut().pop_front();
            match next {
                Some(command) => app.execute(command),
                None => break,
            }
        },
        // The low word of lparam is the mouse message that hit the tray icon.
        WM_APP_TRAY if matches!(lparam.0 as u32 & 0xFFFF, WM_LBUTTONUP | WM_RBUTTONUP | WM_CONTEXTMENU) => {
            app.on_menu()
        }
        WM_TIMER if wparam.0 == TIMER_WATCHDOG => app.watchdog(),
        WM_HOTKEY if wparam.0 as i32 == HOTKEY_TOGGLE => app.execute(Command::ToggleInteractive),
        WM_COPYDATA => {
            // Reply immediately (the sender is blocked) and act from the loop.
            let Some(command) = instance::decode(lparam) else { return LRESULT(0) };
            app.commands.borrow_mut().push_back(command);
            app.post(WM_APP_COMMAND);
            return LRESULT(1);
        }
        // Resolution or monitor layout changed: re-attach with fresh geometry.
        WM_DISPLAYCHANGE => app.request_rebuild(),
        WM_ENDSESSION if wparam.0 != 0 => app.teardown(),
        WM_DESTROY => {
            // SAFETY: no preconditions; ends the message loop in `run_loop`.
            unsafe { PostQuitMessage(0) }
        }
        // Explorer restarted: our tray icon and shell child windows are gone.
        _ if msg == app.taskbar_created => {
            if let Some(tray) = app.tray.borrow().clone() {
                tray.add();
            }
            app.request_rebuild();
        }
        // SAFETY: forwarding the unmodified arguments of this very message.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

impl App {
    fn post(&self, message: u32) {
        // SAFETY: posting to our own window; failure only happens during shutdown.
        let _ = unsafe { PostMessageW(Some(self.hwnd), message, WPARAM(0), LPARAM(0)) };
    }

    fn view(&self) -> Option<Rc<WebView>> {
        self.view.borrow().clone()
    }

    fn request_rebuild(&self) {
        if !self.rebuild_pending.replace(true) {
            self.post(WM_APP_REBUILD);
        }
    }

    fn update_config(&self, change: impl FnOnce(&mut Config)) {
        change(&mut self.config.borrow_mut());
        self.save_config();
    }

    fn save_config(&self) {
        let config = self.config.borrow().clone();
        log_err("save config", config.save(&config_path()));
    }

    fn apply_hotkey(&self) {
        let wanted = self.config.borrow().hotkey;
        // SAFETY: (un)registers a hotkey owned by our own window.
        unsafe {
            let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_TOGGLE);
            if wanted {
                let modifiers = MOD_CONTROL | MOD_ALT | MOD_NOREPEAT;
                log_err("register Ctrl+Alt+I", RegisterHotKey(Some(self.hwnd), HOTKEY_TOGGLE, modifiers, u32::from(b'I')));
            }
        }
    }

    /// Closes the WebView and removes our windows from the shell.
    fn teardown(&self) {
        self.generation.set(self.generation.get().wrapping_add(1));
        let view = self.view.borrow_mut().take();
        self.forwarder.reset(None);
        if let Some(view) = view {
            view.close();
        }
        if let Some(attachment) = self.attachment.take() {
            desktop::detach(&attachment);
        }
    }

    /// (Re)creates the shell windows and the WebView from the current config.
    fn rebuild(self: &Rc<Self>) {
        self.teardown();
        self.covered.set(false);
        let config = self.config.borrow().clone();
        let attachment = match desktop::attach(config.monitor_target()) {
            Ok(attachment) => attachment,
            Err(e) => {
                // The shell may still be starting; the watchdog retries.
                log(&format!("attach failed: {e}"));
                self.retry_in.set(RETRY_TICKS);
                return;
            }
        };
        self.attachment.set(Some(attachment));

        let generation = self.generation.get();
        let weak = Rc::downgrade(self);
        let ready: Ready = Rc::new(move |result| {
            if let Some(app) = weak.upgrade() {
                app.on_view_ready(generation, result);
            }
        });
        let weak = Rc::downgrade(self);
        let crashed: Crashed = Rc::new(move || {
            if let Some(app) = weak.upgrade() {
                app.on_webview_crashed();
            }
        });
        let (width, height) = (attachment.width(), attachment.height());
        if let Err(e) = webview::create(attachment.surface, width, height, config, ready, crashed) {
            self.report_webview_failure(&e);
        }
    }

    fn on_view_ready(&self, generation: u32, result: windows::core::Result<WebView>) {
        match result {
            // A rebuild happened while this WebView was being created.
            Ok(view) if generation != self.generation.get() => view.close(),
            Ok(view) => {
                let view = Rc::new(view);
                *self.view.borrow_mut() = Some(view.clone());
                self.navigate(&view);
                // A WebView rebuilt while covered must be parked immediately.
                if self.covered.get() {
                    view.set_visible(false);
                    view.suspend();
                }
            }
            Err(e) => self.report_webview_failure(&e),
        }
    }

    fn on_webview_crashed(&self) {
        self.teardown();
        let now = Instant::now();
        let recent = self.last_crash.get().is_some_and(|t| now.saturating_duration_since(t) <= CRASH_WINDOW);
        let count = if recent { self.crash_count.get().saturating_add(1) } else { 1 };
        self.crash_count.set(count);
        self.last_crash.set(Some(now));
        let delay = (1u32 << count.min(4)).min(MAX_CRASH_BACKOFF_TICKS);
        log(&format!("scheduling rebuild in {delay}s after webview crash #{count}"));
        self.retry_in.set(delay);
    }

    fn report_webview_failure(&self, error: &Error) {
        log(&format!("webview creation failed: {error}"));
        if !self.reported.replace(true) {
            message_box(
                &format!(
                    "The WebView2 runtime could not be started:\n{error}\n\nIt ships with Windows 11. If it was \
                     removed, reinstall the \"Evergreen WebView2 Runtime\" from Microsoft and restart LiveCanvas."
                ),
                true,
            );
        }
    }

    fn navigate(&self, view: &WebView) {
        let source = self.config.borrow().source.clone();
        let navigation = if source.trim().is_empty() {
            demo()
        } else {
            resolve(&source).or_else(|e| {
                log(&format!("source unusable ({e}); showing the demo instead"));
                demo()
            })
        };
        match navigation {
            Ok(navigation) => {
                log(&format!("navigating to {}", navigation.url));
                log_err("navigate", view.navigate(&navigation));
            }
            Err(e) => log(&format!("nothing to show: {e}")),
        }
    }

    fn set_source(self: &Rc<Self>, source: String) {
        if !source.trim().is_empty() {
            if let Err(e) = resolve(&source) {
                message_box(&format!("This source cannot be used:\n{e}"), true);
                return;
            }
        }
        self.crash_count.set(0);
        self.update_config(|config| config.source = source);
        match self.view() {
            Some(view) => self.navigate(&view),
            None => self.request_rebuild(),
        }
    }

    fn execute(self: &Rc<Self>, command: Command) {
        match command {
            Command::Run { source: Some(source) } => self.set_source(source),
            Command::Run { source: None } | Command::Help | Command::Version => {}
            Command::Reload => {
                if let Some(view) = self.view() {
                    view.reload();
                }
            }
            Command::Quit => {
                // SAFETY: destroying our own window from its own thread.
                let _ = unsafe { DestroyWindow(self.hwnd) };
            }
            Command::ToggleInteractive => {
                self.update_config(|config| config.interactive = !config.interactive);
                self.forwarder.reset(self.view().as_deref());
            }
            Command::SetMuted(muted) => {
                self.update_config(|config| config.muted = muted);
                if let Some(view) = self.view() {
                    view.set_muted(muted);
                }
            }
            Command::Autostart(enable) => {
                if let Err(e) = set_autostart(enable) {
                    message_box(&format!("Could not change the autostart setting:\n{e}"), true);
                }
            }
        }
    }

    /// Replays queued mouse events into the page.
    fn pump_input(&self) {
        // `send_mouse` is a cross-process COM call and may pump messages,
        // re-entering this function; the outer invocation keeps draining.
        if self.pumping_input.replace(true) {
            return;
        }
        loop {
            let events = input::take_events();
            if events.is_empty() {
                break;
            }
            let config = self.config.borrow().clone();
            let (Some(attachment), Some(view)) = (self.attachment.get(), self.view()) else { continue };
            if config.interactive && !self.covered.get() {
                for event in events {
                    self.forwarder.process(event, &attachment, &view, &config);
                }
            }
        }
        self.pumping_input.set(false);
    }

    /// Once a second: recover from explorer restarts and pause when hidden.
    fn watchdog(self: &Rc<Self>) {
        match self.attachment.get() {
            Some(attachment) if attachment.is_alive() => self.update_covered(&attachment),
            Some(_) => self.request_rebuild(),
            None => {
                let remaining = self.retry_in.get().saturating_sub(1);
                self.retry_in.set(remaining);
                if remaining == 0 {
                    self.request_rebuild();
                }
            }
        }
    }

    /// Stops rendering while a full-screen window (game, video) hides the wallpaper.
    fn update_covered(&self, attachment: &Attachment) {
        let enabled = self.config.borrow().pause_when_covered;
        // SAFETY: window queries on handles supplied by the system; `bounds`
        // is a live local.
        let covered = enabled
            && unsafe {
                let foreground = GetForegroundWindow();
                let mut bounds = RECT::default();
                !foreground.is_invalid()
                    && !attachment.is_desktop(foreground)
                    && IsWindowVisible(foreground).as_bool()
                    && !IsIconic(foreground).as_bool()
                    && GetWindowRect(foreground, &mut bounds).is_ok()
                    && bounds.left <= attachment.rect.left
                    && bounds.top <= attachment.rect.top
                    && bounds.right >= attachment.rect.right
                    && bounds.bottom >= attachment.rect.bottom
            };
        if covered != self.covered.replace(covered) {
            if let Some(view) = self.view() {
                self.forwarder.reset(Some(&view));
                // Hiding alone stops rendering but frees no memory: pair it with
                // suspend/resume so a covered wallpaper actually drops its memory.
                if covered {
                    view.set_visible(false);
                    view.suspend();
                } else {
                    view.resume();
                    view.set_visible(true);
                }
            }
        }
    }

    fn on_menu(self: &Rc<Self>) {
        let Some(tray) = self.tray.borrow().clone() else { return };
        let state = {
            let config = self.config.borrow();
            MenuState {
                interactive: config.interactive,
                keyboard: config.keyboard,
                muted: config.muted,
                devtools: config.devtools,
                autostart: autostart_enabled(),
            }
        };
        let Some(choice) = tray.show_menu(state) else { return };
        match choice {
            MenuCommand::Interactive => self.execute(Command::ToggleInteractive),
            MenuCommand::Keyboard => self.update_config(|config| config.keyboard = !config.keyboard),
            MenuCommand::Mute => self.execute(Command::SetMuted(!state.muted)),
            MenuCommand::Reload => self.execute(Command::Reload),
            MenuCommand::OpenFile => dialogs::pick_file(self.hwnd).into_iter().for_each(|path| self.set_source(path)),
            MenuCommand::OpenFolder => {
                dialogs::pick_folder(self.hwnd).into_iter().for_each(|path| self.set_source(path))
            }
            MenuCommand::PasteUrl => match dialogs::clipboard_text(self.hwnd) {
                Some(source) => self.set_source(source),
                None => message_box("The clipboard does not contain text.", true),
            },
            MenuCommand::Demo => self.set_source(String::new()),
            MenuCommand::DevTools => {
                if let Some(view) = self.view() {
                    view.open_devtools();
                }
            }
            MenuCommand::EditConfig => {
                self.save_config();
                let path = wide(&config_path().to_string_lossy());
                // SAFETY: `path` and the verb are NUL-terminated and outlive the call.
                unsafe { ShellExecuteW(None, w!("open"), PCWSTR(path.as_ptr()), None, None, SW_SHOWNORMAL) };
            }
            MenuCommand::ReloadConfig => match Config::load(&config_path()) {
                Ok(config) => {
                    *self.config.borrow_mut() = config;
                    self.apply_hotkey();
                    self.request_rebuild();
                }
                Err(e) => message_box(&format!("config.toml is invalid:\n{e}"), true),
            },
            MenuCommand::Autostart => self.execute(Command::Autostart(!state.autostart)),
            MenuCommand::Exit => self.execute(Command::Quit),
        }
    }
}

/// Unpacks the built-in demo wallpaper and returns how to navigate to it.
fn demo() -> Result<Navigation, String> {
    let dir = demo_dir();
    fs::create_dir_all(&dir).and_then(|()| fs::write(dir.join("index.html"), DEMO_HTML)).map_err(text)?;
    resolve(&dir.to_string_lossy())
}
