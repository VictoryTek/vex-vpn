use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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
    #[serde(default = "default_schema_version")]
    pub version: u32,
    pub auto_connect: bool,
    pub interface: String,    // default "wg0"
    pub max_latency_ms: u32,  // default 100
    pub dns_provider: String, // default "pia"
    #[serde(default)]
    pub selected_region_id: Option<String>,
    #[serde(default)]
    pub kill_switch_enabled: bool,
    #[serde(default = "default_kill_switch_allowed_ifaces")]
    pub kill_switch_allowed_ifaces: Vec<String>,
    /// Automatically restart the VPN tunnel when network connectivity is restored.
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
}

fn default_schema_version() -> u32 {
    1
}

fn default_kill_switch_allowed_ifaces() -> Vec<String> {
    vec!["lo".to_string()]
}

fn default_true() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            auto_connect: false,
            interface: "wg0".to_string(),
            max_latency_ms: 100,
            dns_provider: "pia".to_string(),
            selected_region_id: None,
            kill_switch_enabled: false,
            kill_switch_allowed_ifaces: vec!["lo".to_string()],
            auto_reconnect: true,
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
    /// Load config from the canonical path (`~/.config/vex-vpn/config.toml`).
    /// Returns `Err` if the file exists but cannot be parsed.
    /// Returns `Ok(Config::default())` if the file does not exist.
    pub fn load() -> Result<Self> {
        Self::load_from(&config_path())
    }

    /// Load config from an explicit path — used by integration tests for isolation.
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(anyhow::Error::from(e))
                    .with_context(|| format!("read {}", path.display()))
            }
        };
        let mut cfg: Config =
            toml::from_str(&content).with_context(|| format!("parse {}", path.display()))?;

        // Validate the interface name to prevent nft injection.
        if !validate_interface(&cfg.interface) {
            warn!(
                "Invalid interface name {:?} in config, falling back to \"wg0\"",
                cfg.interface
            );
            cfg.interface = "wg0".to_string();
        }

        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&config_path())
    }

    /// Write config atomically to `path` via a temp file + rename.
    /// Used directly by integration tests for isolation.
    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        use std::io::Write;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        let tmp_path = path.with_extension("toml.tmp");
        {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&tmp_path)?;
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp_path, path)?;
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
        assert!(!c.kill_switch_enabled);
        assert_eq!(c.kill_switch_allowed_ifaces, vec!["lo".to_string()]);
    }

    #[test]
    fn test_config_round_trip() {
        let original = Config {
            version: 1,
            auto_connect: true,
            interface: "wg1".to_string(),
            max_latency_ms: 200,
            dns_provider: "cloudflare".to_string(),
            selected_region_id: Some("us_california".to_string()),
            kill_switch_enabled: false,
            kill_switch_allowed_ifaces: vec![],
            auto_reconnect: true,
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
