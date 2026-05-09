# Specification — Milestone B: Make It Secure

Feature ID: `milestone_b_security`
Phase: 1 — Research & Specification
Project: vex-vpn (Rust / GTK4 / libadwaita on NixOS, PIA WireGuard backend)
Toolchain pins: Rust 2021 edition, gtk4-rs 0.7.x, libadwaita 0.5.x, tokio 1.x, zbus 3.x, ksni 0.2

---

## 1. Executive Summary

Milestone B transforms vex-vpn from a systemd-service remote-control into a real PIA VPN client. The three pillars are:

1. **PIA client** (`src/pia.rs`): A fully functional async HTTP client using `reqwest` that authenticates against PIA, fetches the server list, registers WireGuard keys, and manages port forwarding.
2. **Security hardening**: Validate the WireGuard interface name in `Config` to prevent nft fragment injection. Narrow the sudoers `nft` rule. (The full polkit-gated helper binary is deferred to Milestone C.)
3. **Server/region selection**: Wire the PIA server list into the UI so users can browse, search, and select a server region before connecting.

---

## 2. Scope

### 2.1 Ships in this milestone

| Item | Category | Files affected |
|------|----------|---------------|
| Full `PiaClient` implementation | PIA integration (B5) | `src/pia.rs`, `Cargo.toml`, `flake.nix` |
| PIA CA cert embedding | PIA integration | `assets/ca.rsa.4096.crt`, `src/pia.rs` |
| Interface name validation | Security (B4) | `src/config.rs` |
| Narrowed sudoers nft rule | Security (B4) | `nix/module-gui.nix` |
| Login dialog: server-side validation | PIA integration | `src/ui_login.rs`, `src/main.rs` |
| Server picker UI page | UI (F2) | `src/ui.rs` |
| `AppState` extensions (server list, auth token, selected region) | State | `src/state.rs` |
| `Config` extensions (selected_region_id) | Config | `src/config.rs` |
| Server list caching | PIA integration | `src/pia.rs` |
| Region override for pia-vpn.service | NixOS integration | `nix/module-vpn.nix` |
| Credential delivery to systemd service | NixOS integration | `nix/module-vpn.nix`, `nix/module-gui.nix` |

### 2.2 Deferred to Milestone C

| Item | Reason |
|------|--------|
| Full `vex-vpn-helper` polkit binary | Separate Cargo target, dedicated D-Bus interface — too large for this milestone |
| Secret Service (`oo7`) credential storage | Requires helper binary for session-bus mediation on headless boxes |
| Port-forward bind loop in GUI | Requires connected VPN tunnel to test; will ship after connect flow is validated |
| Standalone (non-NixOS) WireGuard setup | Needs helper binary for privileged `ip link` / `wg` commands |
| Split tunneling | Future milestone |
| Onboarding wizard | Milestone C (F1) |

---

## 3. PIA Client Design (`src/pia.rs`)

### 3.1 API Endpoints (verified from pia-foss/manual-connections)

| Endpoint | Method | URL | Auth | TLS |
|----------|--------|-----|------|-----|
| Token | POST | `https://www.privateinternetaccess.com/api/client/v2/token` | Form fields `username` + `password` | System CA |
| Server list | GET | `https://serverlist.piaservers.net/vpninfo/servers/v6` | None | System CA |
| Add WireGuard key | GET | `https://{wg_hostname}:1337/addKey?pt={token}&pubkey={pubkey}` | Token in query | PIA CA, connect via IP |
| Get PF signature | GET | `https://{pf_hostname}:19999/getSignature?token={token}` | Token in query | PIA CA, connect via IP |
| Bind port | GET | `https://{pf_hostname}:19999/bindPort?payload={payload}&signature={sig}` | Payload+sig in query | PIA CA, connect via IP |

**Note on the token endpoint**: The PIA manual-connections repo uses `POST https://www.privateinternetaccess.com/api/client/v2/token` with form data. The tadfisher NixOS module uses `GET https://{meta_hostname}/authv3/generateToken` with HTTP Basic Auth via the PIA CA. Both work. We use the **public v2/token endpoint** (system CA, no PIA cert needed) as it does not require knowing a meta server IP before authentication.

**Note on `--connect-to` pattern**: PIA meta servers are addressed by IP but serve TLS certificates for their hostname (e.g., `server-name.privacy.network`). In curl this is `--connect-to "$hostname::$ip:"`. In reqwest, we use `ClientBuilder::resolve(hostname, SocketAddr)` to achieve the same effect — the TLS handshake validates the hostname against the PIA CA, while the TCP connection goes to the specified IP.

### 3.2 Data Types

