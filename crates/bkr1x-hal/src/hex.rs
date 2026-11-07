const HEX_LOWER: &[u8; 16] = b"0123456789abcdef";

pub fn encode_hex_spaced(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let mut out = String::with_capacity(bytes.len() * 3 - 1);
    for (idx, &byte) in bytes.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        out.push(HEX_LOWER[(byte >> 4) as usize] as char);
        out.push(HEX_LOWER[(byte & 0x0F) as usize] as char);
    }
    out
}

pub fn decode_flexible_hex(input: &str) -> Result<Vec<u8>, String> {
    let raw = input.as_bytes();
    let mut out = Vec::with_capacity(raw.len() / 2);
    let mut high_nibble: Option<u8> = None;
    let mut idx = 0;

    while idx < raw.len() {
        let byte = raw[idx];
        if high_nibble.is_none() && byte == b'0' && matches!(raw.get(idx + 1), Some(b'x' | b'X')) {
            idx += 2;
            continue;
        }

        if let Some(nib) = decode_hex_nibble(byte) {
            if let Some(hi) = high_nibble.take() {
                out.push((hi << 4) | nib);
            } else {
                high_nibble = Some(nib);
            }
            idx += 1;
            continue;
        }

        if byte.is_ascii_whitespace() || matches!(byte, b',' | b':' | b'-' | b'[' | b']') {
            if high_nibble.is_some() {
                return Err(format!(
                    "incomplete hex byte before separator at offset {idx}"
                ));
            }
            idx += 1;
            continue;
        }

        return Err(format!(
            "unexpected character '{}' at offset {idx}",
            byte as char
        ));
    }

    if high_nibble.is_some() {
        return Err("hex string has an odd number of hex digits".to_owned());
    }
    if out.is_empty() {
        return Err("hex string contains no hexadecimal bytes".to_owned());
    }

    Ok(out)
}

const fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
