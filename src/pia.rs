//! PIA (Private Internet Access) API client.
//!
//! Provides an async HTTP client for PIA authentication, server list retrieval,
//! and WireGuard key registration. Uses two `reqwest::Client` instances:
//! - `public_client`: system CA store for public endpoints
//! - `pia_client`: trusts ONLY the PIA RSA-4096 CA cert for meta/WG server endpoints

use serde::Deserialize;
use std::time::{Duration, Instant, SystemTime};

// ---------------------------------------------------------------------------
// PIA CA certificate — compiled into the binary
// ---------------------------------------------------------------------------

const PIA_CA_CERT: &[u8] = include_bytes!("../assets/ca.rsa.4096.crt");

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A single server entry within a region's server group.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerEntry {
    pub ip: String,
    #[allow(dead_code)]
    pub cn: String,
}

/// Server groups within a region (WireGuard, OpenVPN, meta).
#[derive(Debug, Clone, Deserialize)]
pub struct ServerGroups {
    #[serde(default)]
    #[allow(dead_code)]
    pub wg: Vec<ServerEntry>,
    #[serde(default)]
    pub meta: Vec<ServerEntry>,
    #[serde(default)]
    #[allow(dead_code)]
    pub ovpntcp: Vec<ServerEntry>,
    #[serde(default)]
    #[allow(dead_code)]
    pub ovpnudp: Vec<ServerEntry>,
}

/// A PIA region from the v6 server list.
#[derive(Debug, Clone, Deserialize)]
pub struct Region {
    pub id: String,
    pub name: String,
    #[allow(dead_code)]
    pub country: String,
    pub geo: bool,
    pub port_forward: bool,
    pub servers: ServerGroups,
}

/// Parsed server list with fetch timestamp for cache management.
#[derive(Debug, Clone)]
pub struct ServerList {
    pub regions: Vec<Region>,
    #[allow(dead_code)]
    pub fetched_at: SystemTime,
}

/// Response from the addKey endpoint.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct WgKeyResponse {
    pub status: String,
    pub server_key: String,
    pub server_port: u16,
    pub server_ip: String,
    #[allow(dead_code)]
    pub server_vip: String,
    pub peer_ip: String,
    pub dns_servers: Vec<String>,
}

/// Response from the getSignature endpoint.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PortForwardSignature {
    pub status: String,
    pub payload: String,
    pub signature: String,
}

/// Response from the bindPort endpoint.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PortForwardBind {
    pub status: String,
    pub message: String,
}

/// PIA authentication token (memory-only, never persisted to disk).
#[derive(Clone)]
pub struct AuthToken {
    #[allow(dead_code)]
    pub token: String,
    pub obtained_at: SystemTime,
}

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthToken")
            .field("token", &"***")
            .field("obtained_at", &self.obtained_at)
            .finish()
    }
}

#[allow(dead_code)]
impl AuthToken {
    /// Tokens are valid for 24 hours.
    pub fn is_expired(&self) -> bool {
        match self.obtained_at.elapsed() {
            Ok(elapsed) => elapsed > Duration::from_secs(24 * 3600),
            Err(_) => true,
        }
    }
}

/// Custom error type for PIA API operations.
#[derive(Debug, thiserror::Error)]
pub enum PiaError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("PIA API returned error status: {0}")]
    ApiError(String),
    #[error("Authentication failed — check username/password")]
    AuthFailed,
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("No WireGuard servers available in region {0}")]
    #[allow(dead_code)]
    NoWgServers(String),
    #[error("No meta servers available in region {0}")]
    #[allow(dead_code)]
    NoMetaServers(String),
    #[error("{0}")]
    #[allow(dead_code)]
    Other(String),
}

// ---------------------------------------------------------------------------
// PIA Client
// ---------------------------------------------------------------------------

/// Async HTTP client for PIA API endpoints.
pub struct PiaClient {
    /// Client for public PIA endpoints (token, server list).
    /// Uses system CA store.
    public_client: reqwest::Client,

    /// Client for PIA meta/WG servers (addKey, port-forward).
    /// Trusts ONLY the PIA RSA-4096 CA cert.
    #[allow(dead_code)]
    pia_client: reqwest::Client,
}

impl PiaClient {
    /// Build both reqwest clients. The PIA client embeds the CA cert from
    /// `assets/ca.rsa.4096.crt` via `include_bytes!`.
    pub fn new() -> Result<Self, PiaError> {
        let public_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("vex-vpn")
            .https_only(true)
            .build()?;

        let pia_cert = reqwest::Certificate::from_pem(PIA_CA_CERT)?;
        let pia_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent("vex-vpn")
            .tls_built_in_root_certs(false)
            .add_root_certificate(pia_cert)
            .https_only(true)
            .build()?;

        Ok(Self {
            public_client,
            pia_client,
        })
    }

    /// Authenticate and obtain a token. Tokens are valid for 24 hours.
    /// POST <https://www.privateinternetaccess.com/api/client/v2/token>
    pub async fn generate_token(
        &self,
        username: &str,
        password: &str,
    ) -> Result<AuthToken, PiaError> {
        let resp = self
            .public_client
            .post("https://www.privateinternetaccess.com/api/client/v2/token")
            .form(&[("username", username), ("password", password)])
            .send()
            .await?;

        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(PiaError::AuthFailed);
        }

        if !resp.status().is_success() {
            return Err(PiaError::ApiError(format!(
                "token endpoint returned {}",
                resp.status()
            )));
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            token: String,
        }