```rust
use serde::Deserialize;
use std::net::IpAddr;
use std::time::{Duration, SystemTime};

/// A single server entry within a region's server group.
#[derive(Debug, Clone, Deserialize)]
pub struct ServerEntry {
    pub ip: String,
    pub cn: String,
}

/// Server groups within a region (WireGuard, OpenVPN, meta).
#[derive(Debug, Clone, Deserialize)]
pub struct ServerGroups {
    #[serde(default)]
    pub wg: Vec<ServerEntry>,
    #[serde(default)]
    pub meta: Vec<ServerEntry>,
    #[serde(default)]
    pub ovpntcp: Vec<ServerEntry>,
    #[serde(default)]
    pub ovpnudp: Vec<ServerEntry>,
}

/// A PIA region from the v6 server list.
#[derive(Debug, Clone, Deserialize)]
pub struct Region {
    pub id: String,
    pub name: String,
    pub country: String,
    pub geo: bool,
    pub port_forward: bool,
    pub servers: ServerGroups,
}

/// Parsed server list with fetch timestamp for cache management.
#[derive(Debug, Clone)]
pub struct ServerList {
    pub regions: Vec<Region>,
    pub fetched_at: SystemTime,
}

/// Response from the addKey endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct WgKeyResponse {
    pub status: String,
    pub server_key: String,
    pub server_port: u16,
    pub server_ip: String,
    pub server_vip: String,
    pub peer_ip: String,
    pub dns_servers: Vec<String>,
}

/// Response from the getSignature endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct PortForwardSignature {
    pub status: String,
    pub payload: String,
    pub signature: String,
}

/// Response from the bindPort endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct PortForwardBind {
    pub status: String,
    pub message: String,
}

/// PIA authentication token.
#[derive(Debug, Clone)]
pub struct AuthToken {
    pub token: String,
    pub obtained_at: SystemTime,
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
    NoWgServers(String),
    #[error("No meta servers available in region {0}")]
    NoMetaServers(String),
    #[error("{0}")]
    Other(String),
}
```

### 3.3 `PiaClient` Structure

```rust
pub struct PiaClient {
    /// Client for public PIA endpoints (token, server list).
    /// Uses system CA store.
    public_client: reqwest::Client,

    /// Client for PIA meta/WG servers (addKey, port-forward).
    /// Trusts ONLY the PIA RSA-4096 CA cert.
    /// Has `danger_accept_invalid_certs(false)` — the CA validates the certs.
    pia_client: reqwest::Client,
}
```

### 3.4 Methods

```rust
impl PiaClient {
    /// Build both reqwest clients. The PIA client embeds the CA cert from
    /// `assets/ca.rsa.4096.crt` via `include_bytes!`.
    pub fn new() -> Result<Self, PiaError>;

    /// Authenticate and obtain a token. Tokens are valid for 24 hours.
    /// POST https://www.privateinternetaccess.com/api/client/v2/token
    /// Returns the token string on success.
    pub async fn generate_token(
        &self,
        username: &str,
        password: &str,
    ) -> Result<AuthToken, PiaError>;

    /// Fetch the v6 server list. The response is JSON (the v6 endpoint
    /// appends a signature line after the JSON; we split on the first
    /// newline that follows '}' to extract the JSON body).
    /// GET https://serverlist.piaservers.net/vpninfo/servers/v6
    pub async fn fetch_server_list(&self) -> Result<ServerList, PiaError>;

    /// Register a WireGuard public key with a specific server.
    /// Uses `resolve()` to connect by IP while validating the hostname cert.
    /// GET https://{wg_cn}:1337/addKey?pt={token}&pubkey={pubkey}
    pub async fn add_key(
        &self,
        wg_ip: &str,
        wg_hostname: &str,
        token: &str,
        pubkey: &str,
    ) -> Result<WgKeyResponse, PiaError>;

    /// Get a port-forward signature from the connected server's gateway.
    /// GET https://{pf_hostname}:19999/getSignature?token={token}
    pub async fn get_port_forward_signature(
        &self,
        gateway_ip: &str,
        pf_hostname: &str,
        token: &str,
    ) -> Result<PortForwardSignature, PiaError>;

    /// Bind (activate) a forwarded port. Must be called every 15 minutes
    /// to keep the port alive.
    /// GET https://{pf_hostname}:19999/bindPort?payload={payload}&signature={sig}
    pub async fn bind_port(
        &self,
        gateway_ip: &str,
        pf_hostname: &str,
        payload: &str,
        signature: &str,
    ) -> Result<PortForwardBind, PiaError>;

    /// Measure TCP connect latency to a meta server on port 443.
    /// Returns None on timeout (2s) or connection failure.
    pub async fn measure_latency(ip: &str) -> Option<Duration>;
}
```

### 3.5 Implementation Notes

#### `reqwest` Client Construction

```rust
const PIA_CA_CERT: &[u8] = include_bytes!("../assets/ca.rsa.4096.crt");

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

    Ok(Self { public_client, pia_client })
}
```

