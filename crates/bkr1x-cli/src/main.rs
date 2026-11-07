mod bundle_scan;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use bkr1x_hal::profile::{format_light_mode, parse_light_mode};
use bkr1x_hal::{
    Bkr1xDevice, DaemonStatusSnapshot, DeviceError, HardwareProfile, IpcRequest, IpcResponse,
    decode_flexible_hex, encode_hex_spaced, extract_sfx_from_exe, install_user_application,
    is_autostart_enabled, is_daemon_running, pack_sfx_installer, platform_exe_name,
    resolve_sibling_binary, send_ipc_request, set_autostart_enabled, uninstall_user_application,
};
use bkr1x_protocol::{
    ChecksumStrategy, DecodedPacket, LodHeight, MAX_DPI_STAGES, MacroStep, MouseConfig,
    PacketCodec, PollingRate, ScrollDirection, SensorFlags, validate_dpi,
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
        #[arg(long)]
        json: bool,
        #[arg(long)]
        waybar: bool,
        #[arg(long)]
        refresh: bool,
    },

    /// Update PAW3311 sensor & MCU parameters on the mouse.
    Set {
        #[arg(long)]
        dpi: Option<u16>,
        #[arg(long)]
        stage: Option<u8>,
        #[arg(long)]
        stages: Option<String>,
        #[arg(long)]
        rate: Option<u16>,
        #[arg(long)]
        debounce: Option<u8>,
        #[arg(long)]
        lod: Option<u8>,
        #[arg(long)]
        motion_sync: Option<bool>,
        #[arg(long)]
        angle_snap: Option<bool>,
        #[arg(long)]
        ripple: Option<bool>,
        #[arg(long)]
        sleep: Option<u8>,
        #[arg(long)]
        light: Option<String>,
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
        raw_hex: String,
        #[arg(long, default_value = "none")]
        checksum: String,
    },

    /// Scan downloaded `bk-r1x.yjx2012.com` JS bundles to extract VID/PIDs and opcodes.
    AnalyzeBundle { bundle_dir: PathBuf },

    /// Install BK-R1X into your user profile, create Start Menu shortcut, register Windows Startup, and launch the System Tray icon.
    Install {
        #[arg(long)]
        no_autostart: bool,
        #[arg(long)]
        no_start_tray: bool,
    },

    /// Stop the System Tray daemon and remove Windows Startup + Start Menu entries.
    Uninstall,

    /// Launch or focus the native BK-R1X GUI configurator window.
    Open,

    /// Control the background System Tray daemon (`start`, `stop`, `status`, `autostart`).
    Tray {
        #[command(subcommand)]
        action: TrayAction,
    },

    /// Bundle `bkr1x-installer`, `bkr1x-daemon`, `bkr1x-gui`, and `bkr1x-cli` into a standalone GitHub Release `.exe` Setup Wizard.
    PackageInstaller {
        #[arg(long)]
        bin_dir: Option<PathBuf>,
        #[arg(long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum TrayAction {
    Start,
    Stop,
    Status,
    Autostart {
        #[arg(value_parser = ["on", "off", "true", "false"])]
        state: String,
    },
}

