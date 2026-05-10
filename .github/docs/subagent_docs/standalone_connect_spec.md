# Spec: Standalone Connect Mode (nix run without NixOS module)

**Feature name:** `standalone_connect`  
**Date:** 2026-05-10  
**Status:** DRAFT — Phase 1 Research & Specification  

---

## 1. Current State Analysis

### 1.1 What `pia-vpn.service` Does

`pia-vpn.service` is a systemd **oneshot + RemainAfterExit** service declared in
`nix/module-vpn.nix`. It is only created on NixOS systems where the user has
enabled `services.pia-vpn` in their NixOS configuration. The service:

1. Queries the PIA server list v4 API for latency-based region selection.
2. Authenticates against `https://<meta_hostname>/authv3/generateToken` to get
   a short-lived token.
3. Calls `https://<wg_hostname>:1337/addKey` with a freshly generated ephemeral
   WireGuard keypair to receive peer configuration (`server_key`, `server_port`,
   `server_ip`, `server_vip`, `peer_ip`, `dns_servers`).
4. Writes systemd-networkd `.netdev` and `.network` files to
   `/run/systemd/network/` (NOT a `wg-quick` config).
5. Brings up the WireGuard interface via `networkctl reload` + `networkctl up`.
6. Sets up routing policy rules via `ip route`.
7. Writes state files to `/var/lib/pia-vpn/` (`region.json`, `token.json`,
   `wireguard.json`) that the GUI's `poll_once` reads.

**Critical:** The service does NOT use `wg-quick`. It uses `systemd-networkd`.
When running `nix run` without the module, the service (and its state directory
`/var/lib/pia-vpn/`) do not exist.

### 1.2 Connect Flow (End-to-End)

```
ui.rs: connect_btn.connect_clicked
  └─ glib::spawn_future_local
       └─ crate::dbus::connect_vpn()          [src/dbus.rs:88]
            └─ start_unit("pia-vpn.service")   [src/dbus.rs:115]
                 └─ D-Bus: StartUnit("pia-vpn.service", "replace")
                      └─ ERROR: org.freedesktop.systemd1.NoSuchUnit
                           └─ ui.rs: toast notification "Connect failed: ..."
```

The error is caught and displayed as a non-modal toast (`adw::Toast`), not a
blocking dialog. The status pill shows `● ERROR`.

### 1.3 `add_key` in `src/pia.rs`

The `WgKeyResponse` struct IS fully defined (lines ~77–89):

```rust
pub struct WgKeyResponse {
    pub status: String,
    pub server_key: String,
    pub server_port: u16,
    pub server_ip: String,
    pub server_vip: String,
    pub peer_ip: String,
    pub dns_servers: Vec<String>,
}
```

`add_key` exists as a **stub** that always returns `Err(PiaError::Other("add_key not yet implemented"))`.  
The `pia_client` field (which trusts only the PIA RSA-4096 CA cert) exists on
`PiaClient` but is marked `#[allow(dead_code)]` — it was prepared for this use.

### 1.4 `vex-vpn-helper` Binary (`src/bin/helper.rs`)

Currently handles three operations via stdin/stdout JSON protocol:
- `enable_kill_switch { interface, allowed_interfaces }` — writes nftables ruleset
- `disable_kill_switch` — removes nftables ruleset
- `status` — checks kill switch state

Runs as root via `pkexec`. Does NOT handle WireGuard operations.

### 1.5 `AppState` and Config

- `AppState.interface`: defaults to `"wg0"` (from `Config.interface`).
- `AppState.auth_token`: in-memory PIA token obtained after login.
- `AppState.regions`: populated from PIA v6 API with `wg` server entries per
  region.
- `AppState.selected_region_id`: user-chosen region.
- `/var/lib/pia-vpn/` state directory: **does not exist** in `nix run` mode.

### 1.6 `poll_once` Status Detection (`src/state.rs:330+`)

