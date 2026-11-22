//! WebView2 in *visual hosting* mode on top of DirectComposition.
//!
//! A windowed WebView2 only reacts to real window messages, and a window that
//! sits below the desktop icons never receives any. The composition controller
//! solves this properly: the page renders into a DirectComposition visual we
//! own, and input is injected through the supported `SendMouseInput` API
//! instead of posting fake messages to Chromium's internal windows.
//!
//! Everything here runs on the UI thread (a single-threaded COM apartment), so
//! the COM interface pointers are never shared across threads.

use crate::util::{log, log_err, profile_dir, wide};
use livecanvas_core::{Config, Navigation, VIRTUAL_HOST};
use std::rc::Rc;
use webview2_com::{
    CoreWebView2EnvironmentOptions, CreateCoreWebView2CompositionControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, Microsoft::Web::WebView2::Win32::*,
    NavigationCompletedEventHandler, ProcessFailedEventHandler, TrySuspendCompletedHandler,
};
use windows::{
    core::{Error, Interface, Result, PCWSTR},
    Win32::{
        Foundation::{E_POINTER, HWND, POINT, RECT},
        Graphics::DirectComposition::{
            DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
        },
    },
};

/// Chromium switches: let video/audio wallpapers start without a click.
const BROWSER_ARGUMENTS: &str = "--autoplay-policy=no-user-gesture-required";

/// Receives the finished [`WebView`] (or the reason creation failed).
pub type Ready = Rc<dyn Fn(Result<WebView>)>;
/// Invoked when the browser or renderer process dies.
pub type Crashed = Rc<dyn Fn()>;

/// A live WebView2 instance and the composition objects it renders into.
pub struct WebView {
    controller: ICoreWebView2Controller,
    composition: ICoreWebView2CompositionController,
    core: ICoreWebView2,
    // The visual tree must stay alive for as long as the WebView renders into
    // it. Field order matters: fields drop top to bottom, so the WebView
    // interfaces above are released before the composition objects below.
    _root: IDCompositionVisual,
    _target: IDCompositionTarget,
    _device: IDCompositionDevice,
}

/// Starts asynchronous creation of a WebView that renders into `surface`.
/// `ready` is called later on the UI thread, from the message loop.
pub fn create(surface: HWND, width: i32, height: i32, config: Config, ready: Ready, crashed: Crashed) -> Result<()> {
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: the setter only stores the string inside the options object.
    unsafe { options.set_additional_browser_arguments(BROWSER_ARGUMENTS.to_owned()) };
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let profile = wide(&profile_dir().to_string_lossy());

    let on_environment = {
        let ready = ready.clone();
        CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |status, environment| {
            let started = status
                .and_then(|()| environment.ok_or_else(|| Error::from(E_POINTER)))
                .and_then(|env| create_controller(&env, surface, width, height, config, ready.clone(), crashed));
            if let Err(e) = started {
                ready(Err(e));
            }
            Ok(())
        }))
    };
    // SAFETY: `profile` is NUL-terminated and outlives the call (WebView2
    // copies the path); a null browser folder selects the installed Evergreen
    // runtime; both interface references are valid for the call.
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), PCWSTR(profile.as_ptr()), &options, &on_environment)
    }
}

fn create_controller(
    environment: &ICoreWebView2Environment,
    surface: HWND,
    width: i32,
    height: i32,
    config: Config,
    ready: Ready,
    crashed: Crashed,
) -> Result<()> {
    // Composition hosting was added in ICoreWebView2Environment3 (runtime 86+).
    let environment: ICoreWebView2Environment3 = environment.cast()?;
    let on_controller =
        CreateCoreWebView2CompositionControllerCompletedHandler::create(Box::new(move |status, composition| {
            ready(
                status
                    .and_then(|()| composition.ok_or_else(|| Error::from(E_POINTER)))
                    .and_then(|composition| WebView::build(composition, surface, width, height, &config, crashed)),
            );
            Ok(())
        }));
    // SAFETY: `surface` is a live window owned by this thread and the handler
    // reference is valid for the call (WebView2 AddRefs what it keeps).
    unsafe { environment.CreateCoreWebView2CompositionController(surface, &on_controller) }
}

