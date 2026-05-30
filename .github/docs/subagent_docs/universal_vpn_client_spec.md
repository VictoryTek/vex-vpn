# Universal VPN Client — Transformation Specification

**Feature:** `universal_vpn_client`  
**Date:** 2026-05-29  
**Status:** DRAFT — ready for implementation  

---

## 1. Executive Summary

vex-vpn is currently a tightly-coupled GUI frontend for a single provider (Private Internet Access) using a proprietary bash/API backend baked into the helper binary.  This specification defines how to transform it into a **universal VPN client** that:

- Imports and manages arbitrary **WireGuard** (`.conf`) and **OpenVPN** (`.ovpn`) configuration files.
- Controls WireGuard tunnels through the existing **systemd D-Bus API** (`wg-quick@<iface>.service`) — no shell-script backend required.
- Controls OpenVPN tunnels through the **NetworkManager D-Bus API** (`org.freedesktop.NetworkManager`).
- Exposes a **NixOS module** (`services.vex-vpn.profiles`) for fully declarative profile pre-configuration from a flake.
- Retains the full GTK4/libadwaita UI, ksni tray, Tokio runtime, and TOML config infrastructure.

The transformation removes ~1 200 lines of PIA-specific code and adds a clean, provider-agnostic architecture in their place.

---

## 2. Current State Analysis

### 2.1 Module Inventory

| File | Lines | Purpose | Keep/Change |
|------|-------|---------|-------------|
| `src/main.rs` | ~130 | Entry point, Tokio rt, watcher spawns | **Modify** |
| `src/lib.rs` | 5 | Library root for integration tests | **Keep** |
| `src/config.rs` | ~180 | TOML config, `Config` struct, `validate_interface` | **Modify** |
| `src/dbus.rs` | ~200 | zbus 3.x proxies: systemd + NetworkManager | **Modify** |
| `src/state.rs` | ~400 | `AppState`, `ConnectionStatus`, `poll_loop`, watchers | **Modify** |
| `src/tray.rs` | ~130 | ksni system tray | **Modify** |
| `src/ui.rs` | ~800+ | Main GTK4/adw dashboard | **Modify** |
| `src/ui_login.rs` | ~150 | PIA login dialog | **Delete** |
| `src/ui_onboarding.rs` | ~350 | PIA 5-page onboarding wizard | **Delete** |
| `src/ui_prefs.rs` | ~200 | PIA-centric preferences | **Rewrite** |
| `src/pia.rs` | ~300 | PIA HTTP API client | **Delete** |
| `src/secrets.rs` | ~130 | PIA credentials store | **Delete** |
| `src/history.rs` | ~130 | JSONL connection history | **Modify** |
| `src/helper.rs` | ~150 | Kill switch caller (pkexec) | **Modify** |
| `src/bin/helper.rs` | ~300 | Root helper: nft + PIA shell script install | **Modify** |
| `build.rs` | 7 | GResource compilation | **Keep** |
| `Cargo.toml` | ~50 | Manifest | **Modify** |
| `flake.nix` | ~150 | Nix Flake, Crane build | **Modify** |
| `nix/module-gui.nix` | ~100 | GUI NixOS module | **Rewrite** |
| `nix/module-vpn.nix` | ~200 | PIA/WG systemd backend (vendored from tadfisher) | **Archive** |
| `module.nix` | 8 | Redirect stub | **Modify** |
| `tests/config_integration.rs` | ~80 | Config roundtrip tests | **Modify** |

### 2.2 What Currently Exists — Key Observations

**PIA coupling is deep:**
- `src/pia.rs` contains an HTTP client that authenticates to `privateinternetaccess.com`, fetches the v6 server list, registers WireGuard keys, and manages port forwarding.
- `src/bin/helper.rs` embeds the complete PIA connection script (bash, ~300 lines) and writes it to `/var/lib/vex-vpn/pia-connect.sh` at install time.
- `src/state.rs:poll_once()` reads PIA-specific JSON files from `/var/lib/pia-vpn/` (region.json, wireguard.json, portforward.json) written by the embedded script.
- `AppState` contains `auth_token: Option<pia::AuthToken>`, `regions: Vec<pia::Region>`, PIA-specific `RegionInfo`.
- `src/ui.rs` displays PIA region name, meta IP, and PIA port forwarding toggle.
- `nix/module-vpn.nix` is a vendored copy of tadfisher's `pia-vpn.nix` — entirely PIA-specific.

