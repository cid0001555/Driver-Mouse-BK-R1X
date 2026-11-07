//! Zero-allocation const-generic packet framing, encoders, decoders, and state machine.

use crate::checksum::ChecksumStrategy;
use crate::constants::{
    AUTH_READ, AUTH_WRITE_CONFIG, AUTH_WRITE_KEYS, BUTTON_SLOT_COUNT, MACRO_CHUNK_DATA_LEN,
    MAX_DPI_STAGES, Opcode, REPORT_ID_MACRO_DIRECT, REPORT_ID_VENDOR_CMD, REPORT_PAYLOAD_LEN,
    RX_MAGIC, RX_PAIRING_MAGIC_0, RX_PAIRING_MAGIC_1, TX_MAGIC, VALID_SLEEP_MINUTES_DEFAULT,
    WIRE_FRAME_LEN, offsets,
};
use crate::types::{
    BatteryTelemetry, ButtonAction, ButtonMapping, ConnectionMode, FirmwareVersion, LightMode,
    LodHeight, MouseConfig, PollingRate, ProtocolError, ScrollDirection, SensorFlags,
    VALID_SLEEP_MINUTES,
};

/// Stack-allocated fixed-size HID report buffer (`64` bytes default, `16` or `32` supported via const generics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawReport<const N: usize = REPORT_PAYLOAD_LEN> {
    pub report_id: u8,
    pub payload: [u8; N],
}

impl<const N: usize> Default for RawReport<N> {
    fn default() -> Self {
        Self {
            report_id: REPORT_ID_VENDOR_CMD,
            payload: [0u8; N],
        }
    }
}

impl RawReport<REPORT_PAYLOAD_LEN> {
    /// Materializes the 65-byte `[report_id, payload[0..64]]` buffer expected by `hidapi::HidDevice::write`.
    pub const fn to_wire_bytes(&self) -> [u8; WIRE_FRAME_LEN] {
        let mut out = [0u8; WIRE_FRAME_LEN];
        out[0] = self.report_id;
        let mut i = 0;
        while i < REPORT_PAYLOAD_LEN {
            out[i + 1] = self.payload[i];
            i += 1;
        }
        out
    }

    /// Parses a 64-byte payload or 65-byte report-ID-prefixed frame into a normalized `RawReport<64>`.
    pub fn from_slice(raw: &[u8]) -> Result<Self, ProtocolError> {
        if raw.len() == WIRE_FRAME_LEN {
            let mut payload = [0u8; REPORT_PAYLOAD_LEN];
            payload.copy_from_slice(&raw[1..WIRE_FRAME_LEN]);
            Ok(Self {
                report_id: raw[0],
                payload,
            })
        } else if raw.len() == REPORT_PAYLOAD_LEN {
            let mut payload = [0u8; REPORT_PAYLOAD_LEN];
            payload.copy_from_slice(raw);
            Ok(Self {
                report_id: REPORT_ID_VENDOR_CMD,
                payload,
            })
        } else if raw.len() >= 3 && (raw[0] == 0x00 || raw[0] == REPORT_ID_MACRO_DIRECT) {
            let mut payload = [0u8; REPORT_PAYLOAD_LEN];
            let copy_len = (raw.len() - 1).min(REPORT_PAYLOAD_LEN);
            payload[..copy_len].copy_from_slice(&raw[1..1 + copy_len]);
            Ok(Self {
                report_id: raw[0],
                payload,
            })
        } else if raw.len() >= 2 {
            let mut payload = [0u8; REPORT_PAYLOAD_LEN];
            let copy_len = raw.len().min(REPORT_PAYLOAD_LEN);
            payload[..copy_len].copy_from_slice(&raw[..copy_len]);
            Ok(Self {
                report_id: REPORT_ID_VENDOR_CMD,
                payload,
            })
        } else {
            Err(ProtocolError::PacketTooShort {
                actual: raw.len(),
                expected_min: 2,
            })
        }
    }
}

/// Zero-heap YJX2012 / BK-R1X command encoder parameterized by [`ChecksumStrategy`].
#[derive(Debug, Clone, Copy, Default)]
pub struct PacketCodec {
    pub checksum: ChecksumStrategy,
}

