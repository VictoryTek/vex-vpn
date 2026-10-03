//! Data model for the vexos-vpn backend's JSON output.
//!
//! Source of truth: vexos-nix `pkgs/vexos-vpn/vexos-vpn.sh` (`cmd_status`,
//! `cmd_regions`). Nothing here performs I/O, so it is fully unit-testable.

use anyhow::{Context, Result};
use serde::Deserialize;

/// `state` from `vexos-vpn status --json`. A missing key (daemon never ran)
/// deserialises as `Disconnected`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VpnState {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    Error,
    /// A state this GUI does not know yet; rendered like `Disconnected`.
    #[serde(other)]
    Unknown,
}

impl VpnState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connected => "Connected",
            Self::Connecting => "Connecting\u{2026}",
            Self::Error => "Error",
            Self::Disconnected | Self::Unknown => "Disconnected",
        }
    }

    /// The tunnel is up or being brought up, i.e. the connect toggle is "on".
    pub fn is_active(self) -> bool {
        matches!(self, Self::Connected | Self::Connecting)
    }
}

/// `killswitch_mode` — fixed by the vexos NixOS config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KillSwitchMode {
    Off,
    #[default]
    Manual,
    Always,
}

/// Output of `vexos-vpn status --json`. Contains no secrets.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Status {
    pub state: VpnState,
    pub protocol: String,
    pub region: String,
    pub region_name: String,
    pub server_cn: String,
    pub endpoint_ip: String,
    pub interface: String,
    /// ISO 8601 (`date -Iseconds`), empty when not connected.
    pub since: String,
    pub last_error: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub killswitch: bool,
    pub killswitch_mode: KillSwitchMode,
    pub protocol_setting: String,
    pub region_setting: String,
    pub logged_in: bool,
    pub credentials_editable: bool,
    pub autoconnect: bool,
}

impl Status {
    pub fn parse(json: &str) -> Result<Self> {
        serde_json::from_str(json.trim()).context("parse `vexos-vpn status --json` output")
    }

    /// True when the user has to (re-)enter PIA credentials before the VPN
    /// can connect.
    pub fn needs_sign_in(&self) -> bool {
        !self.logged_in
            || self.last_error.starts_with("no PIA credentials")
            || self.last_error.starts_with("credentials file is malformed")
    }
}

/// One entry of `vexos-vpn regions --json`.
#[derive(Debug, Clone, Deserialize)]
pub struct Region {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub port_forward: bool,
    /// Seconds; `null` until an automatic connect has measured latency.
    pub latency_s: Option<f64>,
}

pub fn parse_regions(json: &str) -> Result<Vec<Region>> {
    serde_json::from_str(json.trim()).context("parse `vexos-vpn regions --json` output")
}

/// Region id used for "Automatic (fastest)".
pub const AUTO_REGION: &str = "auto";

/// Same pattern the vexos polkit rule and `vexos-vpn region` accept for a
/// template-unit instance: `^[a-z0-9_-]+$`.
pub fn is_valid_unit_arg(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Throughput derived from successive byte counters.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rates {
    pub rx_per_s: f64,
    pub tx_per_s: f64,
}

/// One counter sample taken from a `Status`.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub interface: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

impl Rates {
    /// Rates between two samples `dt_s` seconds apart. A changed interface,
    /// a counter that went backwards (interface re-created) or a zero
    /// interval yields zero rather than a bogus spike.
    pub fn between(prev: &Sample, now: &Sample, dt_s: f64) -> Self {
        if prev.interface != now.interface
            || now.interface.is_empty()
            || dt_s <= 0.0
            || now.rx_bytes < prev.rx_bytes
            || now.tx_bytes < prev.tx_bytes
        {
            return Self::default();
        }
        Self {
            rx_per_s: (now.rx_bytes - prev.rx_bytes) as f64 / dt_s,
            tx_per_s: (now.tx_bytes - prev.tx_bytes) as f64 / dt_s,
        }
    }
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

pub fn format_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}s", seconds)
    }
}

pub fn protocol_label(protocol: &str) -> &str {
    match protocol {
        "wireguard" => "WireGuard",
        "openvpn" => "OpenVPN",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1_048_576), "1.0 MiB");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(125), "2m 5s");
        assert_eq!(format_duration(7530), "2h 5m");
    }
}