**Infrastructure worth keeping:**
- `config.rs` — solid TOML persistence with atomic writes, `validate_interface()`, XDG dirs.
- `dbus.rs` — clean zbus 3.x proxies for systemd1 `StartUnit`/`StopUnit`, `ActiveState`; NetworkManager `StateChanged` signal.
- `state.rs` — `ConnectionStatus` enum, broadcast channel, `watch_network_manager`, WireGuard handshake staleness logic.
- `tray.rs` — ksni integration is sound; menu items call `crate::dbus::connect_vpn/disconnect_vpn`.
- `history.rs` — provider-agnostic JSONL log (`region` field can become `profile_name`).
- `helper.rs` — `pkexec` call pattern for privileged nftables operations.

---

## 3. Target State Architecture

### 3.1 Source Module Map (post-transformation)

```
src/
├── main.rs              ← Startup, Tokio rt, watcher spawns (modified)
├── lib.rs               ← Library root (add profile, parser to pub re-exports)
├── config.rs            ← Config struct redesigned for profiles
├── profile.rs           ← NEW: VpnProfile, VpnType, profile I/O
├── parser/
│   ├── mod.rs           ← NEW: parse_wireguard(), parse_openvpn()
│   ├── wireguard.rs     ← NEW: WireGuard INI (.conf) parser
│   └── openvpn.rs       ← NEW: OpenVPN .ovpn parser (header extraction)
├── backend/
│   ├── mod.rs           ← NEW: VpnBackend trait
│   ├── wireguard.rs     ← NEW: wg-quick systemd backend
│   └── openvpn.rs       ← NEW: NetworkManager D-Bus backend
├── dbus.rs              ← Extend: add NM VPN D-Bus proxies
├── state.rs             ← Restructure: remove PIA fields; add active_profile_id
├── tray.rs              ← Minor: rename "Open PIA" → "Open vex-vpn"
├── ui.rs                ← Redesign: profile list replaces region/server UI
├── ui_profiles.rs       ← NEW: profile list & management UI
├── ui_import.rs         ← NEW: file import dialog
├── ui_prefs.rs          ← Rewrite: profile settings (no more PIA-only options)
├── history.rs           ← Modify: rename `region` field → `profile_name`
├── helper.rs            ← Modify: remove PIA install ops; keep kill switch
└── bin/
    └── helper.rs        ← Modify: remove PIA script embed; keep nft ops
```

### 3.2 Threading Model (unchanged)

```
┌──────────────────────────────────────────────────┐
│  GTK4 main thread                                │
│  ui.rs, ui_profiles.rs, ui_import.rs             │
│  ui_prefs.rs                                     │
│  glib::spawn_future_local → Tokio futures        │
└──────────────────┬───────────────────────────────┘
                   │ Arc<RwLock<AppState>>
                   │ async_channel::Sender<TrayMessage>
                   │ tokio::sync::broadcast::Sender<()>
┌──────────────────┴───────────────────────────────┐
│  Tokio multi-threaded runtime                    │
│  poll_loop (3 s interval)                        │
│  watch_vpn_unit_state                            │
│  watch_network_manager                           │
│  backend WireGuard/OpenVPN D-Bus calls           │
└──────────────────┬───────────────────────────────┘
                   │ Arc<RwLock<AppState>> + Handle
┌──────────────────┴───────────────────────────────┐
│  OS thread (ksni)                                │
│  tray.rs — reads state live on menu open         │
└──────────────────────────────────────────────────┘
```

---

## 4. Data Models

### 4.1 `VpnType` Enum

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VpnType {
    WireGuard,
    OpenVpn,
}
```

### 4.2 `VpnProfile` Struct

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VpnProfile {
    /// Stable UUID — never changes after creation, used as directory name.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Protocol type.
    pub vpn_type: VpnType,
    /// Filename of the stored config inside the profile directory.
    /// Relative to `~/.config/vex-vpn/profiles/<id>/`.
    pub config_file: String,
    /// Connect automatically at startup.
    pub auto_connect: bool,
    /// Block all non-VPN traffic when this profile is active.
    pub kill_switch: bool,
    /// Override DNS server while this profile is active.
    /// None = use the DNS specified in the config file.
    pub dns_override: Option<String>,
    /// WireGuard interface name (only meaningful for WireGuard profiles).
    /// Defaults to "wg0" when not set.
    pub interface: Option<String>,
}
```

**Profile storage layout on disk:**