impl PacketCodec {
    pub const fn new(checksum: ChecksumStrategy) -> Self {
        Self { checksum }
    }

    fn finish(&self, mut rpt: RawReport) -> RawReport {
        self.checksum.apply(&mut rpt.payload);
        rpt
    }

    /// `0x03` — Query ASCII firmware version (`X.Y.Z`).
    pub fn encode_get_firmware_version(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetFirmwareVersion as u8;
        self.finish(rpt)
    }

    /// `0x30` — Query battery percentage and USB charging state.
    pub fn encode_get_battery(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetBatteryInfo as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_READ[0];
        rpt.payload[offsets::AUTH_1] = AUTH_READ[1];
        rpt.payload[offsets::DATA_LEN] = 0x2E; // 46
        rpt.payload[5] = 0x01;
        rpt.payload[6] = 0x01;
        self.finish(rpt)
    }

    /// `0xED` — Query whether the wireless mouse MCU is awake/online (`rx[8] == 2`).
    pub fn encode_get_online_status(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetOnlineStatus as u8;
        rpt.payload[offsets::AUTH_0] = 0x00;
        rpt.payload[offsets::AUTH_1] = 0x01;
        rpt.payload[offsets::DATA_LEN] = 0x2E; // 46
        self.finish(rpt)
    }