Primary status source: `crate::dbus::get_service_status("pia-vpn.service")`.  
This calls `load_unit` → then reads `ActiveState` via D-Bus. When the unit
doesn't exist, `load_unit` itself fails with `NoSuchUnit`, and `poll_once`
silently returns `ConnectionStatus::Disconnected`.

Secondary data sources read from `/var/lib/pia-vpn/`:
- `region.json` → `AppState.region`
- `wireguard.json` → `AppState.connection.{server_ip, peer_ip}`

These files don't exist in standalone mode.

WireGuard transfer statistics (`rx_bytes`, `tx_bytes`) come from
`wg show <interface> transfer` subprocess — this works regardless of how the
interface was brought up, as long as the `wg` binary is in PATH.

---

## 2. Problem Definition

When a user runs `nix run github:victorytek/vex-vpn` (standalone, without the
NixOS module), the following preconditions hold:

- ✅ Authentication works: `PiaClient::generate_token()` returns a valid token.
- ✅ Server list loads: `PiaClient::fetch_server_list()` returns 165+ regions.
- ✅ Region selection works.
- ❌ `pia-vpn.service` does not exist → `StartUnit` fails with `NoSuchUnit`.
- ❌ `/var/lib/pia-vpn/` does not exist → state files unreadable.
- ❌ No WireGuard interface is brought up.

The app is fully functional up to the point of clicking Connect.

---

## 3. Option Analysis

### Option A — Graceful Error Dialog

**What:** Catch `NoSuchUnit` specifically; show an `adw::AlertDialog` with
instructions to install the NixOS module.

**Verdict:** ❌ Does NOT make `nix run` usable end-to-end. Improves UX slightly
but the user still cannot connect.

### Option B — Full Standalone Connect (Recommended)

**What:** Implement the complete PIA WireGuard connection flow natively in Rust
without requiring `pia-vpn.service`:

1. Detect missing unit via `NoSuchUnit` in connect path.
2. Generate ephemeral WireGuard keypair (subprocess `wg genkey`/`wg pubkey`).
3. Call PIA `addKey` API via `pia_client` with `resolve()` override.
4. Build `wg-quick` config string from response.
5. Write `/etc/wireguard/<interface>.conf` via `vex-vpn-helper` (root, pkexec).
6. Run `wg-quick up <interface>` via `vex-vpn-helper`.
7. Update `AppState` with connection info in memory.
8. Modify `poll_once` to check WireGuard interface status via `wg show` when
   `pia-vpn.service` is missing.

**Verdict:** ✅ Makes `nix run` fully usable. Leverages existing infrastructure
(`pia_client`, `vex-vpn-helper`, `wg_binary()`, `AppState`). No new Rust crates
needed.

### Option C — Write .service File + daemon-reload

**What:** Helper writes a `.service` file to
`~/.config/systemd/user/pia-vpn.service` and runs `systemctl --user
daemon-reload`, then `StartUnit` proceeds normally.

**Verdict:** ❌ Complex, fragile (user session D-Bus vs. system bus),
and `wg-quick` still needs root — doesn't actually simplify Option B.

### Option D — GetUnit Check + Graceful Fallback

**What:** Before `StartUnit`, call `GetUnit` to verify existence; show dialog if
missing.

**Verdict:** ❌ Same outcome as Option A. No actual connection in standalone mode.

---

## 4. Recommended Solution: Option B

### 4.1 Architecture

Introduce a **standalone connect path** parallel to the existing
`dbus::connect_vpn()` flow:

```
connect_btn click
  ├─ [pia-vpn.service exists]  → dbus::connect_vpn()        (existing path)
  └─ [NoSuchUnit error]        → state::standalone_connect() (new path)
       ├─ generate WireGuard keypair
       ├─ pia_client.add_key(wg_ip, wg_hostname, token, pubkey)
       ├─ build wg-quick config string
       ├─ helper::write_wireguard_config(interface, config)  (new op)
       └─ helper::wg_quick_up(interface)                     (new op)

Disconnect click
  ├─ [pia-vpn.service exists]    → dbus::disconnect_vpn()   (existing path)
  └─ [standalone_mode == true]   → helper::wg_quick_down()  (new op)

poll_once
  ├─ [standalone_mode == false]  → get_service_status() + read /var/lib/pia-vpn/
  └─ [standalone_mode == true]   → wg_show_is_up() + use AppState.connection
```

