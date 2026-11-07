//! YJX2012 V1 firmware authenticates reports via magic byte `0x55` plus command-specific
//! 2-byte auth tokens with zero-padded trailers (`ChecksumStrategy::None`). Alternate OEM
//! checksum algorithms are retained for firmware variants and `sniff-verify` diagnostics.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChecksumStrategy {
    #[default]
    None,
    /// `base.wrapping_sub(sum(payload[..len - 1]))` written to `payload[len - 1]`.
    SumComplement(u8),
    Xor,
    /// CRC-16/CCITT-FALSE (`poly = 0x1021`, `init = 0xFFFF`) written big-endian to `payload[len - 2..len]`.
    Crc16Ccitt,
}

impl ChecksumStrategy {
    pub const fn crc16_ccitt(bytes: &[u8]) -> u16 {
        let mut crc: u16 = 0xFFFF;
        let mut idx = 0;
        while idx < bytes.len() {
            crc ^= (bytes[idx] as u16) << 8;
            let mut bit = 0;
            while bit < 8 {
                if (crc & 0x8000) != 0 {
                    crc = (crc << 1) ^ 0x1021;
                } else {
                    crc <<= 1;
                }
                bit += 1;
            }
            idx += 1;
        }
        crc
    }

    pub fn apply(self, payload: &mut [u8]) {
        match self {
            Self::None => {}
            Self::SumComplement(base) => {
                if let [body @ .., trailer] = payload
                    && !body.is_empty()
                {
                    let sum = body.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
                    *trailer = base.wrapping_sub(sum);
                }
            }
            Self::Xor => {
                if let [body @ .., trailer] = payload
                    && !body.is_empty()
                {
                    *trailer = body.iter().fold(0u8, |acc, &b| acc ^ b);
                }
            }
            Self::Crc16Ccitt => {
                if let [body @ .., hi, lo] = payload
                    && !body.is_empty()
                {
                    let [crc_hi, crc_lo] = Self::crc16_ccitt(body).to_be_bytes();
                    *hi = crc_hi;
                    *lo = crc_lo;
                }
            }
        }
    }

    pub fn verify(self, payload: &[u8]) -> bool {
        match self {
            Self::None => true,
            Self::SumComplement(base) => {
                let [body @ .., expected] = payload else {
                    return false;
                };
                !body.is_empty() && {
                    let sum = body.iter().fold(0u8, |acc, &b| acc.wrapping_add(b));
                    *expected == base.wrapping_sub(sum)
                }
            }
            Self::Xor => {
                let [body @ .., expected] = payload else {
                    return false;
                };
                !body.is_empty() && *expected == body.iter().fold(0u8, |acc, &b| acc ^ b)
            }
            Self::Crc16Ccitt => {
                let [body @ .., hi, lo] = payload else {
                    return false;
                };
                !body.is_empty() && Self::crc16_ccitt(body).to_be_bytes() == [*hi, *lo]
            }
        }
    }
}