```
~/.config/vex-vpn/
├── config.toml            ← global config (profiles list + settings)
└── profiles/
    ├── <uuid-1>/
    │   └── wg.conf        ← imported WireGuard config file
    └── <uuid-2>/
        └── vpn.ovpn       ← imported OpenVPN config file
```

### 4.3 `Config` Struct (redesigned)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_schema_version")]
    pub version: u32,
    /// All managed VPN profiles.
    pub profiles: Vec<VpnProfile>,
    /// UUID of the profile to connect on startup when auto_connect = true.
    pub active_profile_id: Option<String>,
    /// Launch minimized to tray.
    pub start_minimized: bool,
    /// Auto-reconnect when network is restored.
    #[serde(default = "default_true")]
    pub auto_reconnect: bool,
    /// Show icon in the system tray.
    #[serde(default = "default_true")]
    pub show_tray_icon: bool,
}
```

### 4.4 `ConnectionStatus` (extended)

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    KillSwitchActive,
    Error(String),
    /// WireGuard handshake stale; inner = seconds since last handshake.
    Stale(u64),
}
```

### 4.5 `AppState` (restructured)

```rust
pub struct AppState {
    pub status: ConnectionStatus,
    pub active_profile_id: Option<String>,
    pub connection: Option<ConnectionInfo>,
    pub kill_switch_enabled: bool,
    pub auto_reconnect: bool,
    pub stale_cycles: u32,
    pub connection_start_ts: Option<u64>,
    /// Profiles loaded from config at startup (read-only view in state).
    pub profiles: Vec<VpnProfile>,
}
```

`ConnectionInfo` (simplified, provider-agnostic):

```rust
pub struct ConnectionInfo {
    pub local_ip: String,
    pub remote_endpoint: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}
```

---

## 5. Backend Strategy

### 5.1 `VpnBackend` Trait

```rust
#[async_trait::async_trait]
pub trait VpnBackend {
    async fn connect(&self, profile: &VpnProfile) -> Result<()>;
    async fn disconnect(&self, profile: &VpnProfile) -> Result<()>;
    async fn status(&self, profile: &VpnProfile) -> Result<ConnectionStatus>;
    async fn connection_info(&self, profile: &VpnProfile) -> Result<Option<ConnectionInfo>>;
}
```

### 5.2 WireGuard Backend (`backend/wireguard.rs`)

**Mechanism:** Use `wg-quick@<interface>.service` via the existing systemd D-Bus `StartUnit`/`StopUnit` API.

**Connect flow:**
1. Validate the profile's `config_file` path (`~/.config/vex-vpn/profiles/<uuid>/wg.conf`).
2. The config file must contain a valid `[Interface]` block. If it lacks `Address` or `PrivateKey`, return `Err`.
3. Determine the interface name:
   - If `VpnProfile::interface` is set, use it.
   - Else, extract `[Interface] / # Name =` from config comments, or default to `wg0`.
   - Validate with `config::validate_interface()`.
4. Symlink (or copy) the profile config to `/etc/wireguard/<interface>.conf` as root via the helper binary.
5. Call `dbus::start_unit(&format!("wg-quick@{}.service", interface))`.

**Status polling:**  
- Query `ActiveState` of `wg-quick@<interface>.service` via existing `get_service_status()`.
- Parse WireGuard handshake timestamp from `/proc/net/dev` and `wg show` (via `read_wg_handshake()`).
- Upgrade `Connected → Stale` if handshake age > 180 s (retain existing logic).

**Traffic stats:**  
- Parse `rx_bytes` / `tx_bytes` from `wg show <interface> dump` (existing `read_wg_stats()`).

### 5.3 OpenVPN Backend (`backend/openvpn.rs`)

**Mechanism:** Use NetworkManager D-Bus to import and activate the connection.

**D-Bus interfaces required:**
- `org.freedesktop.NetworkManager` → `ActivateConnection` / `DeactivateConnection`
- `org.freedesktop.NetworkManager.Settings` → `AddConnection`, `GetConnectionByUuid`
- `org.freedesktop.NetworkManager.VPN.Connection` → `VpnStateChanged` signal

**Connect flow:**
1. On profile import, call `NM Settings.AddConnection` with the parsed `.ovpn` settings dict.  
   The connection UUID is stored in `VpnProfile::id`.
2. On connect, call `NM.ActivateConnection(connection_path, "/", "/")`.
3. Subscribe to `NM VPN.Connection.VpnStateChanged` signal to track state.

**Status polling:**  
- Subscribe to `org.freedesktop.NetworkManager.Connection.Active` properties signal.
- Map NM VPN states to `ConnectionStatus`: `NM_VPN_CONNECTION_STATE_ACTIVATED` → `Connected`, etc.

