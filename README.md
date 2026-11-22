<div align="center">

# LiveCanvas

**Interactive HTML, WebGL, and Web-App Live Wallpapers for Windows 11**

Run any local `.html` file, Three.js scene, folder with `index.html`, or website **behind your desktop icons** — fully clickable, draggable, and scrollable — powered directly by Windows' built-in WebView2 runtime in a single native Rust binary.

[![Build](https://github.com/Abinanthan-CG/livecanvas/actions/workflows/build.yml/badge.svg)](https://github.com/Abinanthan-CG/livecanvas/actions/workflows/build.yml)
![Platform](https://img.shields.io/badge/platform-Windows%2011%20%7C%2010%20(x64%20%2F%20ARM64)-0078D4?logo=windows&logoColor=white)
![Rust](https://img.shields.io/badge/written%20in-Rust-DEA584?logo=rust&logoColor=white)
![License](https://img.shields.io/badge/license-MIT-2ea44f)

[Showcase](#live-examples) · [Features](#features) · [Install](#install) · [Usage](#usage) · [Configuration](#configuration) · [Build from Source](#build-from-source)

<br>

<img src="docs/screenshots/golden_singularity_wheat_field_at_night.jpg" alt="Golden Singularity — Wheat Field at Night running as an interactive WebGL wallpaper" width="100%">

*`examples/golden_singularity_wheat_field_at_night.html` — 68,000 wind-blown wheat stalks, glowing farmhouse, and golden accretion arc rendered live in WebGL.*

</div>

## Features

- **Zero runtime bloat:** Uses the Evergreen WebView2 runtime already built into Windows 11. No Electron, no Tauri runtime, no Node.js, and statically linked CRT so `livecanvas.exe` is a single portable file.
- **Truly interactive behind desktop icons:** Forwards mouse movement, clicks, double-clicks, drag, scroll wheels, and optional keyboard focus to your wallpaper while keeping desktop icons clickable (via MSAA hit-testing) and never injecting code into `explorer.exe`.
- **Game & battery friendly:** Automatically hides and suspends the WebView2 renderer (freeing CPU, GPU, and RAM) whenever a full-screen game or video covers the desktop.
- **Real HTTPS origin for local files:** Maps local folders onto `https://livecanvas.example/` with `DENY_CORS` isolation so ES modules, sibling `fetch()`, shaders, and `localStorage` work just like a web server—and clears the mapping whenever you switch to a remote URL.
- **Persistent logins & sessions:** Cookies and storage persist in `%LOCALAPPDATA%\LiveCanvas\WebView2`, so web apps and dashboards stay signed in across reboots.
- **Resilient by design:** Re-attaches automatically after `explorer.exe` restarts or monitor layout changes, supports both classic (`WorkerW`) and Windows 11 24H2+ raised-desktop layouts, and applies exponential backoff on renderer crashes.
- **No administrator rights:** Installs per-user into `%LOCALAPPDATA%\Programs\LiveCanvas` with Start Menu and Windows Settings integration.

## Live Examples

Three interactive wallpapers ship in this repository so you can test mouse forwarding, orbit controls, scroll zoom, keyboard shortcuts, and adaptive frame-rate throttling right away.

### 1. At the Edge of Infinity (`examples/at_the_edge_of_infinity.html`)

<img src="docs/screenshots/at_the_edge_of_infinity.jpg" alt="At the Edge of Infinity — interactive Three.js orbital dust river" width="100%">

A sweeping river of 165,000 orbiting starlight particles with HDR bloom, a lone silhouetted traveler, an adaptive 60 / 30 / 15 fps idle loop, and an automatic 2D Canvas fallback.

- **Controls:** Drag to orbit · Scroll to zoom · Move mouse for parallax · Double-click or `R` to reset · `Space` to pause · Arrow keys / `+` `-` to adjust view
- **Launch:**
  ```powershell
  .\livecanvas.exe .\examples\at_the_edge_of_infinity.html
  ```

### 2. Golden Singularity — Wheat Field at Night (`examples/golden_singularity_wheat_field_at_night.html`)

<img src="docs/screenshots/golden_singularity_wheat_field_at_night.jpg" alt="Golden Singularity — Wheat Field at Night" width="100%">

68,000 instanced wind-blown wheat stalks, a warm-lit farmhouse and pickup truck, and a towering golden accretion arc with a 60 / 30 fps eco loop.

- **Controls:** Drag to orbit · Scroll to zoom · Move mouse for subtle camera parallax · Double-click to reset
- **Launch:**
  ```powershell
  .\livecanvas.exe --set .\examples\golden_singularity_wheat_field_at_night.html
  ```

> **Note:** Both Three.js examples fetch `three.min.js` (`r128`) from cdnjs on first load; WebView2 caches it in `%LOCALAPPDATA%\LiveCanvas\WebView2` afterward.

### 3. Built-in Interactive Demo (Default)

<img src="docs/screenshots/builtin_demo.png" alt="LiveCanvas built-in interactive constellation and clock demo" width="100%">

Shown automatically on first launch when no `source` is configured. Features an interactive particle constellation, live clock, pointer/click/scroll telemetry, and a keyboard input box to verify focus forwarding.

## Install

Download the latest `livecanvas-setup.exe` or portable `livecanvas.exe` from [**GitHub Releases**](https://github.com/Abinanthan-CG/livecanvas/releases) (or grab the latest CI build from the [**Actions**](https://github.com/Abinanthan-CG/livecanvas/actions/workflows/build.yml) tab).

- **Installer (`livecanvas-setup.exe`):** Installs per-user into `%LOCALAPPDATA%\Programs\LiveCanvas`, creates a Start Menu shortcut, registers in Windows **Settings → Apps → Installed apps**, and never asks for elevation. Uninstall from Windows Settings or via `uninstall.exe --uninstall`.
- **Portable (`livecanvas.exe`):** Run directly from any folder. Settings and WebView2 profile data are stored in `%LOCALAPPDATA%\LiveCanvas`.

## Usage

Right-click or left-click the **LiveCanvas** tray icon to pick an `.html` file, pick a folder with `index.html`, load a URL or path from your clipboard, toggle interactivity/keyboard focus/mute, open DevTools, or edit `config.toml`.

Press **`Ctrl+Alt+I`** anywhere to toggle mouse forwarding on or off.

### Command Line

```text
livecanvas.exe [SOURCE]         URL, .html file, or folder with index.html
livecanvas.exe --set <SOURCE>   Switch a running instance to a new source
livecanvas.exe --reload         Reload the current page
livecanvas.exe --toggle-interactive
livecanvas.exe --mute | --unmute
livecanvas.exe --autostart <on|off>
livecanvas.exe --quit           Stop the running instance
```

Bare local dev addresses such as `localhost:3000` or `127.0.0.1:5173` automatically resolve to `http://`; remote domains without a scheme default to `https://`.

## Configuration

Settings live in `%LOCALAPPDATA%\LiveCanvas\config.toml`. After editing, click **Apply config.toml** in the tray menu.

| Key | Default | Description |
| --- | --- | --- |
| `source` | `""` | URL, `.html` file, or folder with `index.html`. Empty shows the built-in demo. |
| `monitor` | `"primary"` | `"primary"`, `"span"` (all monitors), or a zero-based monitor index (e.g. `"1"`). |
| `interactive` | `true` | Forward mouse input from the bare desktop to the wallpaper. |
| `keyboard` | `true` | Move keyboard focus into the page when clicked. |
| `forward_right_click` | `false` | Also forward right-clicks (the desktop context menu still opens). |
| `muted` | `false` | Mute all audio produced by the page. |
| `pause_when_covered` | `true` | Hide and suspend the renderer while a full-screen window covers the desktop. |
| `zoom` | `1.0` | Page zoom factor (`0.25` to `5.0`). |
| `devtools` | `true` | Allow opening Chromium DevTools from the tray menu. |
| `user_agent` | `""` | Optional custom user-agent string. |
| `hotkey` | `true` | Register `Ctrl+Alt+I` to toggle interactivity. |

## Build from Source

Requirements: Windows 10/11, stable Rust (MSVC toolchain), and Visual Studio Build Tools ("Desktop development with C++").

```powershell
.\build.ps1                                  # x64  -> .\dist\livecanvas.exe + livecanvas-setup.exe
.\build.ps1 -Target aarch64-pc-windows-msvc  # ARM64
```

`build.ps1` runs `cargo test -p livecanvas-core`, compiles `livecanvas.exe`, and then compiles `livecanvas-setup.exe` with `livecanvas.exe` embedded as its payload.

<details>
<summary><strong>Architecture, Verification Status & Known Limitations</strong></summary>

### How It Works

1. **Shell layer (`desktop.rs`):** Message `0x052C` asks `Progman` to split the desktop into wallpaper and icon layers. LiveCanvas parents a child window under the wallpaper `WorkerW` on Windows 10 / 11 up to 23H2, or as a `WS_EX_LAYERED` child of `Progman` directly below `SHELLDLL_DefView` on Windows 11 24H2+.
2. **Rendering (`webview.rs`):** A WebView2 *composition controller* renders into a DirectComposition visual tree owned by our surface window.
3. **Input (`input.rs`):** A low-level mouse hook (`WH_MOUSE_LL`) queues events without swallowing them; the message loop replays events via `ICoreWebView2CompositionController::SendMouseInput` when the cursor is over the bare desktop and not over an icon (`AccessibleObjectFromPoint` / `ROLE_SYSTEM_LISTITEM`).
4. **Local isolation (`source.rs`, `webview.rs`):** Local folders map to `https://livecanvas.example/` with `COREWEBVIEW2_HOST_RESOURCE_ACCESS_KIND_DENY_CORS`, and the mapping is cleared when navigating to remote URLs.
5. **Resilience (`app.rs`, `webview.rs`):** A 1-second watchdog recovers from `explorer.exe` restarts and display changes, ignores recoverable WebView2 sub-process failures, and applies exponential backoff on repeated browser/renderer crashes.

### Verification Status

| Check | Status |
| --- | --- |
| `cargo test -p livecanvas-core` (22 unit tests) | Passes (local + Windows CI) |
| `cargo clippy --workspace -- -D warnings` (`x86_64` & `aarch64`) | Passes (enforces `clippy::undocumented_unsafe_blocks`) |
| Windows MSVC release build (`livecanvas.exe` & `livecanvas-setup.exe`) | Passes on GitHub Actions (`windows-latest`) |
| Live WebGL rendering of bundled examples & built-in demo | Verified in Chromium WebGL |
| Interactive desktop testing on Windows 11 24H2 | Recommended first-run check on target hardware |

### Known Limitations

- The mouse cursor shape does not change to match CSS cursors (a hook cannot override the cursor for an `explorer.exe`-owned window).
- Input is observed rather than swallowed: dragging on the wallpaper also draws the desktop selection rectangle, and forwarded right-clicks still open the desktop context menu.
- Touch, pen, and side mouse buttons are not forwarded.
- `0x052C` is an undocumented (though long-standing since Windows 8) shell message.
- While attached to the shell's input queue, the release binary uses `panic = "abort"` so a bug can never stall the desktop.

</details>

## License

MIT — see [`LICENSE`](LICENSE).
