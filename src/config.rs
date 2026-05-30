use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use crate::profile::{VpnProfile, VpnType};

/// Validate that `name` is a legal Linux network interface name and
/// does not contain characters that could be interpreted by nft.
/// Pattern: starts with a lowercase letter, followed by up to 14
/// lowercase alphanumeric, underscore, or hyphen characters.
#[allow(dead_code)]
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

fn default_schema_version() -> u32 {
    1
}

fn default_true() -> bool {
    true
}

/// Persists user preferences to `~/.config/vex-vpn/config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_schema_version")]
    pub version: u32,
    /// All managed VPN profiles.
    #[serde(default)]
    pub profiles: Vec<VpnProfile>,
    /// UUID of the last/active profile.
    #[serde(default)]
    pub active_profile_id: Option<String>,
    /// Launch minimized to tray.
    #[serde(default)]
    pub start_minimized: bool,
    /// Auto-reconnect when network is restored.
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// Show icon in the system tray.
    #[serde(default = "default_true")]
    pub show_tray_icon: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            profiles: Vec::new(),
            active_profile_id: None,
            start_minimized: false,
            auto_reconnect: true,
            show_tray_icon: true,
        }
    }
}

/// Returns `~/.config/vex-vpn/config.toml` (respects `$XDG_CONFIG_HOME`).
pub fn config_path() -> PathBuf {
    crate::profile::config_base_dir().join("config.toml")
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
        toml::from_str(&content).with_context(|| format!("parse {}", path.display()))
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

    /// Find a profile by ID.
    #[allow(dead_code)]
    pub fn find_profile(&self, id: &str) -> Option<&VpnProfile> {
        self.profiles.iter().find(|p| p.id == id)
    }

    /// Find a mutable profile by ID.
    #[allow(dead_code)]
    pub fn find_profile_mut(&mut self, id: &str) -> Option<&mut VpnProfile> {
        self.profiles.iter_mut().find(|p| p.id == id)
    }

    /// Return the currently active profile, if any.
    #[allow(dead_code)]
    pub fn active_profile(&self) -> Option<&VpnProfile> {
        self.active_profile_id
            .as_deref()
            .and_then(|id| self.find_profile(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let c = Config::default();
        assert!(c.profiles.is_empty());
        assert_eq!(c.active_profile_id, None);
        assert!(!c.start_minimized);
        assert!(c.auto_reconnect);
        assert!(c.show_tray_icon);
        assert_eq!(c.version, 1);
    }

    #[test]
    fn test_config_round_trip_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let original = Config {
            version: 1,
            profiles: vec![],
            active_profile_id: Some("test-uuid".to_string()),
            start_minimized: true,
            auto_reconnect: false,
            show_tray_icon: true,
        };
        original.save_to(&path).unwrap();
        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.active_profile_id, original.active_profile_id);
        assert_eq!(loaded.start_minimized, original.start_minimized);
        assert_eq!(loaded.auto_reconnect, original.auto_reconnect);
        assert_eq!(loaded.version, original.version);
    }

    #[test]
    fn test_config_missing_file_returns_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.toml");
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg.version, 1);
        assert!(cfg.profiles.is_empty());
    }

    #[test]
    fn test_validate_interface_valid() {
        assert!(validate_interface("wg0"));
        assert!(validate_interface("a"));
        assert!(validate_interface("wg-pia_01"));
        assert!(validate_interface("abcdefghijklmno")); // 15 chars
    }

    #[test]
    fn test_validate_interface_invalid() {
        assert!(!validate_interface(""));
        assert!(!validate_interface("0wg")); // starts with digit
        assert!(!validate_interface("Wg0")); // uppercase
        assert!(!validate_interface("abcdefghijklmnop")); // 16 chars
        assert!(!validate_interface("wg0;drop")); // semicolon
    }
}