Key decisions:
- `tls_built_in_root_certs(false)` + `add_root_certificate(pia_cert)` means the `pia_client` trusts ONLY the PIA CA. This prevents MITM even if a rogue cert is in the system store.
- `public_client` uses system CAs for `www.privateinternetaccess.com` and `serverlist.piaservers.net`.
- `timeout(15s)` prevents hangs on unreachable servers.
- `https_only(true)` prevents accidental HTTP downgrades.

#### `--connect-to` Equivalent

For endpoints that connect by IP but validate hostname TLS, we build a per-call client with `resolve()`:

```rust
async fn add_key(...) -> Result<WgKeyResponse, PiaError> {
    let addr: SocketAddr = format!("{}:1337", wg_ip).parse().map_err(|e| ...)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .tls_built_in_root_certs(false)
        .add_root_certificate(reqwest::Certificate::from_pem(PIA_CA_CERT)?)
        .resolve(wg_hostname, addr)
        .https_only(true)
        .build()?;

    let resp = client
        .get(format!("https://{}:1337/addKey", wg_hostname))
        .query(&[("pt", token), ("pubkey", pubkey)])
        .send()
        .await?;
    // ...
}
```

**Trade-off**: Building a new `Client` per `add_key` / `get_signature` / `bind_port` call is slightly less efficient than reusing a pool, but these calls happen at most once per connection (add_key) or once per 15 minutes (bind_port). The cost is negligible. This avoids having to manage a mutable resolver map on the shared client.

#### Server List Parsing (v6 format)

The v6 endpoint returns the JSON payload followed by a newline and a base64 RSA-SHA256 signature. We only need the JSON:

```rust
pub async fn fetch_server_list(&self) -> Result<ServerList, PiaError> {
    let body = self.public_client
        .get("https://serverlist.piaservers.net/vpninfo/servers/v6")
        .send()
        .await?
        .text()
        .await?;

    // The v6 response is: JSON\n<base64_signature>
    // Split at the first newline after the JSON closing brace.
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
```

**Note on signature verification**: The v6 endpoint signs the JSON with PIA's RSA-4096 key. Full signature verification (using the `rsa` + `sha2` crates) is deferred — the fetch is already over HTTPS to a PIA-controlled domain, so MITM is mitigated by TLS. Signature verification would protect against a compromised CDN or DNS redirect, which is a lower-probability threat for a desktop app. We document this as a future hardening item.

#### Token Never Logged, Never Persisted to Disk

- Tokens are held in `AppState.auth_token: Option<AuthToken>` (in memory only).
- `Debug` impl for `AuthToken` redacts the token value: `AuthToken { token: "***", obtained_at: ... }`.
- Tokens expire after 24 hours. `AuthToken::is_expired()` checks `obtained_at + 24h < now`.
- On app startup with saved credentials, auto-generate a fresh token.

### 3.6 Caching Strategy

- **Server list**: Cache to `~/.cache/vex-vpn/servers.json` after fetch. On cold start, load from cache if < 6 hours old. Background refresh every 6 hours via `poll_loop`.
- **Token**: In-memory only. Re-generated from stored credentials on startup and every 23 hours (1 hour before expiry).
- **Latency measurements**: Transient, recalculated every time the server list is refreshed.

### 3.7 Error Handling

All PIA API errors bubble up as `PiaError` variants. The caller in `state.rs` or `ui.rs` decides how to surface them:
- `AuthFailed` → show toast "Invalid username or password"
- `Http(timeout)` → show toast "Could not reach PIA servers"
- `ApiError` → show toast with the API error message
- `NoWgServers` → show toast "No WireGuard servers available in this region"

Never log the token, password, or full request URL (which contains the token as a query parameter).

---

## 4. CA Cert Handling

### 4.1 File Location

```
assets/
  ca.rsa.4096.crt    # PIA's RSA-4096 CA certificate (PEM format)
```

The cert is sourced from `https://github.com/pia-foss/manual-connections/blob/master/ca.rsa.4096.crt` (MIT-licensed).

### 4.2 Embedding

```rust
// In src/pia.rs
const PIA_CA_CERT: &[u8] = include_bytes!("../assets/ca.rsa.4096.crt");
```

This compiles the cert into the binary. No runtime file dependency.

### 4.3 flake.nix / Nix Build

The `craneLib.cleanCargoSource` filter already preserves non-Rust files in the source tree. We add `assets/` to the source filter explicitly if needed:

```nix
src = lib.cleanSourceWith {
  src = craneLib.path ./.;
  filter = path: type:
    (lib.hasSuffix ".crt" path) ||
    (craneLib.filterCargoSources path type);
};
```

### 4.4 NixOS Module CA Path

The existing `module-vpn.nix` takes `certificateFile` as an option. This CA file is the same one we embed. For the NixOS path, we set:

```nix
services.pia-vpn.certificateFile = "${cfg.package}/share/pia/ca.rsa.4096.crt";
```

And install the cert alongside the binary in `flake.nix`:

```nix
postInstall = ''
  mkdir -p $out/share/pia
  cp assets/ca.rsa.4096.crt $out/share/pia/
  # ... existing desktop entry ...
'';
```

---

## 5. Security Hardening

### 5.1 Interface Name Validation

**Problem**: `dbus::apply_kill_switch` interpolates `interface` into an nft ruleset template. While it uses `nft -f -` (stdin, no shell), a malicious interface name like `lo" accept\n}; flush ruleset; table inet x {` could inject arbitrary nft commands.

**Fix**: Add a validation function to `config.rs` and call it on `Config::load()` and before any nft invocation:

```rust
use std::sync::LazyLock;
use regex::Regex;

static INTERFACE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]{0,14}$").unwrap());

/// Validate that `name` is a legal Linux network interface name and
/// does not contain characters that could be interpreted by nft.
pub fn validate_interface(name: &str) -> bool {
    INTERFACE_RE.is_match(name)
}
```

Call sites:
1. `Config::load()` — if the loaded interface name fails validation, fall back to `"wg0"` and log a warning.
2. `dbus::apply_kill_switch()` — bail with an error if the interface name is invalid.
3. `AppState::new_with_config()` — validated transitively via `Config::load()`.

**Dependency note**: `regex` is already a transitive dependency of `tracing-subscriber`. We add it as a direct dependency for clarity. Alternatively, we can implement the check with manual char matching to avoid the dep — but `regex` is already in the tree.

Actually, to avoid adding `regex` as a direct dep, we'll implement this with a manual check:

```rust
pub fn validate_interface(name: &str) -> bool {
    if name.is_empty() || name.len() > 15 {
        return false;
    }
    let bytes = name.as_bytes();
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..].iter().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-'
    })
}
```

### 5.2 Narrowed Sudoers nft Rule

**Current** (in `nix/module-gui.nix`):
```nix
security.sudo.extraRules = [{
  groups = [ "wheel" ];
  commands = [{
    command = "${pkgs.nftables}/bin/nft";
    options = [ "NOPASSWD" ];
  }];
}];
```

This allows any `nft` subcommand — a compromised user process in `wheel` could `nft flush ruleset` and destroy all firewall rules.

**Fix**: Restrict to specific commands using a wrapper script approach. Replace the sudoers entry with two narrower rules:

```nix
security.sudo.extraRules = [
  {
    groups = [ "wheel" ];
    commands = [
      {
        # nft -f - : load ruleset from stdin (used by apply_kill_switch)
        command = "${pkgs.nftables}/bin/nft -f -";
        options = [ "NOPASSWD" ];
      }
      {
        # nft delete table inet pia_kill_switch
        command = "${pkgs.nftables}/bin/nft delete table inet pia_kill_switch";
        options = [ "NOPASSWD" ];
      }
    ];
  }
];
```

**Limitation**: The `nft -f -` rule still allows arbitrary rulesets via stdin. A full fix requires the helper binary (Milestone C). This intermediate step is documented as a partial mitigation — it blocks `nft flush ruleset` and `nft list` from being run without a password, but the stdin path is still powerful. Combined with interface validation (§5.1), the attack surface is significantly reduced.

### 5.3 Kill-switch nft Ruleset: Use Validated Interface

Update `dbus::apply_kill_switch()` to validate the interface before interpolating:

```rust
pub async fn apply_kill_switch(interface: &str) -> Result<()> {
    if !crate::config::validate_interface(interface) {
        anyhow::bail!("invalid interface name: {:?}", interface);
    }
    // ... existing nft template ...
}
```

### 5.4 Credential Security

Credentials continue to use the `credentials.toml` file (mode 0600) from Milestone A. Changes for this milestone:
- The login dialog now validates credentials against the PIA token endpoint before saving.
- Tokens are memory-only and never written to disk.
- Credentials are delivered to the systemd unit via a file in `/etc/vex-vpn/` (see §7).

---

## 6. Server Picker UI

### 6.1 Widget Tree

```
adw::ApplicationWindow
└── adw::ToolbarView
    ├── adw::HeaderBar (top bar)
    │   └── gtk::MenuButton (primary menu)
    └── gtk::Box (horizontal)
        ├── gtk::Box (sidebar — existing)
        └── adw::NavigationView (NEW — replaces the flat main page)
            ├── adw::NavigationPage "dashboard"
            │   └── (existing main page content)
            │       └── server_row (AdwActionRow) → on click → push "servers" page
            └── adw::NavigationPage "servers" (pushed on demand)
                └── gtk::Box (vertical)
                    ├── gtk::SearchEntry (filter by region name)
                    └── gtk::ScrolledWindow
                        └── gtk::ListBox
                            ├── AdwActionRow "US California" [32ms] [PF] ★
                            ├── AdwActionRow "US East" [45ms] [PF]
                            ├── AdwActionRow "UK London" [120ms]
                            └── ...
```

