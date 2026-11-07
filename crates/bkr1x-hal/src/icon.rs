//! Procedural vector-SDF RGBA & `.ico` icon renderer for the Fire Phoenix BK-R1X.
//!
//! Renders a crisp anti-aliased gaming mouse icon with dynamic RGB/battery telemetry accents
//! for the Windows System Tray (`HICON`), `eframe` window/taskbar icon, and `.exe` resources.

/// Telemetry state reflected visually on the System Tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrayIconStatus {
    pub connected: bool,
    pub online: bool,
    pub battery_percent: u8,
    pub charging: bool,
}

impl Default for TrayIconStatus {
    fn default() -> Self {
        Self {
            connected: true,
            online: true,
            battery_percent: 100,
            charging: false,
        }
    }
}

impl TrayIconStatus {
    /// Selects the primary accent RGB color based on connection, charge, and battery level.
    pub const fn accent_rgb(self) -> [u8; 3] {
        if !self.connected {
            [115, 122, 148] // Disconnected slate
        } else if !self.online {
            [135, 145, 180] // 2.4GHz deep sleep cool gray-blue
        } else if self.charging {
            [42, 195, 222] // Electric cyan charging
        } else if self.battery_percent <= 20 {
            [247, 118, 142] // Low battery crimson
        } else if self.battery_percent <= 50 {
            [224, 175, 104] // Medium battery amber
        } else {
            [158, 206, 106] // Healthy battery emerald
        }
    }
}

