//! Cross-platform Hardware Abstraction Layer (`hidraw` on Linux, `hid.dll` on Windows),
//! TOML/JSON profile serializer, procedural icon generator, OS startup manager,
//! and ultra-low-latency IPC primitives for the Fire Phoenix BK-R1X.

pub mod device;
pub mod error;
pub mod icon;
pub mod ipc;
pub mod profile;
pub mod sfx;
pub mod startup;

pub use device::{
    Bkr1xDevice, DEFAULT_DEVICE_RULES, DEFAULT_READ_TIMEOUT_MS, DeviceIdRule, DiscoveredInterface,
    TraceDirection, WireTraceEntry, discover_best_interface,
};
pub use error::DeviceError;
pub use icon::{TrayIconStatus, encode_ico_file, render_mouse_icon_rgba};
pub use ipc::{
    DaemonStatusSnapshot, IpcRequest, IpcResponse, IpcWireTrace, is_daemon_running,
    send_ipc_request, unix_socket_path,
};
pub use profile::HardwareProfile;
pub use sfx::{SFX_FOOTER_MAGIC, SfxBundle, extract_sfx_from_exe, pack_sfx_installer};
pub use startup::{
    InstallOptions, InstallSummary, ensure_user_profiles_dir, install_user_application,
    install_with_options, is_autostart_enabled, is_user_application_installed, list_saved_profiles,
    resolve_sibling_binary, set_autostart_enabled, uninstall_user_application,
    uninstall_user_application_full, user_config_dir, user_install_dir,
};

#[cfg(test)]
mod tests {
    use std::path::Path;

    use bkr1x_protocol::{ButtonMapping, MouseConfig};

    use super::*;

    #[test]
    fn profile_toml_roundtrip_and_validation() {
        let cfg = MouseConfig::factory_default();
        let btns = ButtonMapping::factory_default();
        let prof = HardwareProfile::from_hardware("UnitTest", &cfg, &btns);

        let serialized = toml::to_string_pretty(&prof).unwrap();
        let parsed: HardwareProfile = toml::from_str(&serialized).unwrap();
        assert_eq!(parsed.to_mouse_config().unwrap(), cfg);
        assert_eq!(parsed.to_button_mapping().unwrap(), btns);
    }

    #[test]
    fn bundled_esports_profile_loads_cleanly() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("profiles")
            .join("esports-1000hz.toml");
        let prof = HardwareProfile::load_from_file(&p).unwrap();
        let cfg = prof.to_mouse_config().unwrap();
        assert_eq!(cfg.active_dpi(), 1600);
        assert_eq!(cfg.polling_rate.as_hz(), 1000);
        assert_eq!(prof.to_macro_vecs().unwrap().len(), 1);
    }

    #[test]
    fn procedural_ico_generation_is_valid() {
        let ico = encode_ico_file(&[16, 32, 48], TrayIconStatus::default());
        assert_eq!(&ico[0..6], &[0, 0, 1, 0, 3, 0]); // ICONDIR with 3 images
    }

    #[test]
    fn sfx_bundle_roundtrip_verifies_crc_and_stub_len() {
        let fake_stub = b"MZ_FAKE_PE_STUB_HEADER_BYTES";
        let file_a = vec![0x55u8; 4096];
        let file_b = b"hello bkr1x installer".as_slice();

        let packed = pack_sfx_installer(
            fake_stub,
            &[("bkr1x-daemon.exe", &file_a), ("readme.txt", file_b)],
        )
        .unwrap();

        let tmp = std::env::temp_dir().join("bkr1x_sfx_unit_test.bin");
        std::fs::write(&tmp, &packed).unwrap();
        let extracted = extract_sfx_from_exe(&tmp).unwrap().unwrap();
        let _ = std::fs::remove_file(&tmp);

        assert_eq!(extracted.stub_len, fake_stub.len() as u64);
        assert_eq!(extracted.files.len(), 2);
        assert_eq!(extracted.files[0].0, "bkr1x-daemon.exe");
        assert_eq!(extracted.files[0].1, file_a);
        assert_eq!(extracted.files[1].0, "readme.txt");
        assert_eq!(extracted.files[1].1, file_b);
    }
}
