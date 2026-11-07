use crate::checksum::ChecksumStrategy;
use crate::constants::{
    AUTH_READ, AUTH_WRITE_CONFIG, AUTH_WRITE_KEYS, MACRO_CHUNK_DATA_LEN, MAX_DEBOUNCE_MS,
    MAX_DPI_STAGES, Opcode, PAW3311_DPI_MAX, PAW3311_DPI_MIN, PAW3311_DPI_STEP,
    REPORT_ID_MACRO_DIRECT, REPORT_ID_VENDOR_CMD, REPORT_PAYLOAD_LEN, RX_MAGIC, RX_PAIRING_MAGIC_0,
    RX_PAIRING_MAGIC_1, TX_MAGIC, VALID_SLEEP_MINUTES_DEFAULT, WIRE_FRAME_LEN, offsets,
};
use crate::types::{
    BatteryTelemetry, ButtonAction, ButtonMapping, ConnectionMode, FirmwareVersion, LightMode,
    LodHeight, MouseConfig, PollingRate, ProtocolError, ScrollDirection, SensorFlags,
    VALID_SLEEP_MINUTES,
};

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
    pub const fn to_wire_bytes(&self) -> [u8; WIRE_FRAME_LEN] {
        let mut wire = [0u8; WIRE_FRAME_LEN];
        wire[0] = self.report_id;
        let mut idx = 0;
        while idx < REPORT_PAYLOAD_LEN {
            wire[idx + 1] = self.payload[idx];
            idx += 1;
        }
        wire
    }

    pub fn from_slice(raw: &[u8]) -> Result<Self, ProtocolError> {
        if raw.len() < 2 {
            return Err(ProtocolError::PacketTooShort {
                actual: raw.len(),
                expected_min: 2,
            });
        }

        let has_leading_report_id = raw.len() == WIRE_FRAME_LEN
            || (raw.len() >= 3
                && (raw[0] == REPORT_ID_VENDOR_CMD || raw[0] == REPORT_ID_MACRO_DIRECT)
                && raw[0] != TX_MAGIC
                && raw[0] != RX_MAGIC);

        let (report_id, body) = if has_leading_report_id {
            (raw[0], &raw[1..])
        } else {
            (REPORT_ID_VENDOR_CMD, raw)
        };

        let mut payload = [0u8; REPORT_PAYLOAD_LEN];
        let copy_len = body.len().min(REPORT_PAYLOAD_LEN);
        payload[..copy_len].copy_from_slice(&body[..copy_len]);
        Ok(Self { report_id, payload })
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct PacketCodec {
    pub checksum: ChecksumStrategy,
}

impl PacketCodec {
    pub const fn new(checksum: ChecksumStrategy) -> Self {
        Self { checksum }
    }

    fn finalize(&self, mut report: RawReport) -> RawReport {
        self.checksum.apply(&mut report.payload);
        report
    }

    pub fn encode_get_firmware_version(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetFirmwareVersion as u8;
        self.finalize(rpt)
    }

    pub fn encode_get_battery(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetBatteryInfo as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_READ[0];
        rpt.payload[offsets::AUTH_1] = AUTH_READ[1];
        rpt.payload[offsets::DATA_LEN] = offsets::DATA_LEN_BATTERY_ONLINE;
        rpt.payload[offsets::TX_SUBHEADER_0] = 0x01;
        rpt.payload[offsets::TX_SUBHEADER_1] = 0x01;
        self.finalize(rpt)
    }

    pub fn encode_get_online_status(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetOnlineStatus as u8;
        rpt.payload[offsets::AUTH_0] = 0x00;
        rpt.payload[offsets::AUTH_1] = 0x01;
        rpt.payload[offsets::DATA_LEN] = offsets::DATA_LEN_BATTERY_ONLINE;
        self.finalize(rpt)
    }

    pub fn encode_set_light_mode(&self, mode: LightMode) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::SetLightMode as u8;
        rpt.payload[offsets::DATA_LEN] = offsets::DATA_LEN_LIGHT_MODE;
        rpt.payload[offsets::TX_LIGHT_MODE_VALUE] = mode as u8;
        self.finalize(rpt)
    }

    pub fn encode_get_mouse_keys(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetMouseKeys as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_READ[0];
        rpt.payload[offsets::AUTH_1] = AUTH_READ[1];
        rpt.payload[offsets::DATA_LEN] = offsets::DATA_LEN_MOUSE_KEYS;
        self.finalize(rpt)
    }

    pub fn encode_set_mouse_keys(
        &self,
        mapping: &ButtonMapping,
    ) -> Result<RawReport, ProtocolError> {
        mapping.validate()?;
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::SetMouseKeys as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_WRITE_KEYS[0];
        rpt.payload[offsets::AUTH_1] = AUTH_WRITE_KEYS[1];
        rpt.payload[offsets::DATA_LEN] = offsets::DATA_LEN_MOUSE_KEYS;

        for (slot_idx, action) in mapping.slots.iter().enumerate() {
            let start = offsets::KEYS_DATA_START + slot_idx * 4;
            rpt.payload[start..start + 4].copy_from_slice(&action.to_bytes());
        }
        Ok(self.finalize(rpt))
    }

    pub fn encode_get_mouse_config(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::GetMouseConfig as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_READ[0];
        rpt.payload[offsets::AUTH_1] = AUTH_READ[1];
        rpt.payload[offsets::DATA_LEN] = offsets::DATA_LEN_MOUSE_CONFIG;
        rpt.payload[offsets::TX_SUBHEADER_0] = 0x01;
        rpt.payload[offsets::TX_SUBHEADER_1] = 0x01;
        self.finalize(rpt)
    }

    pub fn encode_set_mouse_config(&self, cfg: &MouseConfig) -> Result<RawReport, ProtocolError> {
        cfg.validate()?;
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::SetMouseConfig as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_WRITE_CONFIG[0];
        rpt.payload[offsets::AUTH_1] = AUTH_WRITE_CONFIG[1];
        rpt.payload[offsets::DATA_LEN] = offsets::DATA_LEN_MOUSE_CONFIG;
        rpt.payload[offsets::TX_SUBHEADER_0] = 0x01;
        rpt.payload[offsets::TX_SUBHEADER_1] = 0x01;

        rpt.payload[offsets::CFG_LIGHT_MODE] = cfg.light_mode as u8;
        rpt.payload[offsets::CFG_REPORT_RATE] = cfg.polling_rate.to_wire_byte();
        rpt.payload[offsets::CFG_DPI_COUNT] = cfg.dpi_count;
        rpt.payload[offsets::CFG_DPI_ACTIVE_ONE_BASED] = cfg.active_dpi_stage + 1;

        for (idx, &dpi) in cfg.dpi_stages.iter().enumerate() {
            let [lo, hi] = dpi.to_le_bytes();
            let pos = offsets::CFG_DPI_STAGES_START + idx * 2;
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

        Ok(self.finalize(rpt))
    }

    pub fn encode_read_macro_chunk(&self, offset: u16, len: u8) -> RawReport {
        let mut rpt = RawReport {
            report_id: REPORT_ID_MACRO_DIRECT,
            payload: [0u8; REPORT_PAYLOAD_LEN],
        };
        let [off_lo, off_hi] = offset.to_le_bytes();
        rpt.payload[offsets::DIRECT_MACRO_OPCODE] = Opcode::ReadMacroChunk as u8;
        rpt.payload[offsets::DIRECT_MACRO_LEN] = len.min(MACRO_CHUNK_DATA_LEN as u8);
        rpt.payload[offsets::DIRECT_MACRO_OFFSET_LO] = off_lo;
        rpt.payload[offsets::DIRECT_MACRO_OFFSET_HI] = off_hi;
        self.finalize(rpt)
    }

    pub fn encode_write_macro_chunk(&self, offset: u16, chunk: &[u8]) -> RawReport {
        let mut rpt = RawReport::default();
        let bounded_len = chunk.len().min(MACRO_CHUNK_DATA_LEN);
        let [off_lo, off_hi] = offset.to_le_bytes();

        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::WriteMacroChunk as u8;
        rpt.payload[offsets::DATA_LEN] = bounded_len as u8;
        rpt.payload[offsets::TX_MACRO_OFFSET_LO] = off_lo;
        rpt.payload[offsets::TX_MACRO_OFFSET_HI] = off_hi;
        rpt.payload[offsets::TX_MACRO_BANK_FLAG] = 0x00;
        rpt.payload[offsets::TX_MACRO_DATA_START..offsets::TX_MACRO_DATA_START + bounded_len]
            .copy_from_slice(&chunk[..bounded_len]);
        self.finalize(rpt)
    }

    pub fn encode_commit_macro_flash(&self) -> RawReport {
        let mut rpt = RawReport::default();
        rpt.payload[offsets::MAGIC] = TX_MAGIC;
        rpt.payload[offsets::OPCODE] = Opcode::CommitMacroFlash as u8;
        rpt.payload[offsets::AUTH_0] = AUTH_WRITE_KEYS[0];
        rpt.payload[offsets::AUTH_1] = AUTH_WRITE_KEYS[1];
        rpt.payload[offsets::TX_COMMIT_MACRO_TAG] = offsets::COMMIT_MACRO_TAG_VALUE;
        self.finalize(rpt)
    }

    pub fn decode(&self, raw: &[u8]) -> Result<DecodedPacket, ProtocolError> {
        let rpt = RawReport::from_slice(raw)?;
        if !self.checksum.verify(&rpt.payload) {
            return Err(ProtocolError::ChecksumMismatch);
        }

        if rpt.report_id == REPORT_ID_MACRO_DIRECT {
            if rpt.payload[offsets::DIRECT_MACRO_OPCODE] == Opcode::ReadMacroChunk as u8 {
                let len = rpt.payload[offsets::DIRECT_MACRO_LEN];
                let offset = u16::from_le_bytes([
                    rpt.payload[offsets::DIRECT_MACRO_OFFSET_LO],
                    rpt.payload[offsets::DIRECT_MACRO_OFFSET_HI],
                ]);
                return Ok(DecodedPacket::TxReadMacroChunk { offset, len });
            }
            return Ok(DecodedPacket::TxResetMacroFlash);
        }

        let p = &rpt.payload;
        match p[offsets::MAGIC] {
            TX_MAGIC => Self::decode_tx_payload(p),
            RX_MAGIC => Self::decode_rx_payload(p),
            RX_PAIRING_MAGIC_0 if p[offsets::OPCODE] == RX_PAIRING_MAGIC_1 => {
                Ok(DecodedPacket::RxDonglePairing)
            }
            other => Err(ProtocolError::InvalidMagicHeader(other)),
        }
    }

    fn decode_tx_payload(p: &[u8; REPORT_PAYLOAD_LEN]) -> Result<DecodedPacket, ProtocolError> {
        let raw_opcode = p[offsets::OPCODE];
        let opcode =
            Opcode::from_byte(raw_opcode).ok_or(ProtocolError::UnknownOpcode(raw_opcode))?;

        match opcode {
            Opcode::GetFirmwareVersion => Ok(DecodedPacket::TxGetFirmwareVersion),
            Opcode::GetBatteryInfo => Ok(DecodedPacket::TxGetBattery),
            Opcode::GetOnlineStatus => Ok(DecodedPacket::TxGetOnlineStatus),
            Opcode::SetLightMode => Ok(DecodedPacket::TxSetLightMode(LightMode::from_wire(
                p[offsets::TX_LIGHT_MODE_VALUE],
            ))),
            Opcode::GetMouseKeys => Ok(DecodedPacket::TxGetMouseKeys),
            Opcode::SetMouseKeys => Ok(DecodedPacket::TxSetMouseKeys(parse_button_mapping(p))),
            Opcode::GetMouseConfig => Ok(DecodedPacket::TxGetMouseConfig),
            Opcode::SetMouseConfig => {
                Ok(DecodedPacket::TxSetMouseConfig(parse_mouse_config(p, true)))
            }
            Opcode::WriteMacroChunk => {
                let len = p[offsets::DATA_LEN].min(MACRO_CHUNK_DATA_LEN as u8);
                let offset = u16::from_le_bytes([
                    p[offsets::TX_MACRO_OFFSET_LO],
                    p[offsets::TX_MACRO_OFFSET_HI],
                ]);
                let mut data = [0u8; MACRO_CHUNK_DATA_LEN];
                let end = offsets::TX_MACRO_DATA_START + len as usize;
                data[..len as usize].copy_from_slice(&p[offsets::TX_MACRO_DATA_START..end]);
                Ok(DecodedPacket::TxWriteMacroChunk { offset, len, data })
            }
            Opcode::CommitMacroFlash => Ok(DecodedPacket::TxCommitMacroFlash),
            Opcode::ReadMacroChunk => Ok(DecodedPacket::TxReadMacroChunk {
                offset: u16::from_le_bytes([
                    p[offsets::TX_MACRO_OFFSET_LO],
                    p[offsets::TX_MACRO_OFFSET_HI],
                ]),
                len: p[offsets::DATA_LEN],
            }),
            Opcode::AsyncTelemetry => Err(ProtocolError::UnknownOpcode(raw_opcode)),
        }
    }

    fn decode_rx_payload(p: &[u8; REPORT_PAYLOAD_LEN]) -> Result<DecodedPacket, ProtocolError> {
        let raw_opcode = p[offsets::OPCODE];
        let opcode =
            Opcode::from_byte(raw_opcode).ok_or(ProtocolError::UnknownOpcode(raw_opcode))?;

        match opcode {
            Opcode::GetFirmwareVersion => Ok(DecodedPacket::RxFirmwareVersion(FirmwareVersion {
                major: ascii_digit_or_zero(p[offsets::RX_FW_MAJOR_ASCII]),
                minor: ascii_digit_or_zero(p[offsets::RX_FW_MINOR_ASCII]),
                patch: ascii_digit_or_zero(p[offsets::RX_FW_PATCH_ASCII]),
            })),
            Opcode::GetBatteryInfo => Ok(DecodedPacket::RxBattery(BatteryTelemetry {
                percent: p[offsets::RX_BATTERY_PERCENT].min(100),
                charging: p[offsets::RX_BATTERY_CHARGING] != 0,
            })),
            Opcode::GetOnlineStatus => Ok(DecodedPacket::RxOnlineStatus {
                online: p[offsets::RX_ONLINE_FLAG] == offsets::ONLINE_AWAKE_VALUE,
            }),
            Opcode::GetMouseKeys => Ok(DecodedPacket::RxMouseKeys(parse_button_mapping(p))),
            Opcode::GetMouseConfig => {
                Ok(DecodedPacket::RxMouseConfig(parse_mouse_config(p, false)))
            }
            Opcode::AsyncTelemetry => {
                if p[offsets::RX_ASYNC_SUBTYPE] == offsets::RX_ASYNC_SUBTYPE_BATTERY {
                    return Ok(DecodedPacket::RxAsyncBattery(BatteryTelemetry {
                        percent: p[offsets::RX_ASYNC_VAL_0].min(100),
                        charging: p[offsets::RX_ASYNC_VAL_1] != 0,
                    }));
                }
                Ok(DecodedPacket::RxAsyncStatus {
                    active_dpi_stage: p[offsets::RX_ASYNC_VAL_0].saturating_sub(1),
                    polling_rate: PollingRate::from_wire_byte_lossy(p[offsets::RX_ASYNC_VAL_1]),
                })
            }
            ack_op => Ok(DecodedPacket::RxCommandAck(ack_op)),
        }
    }
}

const fn ascii_digit_or_zero(byte: u8) -> u8 {
    if byte.is_ascii_digit() {
        byte - b'0'
    } else {
        0
    }
}

fn parse_button_mapping(p: &[u8; REPORT_PAYLOAD_LEN]) -> ButtonMapping {
    let slots = core::array::from_fn(|idx| {
        let base = offsets::KEYS_DATA_START + idx * 4;
        let raw = [p[base], p[base + 1], p[base + 2], p[base + 3]];
        // Blank MCU flash sectors contain all `0x00` or `0xFF`; fall back to factory slot mapping.
        if raw == [0x00; 4] || raw[0] == 0xFF {
            ButtonMapping::DEFAULT_SLOTS[idx]
        } else {
            ButtonAction::from_bytes(raw)
        }
    });
    ButtonMapping { slots }
}

fn parse_mouse_config(p: &[u8; REPORT_PAYLOAD_LEN], is_tx: bool) -> MouseConfig {
    let stage1_probe = &p[offsets::CFG_DPI_STAGES_START..offsets::CFG_DPI_STAGES_START + 3];
    if stage1_probe == [0, 0, 0] || stage1_probe == [0xFF, 0xFF, 0xFF] {
        return MouseConfig::factory_default();
    }

    let dpi_stages = core::array::from_fn(|idx| {
        let pos = offsets::CFG_DPI_STAGES_START + idx * 2;
        let raw_dpi =
            u16::from_le_bytes([p[pos], p[pos + 1]]).clamp(PAW3311_DPI_MIN, PAW3311_DPI_MAX);
        let snapped = (raw_dpi / PAW3311_DPI_STEP) * PAW3311_DPI_STEP;
        snapped.max(PAW3311_DPI_MIN)
    });

    let dpi_count = p[offsets::CFG_DPI_COUNT].clamp(1, MAX_DPI_STAGES as u8);
    let active_dpi_stage = p[offsets::CFG_DPI_ACTIVE_ONE_BASED]
        .saturating_sub(1)
        .min(dpi_count - 1);

    let raw_sleep = p[offsets::CFG_SLEEP_MINUTES];
    let sleep_minutes = if VALID_SLEEP_MINUTES.contains(&raw_sleep) {
        raw_sleep
    } else {
        VALID_SLEEP_MINUTES_DEFAULT
    };

    // YJX2012 V1 quirk: TX packs `(wakeup << 4) | move_lights_off` at byte 54, whereas some RX
    // firmware builds return `0x00` at byte 54 and place `(move_lights_off << 4) | wakeup` at byte 55.
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
        debounce_ms: p[offsets::CFG_DEBOUNCE_MS].min(MAX_DEBOUNCE_MS),
        sensor_flags: SensorFlags::from_bits(p[offsets::CFG_SENSOR_FLAGS]),
        sleep_minutes,
        light_mode: LightMode::from_wire(p[offsets::CFG_LIGHT_MODE]),
        scroll_direction: ScrollDirection::from_wire(p[offsets::CFG_SCROLL_DIR]),
        highspeed_mode: p[offsets::CFG_HIGHSPEED_MODE] != 0,
        wakeup_on_move,
        move_lights_off,
    }
}

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
        Self::new()
    }
}

impl HardwareStateMachine {
    pub const fn new() -> Self {
        Self {
            connection: None,
            online: false,
            firmware: FirmwareVersion::new(1, 0, 0),
            battery: BatteryTelemetry {
                percent: 0,
                charging: false,
            },
            config: MouseConfig::factory_default(),
            buttons: ButtonMapping::factory_default(),
        }
    }

    pub fn apply_packet(&mut self, pkt: &DecodedPacket) {
        match *pkt {
            DecodedPacket::RxFirmwareVersion(ver) => self.firmware = ver,
            DecodedPacket::RxBattery(bat) | DecodedPacket::RxAsyncBattery(bat) => {
                self.battery = bat;
                self.online = true;
            }
            DecodedPacket::RxOnlineStatus { online } => self.online = online,
            DecodedPacket::RxMouseKeys(keys) | DecodedPacket::TxSetMouseKeys(keys) => {
                self.buttons = keys;
            }
            DecodedPacket::RxMouseConfig(cfg) | DecodedPacket::TxSetMouseConfig(cfg) => {
                self.config = cfg;
            }
            DecodedPacket::TxSetLightMode(mode) => self.config.light_mode = mode,
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