The `standalone_mode` flag is held in `AppState` (in-memory only, reset on
app restart).

### 4.2 PIA `addKey` API

```
URL:     https://<wg_hostname>:1337/addKey
Method:  GET
Params:  pt=<token> (URL-encoded), pubkey=<base64_public_key>
TLS:     PIA RSA-4096 CA cert only (already embedded as PIA_CA_CERT)
SNI:     wg_hostname (resolved to wg_ip via reqwest::ClientBuilder::resolve())
```

Response JSON:
```json
{
  "status": "OK",
  "server_key": "<base64 public key>",
  "server_port": 1337,
  "server_ip": "1.2.3.4",
  "server_vip": "10.x.x.x",
  "peer_ip": "10.x.x.x",
  "dns_servers": ["10.0.0.241", "10.0.0.242"]
}
```

### 4.3 WireGuard Config Format (`wg-quick` compatible)

```ini
[Interface]
Address = <peer_ip>/32
PrivateKey = <base64_private_key>
DNS = <dns_servers[0]>

[Peer]
PublicKey = <server_key>
AllowedIPs = 0.0.0.0/0
Endpoint = <server_ip>:<server_port>
PersistentKeepalive = 25
```

---

## 5. Implementation Steps

### Step 1: Implement `add_key` in `src/pia.rs`

Replace the stub at line ~269 with the actual implementation. Build a per-call
reqwest client using `resolve()` to map `wg_hostname → wg_ip:1337` while
keeping SNI as `wg_hostname` for TLS validation.

```rust
pub async fn add_key(
    &self,
    wg_ip: &str,
    wg_hostname: &str,
    token: &str,
    pubkey: &str,
) -> Result<WgKeyResponse, PiaError> {
    // Parse the WireGuard server address.
    let addr: std::net::SocketAddr = format!("{}:1337", wg_ip)
        .parse()
        .map_err(|e| PiaError::Other(format!("parse addr: {e}")))?;

    // Build a dedicated client for this hostname with pinned PIA CA cert.
    let pia_cert = reqwest::Certificate::from_pem(PIA_CA_CERT)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("vex-vpn")
        .tls_built_in_root_certs(false)
        .add_root_certificate(pia_cert)
        .resolve(wg_hostname, addr)
        .https_only(true)
        .build()?;

    let url = format!("https://{}:1337/addKey", wg_hostname);
    let resp = client
        .get(&url)
        .query(&[("pt", token), ("pubkey", pubkey)])
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(PiaError::ApiError(format!(
            "addKey returned {}",
            resp.status()
        )));
    }

    let body: WgKeyResponse = resp.json().await?;
    if body.status != "OK" {
        return Err(PiaError::ApiError(format!(
            "addKey status: {}",
            body.status
        )));
    }
    Ok(body)
}
```

**Note:** `reqwest::ClientBuilder::resolve(hostname, addr)` is available in
reqwest 0.12 without additional feature flags. No new Cargo.toml dependencies
are required.

Also remove the `#[allow(dead_code)]` from `WgKeyResponse` since it will now be
used.

### Step 2: Add New Operations to `src/bin/helper.rs`

Extend the `Command` enum:

```rust
WriteWireguardConfig {
    interface: String,
    config: String,
},
WgQuickUp {
    interface: String,
},
WgQuickDown {
    interface: String,
},
```

**Security validation** for `WriteWireguardConfig`:
- Validate `interface` using `is_valid_interface()` (already exists).
- Validate `config` does not contain path traversal or null bytes.
- Write to `/etc/wireguard/<interface>.conf` with mode `0600` owned by root.
- Use atomic write: write to `/etc/wireguard/<interface>.conf.tmp`, then rename.

