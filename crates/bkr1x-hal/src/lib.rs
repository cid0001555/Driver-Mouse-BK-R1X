//! Cross-platform Hardware Abstraction Layer (`hidraw` on Linux, `hid.dll` on Windows),
//! TOML/JSON profile serializer, and ultra-low-latency IPC primitives for the Fire Phoenix BK-R1X.

pub mod device;
pub mod error;
pub mod ipc;
pub mod profile;

pub use device::{
    Bkr1xDevice, DEFAULT_DEVICE_RULES, DEFAULT_READ_TIMEOUT_MS, DeviceIdRule, DiscoveredInterface,
    TraceDirection, WireTraceEntry, discover_best_interface,
};
pub use error::DeviceError;
pub use ipc::{DaemonStatusSnapshot, IpcRequest, IpcResponse, send_ipc_request, unix_socket_path};
pub use profile::HardwareProfile;

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
}

