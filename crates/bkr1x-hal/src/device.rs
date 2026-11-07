//! Safe vendor-defined HID discovery, tri-mode connection detection, and wireless sleep handling.

use std::collections::VecDeque;
use std::ffi::CString;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bkr1x_protocol::{
    BatteryTelemetry, ButtonMapping, ChecksumStrategy, ConnectionMode,
    FirmwareVersion, HardwareStateMachine, LightMode, MACRO_CHUNK_DATA_LEN, MACRO_FLASH_TOTAL_LEN,
    MacroStep, MouseConfig, Opcode, PID_24GHZ_DONGLE, PID_WIRED, PacketCodec, REPORT_PAYLOAD_LEN,
    RX_MAGIC, RawReport, VENDOR_USAGE_BKR1X, VENDOR_USAGE_PAGE_BKR1X, VENDOR_USAGE_PAGE_MAX,
    VENDOR_USAGE_PAGE_MIN, VID_24GHZ_DONGLE, VID_WIRED, WIRE_FRAME_LEN, compile_macro_bank,
};
use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::error::DeviceError;

/// Non-blocking HID read timeout (150 ms) so wireless deep-sleep never hangs callers.
pub const DEFAULT_READ_TIMEOUT_MS: i32 = 150;
/// Maximum number of retry attempts when waking/polling across transient 2.4GHz RF drops.
pub const MAX_WIRELESS_RETRIES: usize = 3;
/// Base retry backoff delay in milliseconds (`30ms -> 60ms -> 120ms`).
pub const RETRY_BACKOFF_BASE_MS: u64 = 30;
/// Maximum retained raw wire frames in the diagnostic inspector ring buffer.
pub const MAX_TRACE_ENTRIES: usize = 128;

/// Direction of an intercepted HID packet in the HAL trace log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceDirection {
    Out,
    In,
}

/// Captured raw packet entry for the GUI "Raw HID Debugger" and CLI diagnostics.
#[derive(Debug, Clone)]
pub struct WireTraceEntry {
    pub timestamp_unix_ms: u64,
    pub direction: TraceDirection,
    pub report_id: u8,
    pub payload: [u8; REPORT_PAYLOAD_LEN],
    pub summary: String,
}

/// Custom VID/PID matcher entry allowing overrides for OEM batch variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceIdRule {
    pub vendor_id: u16,
    pub product_id: u16,
    pub mode: ConnectionMode,
}

/// Default hardware ID table extracted from `bk-r1x.yjx2012.com`.
pub const DEFAULT_DEVICE_RULES: [DeviceIdRule; 2] = [
    DeviceIdRule {
        vendor_id: VID_WIRED,
        product_id: PID_WIRED,
        mode: ConnectionMode::UsbWired,
    },
    DeviceIdRule {
        vendor_id: VID_24GHZ_DONGLE,
        product_id: PID_24GHZ_DONGLE,
        mode: ConnectionMode::Wireless24Ghz,
    },
];

/// Metadata of a matched vendor-defined HID interface prior to opening.
#[derive(Debug, Clone)]
pub struct DiscoveredInterface {
    pub path: CString,
    pub vendor_id: u16,
    pub product_id: u16,
    pub usage_page: u16,
    pub usage: u16,
    pub product_name: String,
    pub mode: ConnectionMode,
}

/// Active connection to the BK-R1X vendor HID collection (`0xFF01 / 0x0010`).
pub struct Bkr1xDevice {
    device: HidDevice,
    info: DiscoveredInterface,
    codec: PacketCodec,
    state: HardwareStateMachine,
    last_battery_poll: Option<Instant>,
    trace_log: VecDeque<WireTraceEntry>,
}

