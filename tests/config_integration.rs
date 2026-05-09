//! Integration tests for Config persistence.
//!
//! These tests write TOML files to a temp directory so they never touch
//! `~/.config/vex-vpn/config.toml`.

use std::fs;
use vex_vpn::config::Config;

#[test]
fn load_from_path_round_trip() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");

    let original = Config {
        auto_connect: true,
        interface: "wg1".to_string(),
        max_latency_ms: 250,
        dns_provider: "cloudflare".to_string(),
        selected_region_id: Some("us_california".to_string()),
        kill_switch_enabled: true,
        kill_switch_allowed_ifaces: vec!["lo".to_string(), "eth0".to_string()],
        auto_reconnect: false,
    };

    let toml_str = toml::to_string_pretty(&original).expect("serialize config");
    fs::write(&path, toml_str).expect("write config file");

    let loaded = Config::load_from(&path).expect("load config");

    assert_eq!(loaded.auto_connect, original.auto_connect);
    assert_eq!(loaded.interface, original.interface);
    assert_eq!(loaded.max_latency_ms, original.max_latency_ms);
    assert_eq!(loaded.dns_provider, original.dns_provider);
    assert_eq!(loaded.selected_region_id, original.selected_region_id);
    assert_eq!(loaded.kill_switch_enabled, original.kill_switch_enabled);
    assert_eq!(
        loaded.kill_switch_allowed_ifaces,
        original.kill_switch_allowed_ifaces
    );
    assert_eq!(loaded.auto_reconnect, original.auto_reconnect);
}

#[test]
fn load_from_path_missing_file_returns_default() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("nonexistent.toml");

    let cfg = Config::load_from(&path).expect("missing file should yield default");

    let default = Config::default();
    assert_eq!(cfg.auto_connect, default.auto_connect);
    assert_eq!(cfg.interface, default.interface);
    assert_eq!(cfg.max_latency_ms, default.max_latency_ms);
    assert_eq!(cfg.dns_provider, default.dns_provider);
    assert_eq!(cfg.auto_reconnect, default.auto_reconnect);
}

#[test]
fn load_from_path_auto_reconnect_defaults_true_when_missing() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("config.toml");

    // Old-format TOML without auto_reconnect field.
    let toml_str = r#"
auto_connect = false
interface = "wg0"
max_latency_ms = 100
dns_provider = "pia"
"#;
    fs::write(&path, toml_str).expect("write config");

    let cfg = Config::load_from(&path).expect("load config");
    assert!(
        cfg.auto_reconnect,
        "auto_reconnect should default to true when absent from TOML"
    );
}
