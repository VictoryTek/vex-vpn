//! systemd control over the system bus. vexos-nix's polkit rules let the
//! `users` group start/stop the vexos-vpn units; stopping the kill switch in
//! "always" mode triggers an interactive admin prompt.

use std::time::Duration;
use tokio::sync::OnceCell;
use zbus::dbus_proxy;
use zbus::Connection;
use zbus::MethodFlags;

pub const VPN_UNIT: &str = "vexos-vpn.service";
pub const KILLSWITCH_UNIT: &str = "vexos-killswitch.service";

pub fn region_unit(id: &str) -> String {
    format!("vexos-vpn-region@{}.service", id)
}

pub fn protocol_unit(protocol: &str) -> String {
    format!("vexos-vpn-protocol@{}.service", protocol)
}

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
    fn restart_unit(&self, name: &str, mode: &str)
        -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn load_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdUnit {
    #[dbus_proxy(property)]
    fn active_state(&self) -> zbus::Result<String>;
}

#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Job",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdJob {
    #[dbus_proxy(property)]
    fn state(&self) -> zbus::Result<String>;
}

static SYSTEM_CONN: OnceCell<Connection> = OnceCell::const_new();

async fn system_conn() -> zbus::Result<Connection> {
    SYSTEM_CONN
        .get_or_try_init(|| async { zbus::Connection::system().await })
        .await
        .cloned()
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnitError {
    /// polkit authentication dismissed or denied.
    Cancelled,
    /// The unit does not exist (e.g. kill switch with mode "off").
    NotInstalled,
    Failed(String),
}

impl std::fmt::Display for UnitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => write!(f, "Authentication cancelled"),
            Self::NotInstalled => write!(f, "This vexos-vpn unit is not installed"),
            Self::Failed(msg) => write!(f, "{}", msg),
        }
    }
}

fn classify(e: zbus::Error) -> UnitError {
    if let zbus::Error::MethodError(name, _, _) = &e {
        match name.as_str() {
            "org.freedesktop.DBus.Error.AccessDenied"
            | "org.freedesktop.DBus.Error.InteractiveAuthorizationRequired" => {
                return UnitError::Cancelled
            }
            "org.freedesktop.systemd1.NoSuchUnit" => return UnitError::NotInstalled,
            _ => {}
        }
    }
    UnitError::Failed(e.to_string())
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

async fn call(method: &str, unit: &str) -> Result<zbus::zvariant::OwnedObjectPath, UnitError> {
    let conn = system_conn().await.map_err(classify)?;
    let manager = SystemdManagerProxy::new(&conn).await.map_err(classify)?;
    manager
        .inner()
        .call_with_flags::<_, _, zbus::zvariant::OwnedObjectPath>(
            method,
            MethodFlags::AllowInteractiveAuth.into(),
            &(unit, "replace"),
        )
        .await
        .map_err(classify)?
        .ok_or_else(|| UnitError::Failed(format!("{} {}: no reply from systemd", method, unit)))
}

pub async fn start_unit(unit: &str) -> Result<(), UnitError> {
    call("StartUnit", unit).await.map(drop)
}

pub async fn stop_unit(unit: &str) -> Result<(), UnitError> {
    call("StopUnit", unit).await.map(drop)
}

pub async fn restart_unit(unit: &str) -> Result<(), UnitError> {
    call("RestartUnit", unit).await.map(drop)
}

/// Start a `Type=oneshot` setter unit and wait until it has run. StartUnit
/// only enqueues a job; a oneshot's start job exists until ExecStart has
/// finished, so wait for the job object to disappear, then read the result.
pub async fn run_oneshot(unit: &str) -> Result<(), UnitError> {
    let job = call("StartUnit", unit).await?;
    let conn = system_conn().await.map_err(classify)?;
    let job = SystemdJobProxy::builder(&conn)
        .path(job)
        .map_err(classify)?
        .cache_properties(zbus::CacheProperties::No)
        .build()
        .await
        .map_err(classify)?;
    let mut finished = false;
    for _ in 0..120 {
        if job.state().await.is_err() {
            finished = true; // job object gone
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    if !finished {
        return Err(UnitError::Failed(format!("{} is still running", unit)));
    }
    let manager = SystemdManagerProxy::new(&conn).await.map_err(classify)?;
    let path = manager.load_unit(unit).await.map_err(classify)?;
    let proxy = SystemdUnitProxy::builder(&conn)
        .path(path)
        .map_err(classify)?
        .cache_properties(zbus::CacheProperties::No)
        .build()
        .await
        .map_err(classify)?;
    match proxy.active_state().await.map_err(classify)?.as_str() {
        "failed" => Err(UnitError::Failed(format!(
            "{} failed — see: journalctl -u {}",
            unit, unit
        ))),
        _ => Ok(()),
    }
}

/// True while the unit is running or being (re)started — including systemd's
/// auto-restart wait after a failed connect attempt.
pub async fn is_unit_active(unit: &str) -> Result<bool, UnitError> {
    let conn = system_conn().await.map_err(classify)?;
    let manager = SystemdManagerProxy::new(&conn).await.map_err(classify)?;
    let path = manager.load_unit(unit).await.map_err(classify)?;
    let proxy = SystemdUnitProxy::builder(&conn)
        .path(path)
        .map_err(classify)?
        .cache_properties(zbus::CacheProperties::No)
        .build()
        .await
        .map_err(classify)?;
    let state = proxy.active_state().await.map_err(classify)?;
    Ok(matches!(
        state.as_str(),
        "active" | "activating" | "reloading"
    ))
}
