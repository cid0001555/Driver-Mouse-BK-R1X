use std::collections::VecDeque;
use std::ffi::CString;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bkr1x_protocol::{
    BatteryTelemetry, ButtonMapping, ChecksumStrategy, ConnectionMode, DecodedPacket,
    FirmwareVersion, HardwareStateMachine, LightMode, MACRO_CHUNK_DATA_LEN, MACRO_FLASH_TOTAL_LEN,
    MacroStep, MouseConfig, Opcode, PID_24GHZ_DONGLE, PID_WIRED, PacketCodec, REPORT_PAYLOAD_LEN,
    RX_MAGIC, RawReport, VENDOR_USAGE_BKR1X, VENDOR_USAGE_PAGE_BKR1X, VENDOR_USAGE_PAGE_MAX,
    VENDOR_USAGE_PAGE_MIN, VID_24GHZ_DONGLE, VID_WIRED, WIRE_FRAME_LEN, compile_macro_bank,
};
use hidapi::{DeviceInfo, HidApi, HidDevice};

use crate::error::DeviceError;

pub const DEFAULT_READ_TIMEOUT_MS: i32 = 150;
pub const MAX_WIRELESS_RETRIES: usize = 3;
pub const RETRY_BACKOFF_BASE_MS: u64 = 30;
pub const MAX_TRACE_ENTRIES: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceDirection {
    Out,
    In,
}

/// Fixed-size zero-heap wire trace frame; human-readable summaries are formatted lazily on demand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireTraceEntry {
    pub timestamp_unix_ms: u64,
    pub direction: TraceDirection,
    pub report_id: u8,
    pub payload: [u8; REPORT_PAYLOAD_LEN],
}

