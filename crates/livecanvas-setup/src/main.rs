//! LiveCanvas per-user installer / uninstaller.
//!
//! Everything is written below the current user's profile and HKCU, so no
//! elevation is ever requested (the embedded `asInvoker` manifest also stops
//! Windows' installer-detection heuristic from prompting because of the
//! "setup" file name).
//!
//!   livecanvas-setup.exe              install (asks first)
//!   livecanvas-setup.exe --quiet      install without prompts
//!   uninstall.exe --uninstall         remove (asks first; `--quiet` skips prompts)

#![cfg_attr(windows, windows_subsystem = "windows")]
#![deny(unsafe_op_in_unsafe_fn, clippy::undocumented_unsafe_blocks)]

#[cfg(not(windows))]
fn main() {
    eprintln!("The LiveCanvas installer only runs on Windows.");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quiet = args.iter().any(|a| a == "--quiet");
    let outcome = if args.iter().any(|a| a == "--uninstall") { imp::uninstall(quiet) } else { imp::install(quiet) };
    if let Err(message) = outcome {
        imp::notify(&message, true);
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod imp {
    use std::{
        env, fs,
        os::windows::process::CommandExt,
        path::{Path, PathBuf},
        process::Command,
        thread,
        time::Duration,
    };
    use windows::{
        core::{Interface, PCWSTR},
        Win32::{
            System::Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, IPersistFile, CLSCTX_INPROC_SERVER,
                COINIT_APARTMENTTHREADED,
            },
            UI::{
                Shell::{IShellLinkW, ShellLink},
                WindowsAndMessaging::{
                    MessageBoxW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO,
                    MESSAGEBOX_STYLE,
                },
            },
        },
    };
    use windows_registry::CURRENT_USER;

    /// The application binary, staged by build.rs (empty if it was missing).
    const PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin"));
    const APP: &str = "LiveCanvas";
    const VERSION: &str = env!("CARGO_PKG_VERSION");
    const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\LiveCanvas";
    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// Process creation flag: do not open a console window for helper processes.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn message(text: &str, style: MESSAGEBOX_STYLE) -> bool {
        let (text, title) = (wide(text), wide("LiveCanvas Setup"));
        // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call.
        unsafe { MessageBoxW(None, PCWSTR(text.as_ptr()), PCWSTR(title.as_ptr()), style) == IDYES }
    }

    pub fn notify(text: &str, error: bool) {
        message(text, MB_OK | if error { MB_ICONERROR } else { MB_ICONINFORMATION });
    }

    fn confirm(text: &str) -> bool {
        message(text, MB_YESNO | MB_ICONQUESTION)
    }

    fn env_dir(name: &str) -> Result<PathBuf, String> {
        env::var_os(name).map(PathBuf::from).ok_or_else(|| format!("%{name}% is not set"))
    }

    /// `%LOCALAPPDATA%\Programs\LiveCanvas` - the conventional per-user install location.
    fn install_dir() -> Result<PathBuf, String> {
        Ok(env_dir("LOCALAPPDATA")?.join("Programs").join(APP))
    }

    fn shortcut_path() -> Result<PathBuf, String> {
        Ok(env_dir("APPDATA")?.join(r"Microsoft\Windows\Start Menu\Programs").join("LiveCanvas.lnk"))
    }

    fn context<T, E: std::fmt::Display>(what: &str, result: Result<T, E>) -> Result<T, String> {
        result.map_err(|e| format!("{what}: {e}"))
    }

    /// Asks a running LiveCanvas to exit so its executable can be replaced or removed.
    fn stop_running(exe: &Path) {
        if exe.is_file() {
            // Best effort: if it is not running, `--quit` is a no-op.
            let _ = Command::new(exe).arg("--quit").creation_flags(CREATE_NO_WINDOW).status();
        }
    }

    /// Writes a file, retrying while the previous version is still being unloaded.
    fn write_with_retry(path: &Path, bytes: &[u8]) -> Result<(), String> {
        let mut last = String::new();
        for _ in 0..25 {
            match fs::write(path, bytes) {
                Ok(()) => return Ok(()),
                Err(e) => last = e.to_string(),
            }
            thread::sleep(Duration::from_millis(200));
        }
        Err(format!("could not write {}: {last}", path.display()))
    }

    /// Creates the Start Menu shortcut through the shell's IShellLink object.
    fn create_shortcut(target: &Path, link: &Path) -> Result<(), String> {
        let target_w = wide(&target.to_string_lossy());
        let folder_w = wide(&target.parent().unwrap_or(target).to_string_lossy());
        let link_w = wide(&link.to_string_lossy());
        let description = wide("Interactive HTML live wallpaper");
        // SAFETY: COM is initialised for this thread and uninitialised after
        // every interface created in between has been dropped (end of the
        // inner block). All strings are NUL-terminated and outlive the calls.
        unsafe {
            context("initialise COM", CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok())?;
            let result = (|| -> windows::core::Result<()> {
                let shell_link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                shell_link.SetPath(PCWSTR(target_w.as_ptr()))?;
                shell_link.SetWorkingDirectory(PCWSTR(folder_w.as_ptr()))?;
                shell_link.SetDescription(PCWSTR(description.as_ptr()))?;
                shell_link.cast::<IPersistFile>()?.Save(PCWSTR(link_w.as_ptr()), true)
            })();
            CoUninitialize();
            context("create Start Menu shortcut", result)
        }
    }

    pub fn install(quiet: bool) -> Result<(), String> {
        if PAYLOAD.is_empty() {
            return Err("This installer was built without livecanvas.exe and cannot install anything.\n\n\
                        Rebuild it with build.ps1 (or build `livecanvas` before `livecanvas-setup`)."
                .into());
        }
        let dir = install_dir()?;
        if !quiet && !confirm(&format!("Install LiveCanvas {VERSION} for the current user?\n\n{}", dir.display())) {
            return Ok(());
        }
        let exe = dir.join("livecanvas.exe");
        let uninstaller = dir.join("uninstall.exe");
        context("create install folder", fs::create_dir_all(&dir))?;
        stop_running(&exe);
        write_with_retry(&exe, PAYLOAD)?;
        let own_exe = context("locate the installer", env::current_exe())?;
        // When re-run from the installed uninstall.exe there is nothing to copy.
        if own_exe != uninstaller {
            context("copy the uninstaller", fs::copy(&own_exe, &uninstaller))?;
        }
        create_shortcut(&exe, &shortcut_path()?)?;

        // "Apps > Installed apps" entry, per user.
        let key = context("write uninstall information", CURRENT_USER.create(UNINSTALL_KEY))?;
        let strings = [
            ("DisplayName", APP.to_owned()),
            ("DisplayVersion", VERSION.to_owned()),
            ("Publisher", "LiveCanvas contributors".to_owned()),
            ("InstallLocation", dir.display().to_string()),
            ("DisplayIcon", exe.display().to_string()),
            ("UninstallString", format!("\"{}\" --uninstall", uninstaller.display())),
            ("QuietUninstallString", format!("\"{}\" --uninstall --quiet", uninstaller.display())),
        ];
        for (name, value) in strings {
            context("write uninstall information", key.set_string(name, value))?;
        }
        for name in ["NoModify", "NoRepair"] {
            context("write uninstall information", key.set_u32(name, 1))?;
        }

        if !quiet && confirm("Start LiveCanvas automatically when you sign in to Windows?") {
            let status = context("enable autostart", Command::new(&exe).args(["--autostart", "on"]).status())?;
            if !status.success() {
                return Err(format!("enable autostart: livecanvas.exe exited with {status}"));
            }
        }
        context("start LiveCanvas", Command::new(&exe).spawn())?;
        if !quiet {
            notify("LiveCanvas is installed and running.\n\nUse its tray icon to choose a page.", false);
        }
        Ok(())
    }

    pub fn uninstall(quiet: bool) -> Result<(), String> {
        if !quiet && !confirm("Remove LiveCanvas from this user account?") {
            return Ok(());
        }
        let dir = install_dir()?;
        stop_running(&dir.join("livecanvas.exe"));

        // Each step tolerates "already gone" so a partial uninstall can be re-run.
        if let Ok(run) = CURRENT_USER.create(RUN_KEY) {
            let _ = run.remove_value(APP);
        }
        let _ = CURRENT_USER.remove_tree(UNINSTALL_KEY);
        if let Ok(link) = shortcut_path() {
            let _ = fs::remove_file(link);
        }

        // Settings and the browser profile (saved logins) are only removed on request.
        let data = env_dir("LOCALAPPDATA")?.join(APP);
        if !quiet && data.is_dir() && confirm("Also delete LiveCanvas settings and saved website logins?") {
            // The WebView2 processes may need a moment to release the profile.
            for _ in 0..15 {
                if fs::remove_dir_all(&data).is_ok() {
                    break;
                }
                thread::sleep(Duration::from_millis(200));
            }
        }

        // A running executable cannot delete itself, so a detached command
        // processor removes the install folder a few seconds after we exit.
        // `raw_arg` is required: cmd.exe does not follow the C runtime quoting
        // rules that `arg` applies, so the command line is passed verbatim.
        // With `/C "..."`, cmd strips the outer quotes and keeps the inner ones.
        let script = format!("/C \"ping -n 4 127.0.0.1 >nul & rmdir /S /Q \"{}\"\"", dir.display());
        context(
            "schedule removal of the install folder",
            Command::new("cmd").raw_arg(script).creation_flags(CREATE_NO_WINDOW).spawn(),
        )?;
        if !quiet {
            notify("LiveCanvas has been removed.", false);
        }
        Ok(())
    }
}
