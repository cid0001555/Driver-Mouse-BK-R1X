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
    pub const fn accent_rgb(self) -> [u8; 3] {
        if !self.connected {
            [115, 122, 148]
        } else if !self.online {
            [135, 145, 180]
        } else if self.charging {
            [42, 195, 222]
        } else if self.battery_percent <= 20 {
            [247, 118, 142]
        } else if self.battery_percent <= 50 {
            [224, 175, 104]
        } else {
            [158, 206, 106]
        }
    }
}

pub fn render_mouse_icon_rgba(size: u32, status: TrayIconStatus) -> Vec<u8> {
    let mut out = vec![0u8; (size * size * 4) as usize];
    let dim = size as f32;
    let [ar, ag, ab] = status.accent_rgb();
    let accent_r = f32::from(ar);
    let accent_g = f32::from(ag);
    let accent_b = f32::from(ab);
    let aa_width = 2.2 / dim;
    let rim_width = 2.4 / dim;
    let fill_pct = if status.connected {
        f32::from(status.battery_percent.clamp(5, 100)) / 100.0
    } else {
        0.0
    };
    let fill_max_x = -0.66 + 1.32 * fill_pct;

    for py in 0..size {
        let ny = ((py as f32 + 0.5) / dim) * 2.0 - 1.0;
        let cy = ny + 0.08;
        let half_w = 0.44 - 0.04 * cy * cy;
        let qy = cy.abs() - 0.28;

        for px in 0..size {
            let nx = ((px as f32 + 0.5) / dim) * 2.0 - 1.0;
            let qx = nx.abs() - (half_w - 0.36);
            let dist_shell = (qx.max(0.0).hypot(qy.max(0.0)) + qx.min(qy).clamp(-0.36, 0.0)) - 0.36;

            let mut r = 0.0_f32;
            let mut g = 0.0_f32;
            let mut b = 0.0_f32;
            let mut a = 0.0_f32;

            if dist_shell < aa_width {
                let rim = (1.0 - (dist_shell.abs() / rim_width)).clamp(0.0, 1.0) * 0.92;
                r = lerp(26.0, accent_r, rim);
                g = lerp(30.0, accent_g, rim);
                b = lerp(44.0, accent_b, rim);
                a = ((aa_width - dist_shell) / (2.0 * aa_width)).clamp(0.0, 1.0);

                if (-0.60..-0.05).contains(&cy) && nx.abs() < 0.035 {
                    (r, g, b) = (12.0, 14.0, 20.0);
                } else if (cy + 0.04).abs() < 0.03 && nx.abs() < 0.38 {
                    (r, g, b) = (14.0, 16.0, 24.0);
                }

                let on_wheel = nx.abs() < 0.095 && (-0.44..-0.16).contains(&cy);
                let on_dpi_diamond = (nx.abs() + (cy - 0.08).abs()) < 0.085;
                if on_wheel || on_dpi_diamond {
                    (r, g, b) = (accent_r, accent_g, accent_b);
                }
            }

            if (0.75..=0.93).contains(&ny) && nx.abs() <= 0.70 {
                a = 1.0;
                if (0.78..=0.90).contains(&ny) && (-0.66..=fill_max_x).contains(&nx) {
                    (r, g, b) = (accent_r, accent_g, accent_b);
                } else {
                    (r, g, b) = (20.0, 23.0, 34.0);
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

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Builds a standalone Win32 `RT_ICON` 32bpp BGRA DIB (`BITMAPINFOHEADER` + bottom-up XOR + 1bpp AND).
/// `biHeight` is doubled (`size * 2`) as required by the Windows ICO/DIB resource format.
pub fn encode_icon_dib(size: u32, status: TrayIconStatus) -> Vec<u8> {
    let clamped_size = size.clamp(1, 256);
    let rgba = render_mouse_icon_rgba(clamped_size, status);
    let xor_bytes = (clamped_size * clamped_size * 4) as usize;
    let and_row_stride = (clamped_size.div_ceil(32) * 4) as usize;
    let and_bytes = and_row_stride * (clamped_size as usize);

    let mut dib = Vec::with_capacity(40 + xor_bytes + and_bytes);
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(clamped_size as i32).to_le_bytes());
    dib.extend_from_slice(&((clamped_size * 2) as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes());
    dib.extend_from_slice(&(xor_bytes as u32).to_le_bytes());
    dib.extend_from_slice(&[0u8; 16]);

    for row in (0..clamped_size).rev() {
        for col in 0..clamped_size {
            let i = ((row * clamped_size + col) * 4) as usize;
            dib.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }

    let mut row_mask = [0u8; 32];
    for row in (0..clamped_size).rev() {
        row_mask[..and_row_stride].fill(0);
        for col in 0..clamped_size {
            let alpha = rgba[((row * clamped_size + col) * 4 + 3) as usize];
            if alpha == 0 {
                row_mask[(col / 8) as usize] |= 0x80 >> (col % 8);
            }
        }
        dib.extend_from_slice(&row_mask[..and_row_stride]);
    }

    dib
}

pub fn encode_ico_file(sizes: &[u32], status: TrayIconStatus) -> Vec<u8> {
    let entries: Vec<(u32, Vec<u8>)> = sizes
        .iter()
        .map(|&sz| {
            let clamped = sz.clamp(1, 256);
            (clamped, encode_icon_dib(clamped, status))
        })
        .collect();

    let total_dib_bytes: usize = entries.iter().map(|(_, dib)| dib.len()).sum();
    let header_bytes = 6 + entries.len() * 16;
    let mut ico = Vec::with_capacity(header_bytes + total_dib_bytes);

    let count = entries.len() as u16;
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&count.to_le_bytes());

    let mut data_offset = header_bytes as u32;
    for &(sz, ref dib) in &entries {
        let dim_byte = if sz >= 256 { 0u8 } else { sz as u8 };
        ico.extend_from_slice(&[dim_byte, dim_byte, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        ico.extend_from_slice(&data_offset.to_le_bytes());
        data_offset += dib.len() as u32;
    }

    for (_, dib) in entries {
        ico.extend_from_slice(&dib);
    }

    ico
}

#[cfg(windows)]
pub fn create_win32_tray_hicon(
    size: u32,
    status: TrayIconStatus,
) -> windows_sys::Win32::UI::WindowsAndMessaging::HICON {
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, LR_DEFAULTCOLOR};

    let clamped = size.clamp(16, 256);
    let dib = encode_icon_dib(clamped, status);

    // SAFETY: `dib` is a self-contained 32bpp `BITMAPINFOHEADER` + BGRA XOR + 1bpp AND icon resource.
    unsafe {
        CreateIconFromResourceEx(
            dib.as_ptr(),
            dib.len() as u32,
            1,
            0x0003_0000,
            clamped as i32,
            clamped as i32,
            LR_DEFAULTCOLOR,
        )
    }
}