**`wg-quick` binary resolution** (add to helper.rs):
```rust
fn wg_quick_binary() -> &'static str {
    if std::path::Path::new("/run/current-system/sw/bin/wg-quick").exists() {
        "/run/current-system/sw/bin/wg-quick"
    } else {
        "wg-quick"
    }
}
```

**`WriteWireguardConfig` handler** (inside `handle_command`):
```rust
Command::WriteWireguardConfig { interface, config } => {
    if !is_valid_interface(&interface) {
        return Response { ok: false, error: Some(format!("invalid interface: {:?}", interface)), active: None };
    }
    // Reject config strings containing null bytes or path separators.
    if config.contains('\0') {
        return Response { ok: false, error: Some("config contains null byte".into()), active: None };
    }
    let path = format!("/etc/wireguard/{}.conf", interface);
    let tmp_path = format!("/etc/wireguard/{}.conf.tmp", interface);
    match write_wireguard_config_sync(&tmp_path, &path, &config) {
        Ok(()) => Response { ok: true, error: None, active: None },
        Err(e) => Response { ok: false, error: Some(e), active: None },
    }
}
```

```rust
fn write_wireguard_config_sync(tmp_path: &str, final_path: &str, config: &str) -> Result<(), String> {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    // Ensure /etc/wireguard/ exists.
    std::fs::create_dir_all("/etc/wireguard/").map_err(|e| e.to_string())?;

    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(tmp_path)
        .map_err(|e| format!("open {tmp_path}: {e}"))?;
    f.write_all(config.as_bytes()).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    drop(f);

    std::fs::rename(tmp_path, final_path)
        .map_err(|e| format!("rename to {final_path}: {e}"))
}
```

**`WgQuickUp` handler:**
```rust
Command::WgQuickUp { interface } => {
    if !is_valid_interface(&interface) {
        return Response { ok: false, error: Some(format!("invalid interface: {:?}", interface)), active: None };
    }
    let output = std::process::Command::new(wg_quick_binary())
        .args(["up", &interface])
        .output();
    match output {
        Ok(o) if o.status.success() => Response { ok: true, error: None, active: None },
        Ok(o) => Response {
            ok: false,
            error: Some(String::from_utf8_lossy(&o.stderr).to_string()),
            active: None,
        },
        Err(e) => Response { ok: false, error: Some(e.to_string()), active: None },
    }
}
```

**`WgQuickDown` handler:** Same pattern as `WgQuickUp` but with `"down"` arg.

### Step 3: Add New Operations to `src/helper.rs`

Add three new public async functions that call `pkexec vex-vpn-helper` with
the new JSON commands:

```rust
/// Write a wg-quick configuration file to /etc/wireguard/<interface>.conf.
/// Requires root via pkexec. The file is written with mode 0600.
pub async fn write_wireguard_config(interface: &str, config: &str) -> Result<()> {
    if !crate::config::validate_interface(interface) {
        bail!("invalid interface name: {:?}", interface);
    }
    let resp = call_helper(&HelperRequest {
        op: "write_wireguard_config",
        interface: Some(interface),
        allowed_interfaces: None,
        // Need to add a config field to HelperRequest:
        // config: Some(config),
    }).await?;
    // ... check resp.ok
}

pub async fn wg_quick_up(interface: &str) -> Result<()> { ... }
pub async fn wg_quick_down(interface: &str) -> Result<()> { ... }
```

**Note:** `HelperRequest` needs a `config` field added:
```rust
#[derive(Serialize)]
struct HelperRequest<'a> {
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    interface: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_interfaces: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<&'a str>,   // NEW
}
```

And `Command` in `bin/helper.rs` needs matching deserialization:
```rust
WriteWireguardConfig {
    interface: String,
    config: String,
},
```

### Step 4: Add `standalone_mode` to `AppState` (`src/state.rs`)

Add fields to `AppState`:
```rust
/// True when connected via standalone mode (add_key + wg-quick),
/// not via pia-vpn.service.
pub standalone_mode: bool,
/// Private WireGuard key held in memory for reconnect. Cleared on disconnect.
/// Never persisted to disk.
pub standalone_privkey: Option<String>,
```