impl WireTraceEntry {
    pub fn format_summary(&self, checksum: ChecksumStrategy) -> String {
        let mut wire = [0u8; WIRE_FRAME_LEN];
        wire[0] = self.report_id;
        wire[1..].copy_from_slice(&self.payload);
        match PacketCodec::new(checksum).decode(&wire) {
            Ok(pkt) => format!("{pkt:?}"),
            Err(_) => format!("RAW[0x{:02X}, 0x{:02X}]", self.payload[0], self.payload[1]),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceIdRule {
    pub vendor_id: u16,
    pub product_id: u16,
    pub mode: ConnectionMode,
}

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

pub struct Bkr1xDevice {
    device: HidDevice,
    info: DiscoveredInterface,
    codec: PacketCodec,
    state: HardwareStateMachine,
    last_battery_poll: Option<Instant>,
    trace_log: VecDeque<WireTraceEntry>,
}

impl Bkr1xDevice {
    pub fn open_auto() -> Result<Self, DeviceError> {
        Self::open_with_rules(&DEFAULT_DEVICE_RULES, ChecksumStrategy::None)
    }

    pub fn open_with_rules(
        rules: &[DeviceIdRule],
        checksum: ChecksumStrategy,
    ) -> Result<Self, DeviceError> {
        let api = HidApi::new()?;
        let info = discover_best_interface(&api, rules)?;
        let device = api.open_path(info.path.as_c_str())?;
        let mut state = HardwareStateMachine::new();
        state.connection = Some(info.mode);
        state.online = info.mode == ConnectionMode::UsbWired;

        Ok(Self {
            device,
            info,
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

    pub fn checksum_strategy(&self) -> ChecksumStrategy {
        self.codec.checksum
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

    pub fn drain_async_events(&mut self) -> usize {
        let mut processed = 0;
        let mut raw = [0u8; WIRE_FRAME_LEN];
        while let Ok(read_len) = self.device.read_timeout(&mut raw, 0) {
            if read_len == 0 {
                break;
            }
            let Some(payload) = normalize_rx_slice(&raw[..read_len]) else {
                continue;
            };
            self.record_trace(TraceDirection::In, 0x00, &payload);
            if let Ok(decoded) = self.codec.decode(&payload) {
                if matches!(decoded, DecodedPacket::RxAsyncBattery(_)) {
                    self.last_battery_poll = Some(Instant::now());
                }
                self.state.apply_packet(&decoded);
                processed += 1;
            }
        }
        processed
    }

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
            Err(err) => Err(err),
        }
    }

    fn ensure_awake(&mut self) -> Result<(), DeviceError> {
        if self.info.mode == ConnectionMode::Wireless24Ghz && !self.check_online()? {
            return Err(DeviceError::MouseAsleep);
        }
        Ok(())
    }

    pub fn poll_battery(
        &mut self,
        min_interval: Duration,
    ) -> Result<BatteryTelemetry, DeviceError> {
        self.drain_async_events();
        if let Some(last) = self.last_battery_poll
            && last.elapsed() < min_interval
        {
            return Ok(self.state.battery);
        }

        if self.info.mode == ConnectionMode::Wireless24Ghz && !self.check_online()? {
            return Ok(self.state.battery);
        }

        let req = self.codec.encode_get_battery();
        match self.transact(
            &req,
            Some(Opcode::GetBatteryInfo as u8),
            MAX_WIRELESS_RETRIES,
        ) {
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
            Err(err) => Err(err),
        }
    }

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

    pub fn read_config(&mut self) -> Result<MouseConfig, DeviceError> {
        self.ensure_awake()?;
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

    pub fn write_config(&mut self, cfg: &MouseConfig) -> Result<(), DeviceError> {
        cfg.validate()?;
        self.ensure_awake()?;
        let req = self.codec.encode_set_mouse_config(cfg)?;
        let _ = self.transact(&req, None, MAX_WIRELESS_RETRIES)?;
        self.state.config = *cfg;
        Ok(())
    }

    pub fn write_light_mode(&mut self, mode: LightMode) -> Result<(), DeviceError> {
        self.ensure_awake()?;
        let req = self.codec.encode_set_light_mode(mode);
        let _ = self.transact(&req, None, MAX_WIRELESS_RETRIES)?;
        self.state.config.light_mode = mode;
        Ok(())
    }

    pub fn read_buttons(&mut self) -> Result<ButtonMapping, DeviceError> {
        self.ensure_awake()?;
        let req = self.codec.encode_get_mouse_keys();
        let rx = self.transact(&req, Some(Opcode::GetMouseKeys as u8), MAX_WIRELESS_RETRIES)?;
        let pkt = self.codec.decode(&rx)?;
        self.state.apply_packet(&pkt);
        Ok(self.state.buttons)
    }

    pub fn write_buttons(&mut self, buttons: &ButtonMapping) -> Result<(), DeviceError> {
        buttons.validate()?;
        self.ensure_awake()?;
        let req = self.codec.encode_set_mouse_keys(buttons)?;
        let _ = self.transact(&req, None, MAX_WIRELESS_RETRIES)?;
        self.state.buttons = *buttons;
        Ok(())
    }

    pub fn write_macros(&mut self, macros: &[&[MacroStep]]) -> Result<usize, DeviceError> {
        self.ensure_awake()?;
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

    pub fn sync_all(&mut self) -> Result<&HardwareStateMachine, DeviceError> {
        if self.check_online()? {
            let _ = self.read_firmware_version();
            let _ = self.poll_battery(Duration::ZERO);
            let _ = self.read_config();
            let _ = self.read_buttons();
        }
        Ok(&self.state)
    }

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
                let timeout_ms = i32::try_from(remaining.as_millis().max(1)).unwrap_or(i32::MAX);
                let mut rx_raw = [0u8; WIRE_FRAME_LEN];
                let read_len = self.device.read_timeout(&mut rx_raw, timeout_ms)?;
                if read_len == 0 {
                    break;
                }
                let Some(payload) = normalize_rx_slice(&rx_raw[..read_len]) else {
                    continue;
                };
                self.record_trace(TraceDirection::In, 0x00, &payload);

                // Asynchronous battery/DPI push frames can interleave between TX and its ACK.
                if payload[0] == RX_MAGIC && payload[1] == Opcode::AsyncTelemetry as u8 {
                    if let Ok(async_pkt) = self.codec.decode(&payload) {
                        if matches!(async_pkt, DecodedPacket::RxAsyncBattery(_)) {
                            self.last_battery_poll = Some(Instant::now());
                        }
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
        let timestamp_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|dur| dur.as_millis() as u64)
            .unwrap_or(0);
        self.trace_log.push_back(WireTraceEntry {
            timestamp_unix_ms,
            direction,
            report_id,
            payload: *payload,
        });
    }
}

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

pub fn discover_best_interface(
    api: &HidApi,
    rules: &[DeviceIdRule],
) -> Result<DiscoveredInterface, DeviceError> {
    let mut best: Option<(u8, DiscoveredInterface)> = None;
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

        let usage_page = dev.usage_page();
        let usage = dev.usage();

        // Never open Generic Desktop (0x0001) or Consumer (0x000C) collections, which would lock
        // OS cursor/media input. Older Linux kernels may report `usage_page == 0` via hidraw sysfs;
        // in that case skip USB interface 0 (always the composite boot mouse interface).
        if usage_page != 0 && !(VENDOR_USAGE_PAGE_MIN..=VENDOR_USAGE_PAGE_MAX).contains(&usage_page)
        {
            continue;
        }
        #[cfg(target_os = "linux")]
        if usage_page == 0 && dev.interface_number() == 0 {
            continue;
        }

        let base_rank = if usage_page == VENDOR_USAGE_PAGE_BKR1X && usage == VENDOR_USAGE_BKR1X {
            0u8
        } else if usage_page == VENDOR_USAGE_PAGE_BKR1X {
            2u8
        } else {
            10u8
        };
        // Prefer USB-C Wired over 2.4GHz Dongle when both are plugged in simultaneously.
        let priority = base_rank + u8::from(rule.mode == ConnectionMode::Wireless24Ghz);

        if best
            .as_ref()
            .is_none_or(|(best_prio, _)| priority < *best_prio)
        {
            best = Some((
                priority,
                DiscoveredInterface {
                    path: dev.path().to_owned(),
                    vendor_id: dev.vendor_id(),
                    product_id: dev.product_id(),
                    usage_page,
                    usage,
                    product_name: dev.product_string().unwrap_or("BK-R1X").to_owned(),
                    mode: rule.mode,
                },
            ));
        }
    }

    if let Some((_, chosen)) = best {
        return Ok(chosen);
    }
    if saw_bluetooth_bkr1x {
        return Err(DeviceError::BluetoothConfigUnsupported);
    }
    Err(DeviceError::NotFound)
}

fn is_bluetooth_bkr1x_candidate(dev: &DeviceInfo, rules: &[DeviceIdRule]) -> bool {
    let prod = dev.product_string().unwrap_or("");
    let name_matches =
        contains_ignore_ascii_case(prod, "BK-R1X") || contains_ignore_ascii_case(prod, "BKR1X");
    let vid_matches = rules.iter().any(|r| r.vendor_id == dev.vendor_id());
    let is_standard_mouse = dev.usage_page() == 0x0001 && dev.usage() == 0x0002;

    (name_matches && dev.usage_page() < VENDOR_USAGE_PAGE_MIN)
        || (vid_matches && is_standard_mouse && dev.interface_number() < 0)
}

fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    n.is_empty() || h.windows(n.len()).any(|win| win.eq_ignore_ascii_case(n))
}
