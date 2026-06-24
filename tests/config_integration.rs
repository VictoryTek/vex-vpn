//! Integration tests for Config persistence.
//!
//! These tests write TOML files to a temp directory so they never touch
//! `~/.config/vex-vpn/config.toml`.

use std::fs;
use vex_vpn::config::Config;
use vex_vpn::profile::{VpnProfile, VpnType};

#[test]
fn load_from_path_round_trip() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");

    let profile = VpnProfile::new(
        "Test Profile".to_string(),
        VpnType::WireGuard,
        "vpn.conf".to_string(),
    );
    let profile_id = profile.id.clone();

    let original = Config {
        version: 1,
        profiles: vec![profile],
        active_profile_id: Some(profile_id.clone()),
        start_minimized: true,
        auto_reconnect: false,
        show_tray_icon: true,
        kill_switch_service: "vex-vpn-killswitch".to_string(),
    };

    let toml_str = toml::to_string_pretty(&original).expect("serialize config");
    fs::write(&path, toml_str).expect("write config file");

    let loaded = Config::load_from(&path).expect("load config");

    assert_eq!(loaded.active_profile_id, original.active_profile_id);
    assert_eq!(loaded.start_minimized, original.start_minimized);
    assert_eq!(loaded.auto_reconnect, original.auto_reconnect);
    assert_eq!(loaded.show_tray_icon, original.show_tray_icon);
    assert_eq!(loaded.version, original.version);
    assert_eq!(loaded.profiles.len(), 1);
    assert_eq!(loaded.profiles[0].id, profile_id);
    assert_eq!(loaded.profiles[0].name, "Test Profile");
    assert_eq!(loaded.profiles[0].vpn_type, VpnType::WireGuard);
}

#[test]
fn load_from_path_missing_file_returns_default() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("nonexistent.toml");

    let cfg = Config::load_from(&path).expect("missing file should yield default");

    let default = Config::default();
    assert_eq!(cfg.active_profile_id, default.active_profile_id);
    assert_eq!(cfg.start_minimized, default.start_minimized);
    assert_eq!(cfg.auto_reconnect, default.auto_reconnect);
    assert!(cfg.profiles.is_empty());
}

#[test]
fn load_from_path_auto_reconnect_defaults_true_when_missing() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");

    // Minimal TOML without auto_reconnect field.
    let toml_str = r#"
version = 1
"#;
    fs::write(&path, toml_str).expect("write config");

    let cfg = Config::load_from(&path).expect("load config");
    assert!(
        cfg.auto_reconnect,
        "auto_reconnect should default to true when absent"
    );
}

#[test]
fn version_field_defaults_to_1_when_missing() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");

    let toml_str = r#"
start_minimized = false
"#;
    fs::write(&path, toml_str).expect("write config");

    let cfg = Config::load_from(&path).expect("load config");
    assert_eq!(cfg.version, 1, "version should default to 1 when absent");
}

#[test]
fn save_to_path_round_trip() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");

    let original = Config {
        version: 1,
        profiles: vec![],
        active_profile_id: None,
        start_minimized: false,
        auto_reconnect: true,
        show_tray_icon: false,
        kill_switch_service: "vex-vpn-killswitch".to_string(),
    };

    original.save_to(&path).expect("save config");

    let loaded = Config::load_from(&path).expect("load saved config");
    assert_eq!(loaded.start_minimized, original.start_minimized);
    assert_eq!(loaded.auto_reconnect, original.auto_reconnect);
    assert_eq!(loaded.show_tray_icon, original.show_tray_icon);
}

#[test]
fn config_with_openvpn_profile_round_trips() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");

    let profile = VpnProfile {
        id: "test-uuid-123".to_string(),
        name: "Work VPN".to_string(),
        vpn_type: VpnType::OpenVpn,
        config_file: "vpn.ovpn".to_string(),
        auto_connect: true,
        kill_switch: false,
        dns_override: Some("1.1.1.1".to_string()),
        interface: None,
    };

    let cfg = Config {
        version: 1,
        profiles: vec![profile],
        active_profile_id: Some("test-uuid-123".to_string()),
        start_minimized: false,
        auto_reconnect: true,
        show_tray_icon: true,
        kill_switch_service: "vex-vpn-killswitch".to_string(),
    };

    cfg.save_to(&path).expect("save");
    let loaded = Config::load_from(&path).expect("load");

    assert_eq!(loaded.profiles.len(), 1);
    assert_eq!(loaded.profiles[0].vpn_type, VpnType::OpenVpn);
    assert_eq!(loaded.profiles[0].dns_override, Some("1.1.1.1".to_string()));
    assert!(loaded.profiles[0].auto_connect);
}