Initialize both to `false`/`None` in `AppState::new()`.

### Step 5: Add `standalone_connect` and helpers to `src/state.rs`

Add the following public async functions:

#### `generate_wg_keypair() -> Result<(String, String)>`

```rust
/// Generate an ephemeral WireGuard keypair using the `wg` binary.
/// Returns (private_key_base64, public_key_base64).
pub async fn generate_wg_keypair() -> Result<(String, String)> {
    // wg genkey
    let priv_out = tokio::process::Command::new(wg_binary())
        .arg("genkey")
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("wg genkey exec: {}", e))?;
    if !priv_out.status.success() {
        anyhow::bail!("wg genkey failed: {}", String::from_utf8_lossy(&priv_out.stderr));
    }
    let privkey = String::from_utf8(priv_out.stdout)
        .map_err(|e| anyhow::anyhow!("wg genkey output: {}", e))?
        .trim()
        .to_string();

    // wg pubkey (pipe privkey to stdin)
    let mut child = tokio::process::Command::new(wg_binary())
        .arg("pubkey")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("wg pubkey spawn: {}", e))?;

    {
        use tokio::io::AsyncWriteExt;
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("wg pubkey stdin"))?;
        stdin.write_all(format!("{}\n", privkey).as_bytes()).await?;
    }
    let output = child.wait_with_output().await?;
    if !output.status.success() {
        anyhow::bail!("wg pubkey failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    let pubkey = String::from_utf8(output.stdout)?
        .trim()
        .to_string();

    Ok((privkey, pubkey))
}
```

#### `build_wg_config(privkey, resp) -> String`

```rust
pub fn build_wg_config(privkey: &str, resp: &crate::pia::WgKeyResponse) -> String {
    let dns = resp.dns_servers.first().map(|s| s.as_str()).unwrap_or("10.0.0.241");
    format!(
        "[Interface]\nAddress = {peer_ip}/32\nPrivateKey = {privkey}\nDNS = {dns}\n\n\
         [Peer]\nPublicKey = {server_key}\nAllowedIPs = 0.0.0.0/0\n\
         Endpoint = {server_ip}:{server_port}\nPersistentKeepalive = 25\n",
        peer_ip = resp.peer_ip,
        privkey = privkey,
        dns = dns,
        server_key = resp.server_key,
        server_ip = resp.server_ip,
        server_port = resp.server_port,
    )
}
```

#### `standalone_connect(state) -> Result<()>`

```rust
pub async fn standalone_connect(state: Arc<RwLock<AppState>>) -> Result<()> {
    // 1. Read required state
    let (auth_token, region_id, interface, pia_regions) = {
        let s = state.read().await;
        let token = s.auth_token.clone()
            .ok_or_else(|| anyhow::anyhow!("not authenticated — please sign in first"))?;
        let rid = s.selected_region_id.clone()
            .ok_or_else(|| anyhow::anyhow!("no region selected"))?;
        (token, rid, s.interface.clone(), s.regions.clone())
    };

    // 2. Find WireGuard server for selected region
    let region = pia_regions.iter()
        .find(|r| r.id == region_id)
        .ok_or_else(|| anyhow::anyhow!("region {:?} not found in server list", region_id))?;
    let wg_server = region.servers.wg.first()
        .ok_or_else(|| anyhow::anyhow!("region {:?} has no WireGuard servers", region_id))?;

    // 3. Generate ephemeral WireGuard keypair
    let (privkey, pubkey) = generate_wg_keypair().await?;

    // 4. Call PIA addKey API
    let pia_client = crate::pia::PiaClient::new()
        .map_err(|e| anyhow::anyhow!("PiaClient::new: {}", e))?;
    let wg_resp = pia_client
        .add_key(&wg_server.ip, &wg_server.cn, &auth_token.token, &pubkey)
        .await
        .map_err(|e| anyhow::anyhow!("addKey: {}", e))?;

    // 5. Build wg-quick config
    let config_str = build_wg_config(&privkey, &wg_resp);

    // 6. Write config file via helper (root, pkexec)
    crate::helper::write_wireguard_config(&interface, &config_str).await?;

    // 7. Store in-memory connection state (before bringing interface up)
    {
        let mut s = state.write().await;
        s.standalone_mode = true;
        s.standalone_privkey = Some(privkey);
        s.status = ConnectionStatus::Connecting;
        s.connection = Some(ConnectionInfo {
            server_ip: wg_resp.server_ip.clone(),
            peer_ip: wg_resp.peer_ip.clone(),
            rx_bytes: 0,
            tx_bytes: 0,
        });
    }

    // 8. Bring up WireGuard interface via helper (root, pkexec)
    crate::helper::wg_quick_up(&interface).await?;

    Ok(())
}
```

