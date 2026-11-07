//! Self-Extracting (SFX) deflate bundle encoder & decoder for single-file GitHub Release installers.
//!
//! Appends compressed `bkr1x-daemon.exe`, `bkr1x-gui.exe`, and `bkr1x-cli.exe` binaries onto
//! the compiled `bkr1x-installer.exe` PE stub followed by a 26-byte verified trailer:
//! - `payload_offset: u64 LE` (exact length of original PE stub—used to write a tiny `Uninstall BK-R1X.exe`!)
//! - `payload_len: u64 LE`
//! - `crc16_ccitt: u16 LE`
//! - `magic: [u8; 8]` (`*b"BKR1XSFX"`)

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use bkr1x_protocol::ChecksumStrategy;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

/// 8-byte EOF signature identifying a self-contained BK-R1X Setup Wizard binary.
pub const SFX_FOOTER_MAGIC: [u8; 8] = *b"BKR1XSFX";
const SFX_FOOTER_LEN: usize = 26;

/// Extracted self-contained installer bundle.
#[derive(Debug, Clone)]
pub struct SfxBundle {
    /// Exact byte length of the clean PE installer stub (excluding the appended archive).
    pub stub_len: u64,
    /// Decompressed `(filename, bytes)` entries.
    pub files: Vec<(String, Vec<u8>)>,
}

/// Packs `stub_exe` + `entries` (`(filename, raw_bytes)`) into a single self-extracting `.exe`.
pub fn pack_sfx_installer(stub_exe: &[u8], entries: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
    let mut payload = Vec::new();
    let count = u32::try_from(entries.len()).map_err(|_| "too many SFX files".to_owned())?;
    payload.extend_from_slice(&count.to_le_bytes());

    for (name, raw) in entries {
        let name_bytes = name.as_bytes();
        let name_len =
            u16::try_from(name_bytes.len()).map_err(|_| format!("filename '{name}' too long"))?;
        let raw_len =
            u32::try_from(raw.len()).map_err(|_| format!("entry '{name}' exceeds 4 GiB"))?;

        let mut enc = ZlibEncoder::new(Vec::new(), Compression::best());
        enc.write_all(raw)
            .map_err(|e| format!("deflate failed for '{name}': {e}"))?;
        let compressed = enc
            .finish()
            .map_err(|e| format!("deflate finish failed for '{name}': {e}"))?;
        let comp_len = u32::try_from(compressed.len())
            .map_err(|_| format!("compressed '{name}' exceeds 4 GiB"))?;

        payload.extend_from_slice(&name_len.to_le_bytes());
        payload.extend_from_slice(name_bytes);
        payload.extend_from_slice(&raw_len.to_le_bytes());
        payload.extend_from_slice(&comp_len.to_le_bytes());
        payload.extend_from_slice(&compressed);
    }

    let stub_len = stub_exe.len() as u64;
    let payload_len = payload.len() as u64;
    let crc = ChecksumStrategy::crc16_ccitt(&payload);

    let mut out = Vec::with_capacity(stub_exe.len() + payload.len() + SFX_FOOTER_LEN);
    out.extend_from_slice(stub_exe);
    out.extend_from_slice(&payload);
    out.extend_from_slice(&stub_len.to_le_bytes());
    out.extend_from_slice(&payload_len.to_le_bytes());
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&SFX_FOOTER_MAGIC);
    Ok(out)
}

/// Inspects `exe_path` for an appended `BKR1XSFX` trailer and decompresses all embedded files.
/// Returns `Ok(None)` if `exe_path` is a bare PE stub without an appended bundle.
pub fn extract_sfx_from_exe(exe_path: &Path) -> Result<Option<SfxBundle>, String> {
    let mut f = fs::File::open(exe_path)
        .map_err(|e| format!("failed opening {}: {e}", exe_path.display()))?;
    let total_len = f
        .metadata()
        .map_err(|e| format!("metadata failed: {e}"))?
        .len();
    if total_len < SFX_FOOTER_LEN as u64 {
        return Ok(None);
    }

    f.seek(SeekFrom::End(-(SFX_FOOTER_LEN as i64)))
        .map_err(|e| format!("seek footer failed: {e}"))?;
    let mut footer = [0u8; SFX_FOOTER_LEN];
    f.read_exact(&mut footer)
        .map_err(|e| format!("read footer failed: {e}"))?;

    if footer[18..26] != SFX_FOOTER_MAGIC {
        return Ok(None);
    }

    let stub_len = u64::from_le_bytes(footer[0..8].try_into().unwrap());
    let payload_len = u64::from_le_bytes(footer[8..16].try_into().unwrap());
    let expected_crc = u16::from_le_bytes(footer[16..18].try_into().unwrap());

    if stub_len
        .checked_add(payload_len)
        .and_then(|n| n.checked_add(SFX_FOOTER_LEN as u64))
        != Some(total_len)
    {
        return Err("corrupted SFX archive (size mismatch in trailer)".to_owned());
    }

    f.seek(SeekFrom::Start(stub_len))
        .map_err(|e| format!("seek payload failed: {e}"))?;
    let mut payload = vec![0u8; payload_len as usize];
    f.read_exact(&mut payload)
        .map_err(|e| format!("read payload failed: {e}"))?;

    let actual_crc = ChecksumStrategy::crc16_ccitt(&payload);
    if actual_crc != expected_crc {
        return Err(format!(
            "corrupted SFX archive (CRC16 mismatch: expected 0x{expected_crc:04X}, got 0x{actual_crc:04X})"
        ));
    }

    let files = decode_payload_slice(&payload)?;
    Ok(Some(SfxBundle { stub_len, files }))
}

fn decode_payload_slice(buf: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    if buf.len() < 4 {
        return Err("truncated SFX header".to_owned());
    }
    let count = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    let mut pos = 4usize;
    let mut out = Vec::with_capacity(count);

    for _ in 0..count {
        if pos + 2 > buf.len() {
            return Err("truncated SFX filename length".to_owned());
        }
        let name_len = u16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap()) as usize;
        pos += 2;
        if pos + name_len + 8 > buf.len() {
            return Err("truncated SFX entry header".to_owned());
        }
        let name = std::str::from_utf8(&buf[pos..pos + name_len])
            .map_err(|e| format!("invalid UTF-8 filename in SFX: {e}"))?
            .to_owned();
        pos += name_len;

        let raw_len = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        let comp_len = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;

        if pos + comp_len > buf.len() {
            return Err(format!("truncated compressed stream for '{name}'"));
        }
        let mut decoder = ZlibDecoder::new(&buf[pos..pos + comp_len]);
        let mut decompressed = Vec::with_capacity(raw_len);
        decoder
            .read_to_end(&mut decompressed)
            .map_err(|e| format!("zlib decompression failed for '{name}': {e}"))?;
        if decompressed.len() != raw_len {
            return Err(format!(
                "decompressed size mismatch for '{name}' (expected {raw_len}, got {})",
                decompressed.len()
            ));
        }
        pos += comp_len;
        out.push((name, decompressed));
    }

    Ok(out)
}
