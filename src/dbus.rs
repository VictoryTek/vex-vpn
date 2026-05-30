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
    /// Activate an existing NM connection by object path.
    fn activate_connection(
        &self,
        connection: &zbus::zvariant::ObjectPath<'_>,
        device: &zbus::zvariant::ObjectPath<'_>,
        specific_object: &zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    /// Deactivate an active NM connection by object path.
    fn deactivate_connection(
        &self,
        active_connection: &zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::Result<()>;

    /// Active connections list.
    #[dbus_proxy(property)]
    fn active_connections(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;

    /// Emitted when overall connectivity state changes.
    #[dbus_proxy(signal)]
    fn state_changed(&self, state: u32) -> zbus::Result<()>;
}

#[dbus_proxy(
    interface = "org.freedesktop.NetworkManager.Settings",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Settings"
)]
trait NetworkManagerSettings {
    fn get_connection_by_uuid(&self, uuid: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    fn list_connections(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;
}

#[dbus_proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait NmActiveConnection {
    #[dbus_proxy(property)]
    fn state(&self) -> zbus::Result<u32>;

    #[dbus_proxy(property)]
    fn id(&self) -> zbus::Result<String>;

    #[dbus_proxy(property)]
    fn uuid(&self) -> zbus::Result<String>;
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
// Public API — systemd
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

/// Returns `true` if the systemd unit file for `service` is present on disk.
#[allow(dead_code)]
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

/// Start a WireGuard wg-quick service for the given interface name.
pub async fn start_wireguard_unit(interface: &str) -> Result<()> {
    start_unit(&format!("wg-quick@{}.service", interface)).await
}

/// Stop a WireGuard wg-quick service for the given interface name.
pub async fn stop_wireguard_unit(interface: &str) -> Result<()> {
    stop_unit(&format!("wg-quick@{}.service", interface)).await
}

/// Stop then start `wg-quick@<interface>.service` — used by watchdog.
pub async fn restart_wireguard_unit(interface: &str) -> Result<()> {
    stop_wireguard_unit(interface).await?;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    start_wireguard_unit(interface).await
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

// ---------------------------------------------------------------------------
// Public API — NetworkManager
// ---------------------------------------------------------------------------

/// Activate a NetworkManager connection by UUID (matches VpnProfile::id).
/// Returns Ok(()) on success; the connection becomes "active" asynchronously.
#[allow(dead_code)]
pub async fn activate_nm_connection(uuid: &str) -> Result<()> {
    let conn = system_conn().await?;

    let settings = NetworkManagerSettingsProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;

    let conn_path = settings
        .get_connection_by_uuid(uuid)
        .await
        .map_err(|e| anyhow::anyhow!("get_connection_by_uuid({}) failed: {}", uuid, e))?;

    let nm = NetworkManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;

    let any_path = zbus::zvariant::ObjectPath::try_from("/").map_err(anyhow::Error::from)?;

    nm.activate_connection(&conn_path.as_ref(), &any_path, &any_path)
        .await
        .map_err(|e| anyhow::anyhow!("ActivateConnection({}) failed: {}", uuid, e))?;

    Ok(())
}

/// Deactivate a NetworkManager connection by UUID.
#[allow(dead_code)]
pub async fn deactivate_nm_connection(uuid: &str) -> Result<()> {
    let conn = system_conn().await?;

    let nm = NetworkManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;

    let active_connections = nm.active_connections().await.map_err(anyhow::Error::from)?;

    for ac_path in active_connections {
        let ac = NmActiveConnectionProxy::builder(&conn)
            .path(ac_path.as_ref())
            .map_err(anyhow::Error::from)?
            .build()
            .await
            .map_err(anyhow::Error::from)?;

        if let Ok(ac_uuid) = ac.uuid().await {
            if ac_uuid == uuid {
                nm.deactivate_connection(&ac_path.as_ref())
                    .await
                    .map_err(|e| anyhow::anyhow!("DeactivateConnection({}) failed: {}", uuid, e))?;
                return Ok(());
            }
        }
    }

    // Not active — that is fine, consider it already disconnected.
    Ok(())
}

/// Query the NM connection state for a profile UUID.
/// Returns Some(status) if the connection is active, None if not found.
pub async fn get_nm_connection_state(uuid: &str) -> Result<Option<crate::state::ConnectionStatus>> {
    let conn = system_conn().await?;

    let nm = NetworkManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;

    let active_connections = nm.active_connections().await.map_err(anyhow::Error::from)?;

    for ac_path in active_connections {
        let ac = match NmActiveConnectionProxy::builder(&conn)
            .path(ac_path.as_ref())
            .map_err(anyhow::Error::from)?
            .build()
            .await
        {
            Ok(p) => p,
            Err(_) => continue,
        };

        if let Ok(ac_uuid) = ac.uuid().await {
            if ac_uuid == uuid {
                // NM_ACTIVE_CONNECTION_STATE: 0=Unknown, 1=Activating, 2=Activated, 3=Deactivating, 4=Deactivated
                let state = ac.state().await.unwrap_or(0);
                let status = match state {
                    1 => crate::state::ConnectionStatus::Connecting,
                    2 => crate::state::ConnectionStatus::Connected,
                    3 => crate::state::ConnectionStatus::Connecting,
                    _ => crate::state::ConnectionStatus::Disconnected,
                };
                return Ok(Some(status));
            }
        }
    }

    Ok(None)
}