#### `standalone_disconnect(interface) -> Result<()>`

```rust
pub async fn standalone_disconnect(interface: &str) -> Result<()> {
    crate::helper::wg_quick_down(interface).await
}
```

### Step 6: Modify `poll_once` in `src/state.rs`

Read `standalone_mode` and `interface` from `AppState` at the start of
`poll_once`, then branch the status-detection logic:

```rust
pub(crate) async fn poll_once(state: &Arc<RwLock<AppState>>) -> Result<()> {
    let (interface, standalone_mode) = {
        let s = state.read().await;
        (s.interface.clone(), s.standalone_mode)
    };

    let state_dir = "/var/lib/pia-vpn";

    if standalone_mode {
        // --- Standalone mode: check WireGuard interface directly ---
        let (wg_stats_raw, wg_handshake) = tokio::join!(
            read_wg_stats(&interface),
            read_wg_handshake(&interface),
        );

        let new_status = match wg_handshake {
            Some(elapsed) if elapsed > 0 && elapsed < 180 => ConnectionStatus::Connected,
            Some(elapsed) if elapsed >= 180 => ConnectionStatus::Stale(elapsed),
            // Interface is up but no handshake yet → Connecting
            None => {
                // Check if interface exists via wg show
                if wg_interface_is_up(&interface).await {
                    ConnectionStatus::Connecting
                } else {
                    ConnectionStatus::Disconnected
                }
            }
            _ => ConnectionStatus::Disconnected,
        };

        let (rx_bytes, tx_bytes) = wg_stats_raw.unwrap_or((0, 0));

        let mut s = state.write().await;
        s.status = new_status;
        // Update transfer stats but preserve server_ip/peer_ip set during connect
        if let Some(conn) = s.connection.as_mut() {
            conn.rx_bytes = rx_bytes;
            conn.tx_bytes = tx_bytes;
        }
        // Clear standalone_mode if interface went away (unexpected disconnect)
        if matches!(s.status, ConnectionStatus::Disconnected) {
            s.standalone_mode = false;
            s.standalone_privkey = None;
            s.connection = None;
        }

        debug!("State poll (standalone): {:?}", s.status);
        return Ok(());
    }

    // --- Normal mode (pia-vpn.service) --- (existing code below)
    // ... (unchanged from current implementation)
}
```

Add helper function `wg_interface_is_up`:

```rust
async fn wg_interface_is_up(interface: &str) -> bool {
    let out = tokio::process::Command::new(wg_binary())
        .args(["show", interface])
        .output()
        .await;
    matches!(out, Ok(o) if o.status.success())
}
```

### Step 7: Modify `src/ui.rs` — Connect Button Handler

In the `connect_btn.connect_clicked` closure, change the `_ =>` branch (the
connect case) to detect `NoSuchUnit` and redirect to standalone flow:

```rust
_ => {
    // Optimistic UI update
    pill.set_label("● CONNECTING...");
    set_state_class(&pill, "state-connecting");
    set_state_class(&btn, "state-connecting");
    lbl.set_label("CANCEL");
    icon.set_icon_name(Some("network-vpn-acquiring-symbolic"));

    // First attempt: try the NixOS pia-vpn.service (existing path)
    match crate::dbus::connect_vpn().await {
        Ok(()) => { /* unit started successfully */ }
        Err(e) if is_no_such_unit(&e) => {
            // Standalone mode: use add_key + wg-quick path
            tracing::info!("pia-vpn.service not found; attempting standalone connect");
            if let Err(e2) = crate::state::standalone_connect(state.clone()).await {
                tracing::error!("standalone connect: {:#}", e2);
                pill.set_label("● ERROR");
                set_state_class(&pill, "state-error");
                set_state_class(&btn, "state-disconnected");
                lbl.set_label("CONNECT");
                icon.set_icon_name(Some("network-vpn-symbolic"));
                toast.add_toast(adw::Toast::new(&format!("Connect failed: {e2:#}")));
            }
        }
        Err(e) => {
            tracing::error!("connect: {}", e);
            pill.set_label("● ERROR");
            set_state_class(&pill, "state-error");
            set_state_class(&btn, "state-disconnected");
            lbl.set_label("CONNECT");
            icon.set_icon_name(Some("network-vpn-symbolic"));
            toast.add_toast(adw::Toast::new(&format!("Connect failed: {e:#}")));
        }
    }
}
```

Add helper function (module scope or inline):
```rust
fn is_no_such_unit(e: &anyhow::Error) -> bool {
    let msg = e.to_string();
    msg.contains("NoSuchUnit") || msg.contains("No such unit")
}
```

For **disconnect** in standalone mode, in the `ConnectionStatus::Connected |
ConnectionStatus::KillSwitchActive` branch:

```rust
// Check if standalone mode
let standalone = state.read().await.standalone_mode;
let interface = {
    crate::config::Config::load()
        .unwrap_or_default()
        .interface
};

if standalone {
    if let Err(e) = crate::state::standalone_disconnect(&interface).await {
        tracing::error!("standalone disconnect: {}", e);
        // show toast
    }
    // Clear standalone state
    let mut s = state.write().await;
    s.standalone_mode = false;
    s.standalone_privkey = None;
    s.connection = None;
    s.status = ConnectionStatus::Disconnected;
} else {
    if let Err(e) = crate::dbus::disconnect_vpn().await {
        // existing error handling
    }
}
```

---

## 6. Files to Modify

| File | Change | Priority |
|------|--------|----------|
| `src/pia.rs` | Implement `add_key`; remove `#[allow(dead_code)]` from `WgKeyResponse` | Critical |
| `src/bin/helper.rs` | Add `Command` variants: `WriteWireguardConfig`, `WgQuickUp`, `WgQuickDown`; add `wg_quick_binary()`; add handlers | Critical |
| `src/helper.rs` | Add `HelperRequest.config` field; add `write_wireguard_config()`, `wg_quick_up()`, `wg_quick_down()` | Critical |
| `src/state.rs` | Add `standalone_mode`, `standalone_privkey` to `AppState`; add `generate_wg_keypair()`, `build_wg_config()`, `standalone_connect()`, `standalone_disconnect()`, `wg_interface_is_up()`; modify `poll_once()` | Critical |
| `src/ui.rs` | Add `is_no_such_unit()` helper; modify connect/disconnect button handlers to detect standalone mode | Critical |

**No changes needed to:**
- `src/dbus.rs` — `connect_vpn()` returning `Err` on `NoSuchUnit` is correct behavior; the UI layer catches it
- `src/config.rs` — no new config needed
- `Cargo.toml` — no new dependencies (`reqwest::ClientBuilder::resolve()` is in 0.12 without extra features)
- `flake.nix` / `module.nix` / NixOS modules — no changes (standalone mode is a runtime feature)

---

## 7. Dependencies

### New Rust Crates
**None required.**

- `reqwest 0.12` — already in `Cargo.toml`; `ClientBuilder::resolve()` is available.
- `tokio` — already in `Cargo.toml` with `full` features.
- WireGuard key generation via `wg genkey`/`wg pubkey` subprocess (same pattern as existing `wg show` calls in `state.rs`).
- `wg-quick` via helper subprocess.