    /// `0x21` — Set LED lighting effect mode (`0..=6`).
    pub fn encode_set_light_mode(&self, mode: LightMode) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::SetLightMode as u8;
        rpt.payload[offsets::DATA_LEN] = 0x03;
        rpt.payload[10] = mode as u8; // wire[11] -> payload[10]
        self.finish(rpt)
    }

    /// `0x08` — Read the 8 × 4-byte button mapping slots.
    pub fn encode_get_mouse_keys(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetMouseKeys as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_READ[0];
        rpt.payload[offsets::AUTH_1] = AUTH_READ[1];
        rpt.payload[offsets::DATA_LEN] = 0x20; // 32 bytes
        self.finish(rpt)
    }

    /// `0x09` — Validate and serialize all 8 button mapping slots (`32` bytes).
    pub fn encode_set_mouse_keys(&self, mapping: &ButtonMapping) -> Result<RawReport, ProtocolError> {
        mapping.validate()?;
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::SetMouseKeys as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_WRITE_KEYS[0];
        rpt.payload[offsets::AUTH_1] = AUTH_WRITE_KEYS[1];
        rpt.payload[offsets::DATA_LEN] = 0x20; // 32 bytes

        for (slot_idx, action) in mapping.slots.iter().enumerate() {
            let base = offsets::KEYS_DATA_START + slot_idx * 4;
            rpt.payload[base..base + 4].copy_from_slice(&action.to_bytes());
        }
        Ok(self.finish(rpt))
    }

    /// `0x0E` — Read active PAW3311 & MCU configuration block.
    pub fn encode_get_mouse_config(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetMouseConfig as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_READ[0];
        rpt.payload[offsets::AUTH_1] = AUTH_READ[1];
        rpt.payload[offsets::DATA_LEN] = 0x2F; // 47 bytes
        rpt.payload[5] = 0x01;
        rpt.payload[6] = 0x01;
        self.finish(rpt)
    }

    /// `0x0F` — Strictly validate PAW3311 bounds and serialize full configuration block.
    pub fn encode_set_mouse_config(&self, cfg: &MouseConfig) -> Result<RawReport, ProtocolError> {
        cfg.validate()?;
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::SetMouseConfig as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_WRITE_CONFIG[0];
        rpt.payload[offsets::AUTH_1] = AUTH_WRITE_CONFIG[1];
        rpt.payload[offsets::DATA_LEN] = 0x2F; // 47 bytes
        rpt.payload[5] = 0x01;
        rpt.payload[6] = 0x01;

        rpt.payload[offsets::CFG_LIGHT_MODE] = cfg.light_mode as u8;
        rpt.payload[offsets::CFG_REPORT_RATE] = cfg.polling_rate.to_wire_byte();
        rpt.payload[offsets::CFG_DPI_COUNT] = cfg.dpi_count;
        rpt.payload[offsets::CFG_DPI_ACTIVE_ONE_BASED] = cfg.active_dpi_stage + 1;

        for (i, &dpi) in cfg.dpi_stages.iter().enumerate() {
            let [lo, hi] = dpi.to_le_bytes();
            let pos = offsets::CFG_DPI_STAGES_START + i * 2;
            rpt.payload[pos] = lo;
            rpt.payload[pos + 1] = hi;
        }

        rpt.payload[offsets::CFG_SCROLL_DIR] = cfg.scroll_direction as u8;
        rpt.payload[offsets::CFG_LOD] = cfg.lod.as_mm();
        rpt.payload[offsets::CFG_SENSOR_FLAGS] = cfg.sensor_flags.to_bits();
        rpt.payload[offsets::CFG_DEBOUNCE_MS] = cfg.debounce_ms;
        rpt.payload[offsets::CFG_SLEEP_MINUTES] = cfg.sleep_minutes;
        rpt.payload[offsets::CFG_HIGHSPEED_MODE] = u8::from(cfg.highspeed_mode);
        rpt.payload[offsets::CFG_POWER_LIGHT_PACKED_TX] =
            (u8::from(cfg.wakeup_on_move) << 4) | u8::from(cfg.move_lights_off);

        Ok(self.finish(rpt))
    }

    /// `0x0C` (ReportID `0x06`) — Read a chunk of up to 56 bytes from macro flash.
    pub fn encode_read_macro_chunk(&self, offset: u16, len: u8) -> RawReport {
        let mut rpt = RawReport {
            report_id: REPORT_ID_MACRO_DIRECT,
            payload: [0u8; REPORT_PAYLOAD_LEN],
        };
        let [off_lo, off_hi] = offset.to_le_bytes();
        rpt.payload[0] = Opcode::ReadMacroChunk as u8;
        rpt.payload[1] = len.min(MACRO_CHUNK_DATA_LEN as u8);
        rpt.payload[2] = off_lo;
        rpt.payload[3] = off_hi;
        self.finish(rpt)
    }

    /// `0x0D` — Write a chunk of up to 56 bytes into macro flash at `offset`.
    pub fn encode_write_macro_chunk(&self, offset: u16, chunk: &[u8]) -> RawReport {
        let mut rpt = RawReport::default();
        let bounded_len = chunk.len().min(MACRO_CHUNK_DATA_LEN);
        let [off_lo, off_hi] = offset.to_le_bytes();

        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::WriteMacroChunk as u8;
        rpt.payload[offsets::DATA_LEN] = bounded_len as u8;
        rpt.payload[5] = off_lo;
        rpt.payload[6] = off_hi;
        rpt.payload[7] = 0x00;
        rpt.payload[8..8 + bounded_len].copy_from_slice(&chunk[..bounded_len]);
        self.finish(rpt)
    }

    /// `0x10` — Commit macro flash bank to non-volatile storage.
    pub fn encode_commit_macro_flash(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::CommitMacroFlash as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_WRITE_KEYS[0];
        rpt.payload[offsets::AUTH_1] = AUTH_WRITE_KEYS[1];
        rpt.payload[7] = 0x05;
        self.finish(rpt)
    }

    /// Decodes any outgoing (`0x55` / `reportId=0x06`) or incoming (`0xAA` / `0x13 0xE2`) packet.
    pub fn decode(&self, raw: &[u8]) -> Result<DecodedPacket, ProtocolError> {
        let rpt = RawReport::from_slice(raw)?;
        if !self.checksum.verify(&rpt.payload) {
            return Err(ProtocolError::ChecksumMismatch);
        }

        if rpt.report_id == REPORT_ID_MACRO_DIRECT {
            let op = rpt.payload[0];
            if op == Opcode::ReadMacroChunk as u8 {
                let len = rpt.payload[1];
                let offset = u16::from_le_bytes([rpt.payload[2], rpt.payload[3]]);
                return Ok(DecodedPacket::TxReadMacroChunk { offset, len });
            }
            return Ok(DecodedPacket::TxResetMacroFlash);
        }

        let p = &rpt.payload;
        match p[offsets::MAGIC] {
            TX_MAGIC => Self::decode_tx_payload(p),
            RX_MAGIC => Self::decode_rx_payload(p),
            RX_PAIRING_MAGIC_0 if p[1] == RX_PAIRING_MAGIC_1 => Ok(DecodedPacket::RxDonglePairing),
            other => Err(ProtocolError::InvalidMagicHeader(other)),
        }
    }

    fn decode_tx_payload(p: &[u8; REPORT_PAYLOAD_LEN]) -> Result<DecodedPacket, ProtocolError> {
        let opcode = Opcode::from_byte(p[offsets::OPCODE])
            .ok_or(ProtocolError::UnknownOpcode(p[offsets::OPCODE]))?;

        match opcode {
            Opcode::GetFirmwareVersion => Ok(DecodedPacket::TxGetFirmwareVersion),
            Opcode::GetBatteryInfo => Ok(DecodedPacket::TxGetBattery),
            Opcode::GetOnlineStatus => Ok(DecodedPacket::TxGetOnlineStatus),
            Opcode::SetLightMode => Ok(DecodedPacket::TxSetLightMode(LightMode::from_wire(p[10]))),
            Opcode::GetMouseKeys => Ok(DecodedPacket::TxGetMouseKeys),
            Opcode::SetMouseKeys => {
                let mapping = parse_button_mapping(p);
                Ok(DecodedPacket::TxSetMouseKeys(mapping))
            }
            Opcode::GetMouseConfig => Ok(DecodedPacket::TxGetMouseConfig),
            Opcode::SetMouseConfig => {
                let cfg = parse_mouse_config(p, true);
                Ok(DecodedPacket::TxSetMouseConfig(cfg))
            }
            Opcode::WriteMacroChunk => {
                let len = p[offsets::DATA_LEN].min(MACRO_CHUNK_DATA_LEN as u8);
                let offset = u16::from_le_bytes([p[5], p[6]]);
                let mut data = [0u8; MACRO_CHUNK_DATA_LEN];
                data[..len as usize].copy_from_slice(&p[8..8 + len as usize]);
                Ok(DecodedPacket::TxWriteMacroChunk { offset, len, data })
            }
            Opcode::CommitMacroFlash => Ok(DecodedPacket::TxCommitMacroFlash),
            Opcode::ReadMacroChunk => Ok(DecodedPacket::TxReadMacroChunk {
                offset: u16::from_le_bytes([p[5], p[6]]),
                len: p[offsets::DATA_LEN],
            }),
            Opcode::AsyncTelemetry => Err(ProtocolError::UnknownOpcode(p[offsets::OPCODE])),
        }
    }

    fn decode_rx_payload(p: &[u8; REPORT_PAYLOAD_LEN]) -> Result<DecodedPacket, ProtocolError> {
        let opcode = Opcode::from_byte(p[offsets::OPCODE])
            .ok_or(ProtocolError::UnknownOpcode(p[offsets::OPCODE]))?;

        match opcode {
            Opcode::GetFirmwareVersion => {
                let parse_digit = |b: u8| if b.is_ascii_digit() { b - b'0' } else { 0 };
                Ok(DecodedPacket::RxFirmwareVersion(FirmwareVersion {
                    major: parse_digit(p[offsets::RX_FW_MAJOR_ASCII]),
                    minor: parse_digit(p[offsets::RX_FW_MINOR_ASCII]),
                    patch: parse_digit(p[offsets::RX_FW_PATCH_ASCII]),
                }))
            }
            Opcode::GetBatteryInfo => Ok(DecodedPacket::RxBattery(BatteryTelemetry {
                percent: p[offsets::RX_BATTERY_PERCENT].min(100),
                charging: p[offsets::RX_BATTERY_CHARGING] != 0,
            })),
            Opcode::GetOnlineStatus => Ok(DecodedPacket::RxOnlineStatus {
                online: p[offsets::RX_ONLINE_FLAG] == offsets::ONLINE_AWAKE_VALUE,
            }),
            Opcode::GetMouseKeys => Ok(DecodedPacket::RxMouseKeys(parse_button_mapping(p))),
            Opcode::GetMouseConfig => Ok(DecodedPacket::RxMouseConfig(parse_mouse_config(p, false))),
            Opcode::AsyncTelemetry => {
                if p[offsets::RX_ASYNC_SUBTYPE] == offsets::RX_ASYNC_SUBTYPE_BATTERY {
                    Ok(DecodedPacket::RxAsyncBattery(BatteryTelemetry {
                        percent: p[offsets::RX_ASYNC_VAL_0].min(100),
                        charging: p[offsets::RX_ASYNC_VAL_1] != 0,
                    }))
                } else {
                    Ok(DecodedPacket::RxAsyncStatus {
                        active_dpi_stage: p[offsets::RX_ASYNC_VAL_0].saturating_sub(1),
                        polling_rate: PollingRate::from_wire_byte_lossy(p[offsets::RX_ASYNC_VAL_1]),
                    })
                }
            }
            ack_op => Ok(DecodedPacket::RxCommandAck(ack_op)),
        }
    }
}