**Parser (`parser/openvpn.rs`):**  
- Extract `remote`, `dev`, `proto`, `cipher`, `auth` from the `.ovpn` file.
- Inline certificates (`<ca>`, `<cert>`, `<key>`, `<tls-auth>`) into separate temp files if needed.
- Build the NM settings dictionary for `AddConnection`.

### 5.4 Kill Switch (unchanged pattern, extended for multi-profile)

The existing `vex-vpn-helper` binary with `pkexec` handles `enable_kill_switch`/`disable_kill_switch` via nftables. This is extended to:
- Accept the WireGuard interface name per-profile (already parameterised via `HelperRequest.interface`).
- For OpenVPN profiles, the tun interface name is extracted from the NM active connection device.

---

## 6. Parser Modules

### 6.1 WireGuard Parser (`parser/wireguard.rs`)

Parses `.conf` files in INI format using the `configparser` crate (which the `ini` crate re-exports).

**Target format:**
```ini
[Interface]
Address = 10.0.0.2/24
DNS = 1.1.1.1
PrivateKey = <base64>

[Peer]
PublicKey = <base64>
AllowedIPs = 0.0.0.0/0
Endpoint = 1.2.3.4:51820
PersistentKeepalive = 25
```

**Output type:**
```rust
pub struct WireGuardConfig {
    // Interface section
    pub address: String,
    pub private_key: String,
    pub listen_port: Option<u16>,
    pub dns: Option<String>,
    pub mtu: Option<u16>,
    // Peer section (first peer — single-peer VPN profiles)
    pub peer_public_key: String,
    pub endpoint: Option<String>,
    pub allowed_ips: String,
    pub persistent_keepalive: Option<u32>,
    pub preshared_key: Option<String>,
}
```

**Security:** Private key is kept only on disk in the profile directory (mode 0600). Never stored in `AppState` or config.toml.

### 6.2 OpenVPN Parser (`parser/openvpn.rs`)

Light-weight `.ovpn` parser that extracts the fields needed to build a NetworkManager `AddConnection` call. Does not implement full OpenVPN config grammar — only the subset needed for the NM settings dict.

**Fields extracted:** `remote`, `proto`, `dev`, `cipher`, `auth`, `port`, inline certificate blocks.

---

## 7. UI/UX Plan

### 7.1 Screen Map

```
Application Window (760×580 resizable)
├── Sidebar (fixed 200 px)
│   ├── App icon + "vex-vpn" title
│   ├── [Dashboard] nav button
│   ├── [Profiles]  nav button   ← NEW
│   ├── [History]   nav button
│   └── [Settings]  nav button
└── Content (NavigationView)
    ├── Dashboard page
    │   ├── Active profile name
    │   ├── Connection status pill
    │   ├── Connect/Disconnect button (large circular)
    │   ├── IP / endpoint stat cards
    │   ├── RX / TX stat cards
    │   └── Kill switch toggle
    ├── Profiles page (ui_profiles.rs) ← NEW
    │   ├── [+ Import Profile] button
    │   ├── Profile list (adw::ListBox)
    │   │   └── Per-profile row: icon, name, type badge, active indicator
    │   └── Profile detail panel (or NavigationPage push)
    │       ├── Profile name (editable)
    │       ├── VPN type (read-only)
    │       ├── Auto-connect toggle
    │       ├── Kill switch toggle
    │       ├── DNS override entry
    │       ├── [Connect] / [Disconnect]
    │       └── [Delete Profile]
    ├── Import dialog (ui_import.rs) ← NEW
    │   ├── File picker (GTK4 FileDialog, async)
    │   ├── Profile name entry
    │   ├── Type detection (auto from extension)
    │   └── [Import] button
    ├── History page (existing, minor changes)
    │   └── profile_name replaces region
    └── Preferences page (ui_prefs.rs) ← Rewritten
        ├── Page: General
        │   ├── Start minimized toggle
        │   ├── Show tray icon toggle
        │   └── Auto-reconnect toggle
        └── Page: Advanced
            └── Log verbosity combo
```

### 7.2 First-Run Flow

Since there is no longer a fixed "login" provider, the app launches directly to the Dashboard with an empty profiles list and a prominent "Import your first VPN profile" empty-state widget. No onboarding wizard is needed.

---

## 8. NixOS Module Design

### 8.1 New `nix/module-gui.nix` Options

