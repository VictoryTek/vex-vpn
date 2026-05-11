use anyhow::Result;
use tokio::sync::OnceCell;
use zbus::dbus_proxy;
use zbus::Connection;
use zbus::MethodFlags;

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
    fn load_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn get_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdUnit {
    #[dbus_proxy(property)]
    fn active_state(&self) -> zbus::Result<String>;

    #[dbus_proxy(property)]
    fn load_state(&self) -> zbus::Result<String>;
}

/// NetworkManager global connectivity state constants.
pub const NM_CONNECTED_GLOBAL: u32 = 70;

#[dbus_proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    /// Emitted when overall connectivity state changes.
    #[dbus_proxy(signal)]
    fn state_changed(&self, state: u32) -> zbus::Result<()>;
}

// ---------------------------------------------------------------------------
// Connection helper — lazily initialised shared connection
// ---------------------------------------------------------------------------

static SYSTEM_CONN: OnceCell<Connection> = OnceCell::const_new();

pub(crate) async fn system_conn() -> zbus::Result<Connection> {
    SYSTEM_CONN
        .get_or_try_init(|| async { zbus::Connection::system().await })
        .await
        .cloned()
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
        .load_unit(service)
        .await
        .map_err(|e| anyhow::anyhow!("load_unit({}) failed: {}", service, e))?;

    let unit = SystemdUnitProxy::builder(&conn)
        .path(unit_path.as_ref())
        .map_err(anyhow::Error::from)?
        .build()
        .await
        .map_err(anyhow::Error::from)?;

    unit.active_state().await.map_err(anyhow::Error::from)
}

/// Returns `true` if the systemd unit file for `service` is present on disk
/// (`LoadState != "not-found"`). Uses `LoadUnit` so it works even when the
/// unit has never been started. Returns `false` on any D-Bus error.
pub async fn is_service_unit_installed(service: &str) -> bool {
    let Ok(conn) = system_conn().await else {
        return false;
    };
    let Ok(manager) = SystemdManagerProxy::new(&conn).await else {
        return false;
    };
    let Ok(unit_path) = manager.load_unit(service).await else {
        return false;
    };
    let path_ref = unit_path.as_ref();
    let unit = match SystemdUnitProxy::builder(&conn)
        .path(path_ref)
        .map_err(|_| ())
    {
        Ok(b) => match b.build().await {
            Ok(u) => u,
            Err(_) => return false,
        },
        Err(_) => return false,
    };
    unit.load_state()
        .await
        .map(|s| s != "not-found")
        .unwrap_or(false)
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

/// Stop then start pia-vpn.service — used by auto-reconnect and watchdog.
pub async fn restart_vpn_unit() -> Result<()> {
    stop_unit("pia-vpn.service").await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    start_unit("pia-vpn.service").await
}

async fn start_unit(name: &str) -> Result<()> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;
    manager
        .inner()
        .call_with_flags::<_, _, zbus::zvariant::OwnedObjectPath>(
            "StartUnit",
            MethodFlags::AllowInteractiveAuth.into(),
            &(name, "replace"),
        )
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
        .inner()
        .call_with_flags::<_, _, zbus::zvariant::OwnedObjectPath>(
            "StopUnit",
            MethodFlags::AllowInteractiveAuth.into(),
            &(name, "replace"),
        )
        .await
        .map_err(|e| anyhow::anyhow!("stop_unit({}) failed: {}", name, e))?;
    Ok(())
}
