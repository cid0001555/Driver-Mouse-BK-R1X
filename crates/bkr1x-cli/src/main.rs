//! `bkr1x-cli` — Command-line interface for the Fire Phoenix BK-R1X wireless gaming mouse.

mod bundle_scan;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use bkr1x_hal::profile::{format_light_mode, parse_light_mode};
use bkr1x_hal::{
    Bkr1xDevice, DaemonStatusSnapshot, DeviceError, HardwareProfile, IpcRequest, IpcResponse,
    send_ipc_request,
};
use bkr1x_protocol::{
    ChecksumStrategy, DecodedPacket, LodHeight, MacroStep, MouseConfig, PacketCodec, PollingRate,
    validate_dpi,
};
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "bkr1x-cli",
    version,
    about = "Ultra-lightweight native driver CLI for the Fire Phoenix BK-R1X (PAW3311) gaming mouse"
)]
struct Cli {
    /// Bypass `bkr1x-daemon` IPC and communicate directly with the USB/2.4GHz HID interface.
    #[arg(long, global = true)]
    direct: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Query connection mode, battery %, charging state, active DPI stage, and polling rate.
    Status {
        /// Output machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Output Waybar / Polybar custom-module JSON line.
        #[arg(long)]
        waybar: bool,
        /// Force an immediate hardware poll instead of reading the daemon's cached snapshot.
        #[arg(long)]
        refresh: bool,
    },

    /// Update PAW3311 sensor & MCU parameters on the mouse.
    Set {
        /// Set active stage DPI (`50..=18000` in steps of 50).
        #[arg(long)]
        dpi: Option<u16>,
        /// Select active DPI stage (`1..=6`).
        #[arg(long)]
        stage: Option<u8>,
        /// Comma-separated DPI stages (e.g. `800,1200,1600,3200`).
        #[arg(long)]
        stages: Option<String>,
        /// USB / 2.4GHz polling rate in Hz (`125`, `250`, `500`, `1000`).
        #[arg(long)]
        rate: Option<u16>,
        /// Button debounce time in milliseconds (`0..=30`).
        #[arg(long)]
        debounce: Option<u8>,
        /// Lift-Off Distance in millimeters (`1` or `2`).
        #[arg(long)]
        lod: Option<u8>,
        /// Enable or disable PAW3311 Motion Sync (`true`/`false`).
        #[arg(long)]
        motion_sync: Option<bool>,
        /// Enable or disable Angle Snapping / Linear Correction (`true`/`false`).
        #[arg(long)]
        angle_snap: Option<bool>,
        /// Enable or disable Ripple Control (`true`/`false`).
        #[arg(long)]
        ripple: Option<bool>,
        /// Deep-sleep timeout in minutes (`1, 3, 5, 10, 20, 30, 60`).
        #[arg(long)]
        sleep: Option<u8>,
        /// LED mode (`off`, `wave`, `neon`, `flash`, `yoyo`, `single`, `breathing`).
        #[arg(long)]
        light: Option<String>,
        /// Validate and print the generated 65-byte HID packet in hex without writing to hardware.
        #[arg(long)]
        dry_run: bool,
    },

    /// Apply or export TOML/JSON hardware profiles.
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Dry-run HID packet parser, checksum verifier, and protocol debugger.
    SniffVerify {
        /// Raw hex packet string (e.g. `"00 55 0f ae 0a ..."` or `"aa 30 ..."`).
        raw_hex: String,
        /// Checksum strategy (`none`, `sum55`, `sum00`, `xor`, `crc16`).
        #[arg(long, default_value = "none")]
        checksum: String,
    },