### 6.2 Navigation Flow

1. Dashboard is the initial page in the `NavigationView`.
2. Clicking the "Server" `AdwActionRow` pushes the "servers" page.
3. Each server row is an `AdwActionRow` with:
   - **Title**: region name (e.g., "US California")
   - **Subtitle**: latency (e.g., "32 ms") — measured lazily on page open
   - **Prefix**: country flag emoji or `network-server-symbolic` icon
   - **Suffix widgets**: port-forward badge (green `port-badge` label "PF"), favorite star toggle (optional, deferred)
4. Clicking a server row:
   - Sets `AppState.selected_region_id` = region.id
   - Sets `Config.selected_region_id` and persists
   - Pops back to the dashboard
   - Dashboard `server_row` subtitle updates to show the selected region name
5. The `SearchEntry` filters the list by region name (case-insensitive substring match).

### 6.3 Server Row Construction

For each `Region` in the server list, build an `AdwActionRow`:

```rust
fn build_server_row(region: &pia::Region, latency: Option<Duration>) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&region.name);
    row.set_activatable(true);

    // Latency subtitle
    let lat_str = match latency {
        Some(d) => format!("{} ms", d.as_millis()),
        None => "—".to_string(),
    };
    row.set_subtitle(&lat_str);

    // Port-forward badge
    if region.port_forward {
        let badge = gtk4::Label::new(Some("PF"));
        badge.add_css_class("port-badge");
        row.add_suffix(&badge);
    }

    // Geo badge (if it's a virtual/geolocated server)
    if region.geo {
        let geo = gtk4::Label::new(Some("geo"));
        geo.add_css_class("dim-label");
        row.add_suffix(&geo);
    }

    row
}
```

### 6.4 Latency Measurement

When the server page is opened, spawn async latency measurements for the top ~20 regions (sorted by name or previous latency). Update each row's subtitle as results arrive. Use `PiaClient::measure_latency()` which does a TCP connect to port 443 on the meta server IP.

```rust
// On server page open:
for region in &server_list.regions {
    if let Some(meta) = region.servers.meta.first() {
        let ip = meta.ip.clone();
        let row_ref = row.clone();
        glib::spawn_future_local(async move {
            if let Some(lat) = PiaClient::measure_latency(&ip).await {
                row_ref.set_subtitle(&format!("{} ms", lat.as_millis()));
            }
        });
    }
}
```

### 6.5 State Management

Server list data flows through `AppState`:

```
poll_loop (tokio) → fetches server list every 6h → writes to AppState.server_list
                                                  ↓
UI refresh timer (3s) → reads AppState → if server_list changed → rebuild server page
```

The server list is large (~100 regions), so we don't rebuild the ListBox every 3 seconds. Instead:
- `AppState` tracks `server_list_generation: u64` (incremented on each fetch).
- The UI caches the last-seen generation and only rebuilds when it changes.

---

## 7. Connect Flow Integration

### 7.1 Current Flow

```
User clicks Connect → ui.rs connect_btn handler
  → glib::spawn_future_local(async {
      dbus::connect_vpn().await  // starts pia-vpn.service via systemd D-Bus
    })

pia-vpn.service (systemd oneshot script):
  1. Reads PIA_USER/PIA_PASS from EnvironmentFile
  2. Fetches server list (v4), picks lowest-latency region
  3. Generates token via meta server /authv3/generateToken
  4. Calls addKey on the WG server
  5. Writes systemd-networkd .netdev + .network files
  6. Brings up the WireGuard interface
  7. Writes region.json, wireguard.json, token.json to /var/lib/pia-vpn/

poll_loop reads /var/lib/pia-vpn/*.json and updates AppState
```

### 7.2 New Flow (NixOS Module Path)

```
App startup:
  1. Load credentials from ~/.config/vex-vpn/credentials.toml
  2. If credentials present → generate_token() → store in AppState.auth_token
  3. fetch_server_list() → store in AppState.server_list
  4. Populate server picker UI

User selects a region:
  1. Store region_id in Config.selected_region_id → persist to config.toml
  2. Write region_id to /etc/vex-vpn/region.override (privileged write)
  3. Write credentials to /etc/vex-vpn/credentials.env (privileged write)

User clicks Connect:
  1. (Same as before) dbus::connect_vpn() → starts pia-vpn.service
  2. pia-vpn.service NOW reads /etc/vex-vpn/region.override to use the
     user-selected region instead of auto-picking lowest latency
  3. pia-vpn.service reads credentials from /etc/vex-vpn/credentials.env

poll_loop: (same as before) reads state files from /var/lib/pia-vpn/
```

