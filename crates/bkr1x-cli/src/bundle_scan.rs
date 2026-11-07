use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub fn scan_bundle_directory(dir: &Path) -> Result<(), String> {
    let entries = fs::read_dir(dir)
        .map_err(|e| format!("failed opening bundle directory {}: {e}", dir.display()))?;

    let mut vid_pids: BTreeSet<(u16, u16, u16, u16)> = BTreeSet::new();
    let mut array_sizes: BTreeSet<usize> = BTreeSet::new();
    let mut tx_opcodes: BTreeMap<u8, String> = BTreeMap::new();
    let mut rx_headers: BTreeSet<(u8, u8)> = BTreeSet::new();
    let mut usb_vid: Option<u16> = None;
    let mut js_count = 0usize;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("js") {
            continue;
        }
        let Ok(txt) = fs::read_to_string(&path) else {
            continue;
        };
        js_count += 1;

        if let Some(vid) = find_usb_vid(&txt) {
            usb_vid = Some(vid);
        }
        extract_filter_tuples(&txt, &mut vid_pids);
        extract_array_allocs(&txt, &mut array_sizes);
        extract_tx_commands(&txt, &mut tx_opcodes);
        extract_rx_headers(&txt, &mut rx_headers);
    }

    if js_count == 0 {
        return Err(format!("no .js bundle files found in {}", dir.display()));
    }

    println!("==========================================================================");
    println!(
        " BK-R1X NATIVE RUST BUNDLE ANALYZER — Scanned {js_count} JS chunks in {}",
        dir.display()
    );
    println!("==========================================================================");

    println!("\n[1] EXTRACTED WEBHID DEVICE FILTERS");
    for &(vid, pid, up, usage) in &vid_pids {
        let mode = if Some(vid) == usb_vid {
            "USB-C Wired"
        } else {
            "2.4GHz Wireless Dongle"
        };
        println!(
            "  • {mode:<24} VID=0x{vid:04X} ({vid})  PID=0x{pid:04X} ({pid})  UsagePage=0x{up:04X}  Usage=0x{usage:04X}"
        );
    }

    println!("\n[2] REPORT & FLASH BUFFER CONSTANTS");
    println!("  • Detected buffer sizes (bytes): {array_sizes:?}");
    println!("  • HID Output Wire Frame        : 65 bytes (1B ReportID 0x00 + 64B Payload)");

    println!("\n[3] EXTRACTED TX COMMAND OPCODES (Header = 0x55)");
    for (op, fn_name) in &tx_opcodes {
        println!("  • 0x{op:02X} ({op:>3}) -> {fn_name}()");
    }

    println!("\n[4] EXTRACTED RX NOTIFICATION HEADERS");
    for (b0, b1) in &rx_headers {
        println!("  • [0x{b0:02X}, 0x{b1:02X}]");
    }

    println!("\n[5] CHECKSUM & AUTHENTICATION SUMMARY");
    println!("  • Strategy : ChecksumStrategy::None (Zero-padded 64B payload)");
    println!(
        "  • Framing  : TX=0x55, RX=0xAA, ReadAuth=[0xA5, 0x0B], WriteKeys=[0xA5, 0x22], WriteCfg=[0xAE, 0x0A]"
    );
    println!("==========================================================================");
    Ok(())
}

fn leading_ascii_digits(s: &str) -> &str {
    let end = s
        .as_bytes()
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    &s[..end]
}

fn trailing_ascii_digits(s: &str) -> &str {
    let count = s
        .as_bytes()
        .iter()
        .rev()
        .take_while(|b| b.is_ascii_digit())
        .count();
    &s[s.len() - count..]
}

fn leading_js_ident(s: &str) -> &str {
    let end = s
        .as_bytes()
        .iter()
        .take_while(|b| b.is_ascii_alphanumeric() || **b == b'_')
        .count();
    &s[..end]
}

fn find_usb_vid(txt: &str) -> Option<u16> {
    let marker = "getConnectMode(){return ";
    let idx = txt.find(marker)?;
    leading_ascii_digits(&txt[idx + marker.len()..])
        .parse()
        .ok()
}

fn extract_filter_tuples(txt: &str, out: &mut BTreeSet<(u16, u16, u16, u16)>) {
    const VENDOR_KEY: &str = "vendorId:";
    let mut search_from = 0;
    while let Some(rel) = txt[search_from..].find(VENDOR_KEY) {
        let pos = search_from + rel;
        let window_end = txt.floor_char_boundary(pos.saturating_add(120));
        let slice = &txt[pos..window_end];
        let obj = &slice[..slice.find('}').unwrap_or(slice.len())];

        if let (Some(vid), Some(pid), Some(up), Some(usage)) = (
            read_kv_u16(obj, VENDOR_KEY),
            read_kv_u16(obj, "productId:"),
            read_kv_u16(obj, "usagePage:"),
            read_kv_u16(obj, "usage:"),
        ) {
            out.insert((vid, pid, up, usage));
        }
        search_from = pos + VENDOR_KEY.len();
    }
}

