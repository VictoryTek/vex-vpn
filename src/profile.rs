//! VPN profile data model — provider-agnostic WireGuard / OpenVPN profiles.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Supported VPN protocol types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpnType {
    WireGuard,
    OpenVpn,
}

impl std::fmt::Display for VpnType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VpnType::WireGuard => write!(f, "WireGuard"),
            VpnType::OpenVpn => write!(f, "OpenVPN"),
        }
    }
}

/// A managed VPN profile (WireGuard .conf or OpenVPN .ovpn).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnProfile {
    /// Stable UUID v4 — never changes after creation; used as the profile directory name.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Protocol type.
    pub vpn_type: VpnType,
    /// Filename of the stored config inside the profile directory.
    /// Relative to `~/.config/vex-vpn/profiles/<id>/`.
    pub config_file: String,
    /// Connect automatically at startup.
    pub auto_connect: bool,
    /// Block all non-VPN traffic when this profile is active.
    pub kill_switch: bool,
    /// Override DNS server while this profile is active.
    /// None = use the DNS specified in the config file.
    pub dns_override: Option<String>,
    /// WireGuard interface name (only meaningful for WireGuard profiles).
    /// Defaults to "wg0" when not set.
    pub interface: Option<String>,
}

impl VpnProfile {
    /// Create a new profile with a freshly generated UUID.
    pub fn new(name: String, vpn_type: VpnType, config_file: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            vpn_type,
            config_file,
            auto_connect: false,
            kill_switch: false,
            dns_override: None,
            interface: None,
        }
    }

    /// Returns the directory where this profile's config file is stored.
    /// Path: `~/.config/vex-vpn/profiles/<id>/`
    pub fn profile_dir(&self) -> PathBuf {
        profiles_base_dir().join(&self.id)
    }

    /// Returns the full path to this profile's config file.
    #[allow(dead_code)]
    pub fn config_path(&self) -> PathBuf {
        self.profile_dir().join(&self.config_file)
    }

    /// Returns the effective WireGuard interface name.
    /// Uses the explicitly set interface, or defaults to "wg0".
    pub fn effective_interface(&self) -> &str {
        self.interface.as_deref().unwrap_or("wg0")
    }
}

/// Base directory for all profile subdirectories: `~/.config/vex-vpn/profiles/`.
pub fn profiles_base_dir() -> PathBuf {
    config_base_dir().join("profiles")
}

/// Returns the config base dir: `$XDG_CONFIG_HOME/vex-vpn` or `~/.config/vex-vpn`.
pub fn config_base_dir() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        })
        .join("vex-vpn")
}

/// Import a VPN config file: copy to `profiles_base_dir()/<uuid>/`, return a new profile.
/// The profile directory is created with mode 0700; the config file with mode 0600.
pub fn import_profile(
    name: String,
    source_path: &std::path::Path,
    vpn_type: VpnType,
) -> std::io::Result<VpnProfile> {
    let ext = match vpn_type {
        VpnType::WireGuard => "conf",
        VpnType::OpenVpn => "ovpn",
    };
    let config_filename = format!("vpn.{}", ext);
    let profile = VpnProfile::new(name, vpn_type, config_filename.clone());

    let dir = profile.profile_dir();
    create_profile_dir(&dir)?;

    let dest = dir.join(&config_filename);
    copy_with_mode(source_path, &dest)?;

    Ok(profile)
}

/// Delete a profile's directory from disk.
#[allow(dead_code)]
pub fn delete_profile_dir(profile: &VpnProfile) -> std::io::Result<()> {
    let dir = profile.profile_dir();
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Create a profile directory with mode 0700.
fn create_profile_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// Copy a file to `dest` with mode 0600.
fn copy_with_mode(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;

    let content = std::fs::read(src)?;

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(dest)?;
    f.write_all(&content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vpn_profile_new_generates_uuid() {
        let p = VpnProfile::new(
            "test".to_string(),
            VpnType::WireGuard,
            "vpn.conf".to_string(),
        );
        assert!(!p.id.is_empty());
        assert_eq!(p.name, "test");
        assert_eq!(p.vpn_type, VpnType::WireGuard);
        assert!(!p.auto_connect);
        assert!(!p.kill_switch);
    }

    #[test]
    fn test_effective_interface_default() {
        let p = VpnProfile::new(
            "test".to_string(),
            VpnType::WireGuard,
            "vpn.conf".to_string(),
        );
        assert_eq!(p.effective_interface(), "wg0");
    }

    #[test]
    fn test_effective_interface_explicit() {
        let mut p = VpnProfile::new(
            "test".to_string(),
            VpnType::WireGuard,
            "vpn.conf".to_string(),
        );
        p.interface = Some("wg1".to_string());
        assert_eq!(p.effective_interface(), "wg1");
    }

    #[test]
    fn test_vpn_type_display() {
        assert_eq!(VpnType::WireGuard.to_string(), "WireGuard");
        assert_eq!(VpnType::OpenVpn.to_string(), "OpenVPN");
    }

    #[test]
    fn test_vpn_type_serde_roundtrip() {
        let t = VpnType::WireGuard;
        let s = serde_json::to_string(&t).unwrap();
        let d: VpnType = serde_json::from_str(&s).unwrap();
        assert_eq!(d, t);
    }
}
