use anyhow::Result;
use tracing::{debug, warn};
use zbus::{proxy, Connection};

// Systemd D-Bus proxy
#[proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    async fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    async fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    async fn get_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdUnit {
    #[zbus(property)]
    fn active_state(&self) -> zbus::Result<String>;

    #[zbus(property)]
    fn sub_state(&self) -> zbus::Result<String>;
}

async fn get_system_connection() -> Result<Connection> {
    Ok(Connection::system().await?)
}

pub async fn get_service_status(service: &str) -> Result<String> {
    let conn = get_system_connection().await?;
    let manager = SystemdManagerProxy::new(&conn).await?;

    let unit_path = manager.get_unit(service).await.map_err(|e| {
        anyhow::anyhow!("Service {} not found: {}", service, e)
    })?;

    let unit = SystemdUnitProxy::builder(&conn)
        .path(unit_path)?
        .build()
        .await?;

    let state = unit.active_state().await?;
    debug!("Service {} state: {}", service, state);
    Ok(state)
}

pub async fn start_service(service: &str) -> Result<()> {
    let conn = get_system_connection().await?;
    let manager = SystemdManagerProxy::new(&conn).await?;
    manager.start_unit(service, "replace").await?;
    Ok(())
}

pub async fn stop_service(service: &str) -> Result<()> {
    let conn = get_system_connection().await?;
    let manager = SystemdManagerProxy::new(&conn).await?;
    manager.stop_unit(service, "replace").await?;
    Ok(())
}

pub async fn connect_vpn() -> Result<()> {
    start_service("pia-vpn.service").await
}

pub async fn disconnect_vpn() -> Result<()> {
    stop_service("pia-vpn.service").await
}

pub async fn enable_port_forward() -> Result<()> {
    start_service("pia-vpn-portforward.service").await
}

pub async fn disable_port_forward() -> Result<()> {
    stop_service("pia-vpn-portforward.service").await
}

/// Apply kill switch via nftables — blocks all traffic not going through the VPN interface
pub async fn apply_kill_switch(interface: &str) -> Result<()> {
    let ruleset = format!(
        r#"
table inet pia_kill_switch {{
    chain output {{
        type filter hook output priority 0; policy drop;
        oifname "{iface}" accept
        oifname "lo" accept
        ct state established,related accept
    }}
    chain input {{
        type filter hook input priority 0; policy drop;
        iifname "{iface}" accept
        iifname "lo" accept
        ct state established,related accept
    }}
}}
"#,
        iface = interface
    );

    let mut child = tokio::process::Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .spawn()?;

    if let Some(stdin) = child.stdin.as_mut() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(ruleset.as_bytes()).await?;
    }

    let status = child.wait().await?;
    if !status.success() {
        anyhow::bail!("nft failed to apply kill switch rules");
    }
    Ok(())
}

pub async fn remove_kill_switch() -> Result<()> {
    let output = tokio::process::Command::new("nft")
        .args(["delete", "table", "inet", "pia_kill_switch"])
        .output()
        .await?;

    if !output.status.success() {
        warn!("nft delete returned non-zero (may not have existed)");
    }
    Ok(())
}
