//! VPN backend abstraction.

pub mod openvpn;
pub mod wireguard;

use crate::profile::{VpnProfile, VpnType};
use crate::state::ConnectionStatus;
use anyhow::Result;
use async_trait::async_trait;

/// Traffic statistics for an active VPN connection.
#[derive(Debug, Clone, Default)]
pub struct ConnectionInfo {
    pub local_ip: String,
    pub remote_endpoint: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

/// Common interface for all VPN backends.
#[allow(dead_code)]
#[async_trait]
pub trait VpnBackend: Send + Sync {
    /// Establish a VPN connection for the given profile.
    async fn connect(&self, profile: &VpnProfile) -> Result<()>;
    /// Tear down the VPN connection.
    async fn disconnect(&self, profile: &VpnProfile) -> Result<()>;
    /// Query the current connection status.
    async fn status(&self, profile: &VpnProfile) -> Result<ConnectionStatus>;
    /// Return live traffic statistics, if the VPN is connected.
    async fn connection_info(&self, profile: &VpnProfile) -> Result<Option<ConnectionInfo>>;
}

/// Return the appropriate backend instance for the given profile type.
pub fn backend_for_profile(profile: &VpnProfile) -> Box<dyn VpnBackend> {
    match profile.vpn_type {
        VpnType::WireGuard => Box::new(wireguard::WireGuardBackend),
        VpnType::OpenVpn => Box::new(openvpn::OpenVpnBackend),
    }
}