```nix
options.services.vex-vpn = {
  enable = mkEnableOption "vex-vpn universal VPN GUI";
  package = mkOption { type = types.package; ... };
  autostart = mkOption { type = types.bool; default = false; ... };

  profiles = mkOption {
    type = types.attrsOf (types.submodule {
      options = {
        type = mkOption {
          type = types.enum [ "wireguard" "openvpn" ];
          description = "VPN protocol type.";
        };
        configFile = mkOption {
          type = types.path;
          description = "Path to the .conf (WireGuard) or .ovpn (OpenVPN) config file.";
        };
        autoConnect = mkOption { type = types.bool; default = false; };
        killSwitch  = mkOption { type = types.bool; default = false; };
        interface   = mkOption {
          type = types.nullOr types.str;
          default = null;
          description = "WireGuard interface name override (e.g. 'wg0'). Auto-detected if null.";
        };
      };
    });
    default = {};
    description = "Declaratively managed VPN profiles.";
    example = ''
      {
        my-provider = {
          type = "wireguard";
          configFile = ./vpn-configs/my-provider.conf;
          autoConnect = false;
          killSwitch = true;
        };
      }
    '';
  };

  killSwitch = { ... }; # existing option, kept
  dns        = { ... }; # existing option, kept
};
```

### 8.2 Module Implementation Logic

For each profile entry in `services.vex-vpn.profiles`:

**WireGuard profiles:**
```nix
networking.wg-quick.interfaces."${interface}" = {
  configFile = profile.configFile;
};
```
This uses NixOS's built-in `networking.wg-quick` option with `configFile =` (documented in NixOS WireGuard wiki: "Reuse existing wg-quick config file").  
The vex-vpn GUI then controls `wg-quick@<interface>.service` via D-Bus.

**OpenVPN profiles:**
```nix
# Write NM keyfile to /etc/NetworkManager/system-connections/<name>.nmconnection
environment.etc."NetworkManager/system-connections/${name}.nmconnection" = {
  source = pkgs.runCommand "nm-ovpn-${name}" {} ''
    ${pkgs.networkmanager-openvpn}/bin/... # nmcli import or NM keyfile writer
  '';
  mode = "0600";
};
```

**Declarative profile list injection:**  
The module generates a JSON snippet installed to  
`/etc/vex-vpn/declarative-profiles.json`  
which the GUI reads at startup and merges into its profile list as read-only "system" profiles.

### 8.3 Old `nix/module-vpn.nix`

Archived as `nix/module-vpn-pia.nix` (renamed, no longer exported). Removed from `nixosModules`. Users who relied on it can still import it directly. A deprecation notice is added to the file header.

---

## 9. File-by-File Change Plan

### Files to Delete

| File | Reason |
|------|--------|
| `src/pia.rs` | Entire PIA API client — replaced by generic backends |
| `src/secrets.rs` | PIA credential store — no equivalent needed (credentials live in WG/OVPN config files) |
| `src/ui_login.rs` | PIA-specific login dialog |
| `src/ui_onboarding.rs` | PIA 5-page wizard |
| `assets/ca.rsa.4096.crt` | PIA CA certificate (embedded in helper binary) |

### Files to Create

| File | Purpose |
|------|---------|
| `src/profile.rs` | `VpnProfile`, `VpnType`, profile directory helpers |
| `src/parser/mod.rs` | Parser module root + `detect_profile_type()` |
| `src/parser/wireguard.rs` | WireGuard INI parser using `configparser` |
| `src/parser/openvpn.rs` | OpenVPN `.ovpn` header parser |
| `src/backend/mod.rs` | `VpnBackend` trait + `backend_for_profile()` factory |
| `src/backend/wireguard.rs` | `WireGuardBackend` (wg-quick D-Bus) |
| `src/backend/openvpn.rs` | `OpenVpnBackend` (NetworkManager D-Bus) |
| `src/ui_profiles.rs` | Profile list and management UI |
| `src/ui_import.rs` | File import dialog |

### Files to Modify (key changes summarised)

**`src/config.rs`**  
- Replace `Config` struct fields: remove `interface`, `max_latency_ms`, `dns_provider`, `selected_region_id`, `kill_switch_enabled`, `kill_switch_allowed_ifaces`; add `profiles: Vec<VpnProfile>`, `active_profile_id`, `start_minimized`, `show_tray_icon`.
- Add `profile_dir()` helper returning `~/.config/vex-vpn/profiles/<uuid>/`.
- Keep `validate_interface()`, `config_path()`, `save_to()` with atomic write.
- Update integration tests in `tests/config_integration.rs`.

