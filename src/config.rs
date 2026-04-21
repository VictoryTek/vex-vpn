use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Persists user preferences to ~/.config/vex-vpn/config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub auto_connect: bool,
    pub interface: String,    // default "wg0"
    pub max_latency_ms: u32,  // default 100
    pub dns_provider: String, // default "pia"
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_connect: false,
            interface: "wg0".to_string(),
            max_latency_ms: 100,
            dns_provider: "pia".to_string(),
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
        toml::from_str(&content).unwrap_or_default()
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
    }

    #[test]
    fn test_config_round_trip() {
        let original = Config {
            auto_connect: true,
            interface: "wg1".to_string(),
            max_latency_ms: 200,
            dns_provider: "cloudflare".to_string(),
        };
        let serialized = toml::to_string_pretty(&original).unwrap();
        let loaded: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(loaded.auto_connect, original.auto_connect);
        assert_eq!(loaded.interface, original.interface);
        assert_eq!(loaded.max_latency_ms, original.max_latency_ms);
        assert_eq!(loaded.dns_provider, original.dns_provider);
    }
}