    /// Scan downloaded `bk-r1x.yjx2012.com` JS bundles to extract VID/PIDs and opcodes.
    AnalyzeBundle {
        /// Directory containing `.js` chunks.
        bundle_dir: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum ProfileAction {
    /// Validate and flash a `.toml` or `.json` profile to the BK-R1X.
    Apply {
        path: PathBuf,
        /// Validate and preview wire packets without writing to hardware.
        #[arg(long)]
        dry_run: bool,
    },
    /// Read current settings from the mouse (or factory template if `--template`) and save to disk.
    Export {
        path: PathBuf,
        /// Export a factory-default template without requiring hardware connected.
        #[arg(long)]
        template: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Commands::Status {
            json,
            waybar,
            refresh,
        } => cmd_status(cli.direct, json, waybar, refresh).map_err(|e| e.to_string()),
        Commands::Set {
            dpi,
            stage,
            stages,
            rate,
            debounce,
            lod,
            motion_sync,
            angle_snap,
            ripple,
            sleep,
            light,
            dry_run,
        } => cmd_set(
            cli.direct,
            SetOverrides {
                dpi,
                stage,
                stages,
                rate,
                debounce,
                lod,
                motion_sync,
                angle_snap,
                ripple,
                sleep,
                light,
                dry_run,
            },
        )
        .map_err(|e| e.to_string()),
        Commands::Profile { action } => match action {
            ProfileAction::Apply { path, dry_run } => {
                cmd_profile_apply(cli.direct, &path, dry_run).map_err(|e| e.to_string())
            }
            ProfileAction::Export { path, template } => {
                cmd_profile_export(&path, template).map_err(|e| e.to_string())
            }
        },
        Commands::SniffVerify { raw_hex, checksum } => cmd_sniff_verify(&raw_hex, &checksum),
        Commands::AnalyzeBundle { bundle_dir } => bundle_scan::scan_bundle_directory(&bundle_dir),
    }
}

fn fetch_status_snapshot(direct: bool, refresh: bool) -> Result<DaemonStatusSnapshot, DeviceError> {
    if !direct {
        let req = if refresh {
            IpcRequest::Refresh
        } else {
            IpcRequest::Status
        };
        if let Ok(IpcResponse::Ok {
            snapshot: Some(snap),
            ..
        }) = send_ipc_request(&req)
        {
            return Ok(snap);
        }
    }

    let mut dev = Bkr1xDevice::open_auto()?;
    let _ = dev.sync_all()?;
    let st = dev.state();
    let cfg = &st.config;
    Ok(DaemonStatusSnapshot {
        connected: true,
        online: st.online,
        connection_mode: dev.mode().as_str().to_owned(),
        firmware_version: st.firmware.to_string(),
        battery_percent: st.battery.percent,
        charging: st.battery.charging,
        active_dpi: cfg.active_dpi(),
        active_dpi_stage: cfg.active_dpi_stage + 1,
        dpi_stages: cfg.dpi_stages[..cfg.dpi_count as usize].to_vec(),
        polling_rate_hz: cfg.polling_rate.as_hz(),
        lod_mm: cfg.lod.as_mm(),
        debounce_ms: cfg.debounce_ms,
        motion_sync: cfg.sensor_flags.motion_sync,
        angle_snapping: cfg.sensor_flags.angle_snapping,
        ripple_control: cfg.sensor_flags.ripple_control,
        sleep_minutes: cfg.sleep_minutes,
        light_mode: format_light_mode(cfg.light_mode).to_owned(),
        active_profile: None,
    })
}

fn cmd_status(direct: bool, json: bool, waybar: bool, refresh: bool) -> Result<(), DeviceError> {
    let snap = fetch_status_snapshot(direct, refresh)?;

    if waybar {
        let icon = if snap.charging { "⚡" } else { "󰍽" };
        let class = if !snap.connected {
            "disconnected"
        } else if snap.charging {
            "charging"
        } else if snap.battery_percent <= 20 {
            "warning"
        } else {
            "normal"
        };
        let waybar_obj = serde_json::json!({
            "text": format!("{icon} {}% · {} DPI", snap.battery_percent, snap.active_dpi),
            "tooltip": format!(
                "Fire Phoenix BK-R1X ({})\nState: {}\nDPI: {} (Stage {})\nPolling: {} Hz\nLOD: {} mm | Debounce: {} ms",
                snap.connection_mode,
                if snap.online { "Online" } else { "Deep Sleep" },
                snap.active_dpi,
                snap.active_dpi_stage,
                snap.polling_rate_hz,
                snap.lod_mm,
                snap.debounce_ms
            ),
            "percentage": snap.battery_percent,
            "class": class,
        });
        println!("{waybar_obj}");
        return Ok(());
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&snap).unwrap_or_default()
        );
        return Ok(());
    }

