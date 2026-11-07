use std::env;
use std::error::Error;
use std::fs;
use std::path::PathBuf;

const COMMON_CONTROLS_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
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
</assembly>"#;

/// Windows heuristic UAC detection elevates binaries named `*setup*.exe` or `*install*.exe`
/// unless an explicit `asInvoker` trustInfo manifest is embedded.
const AS_INVOKER_SETUP_MANIFEST: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
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
</assembly>"#;

pub fn embed_exe_resources(
    ico_filename: &str,
    product_name: &str,
    file_description: &str,
    accent_bgra: [u8; 4],
    force_as_invoker: bool,
) -> Result<(), Box<dyn Error>> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../win_exe_resources.rs");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing OUT_DIR env var")?);
    let ico_path = out_dir.join(ico_filename);
    fs::write(&ico_path, generate_pe_ico(accent_bgra))?;

    let ico_str = ico_path
        .to_str()
        .ok_or("OUT_DIR contains non-UTF-8 characters")?;

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico_str);
    res.set("ProductName", product_name);
    res.set("FileDescription", file_description);
    res.set_manifest(if force_as_invoker {
        AS_INVOKER_SETUP_MANIFEST
    } else {
        COMMON_CONTROLS_MANIFEST
    });
    res.compile()?;
    Ok(())
}

fn generate_pe_ico(accent_bgra: [u8; 4]) -> Vec<u8> {
    let sizes = [16u32, 32, 48];
    let mut entries = Vec::with_capacity(sizes.len());

    for sz in sizes {
        let dim = sz as f32;
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
                let nx = ((col as f32 + 0.5) / dim) * 2.0 - 1.0;
                let ny = ((row as f32 + 0.5) / dim) * 2.0 - 1.0;
                let qx = nx.abs() - 0.10;
                let qy = ny.abs() - 0.34;
                let dist = (qx.max(0.0).hypot(qy.max(0.0)) + qx.min(qy).clamp(-0.36, 0.0)) - 0.36;
                if dist < 0.04 {
                    let is_accent =
                        (nx.abs() < 0.10 && (-0.48..-0.15).contains(&ny)) || dist > -0.08;
                    dib.extend_from_slice(if is_accent {
                        &accent_bgra
                    } else {
                        &[44, 30, 26, 255]
                    });
                } else {
                    dib.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        dib.resize(dib.len() + and_len, 0);
        entries.push((sz, dib));
    }

    let header_len = 6 + entries.len() * 16;
    let total_dib: usize = entries.iter().map(|(_, d)| d.len()).sum();
    let mut ico = Vec::with_capacity(header_len + total_dib);
    ico.extend_from_slice(&0u16.to_le_bytes());
    ico.extend_from_slice(&1u16.to_le_bytes());
    ico.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    let mut offset = header_len as u32;
    for &(sz, ref dib) in &entries {
        ico.extend_from_slice(&[sz as u8, sz as u8, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += dib.len() as u32;
    }
    for (_, dib) in entries {
        ico.extend_from_slice(&dib);
    }
    ico
}
