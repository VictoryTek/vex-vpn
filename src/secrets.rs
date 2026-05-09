//! On-disk storage for PIA account credentials.
//!
//! Phase-1 MVP: plaintext TOML at `~/.config/vex-vpn/credentials.toml` with
//! mode `0600`. Writes are atomic (write to `.tmp`, fsync, rename). A future
//! milestone will swap this for a Secret Service / `oo7` backed store.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;

/// PIA username / password pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

/// Returns `~/.config/vex-vpn/credentials.toml` (respects `$XDG_CONFIG_HOME`).
pub fn path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("vex-vpn").join("credentials.toml")
}

/// Load saved credentials. Returns `Ok(None)` if the file does not exist.
pub fn load() -> Result<Option<Credentials>> {
    let p = path();
    let content = match fs::read_to_string(&p) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", p.display())),
    };
    let creds: Credentials =
        toml::from_str(&content).with_context(|| format!("parse {}", p.display()))?;
    Ok(Some(creds))
}

/// Persist credentials atomically with mode `0600`.
pub fn save(c: &Credentials) -> Result<()> {
    let final_path = path();
    if let Some(parent) = final_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create dir {}", parent.display()))?;
    }
    let tmp_path = final_path.with_extension("toml.tmp");
    let body = toml::to_string_pretty(c).context("serialize credentials")?;

    {
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp_path)
            .with_context(|| format!("open {}", tmp_path.display()))?;
        f.write_all(body.as_bytes())
            .with_context(|| format!("write {}", tmp_path.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync {}", tmp_path.display()))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perm = fs::Permissions::from_mode(0o600);
        fs::set_permissions(&tmp_path, perm)
            .with_context(|| format!("chmod {}", tmp_path.display()))?;
    }

    fs::rename(&tmp_path, &final_path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), final_path.display()))?;
    Ok(())
}

/// Remove the credentials file (no error if missing).
#[allow(dead_code)]
pub fn delete() -> Result<()> {
    let p = path();
    match fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("remove {}", p.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("vex-vpn-secrets-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);

        assert!(load().unwrap().is_none());

        let c = Credentials {
            username: "p1234567".to_string(),
            password: "hunter2".to_string(),
        };
        save(&c).unwrap();

        let loaded = load().unwrap().expect("credentials present");
        assert_eq!(loaded.username, c.username);
        assert_eq!(loaded.password, c.password);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "credentials file must be 0600");
        }

        delete().unwrap();
        assert!(load().unwrap().is_none());

        let _ = fs::remove_dir_all(&dir);
    }
}