**`src/dbus.rs`**  
- Keep all existing systemd proxies.
- Add NM proxy for `org.freedesktop.NetworkManager` (`AddAndActivateConnection`, `DeactivateConnection`, `ActivateConnection`).
- Add NM Settings proxy for `org.freedesktop.NetworkManager.Settings` (`AddConnection`, `GetConnectionByUuid`, `DeleteConnection`).
- Add NM VPN.Connection proxy for `VpnStateChanged` signal.
- Rename `connect_vpn()` / `disconnect_vpn()` to `start_wireguard_unit(interface: &str)` / `stop_wireguard_unit(interface: &str)`.

**`src/state.rs`**  
- Remove all PIA-specific fields (`auth_token`, `regions`, `selected_region_id`, `region: Option<RegionInfo>`, `latency_ms`, `port_forward_enabled`, `forwarded_port`, `dns_leak_hint`).
- Replace with `active_profile_id: Option<String>`, `profiles: Vec<VpnProfile>`.
- In `poll_once()`: dispatch to `backend_for_profile(profile).status()` instead of reading PIA JSON files.
- Keep `watch_network_manager()` — it auto-reconnects on network restore.
- Keep history recording logic — replace `region` with `profile_name`.

**`src/tray.rs`**  
- Rename `PiaTray` → `VexTray`.
- Change `"Open PIA"` → `"Open vex-vpn"`.
- Change tray title from `"PIA — ..."` to `"vex-vpn — ..."`.
- Keep all icons, connect/disconnect menu items, quit item.

**`src/ui.rs`**  
- Remove PIA-specific dashboard widgets: region/server row, port forward toggle, port badge, `dns_banner`.
- Replace with: active profile name label, protocol badge.
- Remove all `pia::` imports.
- Add `Profiles` nav button linking to `ui_profiles.rs` page.

**`src/ui_prefs.rs`**  
- Complete rewrite: remove PIA-centric Connection page (interface, max_latency_ms, dns_provider).
- New pages: General (start_minimized, show_tray_icon, auto_reconnect), Advanced (log level).

**`src/helper.rs`**  
- Remove `reinstall_unit()` and `install_backend()` (PIA-specific).
- Keep `apply_kill_switch()`, `remove_kill_switch()`.
- Make `apply_kill_switch` accept a `&VpnProfile` and derive the interface from it.

**`src/bin/helper.rs`**  
- Remove embedded PIA shell scripts (`SERVICE_UNIT`, `CONNECT_SCRIPT`, etc.).
- Remove `InstallBackend` / `UninstallBackend` ops.
- Remove `PIA_CA_CERT` include.
- Keep `enable_kill_switch` / `disable_kill_switch` / `status` ops.

**`src/history.rs`**  
- Rename `HistoryEntry::region` → `HistoryEntry::profile_name`.
- Update all usage sites.

**`src/main.rs`**  
- Remove PIA-specific startup logic: `data_installed` check, `reinstall_unit()` call.
- Remove `PiaClient` construction.
- Remove `secrets::load_sync_hint()` check for showing onboarding vs dashboard.
- Simplify `connect_activate`: always show main window; empty-state handled in UI.

**`flake.nix`**  
- Remove `assets/ca.rsa.4096.crt` from `certFilter` (or keep filter generic for future).
- Update module exports: remove `nixosModules.pia-vpn`, add deprecation note.
- Retain Crane build pipeline unchanged.
- Add `networkmanager-openvpn` to `buildInputs` for the NixOS package (needed for NM module generation).

**`nix/module-gui.nix`**  
- Full rewrite per Section 8.

---

## 10. New Dependencies (Cargo.toml)

### Add

