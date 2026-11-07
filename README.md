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
- **`crates/bkr1x-daemon`**: Zero-tokio event-driven background service exposing sub-millisecond JSON-line IPC (`/run/user/$UID/bkr1x.sock` on Linux, `\\.\pipe\bkr1x-daemon` on Windows) and optional foreground per-application profile switching.
- **`crates/bkr1x-cli`**: Fast CLI for telemetry queries, direct sensor configuration, TOML/JSON profile flashing, raw hex packet verification, and static JS bundle scanning.
- **`crates/bkr1x-gui`**: Reactive dark-themed native configurator built on `eframe` / `egui` (`glow` OpenGL backend) with 0.0% idle CPU and a real-time Raw HID Wire Debugger.
- **`tools/`**: WebHID Tampermonkey/DevTools sniffer (`bkr1x-webhid-sniffer.user.js`) and static JS bundle extractor (`analyze_bundle.py`).
- **`dist/linux/`**: Userspace `udev` rules (`99-bkr1x-hidraw.rules`) and `systemd --user` service unit (`bkr1x-daemon.service`).

## Quick Start

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
