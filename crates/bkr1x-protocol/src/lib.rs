#![no_std]
//! `#![no_std]` zero-heap-allocation HID protocol engine for the **Fire Phoenix BK-R1X**
//! wireless gaming mouse (PixArt PAW3311 sensor, YJX2012 OEM MCU).

pub mod checksum;
pub mod codec;
pub mod constants;
pub mod types;

pub use checksum::ChecksumStrategy;
pub use codec::{DecodedPacket, HardwareStateMachine, PacketCodec, RawReport};
pub use constants::*;
pub use types::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_parity_set_mouse_config_matches_oem_js() {
        let codec = PacketCodec::new(ChecksumStrategy::None);
        let cfg = MouseConfig::factory_default();
        let rpt = codec.encode_set_mouse_config(&cfg).unwrap();
        let wire = rpt.to_wire_bytes();

        // Exact byte offsets from app/page-51c144502f8589f3.js `setMouseConfigData`
        assert_eq!(wire[0], 0x00); // Report ID
        assert_eq!(wire[1], 0x55); // TX_MAGIC (85)
        assert_eq!(wire[2], 0x0F); // Opcode 15
        assert_eq!(wire[3], 0xAE); // 174
        assert_eq!(wire[4], 0x0A); // 10
        assert_eq!(wire[5], 0x2F); // 47
        assert_eq!(wire[6], 0x01);
        assert_eq!(wire[7], 0x01);
        assert_eq!(wire[10], 2); // Neon light mode
        assert_eq!(wire[11], 4); // 1000Hz -> report_rate(3) + 1 = 4
        assert_eq!(wire[12], 6); // dpi_count
        assert_eq!(wire[13], 3); // active_dpi_stage(2) + 1 = 3 (1600 DPI)
        assert_eq!(u16::from_le_bytes([wire[14], wire[15]]), 800);
        assert_eq!(u16::from_le_bytes([wire[18], wire[19]]), 1600);
        assert_eq!(u16::from_le_bytes([wire[24], wire[25]]), 18000);
        assert_eq!(wire[50], 1); // LOD 1mm
        assert_eq!(wire[51], 53); // sensor_flag (MotionSync | AngleSnap | Ripple | 0x04)
        assert_eq!(wire[52], 2); // key_respond debounce
        assert_eq!(wire[53], 10); // sleep_light 10 min
        assert_eq!(wire[55], 0x11); // wakeup(1)<<4 | move_light(1)

        let decoded = codec.decode(&wire).unwrap();
        assert_eq!(decoded, DecodedPacket::TxSetMouseConfig(cfg));
    }

    #[test]
    fn paw3311_bounds_validation_rejects_out_of_range_values() {
        let mut cfg = MouseConfig::factory_default();
        cfg.dpi_stages[0] = 25_000;
        assert_eq!(
            cfg.validate(),
            Err(ProtocolError::InvalidDpi {
                stage: 0,
                value: 25_000
            })
        );

        cfg.dpi_stages[0] = 1623; // Not aligned to 50 DPI step
        assert_eq!(
            cfg.validate(),
            Err(ProtocolError::InvalidDpi {
                stage: 0,
                value: 1623
            })
        );

        cfg.dpi_stages[0] = 1600;
        cfg.debounce_ms = 45;
        assert_eq!(cfg.validate(), Err(ProtocolError::InvalidDebounceMs(45)));
    }

    #[test]
    fn checksum_strategies_roundtrip() {
        for strategy in [
            ChecksumStrategy::None,
            ChecksumStrategy::SumComplement(0x55),
            ChecksumStrategy::SumComplement(0x00),
            ChecksumStrategy::Xor,
            ChecksumStrategy::Crc16Ccitt,
        ] {
            let codec = PacketCodec::new(strategy);
            let pkt = codec.encode_get_battery();
            assert!(strategy.verify(&pkt.payload));
            assert_eq!(
                codec.decode(&pkt.to_wire_bytes()).unwrap(),
                DecodedPacket::TxGetBattery
            );
        }
    }

    #[test]
    fn macro_compiler_matches_oem_binary_layout() {
        let steps = [
            MacroStep {
                input: MacroInput::KeyboardKey(0x04), // 'A'
                pressed: true,
                delay_ms: 10,
            },
            MacroStep {
                input: MacroInput::KeyboardKey(0x04),
                pressed: false,
                delay_ms: 15,
            },
        ];
        let mut flash = [0u8; MACRO_FLASH_TOTAL_LEN];
        let used = compile_macro_bank(&[&steps], &mut flash).unwrap();
        assert_eq!(used, 64 + 8);
        assert_eq!(u16::from_le_bytes([flash[0], flash[1]]), 64);
        assert_eq!(&flash[64..68], &[10, 0, 0x42, 0x04]); // Press + Keyboard(2)
        assert_eq!(&flash[68..72], &[15, 0, 0x82, 0x04]); // Last(0x80) + Release + Keyboard(2)
    }
}