#[derive(Subcommand, Debug)]
enum ProfileAction {
    Apply {
        path: PathBuf,
        #[arg(long)]
        dry_run: bool,
    },
    Export {
        path: PathBuf,
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
        Commands::Install {
            no_autostart,
            no_start_tray,
        } => cmd_install(!no_autostart, !no_start_tray),
        Commands::Uninstall => cmd_uninstall(),
        Commands::Open => cmd_open(),
        Commands::Tray { action } => cmd_tray(action),
        Commands::PackageInstaller { bin_dir, output } => {
            cmd_package_installer(bin_dir.as_deref(), output.as_deref())
        }
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
            return Ok(*snap);
        }
    }

    let mut dev = Bkr1xDevice::open_auto()?;
    let _ = dev.sync_all()?;
    let info = dev.info().clone();
    let st = dev.state();
    let cfg = &st.config;
    let raw_buttons: [[u8; 4]; 8] = core::array::from_fn(|idx| st.buttons.slots[idx].to_bytes());
    Ok(DaemonStatusSnapshot {
        connected: true,
        online: st.online,
        connection_mode: dev.mode().as_str().to_owned(),
        vendor_id: info.vendor_id,
        product_id: info.product_id,
        product_name: info.product_name,
        firmware_version: st.firmware.to_string(),
        battery_percent: st.battery.percent,
        charging: st.battery.charging,
        active_dpi: cfg.active_dpi(),
        active_dpi_stage: cfg.active_dpi_stage.saturating_add(1),
        dpi_stages: cfg
            .dpi_stages
            .get(..usize::from(cfg.dpi_count))
            .unwrap_or(&cfg.dpi_stages)
            .to_vec(),
        polling_rate_hz: cfg.polling_rate.as_hz(),
        lod_mm: cfg.lod.as_mm(),
        debounce_ms: cfg.debounce_ms,
        motion_sync: cfg.sensor_flags.motion_sync,
        angle_snapping: cfg.sensor_flags.angle_snapping,
        ripple_control: cfg.sensor_flags.ripple_control,
        reverse_scroll: cfg.scroll_direction == ScrollDirection::Reverse,
        highspeed_mode: cfg.highspeed_mode,
        wakeup_on_move: cfg.wakeup_on_move,
        move_lights_off: cfg.move_lights_off,
        sleep_minutes: cfg.sleep_minutes,
        light_mode: format_light_mode(cfg.light_mode).to_owned(),
        active_profile: None,
        button_slots: Some(raw_buttons),
    })
}