    println!("Fire Phoenix BK-R1X — Hardware Status");
    println!("──────────────────────────────────────────────");
    println!("  Connection Mode : {}", snap.connection_mode);
    println!(
        "  MCU State       : {}",
        if snap.online {
            "Awake / Online"
        } else {
            "2.4GHz Deep Sleep (cached)"
        }
    );
    println!("  Firmware        : v{}", snap.firmware_version);
    println!(
        "  Battery         : {}% ({})",
        snap.battery_percent,
        if snap.charging {
            "Charging"
        } else {
            "Discharging"
        }
    );
    println!(
        "  Active DPI      : {} DPI (Stage {}/{}, Stages: {:?})",
        snap.active_dpi,
        snap.active_dpi_stage,
        snap.dpi_stages.len(),
        snap.dpi_stages
    );
    println!("  Polling Rate    : {} Hz", snap.polling_rate_hz);
    println!("  Lift-Off Dist   : {} mm", snap.lod_mm);
    println!("  Debounce Time   : {} ms", snap.debounce_ms);
    println!(
        "  PAW3311 Flags   : MotionSync={}  AngleSnap={}  RippleCtrl={}",
        snap.motion_sync, snap.angle_snapping, snap.ripple_control
    );
    println!(
        "  Sleep / LED     : {} min / {}",
        snap.sleep_minutes, snap.light_mode
    );
    if let Some(prof) = snap.active_profile {
        println!("  Active Profile  : {prof}");
    }
    Ok(())
}

struct SetOverrides {
    dpi: Option<u16>,
    stage: Option<u8>,
    stages: Option<String>,
    rate: Option<u16>,
    debounce: Option<u8>,
    lod: Option<u8>,
    motion_sync: Option<bool>,
    angle_snap: Option<bool>,
    ripple: Option<bool>,
    sleep: Option<u8>,
    light: Option<String>,
    dry_run: bool,
}

fn cmd_set(direct: bool, ov: SetOverrides) -> Result<(), DeviceError> {
    let mut dev_opt = if ov.dry_run {
        None
    } else {
        Bkr1xDevice::open_auto().ok()
    };

    let mut cfg = if let Some(dev) = dev_opt.as_mut() {
        dev.read_config().unwrap_or_default()
    } else {
        MouseConfig::factory_default()
    };

    if let Some(csv) = &ov.stages {
        let parsed: Result<Vec<u16>, _> = csv
            .split(',')
            .map(|s| s.trim().parse::<u16>())
            .collect();
        let list = parsed.map_err(|e| DeviceError::Profile(format!("invalid --stages CSV: {e}")))?;
        if list.is_empty() || list.len() > 6 {
            return Err(DeviceError::Profile(
                "--stages must specify between 1 and 6 DPI values".to_owned(),
            ));
        }
        for (i, &v) in list.iter().enumerate() {
            validate_dpi(i, v)?;
            cfg.dpi_stages[i] = v;
        }
        cfg.dpi_count = list.len() as u8;
        if cfg.active_dpi_stage >= cfg.dpi_count {
            cfg.active_dpi_stage = 0;
        }
    }

    if let Some(st) = ov.stage {
        if st == 0 || st > cfg.dpi_count {
            return Err(DeviceError::Profile(format!(
                "--stage {st} out of bounds for {} configured stages",
                cfg.dpi_count
            )));
        }
        cfg.active_dpi_stage = st - 1;
    }

    if let Some(dpi) = ov.dpi {
        validate_dpi(cfg.active_dpi_stage as usize, dpi)?;
        if let Some(existing_idx) = cfg.dpi_stages[..cfg.dpi_count as usize]
            .iter()
            .position(|&d| d == dpi)
            && ov.stage.is_none()
        {
            cfg.active_dpi_stage = existing_idx as u8;
        } else {
            cfg.dpi_stages[cfg.active_dpi_stage as usize] = dpi;
        }
    }

    if let Some(hz) = ov.rate {
        cfg.polling_rate = PollingRate::from_hz(hz)?;
    }
    if let Some(ms) = ov.debounce {
        cfg.debounce_ms = ms;
    }
    if let Some(mm) = ov.lod {
        cfg.lod = LodHeight::from_mm(mm)?;
    }
    if let Some(msync) = ov.motion_sync {
        cfg.sensor_flags.motion_sync = msync;
    }
    if let Some(asnap) = ov.angle_snap {
        cfg.sensor_flags.angle_snapping = asnap;
    }
    if let Some(rip) = ov.ripple {
        cfg.sensor_flags.ripple_control = rip;
    }
    if let Some(min) = ov.sleep {
        cfg.sleep_minutes = min;
    }
    if let Some(ref l) = ov.light {
        cfg.light_mode = parse_light_mode(l)?;
    }

    // Strict PAW3311 validation prior to generating or sending HID report
    let codec = PacketCodec::new(ChecksumStrategy::None);
    let rpt = codec.encode_set_mouse_config(&cfg)?;
    let wire = rpt.to_wire_bytes();

    if ov.dry_run {
        println!("[DRY-RUN] Validated PAW3311 configuration:");
        println!(
            "  DPI={} (stage {}/{}) | Rate={}Hz | LOD={}mm | Debounce={}ms | SensorFlags=0x{:02X}",
            cfg.active_dpi(),
            cfg.active_dpi_stage + 1,
            cfg.dpi_count,
            cfg.polling_rate.as_hz(),
            cfg.lod.as_mm(),
            cfg.debounce_ms,
            cfg.sensor_flags.to_bits()
        );
        println!("[DRY-RUN] 65-byte HID Output Frame:");
        println!("  {}", format_hex_slice(&wire));
        return Ok(());
    }

    // Try daemon IPC if not `--direct` and we didn't already open a direct handle
    if !direct && dev_opt.is_none() {
        let prof = HardwareProfile::from_hardware("CLI Set", &cfg, &Default::default());
        if let Ok(IpcResponse::Ok { .. }) = send_ipc_request(&IpcRequest::ApplyProfile {
            profile: Box::new(prof),
        }) {
            println!(
                "Applied settings via bkr1x-daemon: {} DPI @ {} Hz (LOD {}mm, Debounce {}ms)",
                cfg.active_dpi(),
                cfg.polling_rate.as_hz(),
                cfg.lod.as_mm(),
                cfg.debounce_ms
            );
            return Ok(());
        }
    }

    let mut dev = match dev_opt {
        Some(d) => d,
        None => Bkr1xDevice::open_auto()?,
    };
    dev.write_config(&cfg)?;
    if ov.light.is_some() {
        dev.write_light_mode(cfg.light_mode)?;
    }
    println!(
        "Flashed BK-R1X ({}) -> {} DPI @ {} Hz | LOD {}mm | Debounce {}ms",
        dev.mode().as_str(),
        cfg.active_dpi(),
        cfg.polling_rate.as_hz(),
        cfg.lod.as_mm(),
        cfg.debounce_ms
    );
    Ok(())
}