impl Bkr1xDevice {
    /// Enumerates system HID collections and opens the BK-R1X Vendor-Defined interface (`0xFF00..=0xFFFF`).
    ///
    /// - Prefers **USB-C Wired (`0xA8A4:0x2255`)** over **2.4GHz Dongle (`0xA8A5:0x2255`)** when both are present,
    ///   matching OEM WebHID driver priority.
    /// - Strictly filters for `usage_page in 0xFF00..=0xFFFF` so standard OS mouse/keyboard collections are never
    ///   claimed or locked.
    /// - Detects Bluetooth-only connection and returns [`DeviceError::BluetoothConfigUnsupported`].
    pub fn open_auto() -> Result<Self, DeviceError> {
        Self::open_with_rules(&DEFAULT_DEVICE_RULES, ChecksumStrategy::None)
    }

    /// Opens a BK-R1X device using explicit VID/PID rules and [`ChecksumStrategy`].
    pub fn open_with_rules(
        rules: &[DeviceIdRule],
        checksum: ChecksumStrategy,
    ) -> Result<Self, DeviceError> {
        let api = HidApi::new()?;
        let found = discover_best_interface(&api, rules)?;
        let device = api.open_path(found.path.as_c_str())?;
        let mut state = HardwareStateMachine::new();
        state.connection = Some(found.mode);
        state.online = found.mode == ConnectionMode::UsbWired;

        Ok(Self {
            device,
            info: found,
            codec: PacketCodec::new(checksum),
            state,
            last_battery_poll: None,
            trace_log: VecDeque::with_capacity(MAX_TRACE_ENTRIES),
        })
    }

    pub fn info(&self) -> &DiscoveredInterface {
        &self.info
    }

    pub fn mode(&self) -> ConnectionMode {
        self.info.mode
    }

    pub fn state(&self) -> &HardwareStateMachine {
        &self.state
    }

    pub fn trace_log(&self) -> &VecDeque<WireTraceEntry> {
        &self.trace_log
    }

    pub fn clear_trace_log(&mut self) {
        self.trace_log.clear();
    }

    /// Drains any pending asynchronous telemetry reports (`0xAA 0xFA` / `0xAA 0xED`) without blocking.
    pub fn drain_async_events(&mut self) -> usize {
        let mut count = 0;
        let mut raw = [0u8; WIRE_FRAME_LEN];
        while let Ok(n) = self.device.read_timeout(&mut raw, 0) {
            if n == 0 {
                break;
            }
            if let Some(payload) = normalize_rx_slice(&raw[..n]) {
                self.record_trace(TraceDirection::In, 0x00, &payload);
                if let Ok(decoded) = self.codec.decode(&payload) {
                    self.state.apply_packet(&decoded);
                    count += 1;
                }
            }
        }
        count
    }

