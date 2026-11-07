# Fire Phoenix BK-R1X — Native Rust Driver, Daemon, CLI & GUI

Ultra-lightweight (< 2 MB background RSS, 0.0% idle CPU) native Rust driver suite for the **Fire Phoenix BK-R1X** tri-mode wireless gaming mouse (**PixArt PAW3311** sensor, **YJX2012** OEM MCU), replacing the official Chromium WebHID cloud app (`https://bk-r1x.yjx2012.com/`).

## Extracted Hardware Protocol Summary (`bk-r1x.yjx2012.com`)

| Parameter | USB-C Wired | 2.4GHz Wireless Dongle |
| :--- | :--- | :--- |
| **Vendor ID (`VID`)** | `0xA8A4` (`43172`) | `0xA8A5` (`43173`) |
| **Product ID (`PID`)** | `0x2255` (`8789`) | `0x2255` (`8789`) |
| **HID Usage Page** | `0xFF01` (`65281`) | `0xFF01` (`65281`) |
| **HID Usage** | `0x0010` (`16`) | `0x0010` (`16`) |
| **Output Frame** | `65` bytes (`ReportID 0x00` + `64B` payload) | `65` bytes (`ReportID 0x00` + `64B` payload) |
| **Magic Headers** | TX `0x55` / RX `0xAA` | TX `0x55` / RX `0xAA` |

## Workspace Architecture

- **`crates/bkr1x-protocol`**: `#![no_std]`, zero-heap-allocation packet encoder/decoder, configurable `ChecksumStrategy` (`None`, `SumComplement`, `Xor`, `Crc16Ccitt`), 4096-byte macro compiler, and strict PAW3311 bounds validator.
- **`crates/bkr1x-hal`**: Safe cross-platform HID abstraction (`hidraw` on Linux, `hid.dll` on Windows) targeting strictly `UsagePage 0xFF00..=0xFFFF`, Bluetooth mode detection, 2.4GHz sleep handling (`150ms` timeout + backoff), TOML/JSON profile engine, and IPC transport.
- **`crates/bkr1x-daemon`**: Silent background service & native Windows System Tray ("taskbar notification area") host (`#![windows_subsystem = "windows"]`, pure Win32 `Shell_NotifyIconW`, `0.0%` idle CPU) exposing sub-millisecond JSON-line IPC (`/run/user/$UID/bkr1x.sock` on Linux, `\\.\pipe\bkr1x-daemon` on Windows) and automatic per-game profile switching.
- **`crates/bkr1x-cli`**: Fast CLI for user-profile installation (`install` / `uninstall` / `tray`), telemetry queries, direct sensor configuration, TOML/JSON profile flashing, raw hex packet verification, and static JS bundle scanning.
- **`crates/bkr1x-gui`**: On-demand reactive dark-themed native configurator built on `eframe` / `egui` (`glow` OpenGL backend) with `0.0%` idle CPU, G HUB-style **Profiles & Startup** manager, and a real-time Raw HID Wire Debugger. Exiting the window frees 100% of OpenGL/GPU memory while `bkr1x-daemon` stays resident in the taskbar tray.
- **`crates/bkr1x-installer`**: Self-contained 1-click native Windows Setup Wizard & Uninstaller (`BKR1X-Setup-v0.1.0-x64.exe`) for GitHub Releases—zero UAC prompt, registers Windows Startup, Start Menu/Desktop shortcuts, and Windows *Settings -> Installed apps*.
- **`tools/`**: WebHID Tampermonkey/DevTools sniffer (`bkr1x-webhid-sniffer.user.js`) and static JS bundle extractor (`analyze_bundle.py`).
- **`dist/linux/`**: Userspace `udev` rules (`99-bkr1x-hidraw.rules`) and `systemd --user` service unit (`bkr1x-daemon.service`).

## Windows Desktop Installation (System Tray + Auto-Start on Boot)

Like Logitech G HUB—without the 400 MB Electron/CEF overhead—`bkr1x-daemon.exe` runs silently in your Windows taskbar notification area with a live battery-colored mouse icon:

- **Left-click (or double-click) the tray icon**: Opens or focuses `bkr1x-gui.exe` immediately.
- **Close (`X`) the GUI window**: Frees 100% of GUI/OpenGL memory while keeping the lightweight tray icon active in the taskbar.
- **Right-click the tray icon**: Native quick menu for 1-click **DPI Sensitivity**, **Polling Rate**, **Saved Profiles**, **Launch at Windows Startup**, **Refresh Telemetry**, and **Quit BK-R1X**.

```powershell
# Build stripped fat-LTO release binaries
cargo build --release --workspace

# Package a standalone single-file 1-Click Setup Wizard (.exe) for GitHub Releases:
.\target\release\bkr1x-cli.exe package-installer
# -> Outputs dist\windows\BKR1X-Setup-v0.1.0-x64.exe

# Or install directly from a local build:
.\target\release\bkr1x-cli.exe install

# Manage tray & startup from CLI anytime:
.\target\release\bkr1x-cli.exe tray status
.\target\release\bkr1x-cli.exe open
.\target\release\bkr1x-cli.exe uninstall
```

## Quick Start (CLI & Direct Commands)

```bash
# Build optimized release binaries
cargo build --release --workspace

# Inspect hardware status (via daemon cache or direct HID fallback)
cargo run --release -p bkr1x-cli -- status

# Configure PAW3311 DPI, Polling Rate, Debounce, and LOD
cargo run --release -p bkr1x-cli -- set --dpi 1600 --rate 1000 --debounce 2 --lod 1

# Apply a declarative TOML hardware profile
cargo run --release -p bkr1x-cli -- profile apply profiles/esports-1000hz.toml

# Dry-run decode & verify a raw 65-byte HID packet
cargo run --release -p bkr1x-cli -- sniff-verify "00 55 0f ae 0a 2f 01 01 00 00 02 04 06 03 20 03 b0 04 40 06 80 0c 88 13 50 46"

# Launch the reactive native OpenGL configurator
cargo run --release -p bkr1x-gui
```

## Linux Non-Root Setup (`udev` + `systemd`)

```bash
sudo cp dist/linux/99-bkr1x-hidraw.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger

mkdir -p ~/.config/systemd/user
cp dist/linux/bkr1x-daemon.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now bkr1x-daemon.service
```
