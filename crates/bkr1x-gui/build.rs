fn main() {
    #[cfg(windows)]
    {
        let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let ico_path = out_dir.join("bkr1x-gui.ico");
        let ico_bytes = generate_build_ico();
        std::fs::write(&ico_path, ico_bytes).expect("failed writing bkr1x-gui.ico");

        let mut res = winresource::WindowsResource::new();
        res.set_icon(ico_path.to_str().unwrap());
        res.set("ProductName", "Fire Phoenix BK-R1X Configurator");
        res.set(
            "FileDescription",
            "Fire Phoenix BK-R1X Native PAW3311 Configurator",
        );
        res.set_manifest(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*" />
    </dependentAssembly>
  </dependency>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2</dpiAwareness>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>"#,
        );
        let _ = res.compile();
    }
}

#[cfg(windows)]
fn generate_build_ico() -> Vec<u8> {
    let sizes = [16u32, 32, 48];
    let mut entries = Vec::new();
    for &sz in &sizes {
        let s = sz as f32;
        let xor_len = (sz * sz * 4) as usize;
        let and_stride = (sz.div_ceil(32) * 4) as usize;
        let and_len = and_stride * (sz as usize);
        let mut dib = Vec::with_capacity(40 + xor_len + and_len);
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&(sz as i32).to_le_bytes());
        dib.extend_from_slice(&((sz * 2) as i32).to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&0u32.to_le_bytes());
        dib.extend_from_slice(&(xor_len as u32).to_le_bytes());
        dib.extend_from_slice(&[0u8; 16]);

        for row in (0..sz).rev() {
            for col in 0..sz {
                let nx = ((col as f32 + 0.5) / s) * 2.0 - 1.0;
                let ny = ((row as f32 + 0.5) / s) * 2.0 - 1.0;
                let qx = nx.abs() - 0.10;
                let qy = ny.abs() - 0.34;
                let d = (qx.max(0.0).hypot(qy.max(0.0)) + qx.min(qy).clamp(-0.36, 0.0)) - 0.36;
                if d < 0.04 {
                    let is_accent = (nx.abs() < 0.10 && ny > -0.48 && ny < -0.15) || d > -0.08;
                    if is_accent {
                        dib.extend_from_slice(&[106, 206, 158, 255]); // #9ECE6A in BGRA
                    } else {
                        dib.extend_from_slice(&[44, 30, 26, 255]);
                    }
                } else {
                    dib.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        dib.extend(std::iter::repeat_n(0u8, and_len));
        entries.push((sz, dib));
    }

    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut off = 6u32 + (entries.len() as u32) * 16;
    for (sz, dib) in &entries {
        ico.extend_from_slice(&[*sz as u8, *sz as u8, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        ico.extend_from_slice(&off.to_le_bytes());
        off += dib.len() as u32;
    }
    for (_, dib) in entries {
        ico.extend_from_slice(&dib);
    }
    ico
}