        let body = resp.json::<TokenResponse>().await?;
        Ok(AuthToken {
            token: body.token,
            obtained_at: SystemTime::now(),
        })
    }

    /// Fetch the v6 server list. The response body is JSON followed by a
    /// newline and a base64 RSA-SHA256 signature — we only parse the JSON.
    /// GET <https://serverlist.piaservers.net/vpninfo/servers/v6>
    pub async fn fetch_server_list(&self) -> Result<ServerList, PiaError> {
        let body = self
            .public_client
            .get("https://serverlist.piaservers.net/vpninfo/servers/v6")
            .send()
            .await?
            .text()
            .await?;

        // The v6 response is: JSON\n<base64_signature>
        let json_str = body
            .split_once('\n')
            .map(|(json, _sig)| json)
            .unwrap_or(&body);

        #[derive(Deserialize)]
        struct ServerListJson {
            regions: Vec<Region>,
        }

        let parsed: ServerListJson = serde_json::from_str(json_str)?;
        Ok(ServerList {
            regions: parsed.regions,
            fetched_at: SystemTime::now(),
        })
    }

    /// Register a WireGuard public key with a specific server.
    /// Deferred to a future milestone.
    #[allow(dead_code)]
    pub async fn add_key(
        &self,
        _wg_hostname: &str,
        _wg_ip: &str,
        _token: &str,
        _pubkey: &str,
    ) -> Result<WgKeyResponse, PiaError> {
        Err(PiaError::Other("add_key not yet implemented".into()))
    }

    /// Get a port-forward signature from the connected server's gateway.
    /// Deferred to a future milestone.
    #[allow(dead_code)]
    pub async fn get_port_forward_signature(
        &self,
        _gateway_ip: &str,
        _pf_hostname: &str,
        _token: &str,
    ) -> Result<PortForwardSignature, PiaError> {
        Err(PiaError::Other(
            "get_port_forward_signature not yet implemented".to_string(),
        ))
    }

    /// Bind (activate) a forwarded port.
    /// Deferred to a future milestone.
    #[allow(dead_code)]
    pub async fn bind_port(
        &self,
        _gateway_ip: &str,
        _pf_hostname: &str,
        _payload: &str,
        _signature: &str,
    ) -> Result<PortForwardBind, PiaError> {
        Err(PiaError::Other("bind_port not yet implemented".to_string()))
    }

    /// Measure TCP connect latency to a server on port 443.
    /// Returns `None` on timeout (2s) or connection failure.
    pub async fn measure_latency(ip: &str) -> Option<Duration> {
        let addr = format!("{}:443", ip);
        let start = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_millis(2000),
            tokio::net::TcpStream::connect(addr.as_str()),
        )
        .await;
        match result {
            Ok(Ok(_)) => Some(start.elapsed()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_region_deserialize() {
        let json = r#"{
            "id": "us_california",
            "name": "US California",
            "country": "US",
            "geo": false,
            "port_forward": true,
            "servers": {
                "wg": [{"ip": "1.2.3.4", "cn": "wg-ca.example.com"}],
                "meta": [{"ip": "5.6.7.8", "cn": "meta-ca.example.com"}],
                "ovpntcp": [],
                "ovpnudp": []
            }
        }"#;
        let region: Region = serde_json::from_str(json).expect("parse region");
        assert_eq!(region.id, "us_california");
        assert_eq!(region.name, "US California");
        assert!(region.port_forward);
        assert!(!region.geo);
        assert_eq!(region.servers.wg.len(), 1);
        assert_eq!(region.servers.wg[0].ip, "1.2.3.4");
    }

    #[test]
    fn test_server_list_json_parse() {
        let json = r#"{"regions":[{"id":"us","name":"US","country":"US","geo":false,"port_forward":true,"servers":{"wg":[],"meta":[]}}]}"#;
        // Simulate the v6 format: JSON + newline + signature
        let body = format!("{}\nSOME_BASE64_SIG", json);
        let json_str = body.split_once('\n').map(|(j, _)| j).unwrap_or(&body);

        #[derive(Deserialize)]
        struct ServerListJson {
            regions: Vec<Region>,
        }
        let parsed: ServerListJson = serde_json::from_str(json_str).expect("parse");
        assert_eq!(parsed.regions.len(), 1);
        assert_eq!(parsed.regions[0].id, "us");
    }

    #[test]
    fn test_auth_token_expiry() {
        let fresh = AuthToken {
            token: "test".to_string(),
            obtained_at: SystemTime::now(),
        };
        assert!(!fresh.is_expired());

        let expired = AuthToken {
            token: "test".to_string(),
            obtained_at: SystemTime::now() - Duration::from_secs(25 * 3600),
        };
        assert!(expired.is_expired());
    }

    #[test]
    fn test_auth_token_debug_redacts() {
        let token = AuthToken {
            token: "super-secret".to_string(),
            obtained_at: SystemTime::now(),
        };
        let debug = format!("{:?}", token);
        assert!(!debug.contains("super-secret"));
        assert!(debug.contains("***"));
    }

    #[test]
    fn test_pia_error_display() {
        let err = PiaError::AuthFailed;
        assert_eq!(
            err.to_string(),
            "Authentication failed \u{2014} check username/password"
        );

        let err = PiaError::NoWgServers("us_east".to_string());
        assert!(err.to_string().contains("us_east"));
    }
}
