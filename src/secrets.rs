//! On-disk storage for PIA account credentials.
//!
//! Plaintext TOML at `~/.config/vex-vpn/credentials.toml` with mode `0600`.
//! Writes are atomic (write to `.tmp`, fsync, rename).
//! All public functions are async (wrapping sync I/O via spawn_blocking).
//!
//! Note: Secret Service (oo7) integration is deferred until oo7 stabilises
//! on a single zbus major version. Tracked in the Milestone C spec §4.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use tracing::warn;

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
/// Emits a warning if the file is world-readable (permissions check).
#[allow(dead_code)]
pub async fn load() -> Result<Option<Credentials>> {
    tokio::task::spawn_blocking(load_sync)
        .await
        .map_err(|e| anyhow::anyhow!("load credentials task: {}", e))?
}

/// Synchronous credential probe used from the GTK `connect_activate` handler
/// (which runs on the GTK main thread, outside any Tokio async task).
/// The call is fast (one stat + one small file read or NotFound), so blocking
/// the main thread briefly is acceptable here.
pub fn load_sync_hint() -> Result<Option<Credentials>> {
    load_sync()
}

/// Persist credentials atomically with mode `0600`.
pub async fn save(c: &Credentials) -> Result<()> {
    let c = c.clone();
    tokio::task::spawn_blocking(move || save_sync(&c))
        .await
        .map_err(|e| anyhow::anyhow!("save credentials task: {}", e))?
}

/// Remove the credentials file (no error if missing).
#[allow(dead_code)]
pub async fn delete() -> Result<()> {
    tokio::task::spawn_blocking(delete_sync)
        .await
        .map_err(|e| anyhow::anyhow!("delete credentials task: {}", e))?
}

// ---------------------------------------------------------------------------
// Sync implementations (run inside spawn_blocking)
// ---------------------------------------------------------------------------

fn load_sync() -> Result<Option<Credentials>> {
    let p = path();

    // Warn if permissions are too open.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(meta) = fs::metadata(&p) {
            if meta.mode() & 0o077 != 0 {
                warn!(
                    "Credentials file {} has overly permissive permissions (mode {:#o}). \
                     Consider running: chmod 0600 {}",
                    p.display(),
                    meta.mode() & 0o777,
                    p.display()
                );
            }
        }
    }

    let content = match fs::read_to_string(&p) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {}", p.display())),
    };
    let creds: Credentials =
        toml::from_str(&content).with_context(|| format!("parse {}", p.display()))?;
    Ok(Some(creds))
}

fn save_sync(c: &Credentials) -> Result<()> {
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

fn delete_sync() -> Result<()> {
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

    #[tokio::test]
    async fn round_trip_in_temp_dir() {
        let dir = std::env::temp_dir().join(format!("vex-vpn-secrets-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        std::env::set_var("XDG_CONFIG_HOME", &dir);

        assert!(load().await.unwrap().is_none());

        let c = Credentials {
            username: "p1234567".to_string(),
            password: "hunter2".to_string(),
        };
        save(&c).await.unwrap();

        let loaded = load().await.unwrap().expect("credentials present");
        assert_eq!(loaded.username, c.username);
        assert_eq!(loaded.password, c.password);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(path()).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "credentials file must be 0600");
        }

        delete().await.unwrap();
        assert!(load().await.unwrap().is_none());

        let _ = fs::remove_dir_all(&dir);
    }
}
