# LiveCanvas

An interactive HTML live wallpaper for Windows 11, written in Rust.

LiveCanvas renders a web page - a local `.html` file, a folder with `index.html`,
or any URL - **behind the desktop icons**, and forwards mouse input to it so the
page stays clickable, draggable and scrollable. It uses the WebView2 runtime that
ships with Windows 11 directly: no Electron, no Tauri runtime, no Node.js, no
administrator rights, no drivers and no code injected into `explorer.exe`.

## Verification status - read this first

This workspace was developed on a Linux host. What has and has not been verified:

| Check | Status |
| --- | --- |
| `cargo test -p livecanvas-core` (20 unit tests) | passes |
| `cargo clippy --workspace -- -D warnings` for `x86_64-pc-windows-msvc` | passes |
| Same for `aarch64-pc-windows-msvc` | passes |
| Every `unsafe` block has a `SAFETY` comment (`clippy::undocumented_unsafe_blocks` is `deny`) | enforced |
| Linking a final `.exe` with the MSVC toolchain | **not done** (no MSVC linker on the dev host) |
| Running on a real Windows 11 desktop | **not done** |

So the code is type-checked against the real `windows` 0.62 and `webview2-com`
0.39 APIs for both targets, but its runtime behaviour has not been observed.
Test first, on a real machine, the items under "Things to verify on Windows".

## Build

Requirements: Windows 10/11, stable Rust (MSVC toolchain) and the Visual Studio
Build Tools ("Desktop development with C++").

```powershell
.\build.ps1                                  # x64  -> .\dist
.\build.ps1 -Target aarch64-pc-windows-msvc  # ARM64
```

`build.ps1` builds `livecanvas.exe` first and `livecanvas-setup.exe` second,
because the installer embeds the application binary and stable Cargo cannot
express that ordering. A plain `cargo build --release` also works for
`livecanvas.exe`; if you use it for the installer, run it twice (the first run
warns that the payload was missing; the second run embeds it).

The C runtime is linked statically (`.cargo/config.toml`), so `livecanvas.exe`
is a single portable file.

## Install and run

* **Portable:** run `livecanvas.exe`. Settings go to `%LOCALAPPDATA%\LiveCanvas`.
* **Installer:** run `livecanvas-setup.exe`. It installs per user into
  `%LOCALAPPDATA%\Programs\LiveCanvas`, adds a Start Menu shortcut and an
  "Installed apps" entry, and never asks for elevation. Uninstall from Windows
  Settings or with `uninstall.exe --uninstall`.

## Usage

```text
livecanvas.exe [SOURCE]         URL, .html file, or folder with index.html
livecanvas.exe --set <SOURCE>   same; a running instance switches to it
livecanvas.exe --reload | --toggle-interactive | --mute | --unmute | --quit
livecanvas.exe --autostart <on|off>
```

The tray icon offers the same actions plus file/folder pickers, "use URL or path
from clipboard", DevTools and config editing. `Ctrl+Alt+I` toggles interactivity.

Logins persist: cookies and storage live in `%LOCALAPPDATA%\LiveCanvas\WebView2`.

## Configuration (`%LOCALAPPDATA%\LiveCanvas\config.toml`)

| Key | Default | Meaning |
| --- | --- | --- |
| `source` | `""` | URL, file or folder. Empty shows the built-in demo. |
| `monitor` | `"primary"` | `"primary"`, `"span"` (all monitors), or a zero-based index. |
| `interactive` | `true` | Forward mouse input to the page. |
| `keyboard` | `true` | Move keyboard focus into the page when it is clicked. |
| `forward_right_click` | `false` | Also forward right-clicks. |
| `muted` | `false` | Mute page audio. |
| `pause_when_covered` | `true` | Stop rendering while a full-screen window hides the wallpaper. |
| `zoom` | `1.0` | Page zoom, `0.25` to `5.0`. |
| `devtools` | `true` | Allow DevTools. |
| `user_agent` | `""` | Optional user-agent override. |
| `hotkey` | `true` | Register `Ctrl+Alt+I`. |

Use "Apply config.toml" in the tray menu after editing.

## How it works

1. **Shell layer** (`desktop.rs`): message `0x052C` makes `Progman` create the
   wallpaper layer. LiveCanvas creates a child window there - under the top-level
   `WorkerW` on Windows 10 / 11 up to 23H2, or as a layered child of `Progman`
   directly below `SHELLDLL_DefView` on 24H2 and later.
2. **Rendering** (`webview.rs`): a WebView2 *composition controller* renders the
   page into a DirectComposition visual targeted at that window.
3. **Input** (`input.rs`): the icon view covers the desktop and gets all real
   input. A low-level mouse hook only queues events; from the message loop they
   are replayed with `ICoreWebView2CompositionController::SendMouseInput` when
   the cursor is over the bare desktop. Clicks on icons are detected with MSAA
   and not forwarded. The hook never swallows input.
4. **Local pages** (`source.rs`): folders are mapped to
   `https://livecanvas.example/`, giving local pages a secure origin.
5. **Resilience** (`app.rs`): a watchdog re-attaches after `explorer.exe`
   restarts, display changes or a WebView2 process crash.

## Things to verify on Windows

Ordered by risk. These are design decisions that compile but can only be
confirmed on a real desktop:

1. **24H2 layout:** a DirectComposition target on a plain child of a
   `WS_EX_LAYERED` child of `Progman`. This mirrors how Chromium hosts its own
   D3D window, but is the least certain part.
2. **Keyboard focus:** `MoveFocus` is called after a forwarded left click and
   relies on the input-queue attachment that cross-process parenting creates.
3. **Icon hit-testing:** `AccessibleObjectFromPoint` returning
   `ROLE_SYSTEM_LISTITEM` for desktop icons.
4. **Runtime-drawn tray icon** (`CreateIconIndirect` with a 32-bit bitmap).

## Known limitations

* The mouse cursor shape does not follow the page (a hook cannot set the cursor
  for a window owned by `explorer.exe`).
* Input is observed, not intercepted: dragging on the wallpaper also draws the
  desktop's selection rectangle, and a forwarded right-click still opens the
  desktop context menu.
* Touch and pen input are not forwarded. Side mouse buttons are ignored.
* `0x052C` is undocumented. It has been stable since Windows 8 and is what other
  wallpaper engines use, but Microsoft may change it, as happened with 24H2.
* While attached, the UI thread shares an input queue with the shell; the
  process therefore aborts on panic rather than risk stalling the desktop.

## License

MIT - see `LICENSE`.