/// Renders an anti-aliased `size × size` RGBA8888 icon of the BK-R1X mouse + battery bar.
pub fn render_mouse_icon_rgba(size: u32, status: TrayIconStatus) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let s = size as f32;
    let [ar, ag, ab] = status.accent_rgb();

    for py in 0..size {
        for px in 0..size {
            // Normalized coordinates in [-1.0, 1.0]
            let nx = ((px as f32 + 0.5) / s) * 2.0 - 1.0;
            let ny = ((py as f32 + 0.5) / s) * 2.0 - 1.0;

            let mut r = 0.0_f32;
            let mut g = 0.0_f32;
            let mut b = 0.0_f32;
            let mut a = 0.0_f32;

            // 1. Ergonomic mouse shell SDF (rounded capsule shifted slightly up to leave room for battery bar)
            let cy = ny + 0.08;
            let half_w = 0.44 - 0.04 * cy * cy;
            let half_h = 0.64;
            let corner = 0.36;
            let qx = nx.abs() - (half_w - corner);
            let qy = cy.abs() - (half_h - corner);
            let dist_shell =
                (qx.max(0.0).hypot(qy.max(0.0)) + qx.min(qy).clamp(-corner, 0.0)) - corner;

            let aa = 2.2 / s;
            if dist_shell < aa {
                let shell_alpha = ((aa - dist_shell) / (2.0 * aa)).clamp(0.0, 1.0);
                // Dark anodized shell fill
                r = 26.0;
                g = 30.0;
                b = 44.0;
                a = shell_alpha;

                // Illuminated outer rim border
                let rim = (1.0 - (dist_shell.abs() / (2.4 / s))).clamp(0.0, 1.0);
                r = lerp(r, f32::from(ar), rim * 0.92);
                g = lerp(g, f32::from(ag), rim * 0.92);
                b = lerp(b, f32::from(ab), rim * 0.92);

                // Button split seam (horizontal + vertical center split on upper shell)
                if cy > -0.60 && cy < -0.05 && nx.abs() < 0.035 {
                    r = 12.0;
                    g = 14.0;
                    b = 20.0;
                }
                if (cy + 0.04).abs() < 0.03 && nx.abs() < 0.38 {
                    r = 14.0;
                    g = 16.0;
                    b = 24.0;
                }

                // Illuminated scroll wheel
                if nx.abs() < 0.095 && cy > -0.44 && cy < -0.16 {
                    r = f32::from(ar);
                    g = f32::from(ag);
                    b = f32::from(ab);
                }

                // Center DPI indicator diamond
                if (nx.abs() + (cy - 0.08).abs()) < 0.085 {
                    r = f32::from(ar);
                    g = f32::from(ag);
                    b = f32::from(ab);
                }
            }

            // 2. Bottom mini battery gauge bar (`ny` in `[0.76, 0.92]`, `nx` in `[-0.68, 0.68]`)
            if (0.75..=0.93).contains(&ny) && nx.abs() <= 0.70 {
                // Track background
                r = 20.0;
                g = 23.0;
                b = 34.0;
                a = 1.0;

                let pct = if status.connected {
                    f32::from(status.battery_percent.clamp(5, 100)) / 100.0
                } else {
                    0.0
                };
                let fill_max_x = -0.66 + 1.32 * pct;
                if (0.78..=0.90).contains(&ny) && (-0.66..=fill_max_x).contains(&nx) {
                    r = f32::from(ar);
                    g = f32::from(ag);
                    b = f32::from(ab);
                }
            }

            let idx = ((py * size + px) * 4) as usize;
            out[idx] = r.round().clamp(0.0, 255.0) as u8;
            out[idx + 1] = g.round().clamp(0.0, 255.0) as u8;
            out[idx + 2] = b.round().clamp(0.0, 255.0) as u8;
            out[idx + 3] = (a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }

    out
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Encodes a multi-size Windows `.ico` binary containing 32-bit BGRA DIB images.
pub fn encode_ico_file(sizes: &[u32], status: TrayIconStatus) -> Vec<u8> {
    let mut payloads: Vec<(u32, Vec<u8>)> = Vec::with_capacity(sizes.len());

    for &sz in sizes {
        let rgba = render_mouse_icon_rgba(sz, status);
        let xor_bytes = (sz * sz * 4) as usize;
        let and_row_stride = (sz.div_ceil(32) * 4) as usize;
        let and_bytes = and_row_stride * (sz as usize);
        let dib_len = 40 + xor_bytes + and_bytes;

        let mut dib = Vec::with_capacity(dib_len);
        // BITMAPINFOHEADER (40 bytes) — height is doubled (`sz * 2`) per ICO spec for XOR + AND masks
        dib.extend_from_slice(&40u32.to_le_bytes()); // biSize
        dib.extend_from_slice(&(sz as i32).to_le_bytes()); // biWidth
        dib.extend_from_slice(&((sz * 2) as i32).to_le_bytes()); // biHeight
        dib.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
        dib.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
        dib.extend_from_slice(&0u32.to_le_bytes()); // biCompression (BI_RGB)
        dib.extend_from_slice(&(xor_bytes as u32).to_le_bytes()); // biSizeImage
        dib.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
        dib.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
        dib.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

        // Bottom-up BGRA pixel rows
        for row in (0..sz).rev() {
            for col in 0..sz {
                let i = ((row * sz + col) * 4) as usize;
                dib.push(rgba[i + 2]); // B
                dib.push(rgba[i + 1]); // G
                dib.push(rgba[i]); // R
                dib.push(rgba[i + 3]); // A
            }
        }

        // 1-bit transparency fallback AND mask (bottom-up)
        for row in (0..sz).rev() {
            let mut row_bits = vec![0u8; and_row_stride];
            for col in 0..sz {
                let alpha = rgba[((row * sz + col) * 4 + 3) as usize];
                if alpha == 0 {
                    row_bits[(col / 8) as usize] |= 0x80 >> (col % 8);
                }
            }
            dib.extend_from_slice(&row_bits);
        }

        payloads.push((sz, dib));
    }

    let count = payloads.len() as u16;
    let mut ico = Vec::new();
    // ICONDIR header (6 bytes)
    ico.extend_from_slice(&0u16.to_le_bytes()); // Reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // Type = 1 (ICO)
    ico.extend_from_slice(&count.to_le_bytes());

    let mut data_offset = 6u32 + (u32::from(count) * 16);
    for (sz, dib) in &payloads {
        let dim_byte = if *sz >= 256 { 0u8 } else { *sz as u8 };
        ico.push(dim_byte); // bWidth
        ico.push(dim_byte); // bHeight
        ico.push(0); // bColorCount
        ico.push(0); // bReserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // wPlanes
        ico.extend_from_slice(&32u16.to_le_bytes()); // wBitCount
        ico.extend_from_slice(&(dib.len() as u32).to_le_bytes()); // dwBytesInRes
        ico.extend_from_slice(&data_offset.to_le_bytes()); // dwImageOffset
        data_offset += dib.len() as u32;
    }

    for (_, dib) in payloads {
        ico.extend_from_slice(&dib);
    }

    ico
}

/// Creates a Win32 32-bit alpha-blended `HICON` directly in memory for `Shell_NotifyIconW`.
#[cfg(windows)]
pub fn create_win32_tray_hicon(
    size: u32,
    status: TrayIconStatus,
) -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, LR_DEFAULTCOLOR};

    // Single-image ICO: 6-byte ICONDIR + 16-byte ICONDIRENTRY = 22-byte header, followed by exact Win32 RT_ICON DIB
    let ico = encode_ico_file(&[size], status);
    let dib = &ico[22..];

    // SAFETY: `dib` is a valid self-contained 32bpp BITMAPINFOHEADER + BGRA XOR + 1bpp AND resource buffer.
    unsafe {
        CreateIconFromResourceEx(
            dib.as_ptr(),
            dib.len() as u32,
            1, // fIcon = TRUE
            0x0003_0000,
            size as i32,
            size as i32,
            LR_DEFAULTCOLOR,
        )
    }
}
