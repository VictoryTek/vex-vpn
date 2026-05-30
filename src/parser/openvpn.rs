//! OpenVPN `.ovpn` config file parser (light-weight header extraction).
//!
//! Does not implement the full OpenVPN config grammar.
//! Only extracts the subset needed to build a NetworkManager connection dict.
#![allow(dead_code)]

use anyhow::{anyhow, Result};
use std::path::Path;

/// Parsed OpenVPN configuration (header fields only).
#[derive(Debug, Clone)]
pub struct OpenVpnConfig {
    pub remote: String,
    pub port: u16,
    pub proto: String,
    pub dev: String,
    pub cipher: Option<String>,
    pub auth: Option<String>,
    /// Inline CA certificate block content (between <ca> ... </ca>).
    pub ca_cert: Option<String>,
    /// Inline client certificate block content.
    pub client_cert: Option<String>,
    /// Inline client key block content.
    pub client_key: Option<String>,
    /// Inline TLS auth/crypt block content.
    pub tls_auth: Option<String>,
}

/// Parse an `.ovpn` file and return the extracted header fields.
pub fn parse(path: &Path) -> Result<OpenVpnConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow!("failed to read OpenVPN config {:?}: {}", path, e))?;
    parse_str(&content)
}

/// Parse OpenVPN config from a string.
pub fn parse_str(content: &str) -> Result<OpenVpnConfig> {
    let mut remote: Option<String> = None;
    let mut port: u16 = 1194;
    let mut proto = "udp".to_string();
    let mut dev = "tun".to_string();
    let mut cipher: Option<String> = None;
    let mut auth: Option<String> = None;
    let mut ca_cert: Option<String> = None;
    let mut client_cert: Option<String> = None;
    let mut client_key: Option<String> = None;
    let mut tls_auth: Option<String> = None;

    let mut current_block: Option<&str> = None;
    let mut block_buf = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // Inline block tags.
        if trimmed.starts_with('<') {
            if trimmed.starts_with("</") {
                // End of block.
                let tag = trimmed
                    .trim_start_matches('<')
                    .trim_end_matches('>')
                    .trim_start_matches('/');
                if Some(tag) == current_block {
                    match tag {
                        "ca" => ca_cert = Some(block_buf.clone()),
                        "cert" => client_cert = Some(block_buf.clone()),
                        "key" => client_key = Some(block_buf.clone()),
                        "tls-auth" | "tls-crypt" => tls_auth = Some(block_buf.clone()),
                        _ => {}
                    }
                    block_buf.clear();
                    current_block = None;
                }
            } else {
                // Start of block — extract tag name.
                let tag_end = trimmed.find('>').unwrap_or(trimmed.len());
                let tag = &trimmed[1..tag_end];
                current_block = match tag {
                    "ca" | "cert" | "key" | "tls-auth" | "tls-crypt" => Some(
                        // Leak the string for 'static lifetime within loop — we only use
                        // named string literals above, so match on them instead.
                        match tag {
                            "ca" => "ca",
                            "cert" => "cert",
                            "key" => "key",
                            "tls-auth" => "tls-auth",
                            "tls-crypt" => "tls-crypt",
                            _ => unreachable!(),
                        },
                    ),
                    _ => None,
                };
            }
            continue;
        }

        if current_block.is_some() {
            if !trimmed.is_empty() {
                block_buf.push_str(trimmed);
                block_buf.push('\n');
            }
            continue;
        }

        // Skip comments and blank lines.
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }

        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let key = parts.next().unwrap_or("").to_lowercase();
        let val = parts.next().map(|s| s.trim()).unwrap_or("");

        match key.as_str() {
            "remote" => {
                // remote <host> [port] [proto]
                let mut rparts = val.split_whitespace();
                if let Some(host) = rparts.next() {
                    remote = Some(host.to_string());
                    if let Some(p) = rparts.next().and_then(|s| s.parse::<u16>().ok()) {
                        port = p;
                    }
                    if let Some(pr) = rparts.next() {
                        proto = pr.to_string();
                    }
                }
            }
            "port" => {
                if let Ok(p) = val.parse::<u16>() {
                    port = p;
                }
            }
            "proto" => proto = val.to_string(),
            "dev" => dev = val.to_string(),
            "cipher" => cipher = Some(val.to_string()),
            "auth" => auth = Some(val.to_string()),
            _ => {}
        }
    }

    let remote = remote.ok_or_else(|| anyhow!("OpenVPN config missing 'remote' directive"))?;

    Ok(OpenVpnConfig {
        remote,
        port,
        proto,
        dev,
        cipher,
        auth,
        ca_cert,
        client_cert,
        client_key,
        tls_auth,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_OVPN: &str = r#"
client
dev tun
proto udp
remote vpn.example.com 1194
resolv-retry infinite
nobind
persist-key
persist-tun
cipher AES-256-GCM
auth SHA256
<ca>
-----BEGIN CERTIFICATE-----
MIICfake
-----END CERTIFICATE-----
</ca>
"#;

    #[test]
    fn test_parse_valid_ovpn() {
        let cfg = parse_str(VALID_OVPN).unwrap();
        assert_eq!(cfg.remote, "vpn.example.com");
        assert_eq!(cfg.port, 1194);
        assert_eq!(cfg.proto, "udp");
        assert_eq!(cfg.dev, "tun");
        assert_eq!(cfg.cipher, Some("AES-256-GCM".to_string()));
        assert_eq!(cfg.auth, Some("SHA256".to_string()));
        assert!(cfg.ca_cert.is_some());
    }

    #[test]
    fn test_parse_missing_remote_fails() {
        let conf = "client\ndev tun\nproto udp\n";
        assert!(parse_str(conf).is_err());
    }

    #[test]
    fn test_parse_remote_with_port() {
        let conf = "remote myserver.com 443 tcp\n";
        let cfg = parse_str(conf).unwrap();
        assert_eq!(cfg.remote, "myserver.com");
        assert_eq!(cfg.port, 443);
        assert_eq!(cfg.proto, "tcp");
    }
}
