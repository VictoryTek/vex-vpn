//! WireGuard `.conf` file parser (INI format).
#![allow(dead_code)]

use anyhow::{anyhow, Result};
use std::path::Path;

/// Parsed WireGuard configuration.
#[derive(Debug, Clone)]
pub struct WireGuardConfig {
    // [Interface] section
    pub address: String,
    pub dns: Option<String>,
    pub listen_port: Option<u16>,
    pub mtu: Option<u16>,
    // [Peer] section (first peer — single-peer VPN profiles)
    pub peer_public_key: String,
    pub endpoint: Option<String>,
    pub allowed_ips: String,
    pub persistent_keepalive: Option<u32>,
    pub preshared_key: Option<String>,
}

/// Parse a WireGuard `.conf` file and return the extracted config.
/// Returns an error if the required fields are missing.
pub fn parse(path: &Path) -> Result<WireGuardConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("failed to read WireGuard config {:?}: {}", path, e))?;
    parse_str(&content)
}

/// Parse WireGuard INI config from a string.
pub fn parse_str(content: &str) -> Result<WireGuardConfig> {
    use configparser::ini::Ini;

    let mut ini = Ini::new();
    ini.read(content.to_string())
        .map_err(|e| anyhow!("failed to parse WireGuard INI: {}", e))?;

    // [Interface] section — keys are normalised to lowercase by configparser.
    let address = ini
        .get("interface", "address")
        .ok_or_else(|| anyhow!("WireGuard config missing [Interface] Address"))?;

    let dns = ini.get("interface", "dns");
    let listen_port = ini
        .get("interface", "listenport")
        .and_then(|v| v.parse::<u16>().ok());
    let mtu = ini
        .get("interface", "mtu")
        .and_then(|v| v.parse::<u16>().ok());

    // [Peer] section
    let peer_public_key = ini
        .get("peer", "publickey")
        .ok_or_else(|| anyhow!("WireGuard config missing [Peer] PublicKey"))?;

    let endpoint = ini.get("peer", "endpoint");
    let allowed_ips = ini
        .get("peer", "allowedips")
        .unwrap_or_else(|| "0.0.0.0/0".to_string());

    let persistent_keepalive = ini
        .get("peer", "persistentkeepalive")
        .and_then(|v| v.parse::<u32>().ok());

    let preshared_key = ini.get("peer", "presharedkey");

    Ok(WireGuardConfig {
        address,
        dns,
        listen_port,
        mtu,
        peer_public_key,
        endpoint,
        allowed_ips,
        persistent_keepalive,
        preshared_key,
    })
}

/// Extract the WireGuard interface name from a `.conf` file.
/// First looks for a `# Name = <iface>` comment in the [Interface] section,
/// then falls back to the filename stem (e.g. `wg0.conf` → `wg0`).
pub fn extract_interface_name(path: &Path) -> Option<String> {
    // Try to read a Name comment from the file.
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                let rest = trimmed.trim_start_matches('#').trim();
                if let Some(name) = rest.strip_prefix("Name").map(|s| s.trim()) {
                    if let Some(name) = name.strip_prefix('=').map(|s| s.trim()) {
                        if crate::config::validate_interface(name) {
                            return Some(name.to_string());
                        }
                    }
                }
            }
        }
    }

    // Fallback: use the filename stem.
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| crate::config::validate_interface(s))
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_CONF: &str = r#"
[Interface]
Address = 10.0.0.2/24
DNS = 1.1.1.1
PrivateKey = aGVsbG8gd29ybGQgdGhpcyBpcyBub3QgYSByZWFsIGtleQo=

[Peer]
PublicKey = dGhpcyBpcyBhbHNvIG5vdCBhIHJlYWwga2V5IGhlcmUK
AllowedIPs = 0.0.0.0/0
Endpoint = 1.2.3.4:51820
PersistentKeepalive = 25
"#;

    #[test]
    fn test_parse_valid_config() {
        let cfg = parse_str(VALID_CONF).unwrap();
        assert_eq!(cfg.address, "10.0.0.2/24");
        assert_eq!(cfg.dns, Some("1.1.1.1".to_string()));
        assert_eq!(cfg.endpoint, Some("1.2.3.4:51820".to_string()));
        assert_eq!(cfg.allowed_ips, "0.0.0.0/0");
        assert_eq!(cfg.persistent_keepalive, Some(25));
    }

    #[test]
    fn test_parse_missing_address_fails() {
        let conf = "[Interface]\nDNS = 1.1.1.1\n[Peer]\nPublicKey = abc\n";
        assert!(parse_str(conf).is_err());
    }

    #[test]
    fn test_parse_missing_peer_public_key_fails() {
        let conf = "[Interface]\nAddress = 10.0.0.1/24\n[Peer]\nAllowedIPs = 0.0.0.0/0\n";
        assert!(parse_str(conf).is_err());
    }
}
