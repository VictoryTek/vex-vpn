//! VPN config file parsers.

pub mod openvpn;
pub mod wireguard;

use crate::profile::VpnType;
use std::path::Path;

/// Detect the VPN type from file extension.
pub fn detect_vpn_type(path: &Path) -> Option<VpnType> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("conf") => Some(VpnType::WireGuard),
        Some("ovpn") => Some(VpnType::OpenVpn),
        _ => None,
    }
}