fn parse_button_mapping(p: &[u8; REPORT_PAYLOAD_LEN]) -> ButtonMapping {
    let mut slots = [ButtonAction::Disabled; BUTTON_SLOT_COUNT];
    for (i, slot) in slots.iter_mut().enumerate() {
        let base = offsets::KEYS_DATA_START + i * 4;
        let raw = [p[base], p[base + 1], p[base + 2], p[base + 3]];
        if raw == [0x00, 0x00, 0x00, 0x00] || raw[0] == 0xFF {
            *slot = ButtonMapping::factory_default().slots[i];
        } else {
            *slot = ButtonAction::from_bytes(raw);
        }
    }
    ButtonMapping { slots }
}

fn parse_mouse_config(p: &[u8; REPORT_PAYLOAD_LEN], is_tx: bool) -> MouseConfig {
    // Handle uninitialized MCU flash (`0x00` or `0xFF` in DPI stage 1) identically to OEM JS
    let d0 = &p[offsets::CFG_DPI_STAGES_START..offsets::CFG_DPI_STAGES_START + 3];
    if d0 == [0, 0, 0] || d0 == [0xFF, 0xFF, 0xFF] {
        return MouseConfig::factory_default();
    }

    let mut dpi_stages = [800u16; MAX_DPI_STAGES];
    for (i, stage_out) in dpi_stages.iter_mut().enumerate() {
        let pos = offsets::CFG_DPI_STAGES_START + i * 2;
        let val = u16::from_le_bytes([p[pos], p[pos + 1]]);
        *stage_out = val.clamp(50, 18_000);
    }

    let dpi_count = p[offsets::CFG_DPI_COUNT].clamp(1, MAX_DPI_STAGES as u8);
    let active_dpi_stage = p[offsets::CFG_DPI_ACTIVE_ONE_BASED]
        .saturating_sub(1)
        .min(dpi_count - 1);

    let sleep_raw = p[offsets::CFG_SLEEP_MINUTES];
    let sleep_minutes = if VALID_SLEEP_MINUTES.contains(&sleep_raw) {
        sleep_raw
    } else {
        VALID_SLEEP_MINUTES_DEFAULT
    };

    let (wakeup_on_move, move_lights_off) = if is_tx || p[offsets::CFG_POWER_LIGHT_PACKED_TX] != 0 {
        let packed = p[offsets::CFG_POWER_LIGHT_PACKED_TX];
        (((packed >> 4) & 0x0F) != 0, (packed & 0x0F) != 0)
    } else {
        let packed = p[offsets::CFG_POWER_LIGHT_PACKED_RX_ALT];
        ((packed & 0x0F) != 0, ((packed >> 4) & 0x0F) != 0)
    };

    MouseConfig {
        dpi_stages,
        dpi_count,
        active_dpi_stage,
        polling_rate: PollingRate::from_wire_byte_lossy(p[offsets::CFG_REPORT_RATE]),
        lod: if p[offsets::CFG_LOD] == 2 {
            LodHeight::Mm2
        } else {
            LodHeight::Mm1
        },
        debounce_ms: p[offsets::CFG_DEBOUNCE_MS].min(30),
        sensor_flags: SensorFlags::from_bits(p[offsets::CFG_SENSOR_FLAGS]),
        sleep_minutes,
        light_mode: LightMode::from_wire(p[offsets::CFG_LIGHT_MODE]),
        scroll_direction: ScrollDirection::from_wire(p[offsets::CFG_SCROLL_DIR]),
        highspeed_mode: p[offsets::CFG_HIGHSPEED_MODE] != 0,
        wakeup_on_move,
        move_lights_off,
    }
}