### 7.3 Credential and Region Delivery (Privileged Write)

The GUI runs as an unprivileged user. The systemd service runs as root. We need a bridge.

**Approach for Milestone B**: Use `pkexec` to write the two small files. This is a stopgap until the helper binary ships in Milestone C.

```rust
// In src/pia.rs or a new src/privileged.rs module:
pub async fn write_credentials_env(username: &str, password: &str) -> Result<()> {
    let content = format!("PIA_USER={}\nPIA_PASS={}\n", username, password);
    write_privileged_file("/etc/vex-vpn/credentials.env", &content, "0640").await
}

pub async fn write_region_override(region_id: &str) -> Result<()> {
    write_privileged_file("/etc/vex-vpn/region.override", region_id, "0644").await
}

async fn write_privileged_file(path: &str, content: &str, mode: &str) -> Result<()> {
    // Use tee via pkexec to write the file
    let mut child = tokio::process::Command::new("pkexec")
        .args(["tee", path])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(content.as_bytes()).await?;
    }

    let output = child.wait_with_output().await?;
    if !output.status.success() {
        anyhow::bail!("pkexec write to {} failed", path);
    }
    Ok(())
}
```

**NixOS module changes** (`nix/module-vpn.nix`):

Add tmpfiles rule for the directory and update the service:

```nix
systemd.tmpfiles.rules = [
  "d /etc/vex-vpn 0755 root root -"
];

systemd.services.pia-vpn.serviceConfig.EnvironmentFile = [
  "-/etc/vex-vpn/credentials.env"   # leading '-' = optional
  cfg.environmentFile                # legacy path still honoured
];
```

Add region override logic to the service script (prepended to the existing script):

```bash
# Check for GUI-selected region override
if [ -s /etc/vex-vpn/region.override ]; then
  preferred="$(cat /etc/vex-vpn/region.override)"
  region_override="$(echo "$allregions" | jq --arg id "$preferred" -r \
      '.regions[] | select(.id==$id)')"
  if [ -n "$region_override" ]; then
    best="$preferred"
    region="$region_override"
  fi
fi
```

### 7.4 Login Dialog Enhancement

`ui_login.rs` currently accepts credentials and saves them locally. For Milestone B:

1. After the user clicks "Sign in", show a spinner on the button.
2. Call `pia_client.generate_token(username, password)` to validate.
3. On success:
   - Save credentials via `secrets::save()`
   - Write to `/etc/vex-vpn/credentials.env` via `write_credentials_env()`
   - Store token in `AppState.auth_token`
   - Fetch server list
   - Show success toast
   - Close dialog
4. On failure:
   - Show error message in the dialog (e.g., "Invalid credentials")
   - Don't save, don't close

### 7.5 Standalone (Non-NixOS) Path — DEFERRED

The standalone path where the GUI itself does WireGuard setup requires:
- Calling `add_key()` to get WireGuard config
- Creating a `.netdev` + `.network` file (or `wg-quick` config)
- Running `wg`/`ip` commands with elevated privileges

This requires the helper binary and is deferred to Milestone C. For now, `nix run` without the NixOS module will show the server list and allow login, but the Connect button will attempt to start `pia-vpn.service` which won't exist — the error is surfaced as a toast.

---

## 8. State Changes

### 8.1 `AppState` (src/state.rs)

New fields:

```rust
#[derive(Debug, Clone)]
pub struct AppState {
    // ... existing fields ...

    /// PIA authentication token (memory-only, never persisted).
    pub auth_token: Option<AuthToken>,

    /// Full PIA server list from the v6 API.
    pub server_list: Option<ServerList>,

    /// Generation counter for server list (incremented on each fetch).
    pub server_list_generation: u64,

    /// User-selected region ID (persisted via Config).
    pub selected_region_id: Option<String>,
}
```

### 8.2 `Config` (src/config.rs)

New fields:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    // ... existing fields ...

    /// User-selected PIA region ID (e.g., "us_california").
    #[serde(default)]
    pub selected_region_id: Option<String>,
}
```

The `#[serde(default)]` ensures backward compatibility with existing config files that don't have this field.

### 8.3 `RegionInfo` Alignment

The existing `RegionInfo` in `state.rs` is a subset of the PIA region data. After Milestone B, we keep `RegionInfo` for the *connected* region (populated from `/var/lib/pia-vpn/region.json` as before) and use `pia::Region` for the full server list. They coexist:

- `AppState.region: Option<RegionInfo>` — currently connected region (from systemd service state files)
- `AppState.server_list: Option<ServerList>` — all available regions (from PIA API)
- `AppState.selected_region_id: Option<String>` — user's choice for the next connection

---