fn cmd_profile_apply(direct: bool, path: &Path, dry_run: bool) -> Result<(), DeviceError> {
    let profile = HardwareProfile::load_from_file(path)?;
    let cfg = profile.to_mouse_config()?;
    let buttons = profile.to_button_mapping()?;
    let macro_vecs = profile.to_macro_vecs()?;

    let codec = PacketCodec::new(ChecksumStrategy::None);
    let cfg_wire = codec.encode_set_mouse_config(&cfg)?.to_wire_bytes();
    let btn_wire = codec.encode_set_mouse_keys(&buttons)?.to_wire_bytes();

    if dry_run {
        println!(
            "[DRY-RUN] Profile '{}' ({}) validated successfully.",
            profile.name,
            path.display()
        );
        println!("  SET_MOUSE_CONFIG (0x0F): {}", format_hex_slice(&cfg_wire));
        if profile.buttons.is_some() {
            println!("  SET_MOUSE_KEYS   (0x09): {}", format_hex_slice(&btn_wire));
        }
        println!("  Hardware Macros        : {} sequence(s)", macro_vecs.len());
        return Ok(());
    }

    if !direct
        && let Ok(IpcResponse::Ok { message, .. }) = send_ipc_request(&IpcRequest::ApplyProfile {
            profile: Box::new(profile.clone()),
        })
    {
        println!(
            "Applied profile '{}' via daemon ({})",
            profile.name,
            message.unwrap_or_else(|| "ok".to_owned())
        );
        return Ok(());
    }

    let mut dev = Bkr1xDevice::open_auto()?;
    dev.write_config(&cfg)?;
    dev.write_light_mode(cfg.light_mode)?;
    if profile.buttons.is_some() {
        dev.write_buttons(&buttons)?;
    }
    if !macro_vecs.is_empty() {
        let refs: Vec<&[MacroStep]> = macro_vecs.iter().map(Vec::as_slice).collect();
        let bytes = dev.write_macros(&refs)?;
        println!("Flashed {bytes} bytes of hardware macro data.");
    }
    println!(
        "Applied profile '{}' directly over {}.",
        profile.name,
        dev.mode().as_str()
    );
    Ok(())
}