    /// Queries whether the mouse MCU is awake/online. Always `true` on USB-C Wired.
    pub fn check_online(&mut self) -> Result<bool, DeviceError> {
        if self.info.mode == ConnectionMode::UsbWired {
            self.state.online = true;
            return Ok(true);
        }
        let req = self.codec.encode_get_online_status();
        match self.transact(&req, Some(Opcode::GetOnlineStatus as u8), 1) {
            Ok(rx) => {
                if let Ok(pkt) = self.codec.decode(&rx) {
                    self.state.apply_packet(&pkt);
                }
                Ok(self.state.online)
            }
            Err(DeviceError::Timeout) => {
                self.state.online = false;
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// Queries battery telemetry, falling back gracefully to cached state if the 2.4GHz MCU is asleep.
    pub fn poll_battery(&mut self, min_interval: Duration) -> Result<BatteryTelemetry, DeviceError> {
        self.drain_async_events();
        if let Some(last) = self.last_battery_poll
            && last.elapsed() < min_interval
            && self.state.battery.percent > 0
        {
            return Ok(self.state.battery);
        }

        if self.info.mode == ConnectionMode::Wireless24Ghz && !self.check_online()? {
            return Ok(self.state.battery);
        }

        let req = self.codec.encode_get_battery();
        match self.transact(&req, Some(Opcode::GetBatteryInfo as u8), MAX_WIRELESS_RETRIES) {
            Ok(rx) => {
                let decoded = self.codec.decode(&rx)?;
                self.state.apply_packet(&decoded);
                self.last_battery_poll = Some(Instant::now());
                Ok(self.state.battery)
            }
            Err(DeviceError::Timeout) if self.info.mode == ConnectionMode::Wireless24Ghz => {
                self.state.online = false;
                Ok(self.state.battery)
            }
            Err(e) => Err(e),
        }
    }

    /// Queries firmware version (`X.Y.Z`).
    pub fn read_firmware_version(&mut self) -> Result<FirmwareVersion, DeviceError> {
        let req = self.codec.encode_get_firmware_version();
        let rx = self.transact(
            &req,
            Some(Opcode::GetFirmwareVersion as u8),
            MAX_WIRELESS_RETRIES,
        )?;
        let pkt = self.codec.decode(&rx)?;
        self.state.apply_packet(&pkt);
        Ok(self.state.firmware)
    }

    /// Reads the active PAW3311 & MCU configuration block (`0x0E`).
    pub fn read_config(&mut self) -> Result<MouseConfig, DeviceError> {
        if self.info.mode == ConnectionMode::Wireless24Ghz && !self.check_online()? {
            return Err(DeviceError::MouseAsleep);
        }
        let req = self.codec.encode_get_mouse_config();
        let rx = self.transact(
            &req,
            Some(Opcode::GetMouseConfig as u8),
            MAX_WIRELESS_RETRIES,
        )?;
        let pkt = self.codec.decode(&rx)?;
        self.state.apply_packet(&pkt);
        Ok(self.state.config)
    }

    /// Validates and writes a new PAW3311 & MCU configuration block (`0x0F`) to flash.
    pub fn write_config(&mut self, cfg: &MouseConfig) -> Result<(), DeviceError> {
        cfg.validate()?;
        if self.info.mode == ConnectionMode::Wireless24Ghz && !self.check_online()? {
            return Err(DeviceError::MouseAsleep);
        }
        let req = self.codec.encode_set_mouse_config(cfg)?;
        let _ = self.transact(&req, None, MAX_WIRELESS_RETRIES)?;
        self.state.config = *cfg;
        Ok(())
    }

    /// Writes the standalone LED mode (`0x21`).
    pub fn write_light_mode(&mut self, mode: LightMode) -> Result<(), DeviceError> {
        if self.info.mode == ConnectionMode::Wireless24Ghz && !self.check_online()? {
            return Err(DeviceError::MouseAsleep);
        }
        let req = self.codec.encode_set_light_mode(mode);
        let _ = self.transact(&req, None, MAX_WIRELESS_RETRIES)?;
        self.state.config.light_mode = mode;
        Ok(())
    }

    /// Reads the 8 button mapping slots (`0x08`).
    pub fn read_buttons(&mut self) -> Result<ButtonMapping, DeviceError> {
        if self.info.mode == ConnectionMode::Wireless24Ghz && !self.check_online()? {
            return Err(DeviceError::MouseAsleep);
        }
        let req = self.codec.encode_get_mouse_keys();
        let rx = self.transact(
            &req,
            Some(Opcode::GetMouseKeys as u8),
            MAX_WIRELESS_RETRIES,
        )?;
        let pkt = self.codec.decode(&rx)?;
        self.state.apply_packet(&pkt);
        Ok(self.state.buttons)
    }

    /// Validates and writes all 8 button mapping slots (`0x09`).
    pub fn write_buttons(&mut self, buttons: &ButtonMapping) -> Result<(), DeviceError> {
        buttons.validate()?;
        if self.info.mode == ConnectionMode::Wireless24Ghz && !self.check_online()? {
            return Err(DeviceError::MouseAsleep);
        }
        let req = self.codec.encode_set_mouse_keys(buttons)?;
        let _ = self.transact(&req, None, MAX_WIRELESS_RETRIES)?;
        self.state.buttons = *buttons;
        Ok(())
    }

    /// Compiles and streams a bank of hardware macros in 56-byte chunks (`0x0D`) + commit (`0x10`).
    pub fn write_macros(&mut self, macros: &[&[MacroStep]]) -> Result<usize, DeviceError> {
        if self.info.mode == ConnectionMode::Wireless24Ghz && !self.check_online()? {
            return Err(DeviceError::MouseAsleep);
        }
        let mut flash = [0u8; MACRO_FLASH_TOTAL_LEN];
        let total_len = compile_macro_bank(macros, &mut flash)?;

        let mut offset = 0usize;
        while offset < total_len {
            let end = (offset + MACRO_CHUNK_DATA_LEN).min(total_len);
            let pkt = self
                .codec
                .encode_write_macro_chunk(offset as u16, &flash[offset..end]);
            let _ = self.transact(&pkt, None, MAX_WIRELESS_RETRIES)?;
            offset = end;
        }

        let commit = self.codec.encode_commit_macro_flash();
        let _ = self.transact(&commit, None, MAX_WIRELESS_RETRIES)?;
        Ok(total_len)
    }

    /// Synchronizes full device state (online, firmware, battery, config, button mappings).
    pub fn sync_all(&mut self) -> Result<&HardwareStateMachine, DeviceError> {
        let online = self.check_online()?;
        if online {
            let _ = self.read_firmware_version();
            let _ = self.poll_battery(Duration::ZERO);
            let _ = self.read_config();
            let _ = self.read_buttons();
        }
        Ok(&self.state)
    }

    /// Executes a single write + timeout-bounded read transaction with wireless backoff retries.
    fn transact(
        &mut self,
        report: &RawReport,
        expected_opcode: Option<u8>,
        max_attempts: usize,
    ) -> Result<[u8; REPORT_PAYLOAD_LEN], DeviceError> {
        let wire = report.to_wire_bytes();
        let attempts = max_attempts.max(1);

        for attempt in 0..attempts {
            self.drain_async_events();
            self.record_trace(TraceDirection::Out, report.report_id, &report.payload);
            self.device.write(&wire)?;

            let deadline = Instant::now() + Duration::from_millis(DEFAULT_READ_TIMEOUT_MS as u64);
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    break;
                }
                let mut rx_raw = [0u8; WIRE_FRAME_LEN];
                let n = self
                    .device
                    .read_timeout(&mut rx_raw, remaining.as_millis().max(1) as i32)?;
                if n == 0 {
                    break;
                }
                let Some(payload) = normalize_rx_slice(&rx_raw[..n]) else {
                    continue;
                };
                self.record_trace(TraceDirection::In, 0x00, &payload);

                // If an async battery/DPI event arrived mid-command, apply it and keep reading for our ACK
                if payload[0] == RX_MAGIC && payload[1] == Opcode::AsyncTelemetry as u8 {
                    if let Ok(async_pkt) = self.codec.decode(&payload) {
                        self.state.apply_packet(&async_pkt);
                    }
                    continue;
                }

                if let Some(exp_op) = expected_opcode
                    && payload[0] == RX_MAGIC
                    && payload[1] != exp_op
                {
                    continue;
                }

                return Ok(payload);
            }

            if attempt + 1 < attempts {
                let backoff_ms = RETRY_BACKOFF_BASE_MS * (1u64 << attempt);
                std::thread::sleep(Duration::from_millis(backoff_ms));
            }
        }

        Err(DeviceError::Timeout)
    }

    fn record_trace(
        &mut self,
        direction: TraceDirection,
        report_id: u8,
        payload: &[u8; REPORT_PAYLOAD_LEN],
    ) {
        if self.trace_log.len() >= MAX_TRACE_ENTRIES {
            self.trace_log.pop_front();
        }
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let summary = match self.codec.decode(payload) {
            Ok(pkt) => format!("{pkt:?}"),
            Err(_) => format!("RAW[0x{:02X}, 0x{:02X}]", payload[0], payload[1]),
        };
        self.trace_log.push_back(WireTraceEntry {
            timestamp_unix_ms: ts_ms,
            direction,
            report_id,
            payload: *payload,
            summary,
        });
    }
}

/// Normalizes a `hidapi` read buffer (64 bytes raw or 65 bytes with leading ReportID `0x00`) into `[u8; 64]`.
fn normalize_rx_slice(raw: &[u8]) -> Option<[u8; REPORT_PAYLOAD_LEN]> {
    if raw.is_empty() {
        return None;
    }
    let mut out = [0u8; REPORT_PAYLOAD_LEN];
    if raw.len() == WIRE_FRAME_LEN && raw[0] == 0x00 {
        out.copy_from_slice(&raw[1..WIRE_FRAME_LEN]);
    } else {
        let len = raw.len().min(REPORT_PAYLOAD_LEN);
        out[..len].copy_from_slice(&raw[..len]);
    }
    Some(out)
}

/// Inspects `HidApi::device_list()` to safely select ONLY the Vendor-Defined collection (`0xFF00..=0xFFFF`).
pub fn discover_best_interface(
    api: &HidApi,
    rules: &[DeviceIdRule],
) -> Result<DiscoveredInterface, DeviceError> {
    let mut candidates: Vec<(u8, DiscoveredInterface)> = Vec::new();
    let mut saw_bluetooth_bkr1x = false;

    for dev in api.device_list() {
        if is_bluetooth_bkr1x_candidate(dev, rules) {
            saw_bluetooth_bkr1x = true;
        }

        let Some(rule) = rules
            .iter()
            .find(|r| r.vendor_id == dev.vendor_id() && r.product_id == dev.product_id())
        else {
            continue;
        };

        let up = dev.usage_page();
        let usage = dev.usage();

        // CRITICAL SAFETY FILTER: Never claim Generic Desktop (0x0001) or Consumer (0x000C) interfaces!
        // On Linux, hidraw sometimes reports 0/0 when sysfs usage parsing isn't populated on older kernels;
        // if up != 0, enforce strictly `0xFF00..=0xFFFF`.
        if up != 0 && !(VENDOR_USAGE_PAGE_MIN..=VENDOR_USAGE_PAGE_MAX).contains(&up) {
            continue;
        }
        #[cfg(target_os = "linux")]
        if up == 0 && dev.interface_number() == 0 {
            // Interface 0 on USB composite mice is always the boot mouse cursor collection; skip it!
            continue;
        }

        // Rank exact UsagePage 0xFF01 + Usage 0x0010 + Wired mode highest
        let mut priority = 10u8;
        if up == VENDOR_USAGE_PAGE_BKR1X && usage == VENDOR_USAGE_BKR1X {
            priority = 0;
        } else if up == VENDOR_USAGE_PAGE_BKR1X {
            priority = 2;
        }
        if rule.mode == ConnectionMode::Wireless24Ghz {
            priority += 1; // Prefer USB-C Wired when both cable and dongle are connected
        }

        candidates.push((
            priority,
            DiscoveredInterface {
                path: dev.path().to_owned(),
                vendor_id: dev.vendor_id(),
                product_id: dev.product_id(),
                usage_page: up,
                usage,
                product_name: dev.product_string().unwrap_or("BK-R1X").to_owned(),
                mode: rule.mode,
            },
        ));
    }

    candidates.sort_by_key(|(prio, _)| *prio);
    if let Some((_, best)) = candidates.into_iter().next() {
        return Ok(best);
    }

    if saw_bluetooth_bkr1x {
        return Err(DeviceError::BluetoothConfigUnsupported);
    }

    Err(DeviceError::NotFound)
}

fn is_bluetooth_bkr1x_candidate(dev: &DeviceInfo, rules: &[DeviceIdRule]) -> bool {
    let prod = dev.product_string().unwrap_or("").to_ascii_uppercase();
    let name_matches = prod.contains("BK-R1X") || prod.contains("BKR1X");
    let vid_matches = rules.iter().any(|r| r.vendor_id == dev.vendor_id());
    let is_standard_mouse = dev.usage_page() == 0x0001 && dev.usage() == 0x0002;

    // Bluetooth HOGP exposes standard HID mouse (0x0001:0x0002) without any 0xFF01 vendor collection
    (name_matches && (dev.usage_page() < VENDOR_USAGE_PAGE_MIN))
        || (vid_matches && is_standard_mouse && dev.interface_number() < 0)
}
