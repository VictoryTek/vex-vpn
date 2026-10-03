//! Parsing of captured `vexos-vpn status --json` / `regions --json` output.
//! Fixture shapes follow `cmd_status` / `cmd_regions` in vexos-nix
//! `pkgs/vexos-vpn/vexos-vpn.sh`.

use vex_vpn::vexos::{
    is_valid_unit_arg, parse_regions, KillSwitchMode, Rates, Sample, Status, VpnState,
};

fn status(name: &str) -> Status {
    let path = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), name);
    Status::parse(&std::fs::read_to_string(&path).expect("read fixture")).expect("parse fixture")
}

#[test]
fn connected() {
    let s = status("status_connected.json");
    assert_eq!(s.state, VpnState::Connected);
    assert_eq!(s.protocol, "wireguard");
    assert_eq!(s.region, "de-frankfurt");
    assert_eq!(s.region_name, "DE Frankfurt");
    assert_eq!(s.server_cn, "frankfurt405");
    assert_eq!(s.interface, "pia0");
    assert_eq!(s.since, "2026-10-03T12:00:00+02:00");
    assert_eq!(s.rx_bytes, 123_456_789);
    assert_eq!(s.tx_bytes, 2_345_678);
    assert!(s.killswitch);
    assert_eq!(s.killswitch_mode, KillSwitchMode::Manual);
    assert_eq!(s.region_setting, "auto");
    assert!(s.logged_in);
    assert!(s.credentials_editable);
    assert!(!s.needs_sign_in());
}

#[test]
fn connecting() {
    let s = status("status_connecting.json");
    assert_eq!(s.state, VpnState::Connecting);
    assert!(s.state.is_active());
    assert_eq!(s.protocol_setting, "openvpn");
}

#[test]
fn disconnected_when_state_key_missing() {
    // The daemon never ran: status.json is absent and `state` is not emitted.
    let s = status("status_disconnected.json");
    assert_eq!(s.state, VpnState::Disconnected);
    assert!(!s.state.is_active());
    assert!(s.region_name.is_empty());
    assert!(s.last_error.is_empty());
}

#[test]
fn error_missing_credentials_needs_sign_in() {
    let s = status("status_error_no_credentials.json");
    assert_eq!(s.state, VpnState::Error);
    assert!(!s.logged_in);
    assert!(s.last_error.starts_with("no PIA credentials"));
    assert!(s.needs_sign_in());
}

#[test]
fn malformed_credentials_error_needs_sign_in_even_if_file_exists() {
    let s = Status::parse(
        r#"{"state":"error","last_error":"credentials file is malformed — run: sudo vexos-vpn login","logged_in":true}"#,
    )
    .unwrap();
    assert!(s.needs_sign_in());
}

#[test]
fn credentials_managed_by_sops() {
    let s = status("status_sops.json");
    assert_eq!(s.state, VpnState::Disconnected);
    assert!(s.logged_in);
    assert!(!s.credentials_editable);
    assert_eq!(s.killswitch_mode, KillSwitchMode::Always);
    assert!(s.autoconnect);
}

#[test]
fn unknown_state_does_not_fail() {
    let s = status("status_unknown_state.json");
    assert_eq!(s.state, VpnState::Unknown);
    assert_eq!(s.killswitch_mode, KillSwitchMode::Off);
}

#[test]
fn garbage_is_an_error() {
    assert!(Status::parse("vexos-vpn: error: something").is_err());
}

#[test]
fn regions_with_null_latency() {
    let path = format!("{}/tests/fixtures/regions.json", env!("CARGO_MANIFEST_DIR"));
    let regions = parse_regions(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(regions.len(), 3);
    assert_eq!(regions[0].id, "de-frankfurt");
    assert_eq!(regions[0].latency_s, Some(0.021));
    assert!(regions[0].port_forward);
    assert_eq!(regions[1].id, "us_east");
    assert_eq!(regions[1].latency_s, None);
    assert!(!regions[1].port_forward);
    assert_eq!(regions[2].country, "GB");
}

#[test]
fn unit_arg_validation_matches_polkit_rule() {
    for ok in ["auto", "us_east", "de-frankfurt", "uk", "ca_toronto2"] {
        assert!(is_valid_unit_arg(ok), "{ok}");
    }
    for bad in ["", "../x", "a b", "US", "x.service", "a@b", "a\nb"] {
        assert!(!is_valid_unit_arg(bad), "{bad:?}");
    }
}

fn sample(iface: &str, rx: u64, tx: u64) -> Sample {
    Sample {
        interface: iface.to_string(),
        rx_bytes: rx,
        tx_bytes: tx,
    }
}

#[test]
fn rates_from_counters() {
    let r = Rates::between(&sample("pia0", 1000, 500), &sample("pia0", 3000, 1500), 2.0);
    assert_eq!(r.rx_per_s, 1000.0);
    assert_eq!(r.tx_per_s, 500.0);
}

#[test]
fn rates_reset_on_counter_reset_or_interface_change() {
    let zero = Rates::default();
    assert_eq!(
        Rates::between(&sample("pia0", 5000, 5000), &sample("pia0", 10, 10), 2.0),
        zero
    );
    assert_eq!(
        Rates::between(&sample("pia0", 0, 0), &sample("pia0-ovpn", 999, 999), 2.0),
        zero
    );
    assert_eq!(
        Rates::between(&sample("", 0, 0), &sample("", 0, 0), 2.0),
        zero
    );
    assert_eq!(
        Rates::between(&sample("pia0", 0, 0), &sample("pia0", 10, 10), 0.0),
        zero
    );
}