### Runtime Requirements (for `nix run` users)
- `wireguard-tools` (`wg` and `wg-quick` binaries) must be on the system.
- WireGuard kernel module must be loaded (`modprobe wireguard`).
- `/etc/wireguard/` must be writable by root (standard — created by helper).
- `pkexec` (polkit) must be available — already required for kill switch.

### Polkit Policy
No changes to `nix/polkit-vex-vpn.policy` needed. The helper already runs
as root via pkexec. New operations are handled within the existing helper
process.

**However:** On a `nix run` system without the NixOS module, the polkit policy
for `vex-vpn-helper` may not be installed. The implementation subagent should
note this and log a clear error if `pkexec` fails with "Not authorized".

---

## 8. Security Considerations

### `WriteWireguardConfig` security
- Interface name validated via `is_valid_interface()` before path construction.
- Config content validated: reject null bytes.
- Output file created with mode `0600` owned by root.
- Atomic write (tmp + rename) prevents partial writes.
- No shell expansion — `config` is written as-is to file, never passed to shell.

### Private Key Security
- `standalone_privkey` is stored in `AppState` (in-memory only).
- The key is written to `/etc/wireguard/<interface>.conf` (root-owned, 0600).
- On disconnect, `standalone_privkey` is cleared from `AppState`.
- Keys are ephemeral — new keypair generated on each connect (matching PIA's
  manual-connections behavior).
- Never persisted to `~/.config/vex-vpn/` or any user-readable location.

### Token Security
- PIA token is already in `AppState.auth_token` (memory-only, per existing design).
- Token is passed to `add_key` over HTTPS with pinned PIA CA cert.
- Token is NOT written to any file during standalone connect.

---

## 9. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `wg-quick` not available in `nix run` PATH | Medium | Check `/run/current-system/sw/bin/wg-quick` first (NixOS); show clear error if missing |
| WireGuard kernel module not loaded | Low | `wg-quick` will fail with clear message; propagate to UI |
| `/etc/wireguard/` not writable (SELinux etc.) | Low | Helper runs as root; show `pkexec` failure clearly |
| `add_key` fails (bad token, server down) | Medium | Return `PiaError` from `add_key`; propagated to UI toast |
| `poll_once` not detecting standalone status | Medium | Covered by `wg_interface_is_up()` + handshake check |
| Stale WireGuard config on reconnect | Low | `wg-quick up` fails if interface already up; call `wg-quick down` first in standalone reconnect |
| polkit policy missing on `nix run` system | Medium | Show clear error: "vex-vpn-helper not authorized — see README for standalone setup" |
| `pia_client.add_key` fails due to resolve() | Low | reqwest 0.12 `resolve()` is stable; test with `wg show` status |

---

## 10. Test Plan

### Unit Tests
- `build_wg_config()` — verify correct `wg-quick` config format.
- `is_no_such_unit()` — verify pattern matching on various error strings.

### Integration Tests
- `generate_wg_keypair()` — requires `wg` binary; skip in CI without `wireguard-tools`.
- `write_wireguard_config` helper op — test via helper binary directly (requires root).

### Manual Tests (NixOS `nix run`)
1. `nix run github:victorytek/vex-vpn` without module installed.
2. Sign in with PIA credentials.
3. Select a region.
4. Click Connect → polkit prompt → VPN connects.
5. Verify status shows Connected and transfer stats update.
6. Click Disconnect → VPN disconnects.
7. Verify app restarts cleanly (no stale `standalone_mode` state).

---

## 11. Out of Scope

- Port forwarding in standalone mode (requires `pia-vpn-portforward.service`).
- DNS-over-HTTPS configuration in standalone mode.
- IPv6 disabling (not required; standalone mode uses `AllowedIPs = 0.0.0.0/0`).
- Auto-reconnect on network change in standalone mode (watchdog still functional
  via `wg show` status check in `poll_once`).
- `systemd-networkd` integration (out of scope; `wg-quick` is the appropriate
  tool for `nix run` standalone use).

---

*Spec produced by Research Subagent — Phase 1 complete.*  
*Spec path: `.github/docs/subagent_docs/standalone_connect_spec.md`*