/// Decoded semantic representation of any BK-R1X wire packet (TX or RX).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodedPacket {
    TxGetFirmwareVersion,
    TxGetBattery,
    TxGetOnlineStatus,
    TxSetLightMode(LightMode),
    TxGetMouseKeys,
    TxSetMouseKeys(ButtonMapping),
    TxGetMouseConfig,
    TxSetMouseConfig(MouseConfig),
    TxReadMacroChunk {
        offset: u16,
        len: u8,
    },
    TxWriteMacroChunk {
        offset: u16,
        len: u8,
        data: [u8; MACRO_CHUNK_DATA_LEN],
    },
    TxCommitMacroFlash,
    TxResetMacroFlash,
    RxFirmwareVersion(FirmwareVersion),
    RxBattery(BatteryTelemetry),
    RxOnlineStatus {
        online: bool,
    },
    RxMouseKeys(ButtonMapping),
    RxMouseConfig(MouseConfig),
    RxAsyncBattery(BatteryTelemetry),
    RxAsyncStatus {
        active_dpi_stage: u8,
        polling_rate: PollingRate,
    },
    RxDonglePairing,
    RxCommandAck(Opcode),
}

/// Zero-allocation in-memory hardware state machine tracking live BK-R1X state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HardwareStateMachine {
    pub connection: Option<ConnectionMode>,
    pub online: bool,
    pub firmware: FirmwareVersion,
    pub battery: BatteryTelemetry,
    pub config: MouseConfig,
    pub buttons: ButtonMapping,
}

