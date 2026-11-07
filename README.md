# Fire Phoenix BK-R1X Driver

Lightweight native desktop driver and configurator for the **Fire Phoenix BK-R1X** wireless gaming mouse (`< 2 MB` RAM, `0%` idle CPU), replacing the official browser-based WebHID app.

> **Note:** Connect your mouse via **USB-C cable** or the **2.4GHz wireless dongle** to configure settings (Bluetooth mode uses standard OS HID only).

---

## 1. Install via Setup Wizard (Recommended)

### Windows
1. Download **`BKR1X-Setup-v0.1.0-x64.exe`** from the [**Releases**](https://github.com/cid0001555/Driver-Mouse-BK-R1X/releases/latest) page.
2. Double-click it and click **Install Fire Phoenix BK-R1X** (no Administrator / UAC prompt needed).
3. To uninstall later, use **Windows Settings → Apps → Installed apps → Fire Phoenix BK-R1X Driver**.

### Linux (`x86_64`)
Download `BKR1X-v0.1.0-linux-x86_64.tar.gz` from [**Releases**](https://github.com/cid0001555/Driver-Mouse-BK-R1X/releases/latest), extract it, and install the non-root `udev` rule:

```bash
tar -xzf BKR1X-v0.1.0-linux-x86_64.tar.gz && cd bkr1x-linux-x86_64
install -Dm755 bkr1x-daemon bkr1x-gui bkr1x-cli -t ~/.local/bin/
sudo cp dist/linux/99-bkr1x-hidraw.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
```

---

## How to Use

- **System Tray Icon:** Runs silently in your taskbar notification area (`^` arrow near the clock on Windows 11—drag the mouse icon onto your taskbar to keep it pinned).
- **Left-click the tray icon:** Opens the BK-R1X Configurator window (`DPI`, `Polling Rate`, `LOD`, `Debounce`, `Buttons & Macros`, `Profiles`).
- **Right-click the tray icon:** Quick menu for 1-click DPI, Polling Rate, Profile switching, and Windows Startup toggle.
- **Close (`X`) the window:** Frees 100% of UI memory while keeping the background tray icon active.

---

## 2. Build from Source

Requires the [Rust stable toolchain](https://rustup.rs/).

### Windows (PowerShell)
```powershell
# Compile release binaries
cargo build --release --workspace

# Run the GUI directly without installing
.\target\release\bkr1x-gui.exe

# Or install into your user profile + Start Menu + Windows Startup
.\target\release\bkr1x-cli.exe install

# Optional: package the standalone Setup Wizard (.exe) into dist\windows\
.\target\release\bkr1x-cli.exe package-installer
```

### Linux
```bash
# Install system headers (Debian / Ubuntu)
sudo apt-get install -y pkg-config libudev-dev libx11-dev libxcb-shape0-dev \
  libxcb-xfixes0-dev libxkbcommon-dev libwayland-dev libgl1-mesa-dev

# Build and launch the GUI
cargo build --release -p bkr1x-daemon -p bkr1x-cli -p bkr1x-gui
./target/release/bkr1x-gui
```

### Optional CLI Commands
```bash
bkr1x-cli status
bkr1x-cli set --dpi 1600 --rate 1000 --debounce 2 --lod 1
bkr1x-cli profile apply profiles/esports-1000hz.toml
```
