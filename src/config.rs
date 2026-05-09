use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::warn;

/// Validate that `name` is a legal Linux network interface name and
/// does not contain characters that could be interpreted by nft.
/// Pattern: starts with a lowercase letter, followed by up to 14
/// lowercase alphanumeric, underscore, or hyphen characters.
pub fn validate_interface(name: &str) -> bool {
    if name.is_empty() || name.len() > 15 {
        return false;
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

/// Persists user preferences to ~/.config/vex-vpn/config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub auto_connect: bool,
    pub interface: String,    // default "wg0"
    pub max_latency_ms: u32,  // default 100
    pub dns_provider: String, // default "pia"
    #[serde(default)]
    pub selected_region_id: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_connect: false,
            interface: "wg0".to_string(),
            max_latency_ms: 100,
            dns_provider: "pia".to_string(),
            selected_region_id: None,
        }
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("vex-vpn").join("config.toml")
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => return Self::default(),
        };
        let mut cfg: Config = toml::from_str(&content).unwrap_or_default();

        // Validate the interface name to prevent nft injection.
        if !validate_interface(&cfg.interface) {
            warn!(
                "Invalid interface name {:?} in config, falling back to \"wg0\"",
                cfg.interface
            );
            cfg.interface = "wg0".to_string();
        }

        cfg
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let c = Config::default();
        assert!(!c.auto_connect);
        assert_eq!(c.interface, "wg0");
        assert_eq!(c.max_latency_ms, 100);
        assert_eq!(c.dns_provider, "pia");
        assert_eq!(c.selected_region_id, None);
    }

    #[test]
    fn test_config_round_trip() {
        let original = Config {
            auto_connect: true,
            interface: "wg1".to_string(),
            max_latency_ms: 200,
            dns_provider: "cloudflare".to_string(),
            selected_region_id: Some("us_california".to_string()),
        };
        let serialized = toml::to_string_pretty(&original).unwrap();
        let loaded: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(loaded.auto_connect, original.auto_connect);
        assert_eq!(loaded.interface, original.interface);
        assert_eq!(loaded.max_latency_ms, original.max_latency_ms);
        assert_eq!(loaded.dns_provider, original.dns_provider);
        assert_eq!(loaded.selected_region_id, original.selected_region_id);
    }

    #[test]
    fn test_config_backward_compat_no_region() {
        // Config TOML without selected_region_id should deserialize with None.
        let toml_str = r#"
auto_connect = false
interface = "wg0"
max_latency_ms = 100
dns_provider = "pia"
"#;
        let cfg: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.selected_region_id, None);
    }

    #[test]
    fn test_validate_interface_valid() {
        assert!(validate_interface("wg0"));
        assert!(validate_interface("a"));
        assert!(validate_interface("wg-pia_01"));
        // 15 chars (max Linux iface name length)
        assert!(validate_interface("abcdefghijklmno"));
    }

    #[test]
    fn test_validate_interface_invalid() {
        assert!(!validate_interface(""));
        assert!(!validate_interface("WG0")); // uppercase first char
        assert!(!validate_interface("0abc")); // starts with digit
        assert!(!validate_interface(&"a".repeat(16))); // too long
        assert!(!validate_interface("wg;rm")); // semicolon
        assert!(!validate_interface("wg\nrf")); // newline
        assert!(!validate_interface("wg rf")); // space
        assert!(!validate_interface("wg\"x")); // quote
    }
}