impl Default for HardwareStateMachine {
    fn default() -> Self {
        Self {
            connection: None,
            online: false,
            firmware: FirmwareVersion::default(),
            battery: BatteryTelemetry {
                percent: 0,
                charging: false,
            },
            config: MouseConfig::factory_default(),
            buttons: ButtonMapping::factory_default(),
        }
    }
}

impl HardwareStateMachine {
    pub const fn new() -> Self {
        Self {
            connection: None,
            online: false,
            firmware: FirmwareVersion {
                major: 1,
                minor: 0,
                patch: 0,
            },
            battery: BatteryTelemetry {
                percent: 0,
                charging: false,
            },
            config: MouseConfig::factory_default(),
            buttons: ButtonMapping::factory_default(),
        }
    }

    /// Applies a decoded incoming or outgoing packet to synchronize cached state.
    pub fn apply_packet(&mut self, pkt: &DecodedPacket) {
        match *pkt {
            DecodedPacket::RxFirmwareVersion(v) => self.firmware = v,
            DecodedPacket::RxBattery(b) | DecodedPacket::RxAsyncBattery(b) => {
                self.battery = b;
                self.online = true;
            }
            DecodedPacket::RxOnlineStatus { online } => self.online = online,
            DecodedPacket::RxMouseKeys(k) | DecodedPacket::TxSetMouseKeys(k) => self.buttons = k,
            DecodedPacket::RxMouseConfig(c) | DecodedPacket::TxSetMouseConfig(c) => self.config = c,
            DecodedPacket::TxSetLightMode(m) => self.config.light_mode = m,
            DecodedPacket::RxAsyncStatus {
                active_dpi_stage,
                polling_rate,
            } => {
                if active_dpi_stage < self.config.dpi_count {
                    self.config.active_dpi_stage = active_dpi_stage;
                }
                self.config.polling_rate = polling_rate;
                self.online = true;
            }
            _ => {}
        }
    }
}