| Crate | Version | Purpose |
|-------|---------|---------|
| `configparser` | `"3"` | WireGuard INI `.conf` parsing (via `ini` crate's re-export or directly) |
| `uuid` | `"1"` features = `["v4", "serde"]` | Generate stable profile UUIDs |
| `async-trait` | `"0.1"` | `VpnBackend` trait with async methods |

### Remove

| Crate | Reason |
|-------|--------|
| `reqwest` | PIA HTTP client removed; no provider API calls |
| `base64` | PIA port-forward payload decode removed |

### Keep (all other dependencies unchanged)

`gtk4`, `libadwaita`, `glib`, `gio`, `tokio`, `zbus`, `serde`, `serde_json`, `ksni`, `anyhow`, `thiserror`, `toml`, `tracing`, `tracing-subscriber`, `notify-rust`, `libc`, `async-channel`, `futures-util`

### Dev dependencies (unchanged)

`wiremock`, `tempfile`

---

## 11. Security Considerations

### 11.1 Private Key Handling

- WireGuard private keys live **only** in `~/.config/vex-vpn/profiles/<uuid>/wg.conf` with mode `0600`.
- Private keys are **never** loaded into `AppState`, passed over broadcast channels, or serialised to `config.toml`.
- The parser (`parser/wireguard.rs`) reads the config file path from `VpnProfile::config_file` at connect time and passes it directly to the backend — it is not stored in memory beyond the connection handoff.

### 11.2 Config File Permissions

- `profile_dir()` creates profile directories with mode `0700` (user-only).
- Imported config files are copied into the profile dir with mode `0600` via `OpenOptions::mode(0o600)` (existing pattern from `secrets.rs`).

### 11.3 Input Validation

- `validate_interface()` (existing) is applied to any user-supplied interface name before passing to nftables or systemd.
- Profile names are sanitised before use as filesystem directory names (allow only alphanumeric, `-`, `_`, `.`; max 64 chars).
- The UUID-based `profile.id` is always the directory name — user-visible `name` is never used in filesystem paths.

### 11.4 Privilege Boundary

- The `vex-vpn-helper` binary (polkit-gated root) retains only nftables kill switch operations.
- All WireGuard operations go through the systemd D-Bus system bus (no shell execution from the GUI process).
- All OpenVPN operations go through the NetworkManager D-Bus system bus.
- Neither backend requires the GUI to execute arbitrary shell commands.

### 11.5 Credential Storage

- OpenVPN credentials (username/password if needed) will follow the same `0600` file pattern as the existing `secrets.rs` store, but stored in the profile directory: `~/.config/vex-vpn/profiles/<uuid>/credentials`.
- WireGuard has no separate credentials — the private key is already in the `.conf` file.

### 11.6 OWASP Top 10 Relevance

- **A01 Broken Access Control:** Profile directories are `0700`; config files `0600`. Validated on write.
- **A03 Injection:** Interface name validated by `validate_interface()` before use in D-Bus calls or nftables expressions. Profile names sanitised before filesystem use.
- **A05 Security Misconfiguration:** polkit policy limits `vex-vpn-helper` invocation to the local session user.
- **A09 Security Logging:** Disconnect reason and timestamps are logged to JSONL history.

---

## 12. Implementation Phases

### Phase A — Data Model & Config (foundation)

1. Add `configparser = "3"` and `uuid = { version = "1", features = ["v4", "serde"] }` to `Cargo.toml`.
2. Remove `reqwest` and `base64` from `Cargo.toml`.
3. Create `src/profile.rs` with `VpnType`, `VpnProfile`, `profile_dir()`.
4. Rewrite `src/config.rs`: new `Config` struct with `profiles: Vec<VpnProfile>`.
5. Update `tests/config_integration.rs` for new Config shape.
6. Verify: `nix develop --command cargo test`.

### Phase B — Parsers

7. Create `src/parser/mod.rs`, `src/parser/wireguard.rs`, `src/parser/openvpn.rs`.
8. Write unit tests for WireGuard parser (valid config, missing PrivateKey, missing Peer).
9. Write unit tests for OpenVPN parser (basic `.ovpn` header extraction).
10. Verify: `nix develop --command cargo test`.

### Phase C — Backends & D-Bus Extensions

11. Add `async-trait = "0.1"` to `Cargo.toml`.
12. Create `src/backend/mod.rs` with `VpnBackend` trait.
13. Create `src/backend/wireguard.rs` implementing `WireGuardBackend`.
14. Extend `src/dbus.rs` with NM proxies.
15. Create `src/backend/openvpn.rs` implementing `OpenVpnBackend`.
16. Verify: `nix develop --command cargo build`.

### Phase D — State Machine Restructure

17. Rewrite `AppState` in `src/state.rs` (remove PIA fields, add `active_profile_id`, `profiles`).
18. Rewrite `poll_once()` to dispatch to `backend_for_profile()`.
19. Update `watch_network_manager()` — no changes needed (already provider-agnostic).
20. Update `history.rs`: rename `region` → `profile_name`.
21. Update `main.rs`: remove PIA startup logic; simplify activate handler.
22. Verify: `nix develop --command cargo build`.

### Phase E — Remove PIA Code

23. Delete `src/pia.rs`, `src/secrets.rs`, `src/ui_login.rs`, `src/ui_onboarding.rs`.
24. Delete `assets/ca.rsa.4096.crt`.
25. Strip PIA logic from `src/bin/helper.rs` (remove script embeds, PIA install ops).
26. Strip `reinstall_unit()` from `src/helper.rs`.
27. Verify: `nix develop --command cargo build --release`.

### Phase F — New UI

28. Create `src/ui_import.rs` (file picker dialog using `gtk4::FileDialog`).
29. Create `src/ui_profiles.rs` (profile list, profile detail panel).
30. Rewrite `src/ui_prefs.rs` (General + Advanced pages, no PIA-specific options).
31. Rewrite dashboard section of `src/ui.rs` (remove PIA widgets; add profile display).
32. Update `src/tray.rs` (rename labels).
33. Verify: `nix develop --command cargo build` + smoke test.

### Phase G — NixOS Module

34. Rewrite `nix/module-gui.nix` with `profiles` option (Section 8).
35. Rename `nix/module-vpn.nix` → `nix/module-vpn-pia.nix`, add deprecation notice.
36. Update `flake.nix` module exports.
37. Verify: `nix build`.

### Phase H — Preflight & Tests

38. Run full preflight: `scripts/preflight.sh`.
39. Verify all phases: clippy → build → test → release build → `nix build`.

---

## 13. Risks and Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| **NetworkManager not present on target system** | Medium | High | Make OpenVPN backend optional at runtime; detect NM D-Bus availability at startup and disable OpenVPN import if absent. Show user-visible warning. |
| **wg-quick@.service not enabled in NixOS by default** | Medium | High | Add assertion in NixOS module: `config.networking.wg-quick != {}` or detect at runtime. Surface error through `ConnectionStatus::Error`. |
| **OpenVPN .ovpn files with inline certs** | High | Medium | Parser handles `<ca>`, `<cert>`, `<key>`, `<tls-auth>` blocks; writes them to temp files if NM requires separate PEM files. |
| **zbus 3.x NM D-Bus proxy for complex settings dict** | Medium | Medium | NM `AddConnection` takes `a{sa{sv}}` — test with a minimal WireGuard NM profile first to validate zvariant serialisation. |
| **Private key in WG config file world-readable** | Low | High | Enforce `0600` on import copy; warn in UI if existing file has wrong permissions (same pattern as `secrets.rs`). |
| **Profile UUID collision** | Very Low | Low | `uuid::Uuid::new_v4()` — negligible collision probability. |
| **tadfisher module users broken by removal of `nixosModules.pia-vpn`** | Low | Medium | Keep archived `nix/module-vpn-pia.nix`; add migration note to README; keep `nixosModules.pia-vpn` re-exporting the archived file with deprecation warning. |
| **Cargo.toml `reqwest` removal breaks dev builds** | Low | Low | Remove cleanly; no other crate depends on it. |
| **GTK4 `FileDialog` API requires gtk4 0.7+ with `v4_10` feature** | None | None | Already declared in Cargo.toml. |

---

## 14. Appendix: Reference Sources Consulted

1. **tadfisher/flake pia-vpn.nix** — Declarative PIA systemd/networkd module pattern  
   `https://github.com/tadfisher/flake/blob/main/nixos/modules/pia-vpn.nix`

2. **wireguard-keys crate** — Rust WireGuard key types (Privkey, Pubkey, Secret; x25519-dalek, serde, zeroize)  
   `https://docs.rs/wireguard-keys/latest/wireguard_keys/`

3. **ini / configparser crate** — INI-format parser for WireGuard `.conf` files  
   `https://docs.rs/ini/latest/ini/`

4. **NixOS Wiki — WireGuard** — Four NixOS integration modules (systemd.network, wg-quick, networking.wireguard, NetworkManager); `networking.wg-quick.interfaces.<name>.configFile` pattern; `nmcli connection import type wireguard` for NM  
   `https://wiki.nixos.org/wiki/WireGuard`

5. **NetworkManager Reference Manual v1.56** — D-Bus API: `org.freedesktop.NetworkManager.Settings`, `org.freedesktop.NetworkManager.VPN.Connection`, `Secret Agent` interface, VPN plugin D-Bus API  
   `https://networkmanager.dev/docs/api/latest/`

6. **vex-vpn codebase** — Complete analysis of all 18 source files; full understanding of existing patterns for config persistence, D-Bus proxies, state machine, UI structure, helper binary, and Nix build system.

---

*End of Specification*
