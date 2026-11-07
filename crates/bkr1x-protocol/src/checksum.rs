//! Configurable zero-allocation packet checksum engine.
//!
//! Note: The verified Fire Phoenix BK-R1X YJX2012 V1 firmware authenticates packets using
//! header `0x55` + command tokens (`0xA5 0x0B`, `0xA5 0x22`, `0xAE 0x0A`) with zero-padded
//! trailing bytes (`ChecksumStrategy::None`). Additional OEM YJX2012/Compx/Sinowealth strategies
//! (`SumComplement`, `Xor`, `Crc16Ccitt`) are built in for alternate firmware revisions and
//! `bkr1x-cli sniff-verify` diagnostics.

/// Packet checksum calculation and verification strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChecksumStrategy {
    /// No trailing checksum byte (default for BK-R1X YJX2012 V1 firmware).
    #[default]
    None,
    /// 8-bit sum complement stored in the final byte: `base.wrapping_sub(sum(payload[..len-1]))`.
    /// Common OEM bases: `0x55` (`0x55 - sum`) or `0x00` (`two's complement`).
    SumComplement(u8),
    /// 8-bit XOR fold of `payload[..len-1]` stored in `payload[len-1]`.
    Xor,
    /// 16-bit CRC-16/CCITT-FALSE (`poly = 0x1021`, `init = 0xFFFF`) stored big-endian in `payload[len-2..len]`.
    Crc16Ccitt,
}

impl ChecksumStrategy {
    /// Computes CRC-16/CCITT-FALSE over an arbitrary byte slice in `O(n)` without lookup tables.
    pub const fn crc16_ccitt(bytes: &[u8]) -> u16 {
        let mut crc: u16 = 0xFFFF;
        let mut i = 0;
        while i < bytes.len() {
            crc ^= (bytes[i] as u16) << 8;
            let mut bit = 0;
            while bit < 8 {
                if (crc & 0x8000) != 0 {
                    crc = (crc << 1) ^ 0x1021;
                } else {
                    crc <<= 1;
                }
                bit += 1;
            }
            i += 1;
        }
        crc
    }

    /// Writes the computed checksum into the end of `payload` in-place.
    pub fn apply(self, payload: &mut [u8]) {
        let len = payload.len();
        match self {
            Self::None => {}
            Self::SumComplement(base) => {
                if len >= 2 {
                    let sum = payload[..len - 1]
                        .iter()
                        .fold(0u8, |acc, &b| acc.wrapping_add(b));
                    payload[len - 1] = base.wrapping_sub(sum);
                }
            }
            Self::Xor => {
                if len >= 2 {
                    let xor = payload[..len - 1].iter().fold(0u8, |acc, &b| acc ^ b);
                    payload[len - 1] = xor;
                }
            }
            Self::Crc16Ccitt => {
                if len >= 3 {
                    let crc = Self::crc16_ccitt(&payload[..len - 2]);
                    let [hi, lo] = crc.to_be_bytes();
                    payload[len - 2] = hi;
                    payload[len - 1] = lo;
                }
            }
        }
    }

    /// Verifies that `payload` satisfies the configured checksum rule.
    pub fn verify(self, payload: &[u8]) -> bool {
        let len = payload.len();
        match self {
            Self::None => true,
            Self::SumComplement(base) => {
                if len < 2 {
                    return false;
                }
                let sum = payload[..len - 1]
                    .iter()
                    .fold(0u8, |acc, &b| acc.wrapping_add(b));
                payload[len - 1] == base.wrapping_sub(sum)
            }
            Self::Xor => {
                if len < 2 {
                    return false;
                }
                let xor = payload[..len - 1].iter().fold(0u8, |acc, &b| acc ^ b);
                payload[len - 1] == xor
            }
            Self::Crc16Ccitt => {
                if len < 3 {
                    return false;
                }
                let crc = Self::crc16_ccitt(&payload[..len - 2]);
                let expected = crc.to_be_bytes();
                payload[len - 2] == expected[0] && payload[len - 1] == expected[1]
            }
        }
    }
}
