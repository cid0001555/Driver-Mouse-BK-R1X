//! Self-extracting (SFX) archive trailer appended to `bkr1x-installer.exe`:
//! `[PE stub][zlib entries][stub_len: u64 LE][payload_len: u64 LE][crc16: u16 LE]["BKR1XSFX"]`.
//! Storing `stub_len` lets the installer slice out its own clean PE stub as `Uninstall BK-R1X.exe`
//! without duplicating the multi-megabyte compressed payload on disk.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use bkr1x_protocol::ChecksumStrategy;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;

pub const SFX_FOOTER_MAGIC: [u8; 8] = *b"BKR1XSFX";
const SFX_FOOTER_LEN: usize = 26;

#[derive(Debug, Clone)]
pub struct SfxBundle {
    pub stub_len: u64,
    pub files: Vec<(String, Vec<u8>)>,
}

pub fn pack_sfx_installer(stub_exe: &[u8], entries: &[(&str, &[u8])]) -> Result<Vec<u8>, String> {
    let mut payload = Vec::new();
    let entry_count =
        u32::try_from(entries.len()).map_err(|_| "too many SFX archive entries".to_owned())?;
    payload.extend_from_slice(&entry_count.to_le_bytes());

    for &(name, raw_bytes) in entries {
        let name_bytes = name.as_bytes();
        let name_len =
            u16::try_from(name_bytes.len()).map_err(|_| format!("filename '{name}' too long"))?;
        let raw_len =
            u32::try_from(raw_bytes.len()).map_err(|_| format!("entry '{name}' exceeds 4 GiB"))?;

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder
            .write_all(raw_bytes)
            .map_err(|e| format!("deflate write failed for '{name}': {e}"))?;
        let compressed = encoder
            .finish()
            .map_err(|e| format!("deflate finish failed for '{name}': {e}"))?;
        let compressed_len = u32::try_from(compressed.len())
            .map_err(|_| format!("compressed '{name}' exceeds 4 GiB"))?;

        payload.extend_from_slice(&name_len.to_le_bytes());
        payload.extend_from_slice(name_bytes);
        payload.extend_from_slice(&raw_len.to_le_bytes());
        payload.extend_from_slice(&compressed_len.to_le_bytes());
        payload.extend_from_slice(&compressed);
    }

    let stub_len = stub_exe.len() as u64;
    let payload_len = payload.len() as u64;
    let crc = ChecksumStrategy::crc16_ccitt(&payload);

    let mut image = Vec::with_capacity(stub_exe.len() + payload.len() + SFX_FOOTER_LEN);
    image.extend_from_slice(stub_exe);
    image.extend_from_slice(&payload);
    image.extend_from_slice(&stub_len.to_le_bytes());
    image.extend_from_slice(&payload_len.to_le_bytes());
    image.extend_from_slice(&crc.to_le_bytes());
    image.extend_from_slice(&SFX_FOOTER_MAGIC);
    Ok(image)
}

pub fn extract_sfx_from_exe(exe_path: &Path) -> Result<Option<SfxBundle>, String> {
    let mut file = fs::File::open(exe_path)
        .map_err(|e| format!("failed opening {}: {e}", exe_path.display()))?;
    let total_len = file
        .metadata()
        .map_err(|e| format!("metadata failed: {e}"))?
        .len();
    if total_len < SFX_FOOTER_LEN as u64 {
        return Ok(None);
    }

    file.seek(SeekFrom::End(-(SFX_FOOTER_LEN as i64)))
        .map_err(|e| format!("seek footer failed: {e}"))?;
    let mut footer = [0u8; SFX_FOOTER_LEN];
    file.read_exact(&mut footer)
        .map_err(|e| format!("read footer failed: {e}"))?;

    let (header_bytes, magic) = footer.split_at(18);
    if magic != SFX_FOOTER_MAGIC {
        return Ok(None);
    }

    let mut cursor = ByteCursor::new(header_bytes);
    let stub_len = cursor.read_u64_le()?;
    let payload_len = cursor.read_u64_le()?;
    let expected_crc = cursor.read_u16_le()?;

    let expected_total = stub_len
        .checked_add(payload_len)
        .and_then(|sum| sum.checked_add(SFX_FOOTER_LEN as u64));
    if expected_total != Some(total_len) {
        return Err("corrupted SFX archive (size mismatch in trailer)".to_owned());
    }

    let payload_cap = usize::try_from(payload_len)
        .map_err(|_| "SFX archive payload exceeds addressable memory".to_owned())?;
    file.seek(SeekFrom::Start(stub_len))
        .map_err(|e| format!("seek payload failed: {e}"))?;
    let mut payload = vec![0u8; payload_cap];
    file.read_exact(&mut payload)
        .map_err(|e| format!("read payload failed: {e}"))?;

    let actual_crc = ChecksumStrategy::crc16_ccitt(&payload);
    if actual_crc != expected_crc {
        return Err(format!(
            "corrupted SFX archive (CRC16 mismatch: expected 0x{expected_crc:04X}, got 0x{actual_crc:04X})"
        ));
    }

    let files = decode_payload_entries(&payload)?;
    Ok(Some(SfxBundle { stub_len, files }))
}

fn decode_payload_entries(buf: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut cursor = ByteCursor::new(buf);
    let entry_count = cursor.read_u32_le()? as usize;
    let mut entries = Vec::with_capacity(entry_count.min(32));

    for _ in 0..entry_count {
        let name_len = cursor.read_u16_le()? as usize;
        let name_bytes = cursor.read_slice(name_len)?;
        let name = std::str::from_utf8(name_bytes)
            .map_err(|e| format!("invalid UTF-8 filename in SFX: {e}"))?
            .to_owned();

        let raw_len = cursor.read_u32_le()? as usize;
        let compressed_len = cursor.read_u32_le()? as usize;
        let compressed_bytes = cursor.read_slice(compressed_len)?;

        let mut decoder = ZlibDecoder::new(compressed_bytes);
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
        entries.push((name, decompressed));
    }

    Ok(entries)
}

struct ByteCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    const fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn read_slice(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or_else(|| "SFX archive offset overflow".to_owned())?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or_else(|| "truncated SFX archive payload".to_owned())?;
        self.pos = end;
        Ok(slice)
    }

    fn read_u16_le(&mut self) -> Result<u16, String> {
        let bytes = self.read_slice(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32_le(&mut self) -> Result<u32, String> {
        let bytes = self.read_slice(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64_le(&mut self) -> Result<u64, String> {
        let bytes = self.read_slice(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
}