impl WebView {
    fn build(
        composition: ICoreWebView2CompositionController,
        surface: HWND,
        width: i32,
        height: i32,
        config: &Config,
        crashed: Crashed,
    ) -> Result<Self> {
        // SAFETY: straightforward COM calls on interfaces we hold strong
        // references to, all on the creating (UI) thread. `surface` is a live
        // window of this thread. Wide strings outlive the calls they are
        // passed to. Out-parameters point to live locals.
        unsafe {
            // Visual tree: device -> target(surface HWND) -> root <- WebView2.
            let device: IDCompositionDevice = DCompositionCreateDevice(None)?;
            let target = device.CreateTargetForHwnd(surface, true)?;
            let root = device.CreateVisual()?;
            target.SetRoot(&root)?;
            composition.SetRootVisualTarget(&root)?;
            device.Commit()?;

            let controller: ICoreWebView2Controller = composition.cast()?;
            controller.SetBounds(RECT { left: 0, top: 0, right: width, bottom: height })?;
            controller.SetZoomFactor(config.zoom)?;
            // Opaque black until the page paints, instead of a white flash.
            controller
                .cast::<ICoreWebView2Controller2>()?
                .SetDefaultBackgroundColor(COREWEBVIEW2_COLOR { A: 255, R: 0, G: 0, B: 0 })?;

            let core = controller.CoreWebView2()?;
            let settings = core.Settings()?;
            settings.SetIsStatusBarEnabled(false)?;
            settings.SetIsZoomControlEnabled(false)?;
            settings.SetAreDevToolsEnabled(config.devtools)?;
            // A page context menu only makes sense when right-clicks reach the page.
            settings.SetAreDefaultContextMenusEnabled(config.forward_right_click)?;
            if !config.user_agent.is_empty() {
                let agent = wide(&config.user_agent);
                settings.cast::<ICoreWebView2Settings2>()?.SetUserAgent(PCWSTR(agent.as_ptr()))?;
            }

            let mut token = 0i64;
            core.add_ProcessFailed(
                &ProcessFailedEventHandler::create(Box::new(move |_, args| {
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                    if let Some(args) = args {
                        let _ = args.ProcessFailedKind(&mut kind);
                    }
                    log(&format!("webview process failed (kind={})", kind.0));
                    if matches!(
                        kind,
                        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED
                            | COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED
                            | COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE
                    ) {
                        crashed();
                    }
                    Ok(())
                })),
                &mut token,
            )?;
            core.add_NavigationCompleted(
                &NavigationCompletedEventHandler::create(Box::new(|_, args| {
                    if let Some(args) = args {
                        let mut success = Default::default();
                        args.IsSuccess(&mut success)?;
                        if !success.as_bool() {
                            let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
                            args.WebErrorStatus(&mut status)?;
                            log(&format!("navigation failed: web error status {}", status.0));
                        }
                    }
                    Ok(())
                })),
                &mut token,
            )?;

            controller.SetIsVisible(true)?;
            let view = Self { controller, composition, core, _root: root, _target: target, _device: device };
            view.set_muted(config.muted);
            Ok(view)
        }
    }

