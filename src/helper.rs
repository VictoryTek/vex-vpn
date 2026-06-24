//! Kill switch management via systemd D-Bus.
//!
//! Starts/stops the `vex-vpn-killswitch.service` (or the name configured in
//! `kill_switch_service`) using the existing systemd proxy in `dbus.rs`.
//! Polkit interactive auth is handled transparently by `MethodFlags::AllowInteractiveAuth`.

use anyhow::Result;

/// Enable the kill switch by starting the configured systemd service.
pub async fn apply_kill_switch() -> Result<()> {
    let cfg = crate::config::Config::load().unwrap_or_default();
    let unit = format!("{}.service", cfg.kill_switch_service);
    crate::dbus::start_kill_switch_unit(&unit).await
}

/// Disable the kill switch by stopping the configured systemd service.
pub async fn remove_kill_switch() -> Result<()> {
    let cfg = crate::config::Config::load().unwrap_or_default();
    let unit = format!("{}.service", cfg.kill_switch_service);
    crate::dbus::stop_kill_switch_unit(&unit).await
}