fn mouse_config_from_snapshot(snap: &DaemonStatusSnapshot) -> MouseConfig {
    let mut dpi_stages = MouseConfig::factory_default().dpi_stages;
    for (idx, &dpi) in snap.dpi_stages.iter().take(MAX_DPI_STAGES).enumerate() {
        dpi_stages[idx] = dpi;
    }
    let max_stages = MAX_DPI_STAGES as u8;
    let dpi_count = u8::try_from(snap.dpi_stages.len())
        .unwrap_or(max_stages)
        .clamp(1, max_stages);
    let active_dpi_stage = snap.active_dpi_stage.saturating_sub(1).min(dpi_count - 1);

    MouseConfig {
        dpi_stages,
        dpi_count,
        active_dpi_stage,
        polling_rate: PollingRate::from_hz(snap.polling_rate_hz).unwrap_or(PollingRate::Hz1000),
        lod: LodHeight::from_mm(snap.lod_mm).unwrap_or(LodHeight::Mm1),
        debounce_ms: snap.debounce_ms,
        sensor_flags: SensorFlags {
            ripple_control: snap.ripple_control,
            angle_snapping: snap.angle_snapping,
            motion_sync: snap.motion_sync,
            reserved_bits: SensorFlags::BIT_FACTORY_BASE,
        },
        sleep_minutes: snap.sleep_minutes,
        light_mode: parse_light_mode(&snap.light_mode).unwrap_or_default(),
        scroll_direction: if snap.reverse_scroll {
            ScrollDirection::Reverse
        } else {
            ScrollDirection::Normal
        },
        highspeed_mode: snap.highspeed_mode,
        wakeup_on_move: snap.wakeup_on_move,
        move_lights_off: snap.move_lights_off,
    }
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

fn apply_cli_overrides(cfg: &mut MouseConfig, ov: &SetOverrides) -> Result<(), DeviceError> {
    if let Some(csv) = &ov.stages {
        let parsed: Result<Vec<u16>, _> = csv.split(',').map(|s| s.trim().parse::<u16>()).collect();
        let list =
            parsed.map_err(|e| DeviceError::Profile(format!("invalid --stages CSV: {e}")))?;
        if list.is_empty() || list.len() > MAX_DPI_STAGES {
            return Err(DeviceError::Profile(
                "--stages must specify between 1 and 6 DPI values".to_owned(),
            ));
        }
        for (idx, &dpi) in list.iter().enumerate() {
            validate_dpi(idx, dpi)?;
            cfg.dpi_stages[idx] = dpi;
        }
        cfg.dpi_count = list.len() as u8;
        if cfg.active_dpi_stage >= cfg.dpi_count {
            cfg.active_dpi_stage = 0;
        }
    }

    if let Some(stage_one_based) = ov.stage {
        if stage_one_based == 0 || stage_one_based > cfg.dpi_count {
            return Err(DeviceError::Profile(format!(
                "--stage {stage_one_based} out of bounds for {} configured stages",
                cfg.dpi_count
            )));
        }
        cfg.active_dpi_stage = stage_one_based - 1;
    }

    if let Some(dpi) = ov.dpi {
        let active_idx = usize::from(cfg.active_dpi_stage);
        validate_dpi(active_idx, dpi)?;
        let enabled_stages = cfg
            .dpi_stages
            .get(..usize::from(cfg.dpi_count))
            .unwrap_or(&cfg.dpi_stages);
        if let Some(existing_idx) = enabled_stages
            .iter()
            .position(|&stage_dpi| stage_dpi == dpi)
            && ov.stage.is_none()
        {
            cfg.active_dpi_stage = existing_idx as u8;
        } else if let Some(slot) = cfg.dpi_stages.get_mut(active_idx) {
            *slot = dpi;
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
    if let Some(ref light_str) = ov.light {
        cfg.light_mode = parse_light_mode(light_str)?;
    }
    Ok(())
}

fn cmd_set(direct: bool, ov: SetOverrides) -> Result<(), DeviceError> {
    let mut dev_opt: Option<Bkr1xDevice> = None;

    let mut cfg = if ov.dry_run {
        MouseConfig::factory_default()
    } else if !direct
        && let Ok(IpcResponse::Ok {
            snapshot: Some(snap),
            ..
        }) = send_ipc_request(&IpcRequest::GetFullState)
    {
        mouse_config_from_snapshot(&snap)
    } else {
        dev_opt = Bkr1xDevice::open_auto().ok();
        dev_opt
            .as_mut()
            .and_then(|dev| dev.read_config().ok())
            .unwrap_or_default()
    };

    apply_cli_overrides(&mut cfg, &ov)?;

    let codec = PacketCodec::new(ChecksumStrategy::None);
    let wire = codec.encode_set_mouse_config(&cfg)?.to_wire_bytes();

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
        println!("  {}", encode_hex_spaced(&wire));
        return Ok(());
    }

    if !direct && dev_opt.is_none() {
        let mut prof = HardwareProfile::from_hardware("CLI Set", &cfg, &Default::default());
        // Leave `buttons = None` so `bkr1x-cli set` never overwrites custom button remaps.
        prof.buttons = None;
        if let Ok(resp) = send_ipc_request(&IpcRequest::ApplyProfile {
            profile: Box::new(prof),
        }) {
            return match resp {
                IpcResponse::Ok { .. } => {
                    println!(
                        "Applied settings via bkr1x-daemon: {} DPI @ {} Hz (LOD {}mm, Debounce {}ms)",
                        cfg.active_dpi(),
                        cfg.polling_rate.as_hz(),
                        cfg.lod.as_mm(),
                        cfg.debounce_ms
                    );
                    Ok(())
                }
                IpcResponse::Error { message } => Err(DeviceError::Ipc(message)),
            };
        }
    }

    let mut dev = match dev_opt {
        Some(existing) => existing,
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
        println!(
            "  SET_MOUSE_CONFIG (0x0F): {}",
            encode_hex_spaced(&cfg_wire)
        );
        if profile.buttons.is_some() {
            println!(
                "  SET_MOUSE_KEYS   (0x09): {}",
                encode_hex_spaced(&btn_wire)
            );
        }
        println!(
            "  Hardware Macros        : {} sequence(s)",
            macro_vecs.len()
        );
        return Ok(());
    }

    if !direct
        && let Ok(resp) = send_ipc_request(&IpcRequest::ApplyProfile {
            profile: Box::new(profile.clone()),
        })
    {
        return match resp {
            IpcResponse::Ok { message, .. } => {
                println!(
                    "Applied profile '{}' via daemon ({})",
                    profile.name,
                    message.as_deref().unwrap_or("ok")
                );
                Ok(())
            }
            IpcResponse::Error { message } => Err(DeviceError::Ipc(message)),
        };
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
    let strategy = match checksum_arg {
        s if s.eq_ignore_ascii_case("none") => ChecksumStrategy::None,
        s if s.eq_ignore_ascii_case("sum55") => ChecksumStrategy::SumComplement(0x55),
        s if s.eq_ignore_ascii_case("sum00") || s.eq_ignore_ascii_case("twos") => {
            ChecksumStrategy::SumComplement(0x00)
        }
        s if s.eq_ignore_ascii_case("xor") => ChecksumStrategy::Xor,
        s if s.eq_ignore_ascii_case("crc16") => ChecksumStrategy::Crc16Ccitt,
        other => {
            return Err(format!(
                "unknown --checksum '{other}' (expected none, sum55, sum00, xor, crc16)"
            ));
        }
    };

    let bytes = decode_flexible_hex(raw_hex)?;
    println!(
        "Parsed {} byte(s): {}",
        bytes.len(),
        encode_hex_spaced(&bytes)
    );

    let codec = PacketCodec::new(strategy);
    let decoded = codec
        .decode(&bytes)
        .map_err(|e| format!("packet decode failed: {e}"))?;

    println!("Checksum ({strategy:?}) : VALID");
    match decoded {
        DecodedPacket::TxSetMouseConfig(cfg) | DecodedPacket::RxMouseConfig(cfg) => {
            let direction = if matches!(decoded, DecodedPacket::TxSetMouseConfig(_)) {
                "TX Host -> Mouse (0x0F SET_MOUSE_CONFIG)"
            } else {
                "RX Mouse -> Host (0x0E GET_MOUSE_CONFIG Reply)"
            };
            println!("Packet Type            : {direction}");
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
            for (idx, slot) in btns.slots.iter().enumerate() {
                println!("  Slot {idx}: {slot:?} -> {:02X?}", slot.to_bytes());
            }
        }
        other => println!("Decoded Packet         : {other:#?}"),
    }
    Ok(())
}

fn spawn_detached_process(exe: &Path, args: &[&str]) -> Result<(), String> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }

    cmd.spawn()
        .map(|_| ())
        .map_err(|e| format!("failed spawning {}: {e}", exe.display()))
}

fn cmd_install(enable_autostart: bool, start_tray_now: bool) -> Result<(), String> {
    let summary = install_user_application(enable_autostart)?;
    println!("Fire Phoenix BK-R1X — Installed Successfully");
    println!("──────────────────────────────────────────────");
    println!("  Install Directory : {}", summary.install_dir.display());
    println!("  Tray Daemon       : {}", summary.daemon_exe.display());
    println!("  GUI Configurator  : {}", summary.gui_exe.display());
    if let Some(lnk) = &summary.start_menu_shortcut {
        println!("  Start Menu Entry  : {}", lnk.display());
    }
    println!(
        "  Windows Startup   : {}",
        if summary.autostart_registered {
            "Enabled (HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run\\FirePhoenixBKR1X)"
        } else {
            "Skipped (--no-autostart)"
        }
    );

    if start_tray_now {
        spawn_detached_process(&summary.daemon_exe, &["--silent-if-running"])?;
        std::thread::sleep(Duration::from_millis(350));
        println!(
            "  System Tray State : {}",
            if is_daemon_running() {
                "Running in taskbar notification area"
            } else {
                "Launched"
            }
        );
    }
    Ok(())
}

fn cmd_uninstall() -> Result<(), String> {
    uninstall_user_application()?;
    println!(
        "Stopped background System Tray daemon and removed Windows Startup + Start Menu entries."
    );
    Ok(())
}

fn cmd_open() -> Result<(), String> {
    if let Ok(IpcResponse::Ok { .. }) = send_ipc_request(&IpcRequest::OpenGui) {
        println!("Opened / focused Fire Phoenix BK-R1X GUI via System Tray daemon.");
        return Ok(());
    }
    let gui_exe = resolve_sibling_binary("bkr1x-gui")
        .ok_or_else(|| "could not locate bkr1x-gui binary".to_owned())?;
    spawn_detached_process(&gui_exe, &[])?;
    println!("Launched {}", gui_exe.display());
    Ok(())
}

fn cmd_tray(action: TrayAction) -> Result<(), String> {
    match action {
        TrayAction::Start => {
            if is_daemon_running() {
                println!("System Tray daemon (bkr1x-daemon) is already running.");
                return Ok(());
            }
            let daemon_exe = resolve_sibling_binary("bkr1x-daemon")
                .ok_or_else(|| "could not locate bkr1x-daemon binary".to_owned())?;
            spawn_detached_process(&daemon_exe, &["--silent-if-running"])?;
            std::thread::sleep(Duration::from_millis(350));
            println!(
                "Started System Tray daemon ({}) -> Running: {}",
                daemon_exe.display(),
                is_daemon_running()
            );
            Ok(())
        }
        TrayAction::Stop => {
            if send_ipc_request(&IpcRequest::Shutdown).is_ok() {
                println!("Sent clean shutdown signal to System Tray daemon.");
            } else {
                println!("System Tray daemon was not running.");
            }
            Ok(())
        }
        TrayAction::Status => {
            println!("Fire Phoenix BK-R1X — System Tray & Startup");
            println!("──────────────────────────────────────────────");
            println!(
                "  System Tray Daemon : {}",
                if is_daemon_running() {
                    "Running"
                } else {
                    "Stopped"
                }
            );
            println!(
                "  Launch at Login    : {}",
                if is_autostart_enabled() {
                    "Enabled"
                } else {
                    "Disabled"
                }
            );
            Ok(())
        }
        TrayAction::Autostart { state } => {
            let enable = matches!(state.as_str(), "on" | "true");
            set_autostart_enabled(enable, None)?;
            println!(
                "Launch at Windows Login: {}",
                if enable { "Enabled" } else { "Disabled" }
            );
            Ok(())
        }
    }
}

fn cmd_package_installer(
    bin_dir_override: Option<&Path>,
    output_override: Option<&Path>,
) -> Result<(), String> {
    let bin_dir = match bin_dir_override {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .ok_or_else(|| "cannot determine current executable directory".to_owned())?,
    };

    let stub_name = platform_exe_name("bkr1x-installer");
    let daemon_name = platform_exe_name("bkr1x-daemon");
    let gui_name = platform_exe_name("bkr1x-gui");
    let cli_name = platform_exe_name("bkr1x-cli");

    let read_bin = |fname: &str| -> Result<Vec<u8>, String> {
        let path = bin_dir.join(fname);
        std::fs::read(&path).map_err(|e| {
            format!(
                "missing compiled binary {} ({e}) — run `cargo build --release --workspace` first",
                path.display()
            )
        })
    };

    let stub_bytes = read_bin(&stub_name)?;
    let daemon_bytes = read_bin(&daemon_name)?;
    let gui_bytes = read_bin(&gui_name)?;
    let cli_bytes = read_bin(&cli_name)?;

    let raw_total = stub_bytes.len() + daemon_bytes.len() + gui_bytes.len() + cli_bytes.len();
    let sfx_exe = pack_sfx_installer(
        &stub_bytes,
        &[
            (&daemon_name, &daemon_bytes),
            (&gui_name, &gui_bytes),
            (&cli_name, &cli_bytes),
        ],
    )?;

    let out_path = match output_override {
        Some(path) => path.to_path_buf(),
        None => PathBuf::from("dist")
            .join("windows")
            .join(platform_exe_name(&format!(
                "BKR1X-Setup-v{}-x64",
                env!("CARGO_PKG_VERSION")
            ))),
    };

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("failed creating {}: {e}", parent.display()))?;
    }
    std::fs::write(&out_path, &sfx_exe)
        .map_err(|e| format!("failed writing {}: {e}", out_path.display()))?;

    let verified = extract_sfx_from_exe(&out_path)?
        .ok_or_else(|| "verification failed: missing BKR1XSFX trailer".to_owned())?;
    if verified.files.len() != 3 {
        return Err("verification failed: unexpected embedded file count".to_owned());
    }

    println!("Fire Phoenix BK-R1X — GitHub Release Setup Wizard Packaged");
    println!("───────────────────────────────────────────────────────────");
    println!("  Output Installer  : {}", out_path.display());
    println!(
        "  Uncompressed Sum  : {:.2} MB (stub + daemon + gui + cli)",
        raw_total as f64 / (1024.0 * 1024.0)
    );
    println!(
        "  Final Setup Size  : {:.2} MB (zlib level-9 + CRC16 verified)",
        sfx_exe.len() as f64 / (1024.0 * 1024.0)
    );
    println!("  Silent CLI Flag   : --silent (unattended) | --uninstall");
    Ok(())
}
