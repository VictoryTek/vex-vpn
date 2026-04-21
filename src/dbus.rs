use anyhow::Result;
use tracing::warn;
use zbus::dbus_proxy;
use zbus::Connection;

// ---------------------------------------------------------------------------
// zbus 3.x proxy definitions
// ---------------------------------------------------------------------------

#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn get_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdUnit {
    #[dbus_proxy(property)]
    fn active_state(&self) -> zbus::Result<String>;
}

// ---------------------------------------------------------------------------
// Connection helper — per-call for simplicity
// ---------------------------------------------------------------------------

async fn system_conn() -> Result<Connection> {
    Connection::system().await.map_err(anyhow::Error::from)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Returns the systemd ActiveState string ("active", "inactive", "activating", …)
/// or an error if the unit doesn't exist / D-Bus is unavailable.
pub async fn get_service_status(service: &str) -> Result<String> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;

    let unit_path = manager
        .get_unit(service)
        .await
        .map_err(|e| anyhow::anyhow!("get_unit({}) failed: {}", service, e))?;

    let unit = SystemdUnitProxy::builder(&conn)
        .path(unit_path.as_ref())
        .map_err(anyhow::Error::from)?
        .build()
        .await
        .map_err(anyhow::Error::from)?;

    unit.active_state().await.map_err(anyhow::Error::from)
}

pub async fn connect_vpn() -> Result<()> {
    start_unit("pia-vpn.service").await
}

pub async fn disconnect_vpn() -> Result<()> {
    stop_unit("pia-vpn.service").await
}

pub async fn enable_port_forward() -> Result<()> {
    start_unit("pia-vpn-portforward.service").await
}

pub async fn disable_port_forward() -> Result<()> {
    stop_unit("pia-vpn-portforward.service").await
}

async fn start_unit(name: &str) -> Result<()> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;
    manager
        .start_unit(name, "replace")
        .await
        .map_err(|e| anyhow::anyhow!("start_unit({}) failed: {}", name, e))?;
    Ok(())
}

async fn stop_unit(name: &str) -> Result<()> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;
    manager
        .stop_unit(name, "replace")
        .await
        .map_err(|e| anyhow::anyhow!("stop_unit({}) failed: {}", name, e))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Kill switch — nftables
// ---------------------------------------------------------------------------

/// Insert the pia_kill_switch nftables table that drops all non-VPN traffic.
pub async fn apply_kill_switch(interface: &str) -> Result<()> {
    let ruleset = format!(
        r#"table inet pia_kill_switch {{
    chain output {{
        type filter hook output priority 0; policy drop;
        ct state established,related accept
        oifname "{iface}" accept
        oifname "lo" accept
    }}
    chain input {{
        type filter hook input priority 0; policy drop;
        ct state established,related accept
        iifname "{iface}" accept
        iifname "lo" accept
    }}
}}"#,
        iface = interface
    );

    let mut child = tokio::process::Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn nft: {}", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin
            .write_all(ruleset.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("nft stdin write failed: {}", e))?;
    }

    let output = child
        .wait_with_output()
        .await
        .map_err(|e| anyhow::anyhow!("nft wait failed: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nft failed to apply kill switch: {}", stderr);
    }
    Ok(())
}

/// Remove the pia_kill_switch nftables table.
pub async fn remove_kill_switch() -> Result<()> {
    let output = tokio::process::Command::new("nft")
        .args(["delete", "table", "inet", "pia_kill_switch"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to spawn nft: {}", e))?;

    if !output.status.success() {
        warn!(
            "nft delete pia_kill_switch: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
