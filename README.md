<div align="center">

<img src="icon.ico" alt="Kacha logo" width="128" height="128" />

# Kacha

**A macOS-style menu bar and dock that replaces the Windows taskbar. Written in Rust, drawn with plain GDI.**

![Platform](https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078D6)
![Language](https://img.shields.io/badge/language-Rust-orange)
![License](https://img.shields.io/badge/license-MIT-green)

</div>

---

## Table of Contents

- [About](#about)
- [Features](#features)
- [Tech Stack](#tech-stack)
- [Architecture](#architecture)
- [Project Structure](#project-structure)
- [Getting Started](#getting-started)
- [Configuration](#configuration)
- [Security](#security)
- [How to Contribute?](#how-to-contribute)
- [License](#license)
- [Acknowledgements](#acknowledgements)
- [Author](#author)

---

## About

Kacha (binary name: `kaca-shell.exe`) is a lightweight desktop shell layer for Windows. It hides the stock taskbar and puts a translucent acrylic menu bar at the top of the screen and an auto-hiding, magnifying dock at the bottom.

It is built without any UI framework. Everything is drawn with GDI into double-buffered surfaces, and the dock resamples icons itself for sharp zooming. Windows are created only when they are needed, and timers run only while something is animating, so the shell stays quiet when idle.

---

## Features

**Menu bar**

- Acrylic bar registered as an AppBar on the top edge, so maximized windows never cover it.
- Logo menu with About, Settings, Start, Task Manager, Sleep, Restart, Shut Down, Lock and Quit.
- Name of the active application.
- Volume (click to mute, scroll to change, right-click for sound settings), Wi-Fi panel, battery with charging state, and clock with date.
- Tray arrow that opens the hidden notification area icons, including Safely Remove Hardware for ejecting external drives.
- Calendar and Wi-Fi dropdown panels that slide out from the bar.

**Dock**

- Centered at the bottom of the screen with smooth icon magnification.
- Pinned apps are read from the Windows taskbar pins, and running apps appear alongside them with a running indicator.
- Name label on hover.
- Auto-hide: the dock is fully visible only on an empty desktop, and slides up when the cursor touches the bottom edge.
- High-quality icons fetched at 128/256 px and resampled with mipmaps and bilinear filtering.
- Glass backdrop on Windows 11 22H2+, solid fallback color on older builds.

**Shell integration**

- Hides the original taskbar on every monitor and re-hides it if Windows brings it back.
- Restores the original taskbar state on exit, on panic, or with a recovery command.
- Handles `explorer.exe` restarts.
- Steps back for fullscreen apps and stays above overlay windows such as screen recorders.
- Single instance only.

---

## Tech Stack

| Area              | Technology                                                                    |
| ----------------- | ----------------------------------------------------------------------------- |
| Language          | Rust (2021 edition)                                                           |
| Windows API       | [`windows`](https://crates.io/crates/windows) crate (Win32)                   |
| Rendering         | GDI with double buffering                                                     |
| Visual effects    | Acrylic via `SetWindowCompositionAttribute`, DWM backdrop and rounded corners |
| Shell integration | AppBar API (`SHAppBarMessage`), Shell COM interfaces                          |
| Target            | Windows 10 and Windows 11                                                     |

---

## Architecture

Kacha runs on a single UI thread with one message loop. There are no background threads.

```
main.rs ──► taskbar::hide()        hide stock taskbar, save its state
        └─► bar::run()             create menu bar, start the message loop
              ├─ panel.rs          dropdown panels (Apple menu, calendar, Wi-Fi)
              ├─ dock.rs           dock, label tooltip and bottom-edge hotspot
              └─ 500 ms timer      clock, battery, active app, dock refresh,
                                   keep taskbar hidden, keep shell on top
        └─► taskbar::restore()     bring the stock taskbar back on exit
```

- **Menu bar** (`bar.rs`) owns the main window, the AppBar registration, the timer and the hit testing.
- **Dock** (`dock.rs`) is made of three windows: the dock itself, a small label window and a thin hotspot window along the bottom edge.
- **Panels** (`panel.rs`) exist only while open, so they cost no timer or memory when closed.
- **Taskbar control** (`taskbar.rs`) hides the stock taskbar with `ABM_SETSTATE` and `SW_HIDE`, and drives the tray overflow popup.

---

## Project Structure

```
.
├── icon.ico            Application icon
├── Cargo.toml
└── src/
    ├── main.rs         Entry point, single instance guard, panic hook
    ├── bar.rs          Menu bar, AppBar registration, acrylic, input handling
    ├── dock.rs         Dock, magnification, auto-hide, running app tracking
    ├── panel.rs        Apple menu, calendar and Wi-Fi dropdown panels
    ├── taskbar.rs      Hide and restore the stock taskbar, tray overflow popup
    ├── apps.rs         Application list used by the panels
    ├── status.rs       Clock, battery and foreground app info
    ├── wifi.rs         Wi-Fi networks and connection state
    └── fonts.rs        Font loading
```

---

## Getting Started

### Prerequisites

- Windows 10 or Windows 11
- [Rust](https://www.rust-lang.org/tools/install) (stable toolchain, MSVC target)
- Visual Studio Build Tools with the "Desktop development with C++" workload

### Build

```powershell
git clone https://github.com/<your-username>/kacha.git
cd kacha
cargo build --release
```

The executable is created at `target\release\kaca-shell.exe`.

### Run

```powershell
.\target\release\kaca-shell.exe
```

### Quit and recovery

- Press **Ctrl + Alt + Q** to quit. The stock taskbar is restored automatically.
- If the app crashes or is killed and the taskbar stays hidden, run:

```powershell
.\kaca-shell.exe --restore
```

---

## Configuration

Kacha is configured through constants at the top of the source files. Edit them and rebuild.

| File       | Constant                              | Purpose                                                        |
| ---------- | ------------------------------------- | -------------------------------------------------------------- |
| `bar.rs`   | `BAR_HEIGHT`                          | Menu bar height in pixels at 96 DPI                            |
| `bar.rs`   | `TINT`                                | Acrylic tint color, `0xAABBGGRR`                               |
| `bar.rs`   | `ACCENT_STATE`                        | `4` for acrylic, `3` for plain blur                            |
| `dock.rs`  | `BASE_ICON`, `MAX_ZOOM`, `ZOOM_RANGE` | Icon size and magnification                                    |
| `dock.rs`  | `SOLID_BG`                            | Dock color when glass is not available                         |
| `dock.rs`  | `HOTSPOT_H`, `HIDE_DELAY_MS`          | Auto-hide trigger height and delay                             |
| `panel.rs` | `PANEL_TINT`                          | Panel tint color                                               |
| `panel.rs` | `WEEK_START`                          | First day of the week in the calendar (`0` Sunday, `1` Monday) |

Pinned dock apps come from `%AppData%\Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar`, so pin or unpin apps the usual Windows way.

---

## Security

- Kacha does not make network connections and does not collect or send any data.
- It does not need administrator rights.
- It only reads the current user's pinned taskbar shortcuts and the state of running windows.
- Power actions (restart, shut down) ask for confirmation first.

If you find a vulnerability, please open a private security advisory on the repository instead of a public issue.

---

## How to Contribute?

1. Fork the repository.
2. Create a branch: `git checkout -b feature/my-change`
3. Make your changes and make sure `cargo build --release` and `cargo clippy` pass.
4. Commit with a clear message and push the branch.
5. Open a pull request describing what changed and why.

Bug reports and feature ideas are welcome as issues. Please include your Windows version and, for visual bugs, a screenshot.

---

## License

This project is licensed under the [MIT License](LICENSE).

---

## Acknowledgements

- The [`windows`](https://github.com/microsoft/windows-rs) crate by Microsoft, for the Rust bindings to the Win32 API.
- macOS, for the design inspiration behind the menu bar and the dock.

---

## Author

**[Commonly Works]**

- GitHub: [@Commonly Works](https://github.com/commonlyworks)