## 9. Cargo.toml Changes

### 9.1 New Dependencies

```toml
# HTTP client for PIA API — rustls to avoid OpenSSL dependency
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "gzip"] }
```

### 9.2 Version Compatibility

| Existing | Version | Compatible with reqwest 0.12? |
|----------|---------|-------------------------------|
| tokio | 1.x | ✅ reqwest 0.12 uses tokio 1.x |
| serde | 1.x | ✅ |
| serde_json | 1.x | ✅ (already a transitive dep of reqwest) |
| base64 | 0.21 | ✅ (no conflict; reqwest uses base64 internally but doesn't re-export) |
| gtk4 | 0.7.x | ✅ (no interaction with reqwest) |
| zbus | 3.x | ✅ (separate async operation, both use tokio 1.x) |

### 9.3 Note on reqwest 0.11 vs 0.12

reqwest 0.12 is the current stable release. The main difference from 0.11:
- 0.12 uses `hyper` 1.x and `http` 1.x (0.11 used hyper 0.14, http 0.2)
- 0.12 uses `rustls` 0.23 (0.11 used 0.22)
- Both support `tokio` 1.x

We use 0.12 unless it causes a version conflict in the Nix build. The `rustls-tls` feature avoids needing OpenSSL at build or runtime. `pkgs.openssl` is already in `flake.nix` `buildInputs` but is not needed by reqwest with rustls.

### 9.4 Optional: Remove `thiserror` if Unused

The project analysis noted `thiserror` is imported but unused. We now use it for `PiaError`. Keep it.

### 9.5 flake.nix Changes

No new system library deps needed — `reqwest` with `rustls-tls` is pure Rust. The existing `buildInputs` suffice.

The `src` filter in `craneLib.cleanCargoSource` needs to include `.crt` files:

```nix
src = let
  certFilter = path: _type: builtins.match ".*\\.crt$" path != null;
  srcFilter = path: type:
    (certFilter path type) || (craneLib.filterCargoSources path type);
in lib.cleanSourceWith {
  src = craneLib.path ./.;
  filter = srcFilter;
};
```

---

## 10. Implementation Checklist (Ordered)

| # | File | Action | Description |
|---|------|--------|-------------|
| 1 | `assets/ca.rsa.4096.crt` | **Create** | Download PIA RSA-4096 CA cert from pia-foss/manual-connections |
| 2 | `Cargo.toml` | **Modify** | Add `reqwest = { version = "0.12", ... }`, add `mod pia` declaration |
| 3 | `src/pia.rs` | **Rewrite** | Full PIA client: types, PiaClient, all 6 methods, caching |
| 4 | `src/config.rs` | **Modify** | Add `selected_region_id`, `validate_interface()`, validation in `load()` |
| 5 | `src/state.rs` | **Modify** | Add `auth_token`, `server_list`, `server_list_generation`, `selected_region_id` to AppState; extend `poll_loop` to refresh server list every 6h and auto-refresh token |
| 6 | `src/main.rs` | **Modify** | Add `mod pia`; on startup with saved creds, generate token + fetch server list; pass PIA client to UI |
| 7 | `src/ui_login.rs` | **Modify** | Add async token validation on sign-in; show error on auth failure; write credentials.env |
| 8 | `src/ui.rs` | **Modify** | Wrap main page in `NavigationView`; make server_row activatable to push server page; build server list page with search, rows, latency; handle region selection |
| 9 | `src/dbus.rs` | **Modify** | Add `validate_interface()` call in `apply_kill_switch()` |
| 10 | `nix/module-gui.nix` | **Modify** | Narrow sudoers nft rule to `nft -f -` and `nft delete table inet pia_kill_switch` |
| 11 | `nix/module-vpn.nix` | **Modify** | Add tmpfiles for `/etc/vex-vpn/`, add optional EnvironmentFile, add region.override prologue |
| 12 | `flake.nix` | **Modify** | Update src filter to include `.crt` files; install CA cert to `$out/share/pia/` |

---

## 11. Testing Plan

### 11.1 Unit Tests

| Test | File | What |
|------|------|------|
| `validate_interface` valid names | `src/config.rs` | "wg0", "a", "wg-pia_01", 15-char names |
| `validate_interface` invalid names | `src/config.rs` | "", "WG0", "0abc", "a".repeat(16), "wg;rm", "wg\nrf" |
| `Config` round-trip with new fields | `src/config.rs` | Serialize/deserialize with `selected_region_id` |
| `Config` backward compat | `src/config.rs` | Load a TOML without `selected_region_id` → default `None` |
| `ServerList` JSON parsing | `src/pia.rs` | Parse a fixture JSON matching the v6 format |
| `WgKeyResponse` parsing | `src/pia.rs` | Parse a fixture of addKey response |
| `PortForwardSignature` parsing | `src/pia.rs` | Parse getSignature fixture |
| `AuthToken` expiry check | `src/pia.rs` | Token created 25h ago → `is_expired()` = true |
| `PiaError` formatting | `src/pia.rs` | Ensure error messages are user-readable |

### 11.2 Integration Tests (Deferred)

These require network access or mock servers and are not in the Milestone B scope:
- `wiremock`-based PIA endpoint simulation
- End-to-end login → server list → connect flow
- D-Bus mock for systemd service control

### 11.3 Manual Testing Protocol

1. Launch app with no credentials → login dialog appears
2. Enter invalid credentials → error message shown, dialog stays open
3. Enter valid credentials → dialog closes, server list populates
4. Click server row → navigate to server page
5. Search for "US" → list filters
6. Select a region → dashboard shows selected region
7. Click Connect → pia-vpn.service starts with the selected region
8. Verify `/etc/vex-vpn/region.override` contains the region ID
9. Toggle kill switch → verify nft table created with validated interface name
10. Check that no token appears in logs (`RUST_LOG=debug`)

---

## 12. Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| PIA changes API endpoints | Low | High | Endpoints have been stable since 2020. Version in URL. |
| reqwest 0.12 version conflict with existing deps | Low | Medium | Both use tokio 1.x. Test in nix develop. Fall back to 0.11. |
| `pkexec` popup UX friction for credential write | Medium | Medium | Necessary without helper binary. Document in README. Will be replaced by polkit helper in Milestone C. |
| PIA CA cert rotation | Very Low | High | Cert is RSA-4096, valid for years. Monitor PIA repo for changes. |
| Server list v6 format change | Low | Medium | We only parse `.regions[]` — additional top-level fields are ignored. |
| Race condition: UI reads server_list while poll_loop writes it | Low | Low | `RwLock` serializes access. UI clones the data. |
| Token expiry during active session | Medium | Low | Auto-refresh 1h before expiry. If refresh fails, show re-auth toast. |
| `nft -f -` sudoers rule still allows arbitrary rulesets | Medium | Medium | Combined with interface validation, risk is reduced. Full fix in Milestone C (helper binary). |

---

## 13. Deferred Items

| Item | Target Milestone | Notes |
|------|-----------------|-------|
| `vex-vpn-helper` polkit binary | C | Separate `[[bin]]` target, polkit action XML, D-Bus interface for nft + credential write |
| Secret Service (`oo7`) credential storage | C | Requires helper for headless fallback |
| Port-forward bind loop in GUI | C | After connect flow validated; 15-min keepalive timer |
| Standalone WireGuard setup (non-NixOS) | C-D | GUI calls `add_key` + `wg`/`ip` via helper |
| Server list signature verification | D | `rsa` + `sha2` crates to verify v6 payload signature |
| Split tunneling | E | cgroups + nft per-app routing |
| Onboarding wizard | C | `adw::Carousel` flow |
| Connection history | E | `~/.local/state/vex-vpn/history.jsonl` |
| Desktop notifications | C | `notify-rust` on connect/disconnect |

---

## 14. Research Sources

1. PIA Manual Connections — GitHub (`pia-foss/manual-connections`): Token endpoint (`get_token.sh`), server list v6 (`get_region.sh`), WireGuard addKey (`connect_to_wireguard_with_token.sh`), port forwarding (`port_forwarding.sh`), CA cert (`ca.rsa.4096.crt`)
2. reqwest documentation (Context7 `/seanmonstar/reqwest`): `Certificate::from_pem`, `tls_built_in_root_certs(false)`, `add_root_certificate`, `resolve()`, `tls_danger_accept_invalid_certs`, `https_only`
3. PIA API v2/token: `POST https://www.privateinternetaccess.com/api/client/v2/token` with form data
4. PIA API authv3: `GET https://{meta_cn}/authv3/generateToken` with HTTP Basic Auth (used by tadfisher module)
5. PIA server list v6: `GET https://serverlist.piaservers.net/vpninfo/servers/v6` — JSON + RSA signature
6. Linux network interface naming: kernel 15-char limit, `^[a-z][a-z0-9_-]*$` pattern
7. nftables security: `nft -f -` reads from stdin (no shell interpolation), but content injection is possible via malformed interface names
8. polkit reference (`freedesktop.org/software/polkit/docs/latest`): `pkexec` for one-shot privileged writes
9. OWASP ASVS v4 §2 (Authentication), §6 (Stored Cryptography): credential handling best practices
10. GTK4-rs 0.7 / libadwaita 0.5: `NavigationView`, `NavigationPage`, `SearchEntry` APIs
11. systemd EnvironmentFile: dash-prefixed paths (`-/path`) are optional (no failure if missing)
12. Existing vex-vpn codebase and `docs/PROJECT_ANALYSIS.md`