    /// Navigates to a resolved source, mapping its local folder first if needed.
    pub fn navigate(&self, navigation: &Navigation) -> Result<()> {
        self.resume(); // a suspended renderer must wake up before it can navigate
        let url = wide(&navigation.url);
        // SAFETY: COM calls on the UI thread; the wide strings outlive the calls.
        unsafe {
            let core3 = self.core.cast::<ICoreWebView2_3>()?;
            if let Some(mapping) = &navigation.mapping {
                let host = wide(mapping.host);
                let folder = wide(&mapping.folder.to_string_lossy());
                core3.SetVirtualHostNameToFolderMapping(
                    PCWSTR(host.as_ptr()),
                    PCWSTR(folder.as_ptr()),
                    COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS,
                )?;
            } else {
                // Remove any previous local-folder mapping so remote pages cannot access it.
                let host = wide(VIRTUAL_HOST);
                let _ = core3.ClearVirtualHostNameToFolderMapping(PCWSTR(host.as_ptr()));
            }
            self.core.Navigate(PCWSTR(url.as_ptr()))
        }
    }

    pub fn reload(&self) {
        self.resume(); // reloading wakes a suspended renderer too
        // SAFETY: COM call on the UI thread on a live interface.
        log_err("reload", unsafe { self.core.Reload() });
    }

    pub fn set_muted(&self, muted: bool) {
        // SAFETY: COM calls on the UI thread on live interfaces.
        let result = unsafe { self.core.cast::<ICoreWebView2_8>().and_then(|core| core.SetIsMuted(muted)) };
        log_err("set muted", result);
    }

    /// Hiding the controller makes Chromium stop rendering and throttle timers.
    pub fn set_visible(&self, visible: bool) {
        // SAFETY: COM call on the UI thread on a live interface.
        log_err("set visible", unsafe { self.controller.SetIsVisible(visible) });
    }

    /// Suspends the renderer: scripts pause and Chromium drops renderer memory
    /// (used while a full-screen window hides the wallpaper). Pair with
    /// [`resume`](Self::resume); navigation and reload resume automatically.
    pub fn suspend(&self) {
        let handler = TrySuspendCompletedHandler::create(Box::new(|status, succeeded| {
            if let Err(e) = status {
                log(&format!("suspend failed: {e}"));
            } else if !succeeded {
                log("suspend declined (is devtools open?)");
            }
            Ok(())
        }));
        // SAFETY: COM call on the UI thread on a live interface; the callee
        // keeps a reference to `handler` until the async completion fires.
        let result =
            unsafe { self.core.cast::<ICoreWebView2_6>().and_then(|core| core.TrySuspend(&handler)) };
        log_err("suspend", result);
    }

    /// Wakes the renderer back up after [`suspend`](Self::suspend). Calling
    /// this on a webview that was never suspended is harmless.
    pub fn resume(&self) {
        // SAFETY: COM call on the UI thread on a live interface.
        log_err("resume", unsafe { self.core.cast::<ICoreWebView2_6>().and_then(|core| core.Resume()) });
    }

    pub fn open_devtools(&self) {
        // SAFETY: COM call on the UI thread on a live interface.
        log_err("open devtools", unsafe { self.core.OpenDevToolsWindow() });
    }

    /// Moves keyboard focus into the page so typing reaches it.
    pub fn focus(&self) {
        // SAFETY: COM call on the UI thread on a live interface.
        log_err("focus", unsafe { self.controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) });
    }

    /// Injects one mouse event. `point` is in surface client pixels.
    pub fn send_mouse(
        &self,
        kind: COREWEBVIEW2_MOUSE_EVENT_KIND,
        keys: COREWEBVIEW2_MOUSE_EVENT_VIRTUAL_KEYS,
        data: u32,
        point: POINT,
    ) {
        // SAFETY: COM call on the UI thread; all arguments are plain values.
        if let Err(e) = unsafe { self.composition.SendMouseInput(kind, keys, data, point) } {
            log(&format!("SendMouseInput({}) failed: {e}", kind.0));
        }
    }

    /// Shuts the WebView down; its browser processes exit asynchronously.
    pub fn close(&self) {
        // SAFETY: COM call on the UI thread on a live interface. Calls made
        // after Close simply fail with an HRESULT, which every method here logs.
        log_err("close webview", unsafe { self.controller.Close() });
    }
}