fn read_kv_u16(s: &str, key: &str) -> Option<u16> {
    let idx = s.find(key)?;
    leading_ascii_digits(&s[idx + key.len()..]).parse().ok()
}

fn extract_array_allocs(txt: &str, out: &mut BTreeSet<usize>) {
    const MARKER: &str = "Array(";
    let mut from = 0;
    while let Some(rel) = txt[from..].find(MARKER) {
        let pos = from + rel + MARKER.len();
        let rest = &txt[pos..];
        let digits = leading_ascii_digits(rest);
        if let Ok(len) = digits.parse::<usize>()
            && rest[digits.len()..].starts_with(").fill(0)")
            && matches!(len, 16 | 20 | 32 | 64 | 65 | 4032 | 4096)
        {
            out.insert(len);
        }
        from = pos;
    }
}

fn extract_tx_commands(txt: &str, out: &mut BTreeMap<u8, String>) {
    const MARKER: &str = "async ";
    let mut from = 0;
    while let Some(rel) = txt[from..].find(MARKER) {
        let pos = from + rel + MARKER.len();
        let rest = &txt[pos..];
        let fn_name = leading_js_ident(rest);
        if !fn_name.is_empty() {
            let next_fn = rest[fn_name.len()..]
                .find(MARKER)
                .map_or(rest.len(), |idx| fn_name.len() + idx);
            let window_end = rest.floor_char_boundary(next_fn.min(900));
            let body_window = &rest[..window_end];

            for pat in ["[1]=85,e[2]=", "[1]=85,t[2]="] {
                if let Some(p_idx) = body_window.find(pat)
                    && let Ok(op) = leading_ascii_digits(&body_window[p_idx + pat.len()..]).parse()
                {
                    out.entry(op).or_insert_with(|| fn_name.to_owned());
                }
            }

            const INLINE_PAT: &str = "[0,85,";
            let mut in_from = 0;
            while let Some(i_idx) = body_window[in_from..].find(INLINE_PAT) {
                let abs = in_from + i_idx + INLINE_PAT.len();
                if let Ok(op) = leading_ascii_digits(&body_window[abs..]).parse() {
                    out.entry(op).or_insert_with(|| fn_name.to_owned());
                }
                in_from = abs;
            }
        }
        from = pos;
    }
}

fn extract_rx_headers(txt: &str, out: &mut BTreeSet<(u8, u8)>) {
    const MARKER: &str = "==n[0]&&";
    let mut from = 0;
    while let Some(rel) = txt[from..].find(MARKER) {
        let eq_pos = from + rel;
        let b0_str = trailing_ascii_digits(&txt[..eq_pos]);
        let after = &txt[eq_pos + MARKER.len()..];
        let b1_str = leading_ascii_digits(after);
        if let (Ok(b0), Ok(b1)) = (b0_str.parse::<u8>(), b1_str.parse::<u8>())
            && after[b1_str.len()..].starts_with("==n[1]")
        {
            out.insert((b0, b1));
        }
        from = eq_pos + MARKER.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_multibyte_bundle_windows_never_panic() {
        let mut chunk = String::from(
            "vendorId:43172,productId:8789,usagePage:65281,usage:16}鼠标回报率设置鼠标回报率设置鼠标回报率设置鼠标回报率设置",
        );
        chunk.push_str("async setMouseConfigData(){let e=Array(65).fill(0);e[1]=85,e[2]=15;");
        for _ in 0..180 {
            chunk.push_str("回报率");
        }
        chunk.push_str("if(170==n[0]&&48==n[1])return;}");

        let mut filters = BTreeSet::new();
        let mut sizes = BTreeSet::new();
        let mut ops = BTreeMap::new();
        let mut rx = BTreeSet::new();

        extract_filter_tuples(&chunk, &mut filters);
        extract_array_allocs(&chunk, &mut sizes);
        extract_tx_commands(&chunk, &mut ops);
        extract_rx_headers(&chunk, &mut rx);

        assert!(filters.contains(&(0xA8A4, 0x2255, 0xFF01, 0x0010)));
        assert!(sizes.contains(&65));
        assert_eq!(
            ops.get(&0x0F).map(String::as_str),
            Some("setMouseConfigData")
        );
        assert!(rx.contains(&(0xAA, 0x30)));
    }
}