fn cmd_profile_export(path: &Path, template: bool) -> Result<(), DeviceError> {
    let prof = if template {
        HardwareProfile::from_hardware(
            "BK-R1X Esports 1000Hz",
            &MouseConfig::factory_default(),
            &Default::default(),
        )
    } else {
        let mut dev = Bkr1xDevice::open_auto()?;
        let _ = dev.poll_battery(Duration::ZERO);
        let cfg = dev.read_config()?;
        let btns = dev.read_buttons()?;
        HardwareProfile::from_hardware("Exported BK-R1X Profile", &cfg, &btns)
    };

    prof.save_to_file(path)?;
    println!("Saved profile '{}' -> {}", prof.name, path.display());
    Ok(())
}

fn cmd_sniff_verify(raw_hex: &str, checksum_arg: &str) -> Result<(), String> {
    let strategy = match checksum_arg.to_ascii_lowercase().as_str() {
        "none" => ChecksumStrategy::None,
        "sum55" => ChecksumStrategy::SumComplement(0x55),
        "sum00" | "twos" => ChecksumStrategy::SumComplement(0x00),
        "xor" => ChecksumStrategy::Xor,
        "crc16" => ChecksumStrategy::Crc16Ccitt,
        other => {
            return Err(format!(
                "unknown --checksum '{other}' (expected none, sum55, sum00, xor, crc16)"
            ));
        }
    };

    let bytes = parse_flexible_hex(raw_hex)?;
    println!("Parsed {} byte(s): {}", bytes.len(), format_hex_slice(&bytes));

    let codec = PacketCodec::new(strategy);
    let decoded = codec
        .decode(&bytes)
        .map_err(|e| format!("packet decode failed: {e}"))?;

    println!("Checksum ({strategy:?}) : VALID");
    match decoded {
        DecodedPacket::TxSetMouseConfig(cfg) | DecodedPacket::RxMouseConfig(cfg) => {
            let dir = if matches!(decoded, DecodedPacket::TxSetMouseConfig(_)) {
                "TX Host -> Mouse (0x0F SET_MOUSE_CONFIG)"
            } else {
                "RX Mouse -> Host (0x0E GET_MOUSE_CONFIG Reply)"
            };
            println!("Packet Type            : {dir}");
            println!(
                "Active DPI             : {} DPI (Stage {}/{})",
                cfg.active_dpi(),
                cfg.active_dpi_stage + 1,
                cfg.dpi_count
            );
            println!("All 6 DPI Stages       : {:?}", cfg.dpi_stages);
            println!("Polling Rate           : {} Hz", cfg.polling_rate.as_hz());
            println!("Lift-Off Distance      : {} mm", cfg.lod.as_mm());
            println!("Debounce Time          : {} ms", cfg.debounce_ms);
            println!(
                "PAW3311 Sensor Flags   : 0x{:02X} (MotionSync={}, AngleSnap={}, RippleCtrl={})",
                cfg.sensor_flags.to_bits(),
                cfg.sensor_flags.motion_sync,
                cfg.sensor_flags.angle_snapping,
                cfg.sensor_flags.ripple_control
            );
            println!("Sleep Timeout          : {} min", cfg.sleep_minutes);
            println!("LED Mode               : {}", cfg.light_mode.label());
            println!("PAW3311 Validation     : {:?}", cfg.validate());
        }
        DecodedPacket::TxSetMouseKeys(btns) | DecodedPacket::RxMouseKeys(btns) => {
            println!("Packet Type            : Button Mapping Frame (8 slots)");
            for (i, slot) in btns.slots.iter().enumerate() {
                println!("  Slot {i}: {slot:?} -> {:02X?}", slot.to_bytes());
            }
        }
        other => {
            println!("Decoded Packet         : {other:#?}");
        }
    }
    Ok(())
}

fn parse_flexible_hex(input: &str) -> Result<Vec<u8>, String> {
    let cleaned: String = input
        .replace("0x", " ")
        .replace("0X", " ")
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect();

    if cleaned.is_empty() {
        return Err("hex string contains no hexadecimal digits".to_owned());
    }
    if !cleaned.len().is_multiple_of(2) {
        return Err(format!(
            "hex string has odd nibble count ({} hex chars)",
            cleaned.len()
        ));
    }

    let mut out = Vec::with_capacity(cleaned.len() / 2);
    let mut i = 0;
    while i < cleaned.len() {
        let byte = u8::from_str_radix(&cleaned[i..i + 2], 16)
            .map_err(|e| format!("invalid hex byte at offset {i}: {e}"))?;
        out.push(byte);
        i += 2;
    }
    Ok(out)
}

fn format_hex_slice(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}
