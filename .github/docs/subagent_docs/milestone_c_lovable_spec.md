# Milestone C — "Make it lovable" — Implementation Specification

**Date:** 2026-05-09  
**Phase:** 1 — Research & Specification  
**Scope decision:** ALL five features (F1, F3, F4, F5, F6) ship in Milestone C. No deferrals.

---

## 1. Executive Summary & Scope Decision

Milestone C transforms vex-vpn from a functional tool into a polished desktop application.
Five features are included. All are well-bounded; none require new architectural layers.

| Feature | Label | Files touched | Complexity |
|---------|-------|---------------|------------|
| F1 | First-run onboarding wizard | `src/ui_onboarding.rs` (new), `src/main.rs` | Medium |
| F3 | polkit-gated `vex-vpn-helper` binary | `src/bin/helper.rs` (new), `nix/polkit-vex-vpn.policy`, `flake.nix`, `Cargo.toml`, `src/helper.rs`, `src/dbus.rs`, `nix/module-gui.nix` | Medium |
| F4 | Secret Service via `oo7` with plaintext fallback | `src/secrets.rs`, `Cargo.toml` | Low–Medium |
| F5 | Desktop notifications on VPN state change | `src/state.rs`, `Cargo.toml` | Low |
| F6 | `adw::PreferencesWindow` + `gtk4::ShortcutsWindow` | `src/ui_prefs.rs` (new), `assets/shortcuts.ui` (new), `src/ui.rs`, `src/main.rs` | Medium |

### F3 scope decision: SHIP in Milestone C

**Why not defer:**

1. Stubs already exist: `src/helper.rs` has 4 placeholder lines; `nix/polkit-vex-vpn.policy` has 3.
   The scaffolding work from Milestone B is done.
2. The current `sudo nft` NOPASSWD rule (Milestone B) is a temporary measure. Shipping the
   polkit helper completes the security story.
3. The helper binary is small (≈100 LoC), the polkit action XML is boilerplate, and the
   Nix packaging change is additive. Total new code across all files: ≈250 LoC.
4. Deferring F3 means keeping the sudoers NOPASSWD rule indefinitely — a known High-severity issue
   from the PROJECT_ANALYSIS.md.

**Complexity confirmed manageable:** The IPC is minimal (newline-delimited JSON on stdin/stdout),
the pkexec invocation follows a well-understood pattern, and all five files are touched for only
one concern each.

---

## 2. F1 — First-Run Onboarding Wizard

### 2.1 Design rationale

A separate `adw::Window` shown as a **modal transient** over the main window is the cleanest
approach for libadwaita 0.5. Reasons:

- The main window can be built and `present()`-ed exactly as today (no timing change to `build_ui`).
- The wizard is self-contained — no NavigationView modifications.
- Matches the libadwaita HIG pattern: initial setup windows appear over an already-visible
  application window.
- Consistent with the existing `ui_login::show_login_dialog` pattern (same `adw::Window` type).

The wizard replaces `ui_login::show_login_dialog` entirely for the first-run case.
The `show_login_dialog` function is retained for the "Switch account…" action.

### 2.2 New file: `src/ui_onboarding.rs`

```
pub fn show_onboarding_wizard(
    parent: &adw::ApplicationWindow,
    state: Arc<RwLock<AppState>>,
    pia_client: Arc<pia::PiaClient>,
)
```

**Widget tree:**

```
adw::Window (modal, transient_for=parent, 480×560, not resizable)
└── adw::ToolbarView
    ├── [top] adw::HeaderBar (no title, no buttons — only show via CSS)
    └── [content] gtk4::Box (vertical, spacing=0)
        ├── adw::Carousel (vexpand=true)
        │   ├── Page 0 — Welcome
        │   │   └── gtk4::Box (vertical, halign=Center, valign=Center, spacing=18, margin=32)
        │   │       ├── gtk4::Image (icon="network-vpn-symbolic", pixel_size=96)
        │   │       ├── gtk4::Label "Private Internet Access" (css: title-1)
        │   │       ├── gtk4::Label "Secure VPN for NixOS — WireGuard backend" (css: dim-label)
        │   │       └── [spacer vexpand]
        │   ├── Page 1 — Sign In
        │   │   └── gtk4::Box (vertical, spacing=16, margin=24)
        │   │       ├── gtk4::Label "Sign in to PIA" (css: title-2, halign=Start)
        │   │       ├── adw::PreferencesGroup
        │   │       │   ├── adw::EntryRow title="Username"
        │   │       │   └── adw::PasswordEntryRow title="Password"
        │   │       ├── gtk4::Label (error label, css: error, visible=false)
        │   │       └── gtk4::Spinner (visible=false)
        │   ├── Page 2 — Privacy Notice
        │   │   └── gtk4::Box (vertical, spacing=12, margin=24)
        │   │       ├── gtk4::Image (icon="security-symbolic", pixel_size=48)
        │   │       ├── gtk4::Label "What we store" (css: title-2)
        │   │       └── gtk4::Label (multi-line privacy bullets, wrap=true, xalign=0.0)
        │   ├── Page 3 — Kill Switch
        │   │   └── gtk4::Box (vertical, spacing=16, margin=24)
        │   │       ├── gtk4::Image (icon="network-vpn-no-route-symbolic", pixel_size=48)
        │   │       ├── gtk4::Label "Kill Switch" (css: title-2)
        │   │       ├── gtk4::Label (description, wrap=true, css: dim-label)
        │   │       └── gtk4::ListBox (selection=None, css: boxed-list)
        │   │           └── adw::SwitchRow title="Enable Kill Switch" (default: inactive)
        │   └── Page 4 — Done
        │       └── gtk4::Box (vertical, halign=Center, valign=Center, spacing=18, margin=32)
        │           ├── gtk4::Image (icon="emblem-ok-symbolic", pixel_size=80)
        │           ├── gtk4::Label "You're all set!" (css: title-1)
        │           └── gtk4::Label "Connect to any region and browse securely." (css: dim-label)
        ├── adw::CarouselIndicatorDots (carousel=above)
        └── gtk4::Box (navigation row, horizontal, margin=12, spacing=8)
            ├── gtk4::Button "← Back" (id: back_btn, visible=false initially)
            ├── [spacer hexpand]
            └── gtk4::Button "Get started →" (id: next_btn, css: suggested-action)
```

### 2.3 Navigation state machine

| Current page | back_btn | next_btn label | next_btn action |
|---|---|---|---|
| 0 (Welcome) | hidden | "Get started →" | scroll to page 1 |
| 1 (Sign In) | visible | "Sign in →" | attempt auth, on success → page 2 |
| 2 (Privacy) | visible | "I understand →" | scroll to page 3 |
| 3 (Kill switch) | visible | "Next →" | save kill switch choice → page 4 |
| 4 (Done) | hidden | "Start browsing →" | close wizard, present main window |

`adw::Carousel::scroll_to(page, animate=true)` drives transitions.

Connect `carousel.connect_page_changed(|_, idx| update_nav_buttons(idx))`.

### 2.4 Sign-in page logic

Mirrors `ui_login.rs` exactly:
- Validate non-empty fields
- Show spinner, disable next_btn
- `client.generate_token(&username, &password).await`
- On `PiaError::AuthFailed`: show error label
- On success:
  - `secrets::save(&creds).await` (now async — see F4)
  - `state.write().await.auth_token = Some(token)`
  - `client.fetch_server_list().await` → `state.write().await.regions`
  - `carousel.scroll_to(&privacy_page, true)`

### 2.5 Kill switch page logic

- Read `adw::SwitchRow::is_active()`
- On "Next →":
  - If active: `dbus::apply_kill_switch(&iface).await` (best-effort; log error, continue)
  - `let mut cfg = Config::load(); cfg.kill_switch_enabled = active; cfg.save()`
  - NOTE: `Config` needs a new `kill_switch_enabled: bool` field (see §8)

### 2.6 Integration point in `main.rs`

Replace the current credential-check block:

```rust
// CURRENT (sync):
match secrets::load() {
    Ok(Some(creds)) => { glib::spawn_future_local(async move { auto_login(...).await }); }
    Ok(None) => { ui_login::show_login_dialog(&window, state, client); }
    Err(e) => warn!(...),
}

// NEW (async, inside spawn_future_local):
glib::spawn_future_local(async move {
    match secrets::load().await {
        Ok(Some(creds)) => {
            auto_login(client, state, &creds.username, &creds.password).await;
        }
        Ok(None) => {
            ui_onboarding::show_onboarding_wizard(&window, state, client);
        }
        Err(e) => warn!("load credentials: {}", e),
    }
});
```

---

## 3. F3 — polkit-gated `vex-vpn-helper` binary

### 3.1 Architecture overview

```
GUI process (uid=user)
  │
  │  tokio::process::Command::new("pkexec")
  │      .arg(HELPER_PATH)
  │
  ▼
pkexec  →  polkit daemon  →  action: org.vex-vpn.helper.run
  │              ↓
  │         auth_admin_keep (prompts once, caches session)
  ▼
vex-vpn-helper (runs as root)
  │  reads newline-delimited JSON from stdin
  │  executes nft
  │  writes {"ok": true} or {"ok": false, "error": "..."} to stdout
  ▼
GUI reads stdout line
```

### 3.2 New file: `src/bin/helper.rs`

This is the `[[bin]]` target `vex-vpn-helper`. Runs as root via pkexec.

**IPC protocol — stdin (one JSON line):**

```json
{"op": "enable_kill_switch", "interface": "wg0"}
{"op": "disable_kill_switch"}
{"op": "status"}
```

**IPC protocol — stdout (one JSON line):**

```json
{"ok": true}
{"ok": false, "error": "nft: table already exists"}
{"ok": true, "active": false}
```

**Implementation skeleton:**

```rust
// src/bin/helper.rs
use std::io::{self, BufRead, Write};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Command {
    EnableKillSwitch { interface: String },
    DisableKillSwitch,
    Status,
}

#[derive(Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<bool>,
}

fn main() {
    // Security: drop supplementary groups, verify we are root
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        let resp = Response { ok: false, error: Some("must run as root".into()), active: None };
        println!("{}", serde_json::to_string(&resp).unwrap());
        std::process::exit(1);
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let resp = match serde_json::from_str::<Command>(&line) {
            Ok(cmd) => handle_command(cmd),
            Err(e) => Response { ok: false, error: Some(format!("parse error: {}", e)), active: None },
        };
        let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap());
        let _ = out.flush();
    }
}

fn handle_command(cmd: Command) -> Response {
    match cmd {
        Command::EnableKillSwitch { interface } => {
            if !is_valid_interface(&interface) {
                return Response { ok: false, error: Some("invalid interface name".into()), active: None };
            }
            run_nft_enable(&interface)
        }
        Command::DisableKillSwitch => run_nft_disable(),
        Command::Status => check_status(),
    }
}

fn is_valid_interface(name: &str) -> bool {
    if name.is_empty() || name.len() > 15 { return false; }
    let b = name.as_bytes();
    b[0].is_ascii_lowercase()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

fn run_nft_enable(iface: &str) -> Response {
    let ruleset = format!(
        "table inet pia_kill_switch {{\n\
         chain output {{ type filter hook output priority 0; policy drop;\n\
         ct state established,related accept\n\
         oifname \"{iface}\" accept\n\
         oifname \"lo\" accept\n}}\n\
         chain input {{ type filter hook input priority 0; policy drop;\n\
         ct state established,related accept\n\
         iifname \"{iface}\" accept\n\
         iifname \"lo\" accept\n}}\n}}",
        iface = iface
    );
    let output = std::process::Command::new("nft")
        .arg("-f").arg("-")
        .stdin(std::process::Stdio::piped())
        .output();
    // write ruleset to stdin ... (implementation detail)
    // Return ok or error based on exit status
    match output {
        Ok(o) if o.status.success() => Response { ok: true, error: None, active: None },
        Ok(o) => Response { ok: false, error: Some(String::from_utf8_lossy(&o.stderr).into()), active: None },
        Err(e) => Response { ok: false, error: Some(e.to_string()), active: None },
    }
}

fn run_nft_disable() -> Response {
    let output = std::process::Command::new("nft")
        .args(["delete", "table", "inet", "pia_kill_switch"])
        .output();
    match output {
        Ok(o) if o.status.success() => Response { ok: true, error: None, active: None },
        Ok(_) => Response { ok: true, error: None, active: None }, // table may not exist — OK
        Err(e) => Response { ok: false, error: Some(e.to_string()), active: None },
    }
}

fn check_status() -> Response {
    let output = std::process::Command::new("nft")
        .args(["list", "table", "inet", "pia_kill_switch"])
        .output();
    let active = matches!(output, Ok(o) if o.status.success());
    Response { ok: true, error: None, active: Some(active) }
}
```

**Dependencies for `vex-vpn-helper` binary only:**
- `serde`, `serde_json` — already in workspace
- `libc` — add to `Cargo.toml` (small, no transitive deps)

### 3.3 Cargo.toml addition

```toml
[[bin]]
name = "vex-vpn-helper"
path = "src/bin/helper.rs"

[dependencies]
# ...existing deps...
libc = "0.2"
```

### 3.4 Polkit action file: `nix/polkit-vex-vpn.policy`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC
  "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
  "http://www.freedesktop.org/standards/PolicyKit/1/policyconfig.dtd">
<policyconfig>
  <vendor>vex-vpn</vendor>
  <vendor_url>https://github.com/victorytek/vex-vpn</vendor_url>

  <action id="org.vex-vpn.helper.run">
    <description>Manage VPN kill switch via nftables</description>
    <message>Authentication is required to control the VPN kill switch</message>
    <icon_name>network-vpn-symbolic</icon_name>
    <defaults>
      <allow_any>auth_admin</allow_any>
      <allow_inactive>auth_admin</allow_inactive>
      <allow_active>auth_admin_keep</allow_active>
    </defaults>
    <annotate key="org.freedesktop.policykit.exec.path">@HELPER_PATH@</annotate>
    <annotate key="org.freedesktop.policykit.exec.allow_gui">true</annotate>
  </action>
</policyconfig>
```

**Note:** `@HELPER_PATH@` is substituted by Nix to the actual store path. `auth_admin_keep`
means: prompt once per session, cache the authorization.

### 3.5 `src/helper.rs` update

Replace the current stub with pkexec invocation:

```rust
//! Kill switch management via the polkit-gated vex-vpn-helper binary.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Path to the helper binary installed by the NixOS module.
/// Falls back to searching PATH for non-NixOS builds.
fn helper_path() -> &'static str {
    // NixOS installs the helper via environment.systemPackages;
    // it is accessible via /run/current-system/sw/libexec/vex-vpn-helper.
    // For dev builds, fall back to the PATH entry.
    const NIXOS_PATH: &str = "/run/current-system/sw/libexec/vex-vpn-helper";
    if std::path::Path::new(NIXOS_PATH).exists() {
        NIXOS_PATH
    } else {
        "vex-vpn-helper"
    }
}

#[derive(Serialize)]
struct HelperRequest<'a> {
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    interface: Option<&'a str>,
}

#[derive(Deserialize)]
struct HelperResponse {
    ok: bool,
    error: Option<String>,
}

async fn call_helper(req: &HelperRequest<'_>) -> Result<()> {
    let mut child = Command::new("pkexec")
        .arg(helper_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut stdin = child.stdin.take().expect("stdin");
    let line = serde_json::to_string(req)? + "\n";
    stdin.write_all(line.as_bytes()).await?;
    drop(stdin);

    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout).lines();
    let response_line = reader.next_line().await?.unwrap_or_default();

    child.wait().await?;

    let resp: HelperResponse = serde_json::from_str(&response_line)
        .map_err(|_| anyhow::anyhow!("helper returned invalid JSON: {:?}", response_line))?;

    if resp.ok {
        Ok(())
    } else {
        bail!("helper error: {}", resp.error.unwrap_or_default())
    }
}

pub async fn apply_kill_switch(interface: &str) -> Result<()> {
    if !crate::config::validate_interface(interface) {
        bail!("invalid interface name: {:?}", interface);
    }
    call_helper(&HelperRequest { op: "enable_kill_switch", interface: Some(interface) }).await
}

pub async fn remove_kill_switch() -> Result<()> {
    call_helper(&HelperRequest { op: "disable_kill_switch", interface: None }).await
}
```

### 3.6 `src/dbus.rs` update

Remove `apply_kill_switch` and `remove_kill_switch` from `dbus.rs`.
Re-export them from `helper.rs` so all call sites stay the same:

```rust
// In src/dbus.rs — delete the nft functions entirely.
// Call sites in ui.rs use:  crate::dbus::apply_kill_switch(...)
// After F3 these become:    crate::helper::apply_kill_switch(...)
// Therefore also update ui.rs call sites to use crate::helper::
```

### 3.7 `flake.nix` update

Build both binaries from the same Crane build, install helper to `libexec`:

```nix
postInstall = ''
  # existing postInstall content ...

  # Helper binary for polkit-gated nft operations
  mkdir -p $out/libexec
  cp target/${if isRelease then "release" else "debug"}/vex-vpn-helper $out/libexec/

  # Polkit action (path substituted)
  mkdir -p $out/share/polkit-1/actions
  substitute nix/polkit-vex-vpn.policy \
    $out/share/polkit-1/actions/org.vex-vpn.helper.policy \
    --replace '@HELPER_PATH@' "$out/libexec/vex-vpn-helper"
'';
```

### 3.8 `nix/module-gui.nix` update

```nix
# Install polkit action so policykit daemon picks it up
security.polkit.extraConfig = ''
  // (keep existing systemd action grant for wheel group)
'';

# Add the helper to system packages so it lands in /run/current-system/sw/libexec/
environment.systemPackages = [ cfg.package ];
environment.pathsToLink = [ "/libexec" ];

# REMOVE the sudo extraRules block added in Milestone B — the helper replaces it.
# security.sudo.extraRules = [ ... ];  ← DELETE THIS
```

The NixOS module should also install the polkit action file at the system level:
```nix
environment.etc."polkit-1/actions/org.vex-vpn.helper.policy".source =
  "${cfg.package}/share/polkit-1/actions/org.vex-vpn.helper.policy";
```

---

## 4. F4 — Secret Service via `oo7` with Plaintext Fallback

### 4.1 Dependency research (Context7)

Context7 did not have `oo7` in its library database. Based on crates.io and GNOME sources:

- **Crate name:** `oo7`
- **Version:** `0.3.x` (current stable as of 2025–2026)
- **Internal D-Bus:** uses `zbus 4.x` internally — Cargo will compile both zbus 3.x (our dep)
  and zbus 4.x (oo7's transitive dep) simultaneously; this is safe, just increases build time.
- **Async runtime:** oo7 0.3 is runtime-agnostic at the Tokio futures level. Verify the exact
  feature flag (`"tokio"` or `"async-std"`) from `cargo doc` during implementation.
- **Fallback:** `oo7::Keyring::new().await` returns `Err` if no Secret Service daemon is running.
  Our design catches this and falls through to the plaintext path.

**Cargo.toml line (to verify exact feature flags during implementation):**
```toml
oo7 = "0.3"
```

### 4.2 `src/secrets.rs` redesign

The public API signatures **do not change** for callers — only the functions become `async`:

```rust
pub async fn load() -> Result<Option<Credentials>>
pub async fn save(c: &Credentials) -> Result<()>
pub async fn delete() -> Result<()>
```

**Load logic:**

```
try_keyring()
  ├── Ok(keyring):
  │   search_items(ATTRS)
  │   ├── Ok(items) if !items.empty:
  │   │   item[0].secret() → deserialize JSON → Ok(Some(Credentials))
  │   └── else: fall through to plaintext
  └── Err(_): fall through to plaintext

plaintext:
  load_plaintext() → Ok(Some(...)) | Ok(None) | Err(...)
```

**Save logic:**

```
try_keyring()
  ├── Ok(keyring):
  │   create_item(LABEL, ATTRS, json_bytes, replace=true)
  │   ├── Ok(_): delete plaintext file (migration), return Ok(())
  │   └── Err(e): log warning, fall through to plaintext
  └── Err(_): fall through to plaintext

plaintext:
  save_plaintext(c) → Ok(()) | Err(...)
```

**Key constants:**
```rust
const KEYRING_LABEL: &str = "vex-vpn PIA credentials";
const ATTRS: [(&str, &str); 2] = [
    ("application", "vex-vpn"),
    ("service", "pia-credentials"),
];
```

**oo7 API (verify on docs.rs during implementation):**
```rust
// Approximate API — confirm exact method names from docs.rs/oo7
async fn try_keyring() -> Option<oo7::Keyring> {
    oo7::Keyring::new().await.ok()
}

// Store:
keyring.create_item(KEYRING_LABEL, &HashMap::from(ATTRS), secret_bytes, true).await?;

// Retrieve:
let items = keyring.search_items(&HashMap::from(ATTRS)).await?;
let secret: Vec<u8> = items[0].secret().await?;

// Delete:
keyring.delete_item(&items[0]).await?;
```

### 4.3 Caller updates

**`main.rs`** — wrap credential check in `glib::spawn_future_local` (it was previously inline sync):

```rust
// In app.connect_activate:
let state = state_for_ui.clone();
let client = pia_client.clone();
let window_ref = window.clone();

glib::spawn_future_local(async move {
    match secrets::load().await {
        Ok(Some(creds)) => {
            auto_login(client, state, &creds.username, &creds.password).await;
        }
        Ok(None) => {
            ui_onboarding::show_onboarding_wizard(&window_ref, state, client);
        }
        Err(e) => warn!("load credentials: {}", e),
    }
});
```

**`ui_login.rs`** — already inside `glib::spawn_future_local`, just add `.await`:
```rust
// Before:  if let Err(e) = crate::secrets::save(&creds) { ... }
// After:   if let Err(e) = crate::secrets::save(&creds).await { ... }
```

**`ui_onboarding.rs`** — same pattern as ui_login.rs.

**Tests** — convert to `#[tokio::test]`:
```rust
#[tokio::test]
async fn round_trip_in_temp_dir() {
    // ... (same assertions, just with .await on load/save/delete)
}
```

---

## 5. F5 — Desktop Notifications

### 5.1 Dependency research (Context7 — `/hoodie/notify-rust`)

Context7 confirmed:
- **Crate:** `notify-rust = "4"`
- **API:** `Notification::new().summary("...").body("...").icon("...").show()?`
- **Wayland:** works via D-Bus `org.freedesktop.Notifications` (libnotify/mako/dunst)
- **Async:** no built-in async API; `.show()` is sync but nearly instant (single D-Bus message)
- **Non-blocking usage:** wrap in `tokio::task::spawn_blocking`

```toml
notify-rust = "4"
```

### 5.2 `src/state.rs` update

Add `prev_status` tracking to `poll_loop`. Add a private `notify_status_change` function.

```rust
pub async fn poll_loop(state: Arc<RwLock<AppState>>) {
    let mut prev_status = ConnectionStatus::Disconnected;
    loop {
        match poll_once(&state).await {
            Ok(()) => {}
            Err(e) => warn!("Poll error: {}", e),
        }
        let new_status = state.read().await.status.clone();
        if new_status != prev_status {
            let old = prev_status.clone();
            let new = new_status.clone();
            // Fire-and-forget: spawn_blocking so D-Bus call doesn't stall poll
            tokio::task::spawn_blocking(move || notify_status_change(&old, &new));
            prev_status = new_status;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

fn notify_status_change(old: &ConnectionStatus, new: &ConnectionStatus) {
    use notify_rust::{Notification, Urgency};
    let result = match new {
        ConnectionStatus::Connected => Notification::new()
            .summary("vex-vpn")
            .body("Connected")
            .icon("network-vpn-symbolic")
            .show(),
        ConnectionStatus::Disconnected
            if matches!(old, ConnectionStatus::Connected | ConnectionStatus::KillSwitchActive) =>
        {
            Notification::new()
                .summary("vex-vpn")
                .body("Disconnected")
                .icon("network-vpn-disabled-symbolic")
                .show()
        }
        ConnectionStatus::Error(msg) => Notification::new()
            .summary("vex-vpn — Connection Error")
            .body(msg)
            .icon("network-vpn-disabled-symbolic")
            .urgency(Urgency::Critical)
            .show(),
        _ => return,
    };
    if let Err(e) = result {
        warn!("Failed to show desktop notification: {}", e);
    }
}
```

**Note:** `ConnectionStatus` must `impl PartialEq` — it already does per `state.rs`.

### 5.3 Privacy implication

Notifications are sent to the local notification daemon only. No data leaves the device.
Body text never includes the auth token or credentials.

---

## 6. F6 — PreferencesWindow and ShortcutsWindow

### 6.1 libadwaita 0.5 API confirmation (Context7)

Context7 confirmed via `/gnome/libadwaita`:
- `adw::PreferencesWindow`, `adw::PreferencesPage`, `adw::PreferencesGroup` — all available
- `adw::SwitchRow` — available with `features = ["v1_4"]` (added in libadwaita 1.4 C library,
  exposed in Rust crate 0.5 behind the `v1_4` feature gate — **already enabled in Cargo.toml**)
- `adw::EntryRow`, `adw::PasswordEntryRow`, `adw::ComboRow` — all available
- `adw::PreferencesDialog` is **NOT** used — it requires libadwaita 1.5 (crate 0.6+). We use
  `adw::PreferencesWindow` which is stable in 0.5.

Context7 confirmed via `/gtk-rs/gtk4-rs`:
- `gtk4::ShortcutsWindow` — available in 0.7; conventional approach is via XML + Builder
- `app.set_accels_for_action("app.preferences", &["<Control>comma"])` pattern is confirmed

### 6.2 New file: `src/ui_prefs.rs`

```rust
// Lazily created on "app.preferences" action.
// Returns the window so main.rs can set transient_for.
pub fn build_preferences_window(
    parent: &adw::ApplicationWindow,
    state: Arc<RwLock<AppState>>,
) -> adw::PreferencesWindow
```

**Widget tree:**

```
adw::PreferencesWindow
├── Page: "Connection" (icon: "network-server-symbolic")
│   └── adw::PreferencesGroup "Network"
│       ├── adw::EntryRow "Interface name" (default: config.interface)
│       ├── adw::EntryRow "Max latency (ms)" (default: config.max_latency_ms.to_string())
│       └── adw::ComboRow "DNS provider"
│           model: gtk4::StringList ["pia", "google", "cloudflare"]
│           selected: index of config.dns_provider
├── Page: "Privacy" (icon: "security-symbolic")
│   ├── adw::PreferencesGroup "Kill Switch"
│   │   └── adw::SwitchRow "Enable kill switch"
│   │       subtitle: "Block all traffic if VPN tunnel drops"
│   │       active: state.kill_switch_enabled
│   └── adw::PreferencesGroup "Allowed Interfaces"
│       └── adw::EntryRow "Additional allowed interfaces"
│           (comma-separated; maps to Config::kill_switch_allowed_ifaces: Vec<String>)
└── Page: "Advanced" (icon: "preferences-system-symbolic")
    └── adw::PreferencesGroup
        ├── adw::SwitchRow "Auto Connect on Login" (active: config.auto_connect)
        └── adw::ComboRow "Log level"
            model: ["info", "debug", "trace"]
```

**Change propagation:**
Each row connects to `notify::text` or `notify::active` or `notify::selected`.
On change: `Config::load()` → mutate field → `Config::save()`. This is safe because `Config::load`
falls back to defaults on any error and writes are idempotent.

### 6.3 New file: `assets/shortcuts.ui`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<interface>
  <object class="GtkShortcutsWindow" id="help_overlay">
    <property name="modal">true</property>
    <child>
      <object class="GtkShortcutsSection">
        <property name="section-name">shortcuts</property>
        <property name="max-height">10</property>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title" translatable="yes">Application</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title" translatable="yes">Preferences</property>
                <property name="accelerator">&lt;Primary&gt;comma</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title" translatable="yes">Keyboard Shortcuts</property>
                <property name="accelerator">&lt;Primary&gt;question</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title" translatable="yes">Quit</property>
                <property name="accelerator">&lt;Primary&gt;q</property>
              </object>
            </child>
          </object>
        </child>
        <child>
          <object class="GtkShortcutsGroup">
            <property name="title" translatable="yes">VPN</property>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title" translatable="yes">Connect / Disconnect</property>
                <property name="accelerator">&lt;Primary&gt;Return</property>
              </object>
            </child>
            <child>
              <object class="GtkShortcutsShortcut">
                <property name="title" translatable="yes">Select Server</property>
                <property name="accelerator">&lt;Primary&gt;s</property>
              </object>
            </child>
          </object>
        </child>
      </object>
    </child>
  </object>
</interface>
```

### 6.4 `src/ui.rs` updates

Add `show_shortcuts_window` function:

```rust
pub fn show_shortcuts_window(parent: &adw::ApplicationWindow) {
    let builder = gtk4::Builder::from_string(include_str!("../assets/shortcuts.ui"));
    let win: gtk4::ShortcutsWindow = builder.object("help_overlay").expect("shortcuts window");
    win.set_transient_for(Some(parent));
    win.present();
}
```

Update `build_primary_menu` to add Preferences and Shortcuts entries:

```rust
pub fn build_primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let account_section = gio::Menu::new();
    account_section.append(Some("Switch account…"), Some("app.switch-account"));
    menu.append_section(None, &account_section);

    let view_section = gio::Menu::new();
    view_section.append(Some("Preferences"), Some("app.preferences"));
    view_section.append(Some("Keyboard Shortcuts"), Some("app.show-shortcuts"));
    menu.append_section(None, &view_section);

    let app_section = gio::Menu::new();
    app_section.append(Some("About vex-vpn"), Some("app.about"));
    app_section.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &app_section);

    menu
}
```

### 6.5 `src/main.rs` — register new actions and accelerators

In `register_app_actions`:

```rust
// Preferences — Ctrl+,
let prefs_action = gio::SimpleAction::new("preferences", None);
{
    let app = app.clone();
    let state_c = state.clone();
    prefs_action.connect_activate(move |_, _| {
        if let Some(window) = app.active_window()
            .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok())
        {
            let prefs = ui_prefs::build_preferences_window(&window, state_c.clone());
            prefs.set_transient_for(Some(&window));
            prefs.present();
        }
    });
}
app.add_action(&prefs_action);
app.set_accels_for_action("app.preferences", &["<Control>comma"]);

// Shortcuts — Ctrl+?
let shortcuts_action = gio::SimpleAction::new("show-shortcuts", None);
{
    let app = app.clone();
    shortcuts_action.connect_activate(move |_, _| {
        if let Some(window) = app.active_window()
            .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok())
        {
            ui::show_shortcuts_window(&window);
        }
    });
}
app.add_action(&shortcuts_action);
app.set_accels_for_action("app.show-shortcuts", &["<Control>question"]);

// Quit — Ctrl+Q (add accelerator to existing quit action)
app.set_accels_for_action("app.quit", &["<Control>q"]);
```

---

## 7. Startup Flow Diagram

```
main()
  │
  ├─── Config::load()
  │
  ├─── AppState::new_with_config(&cfg)
  │
  ├─── [Tokio] rt.spawn → state::poll_loop(state)
  │         (every 3s: D-Bus queries + notification dispatch)
  │
  ├─── [OS thread] tray::run_tray(state, tx, rt.handle())
  │
  └─── rt.enter() [guard]
        │
        app = adw::Application::new("com.vex.vpn.nixos")
        register_app_actions(app, state)
        │
        app.connect_activate(|app| {
            window = ui::build_ui(app, state, rx)
            window.present()                    ← main window is now visible
            │
            pia_client = PiaClient::new()
            │
            glib::spawn_future_local(async {
                secrets::load().await
                ├── Ok(Some(creds)):
                │   auto_login(client, state, creds).await
                │       ├─ client.generate_token(user, pass)
                │       └─ client.fetch_server_list() → state.regions
                │
                └── Ok(None):
                    ui_onboarding::show_onboarding_wizard(&window, state, client)
                    (modal over main window — user completes wizard)
                    wizard completes:
                        ├─ secrets::save(creds).await
                        │   ├─ try oo7::Keyring → store in Secret Service
                        │   └─ fallback: write credentials.toml (0600)
                        ├─ state.auth_token = token
                        ├─ state.regions = fetch_server_list().await
                        ├─ if kill_switch_chosen: helper::apply_kill_switch(iface).await
                        └─ wizard.close() [main window regains focus]
            })
        })
        │
        app.run() [GTK main loop]
```

---

## 8. Cargo.toml Changes (Exact Lines)

**Add to `[dependencies]` section:**

```toml
# Secret Service credential storage — GNOME Keyring / KWallet via D-Bus
# Fallback: existing plaintext credentials.toml (mode 0600)
oo7 = "0.3"

# Desktop notifications via org.freedesktop.Notifications (D-Bus, Wayland+X11)
notify-rust = "4"

# Required for vex-vpn-helper binary (root-uid verification)
libc = "0.2"
```

**Add new `[[bin]]` section (after the existing `[[bin]]` block):**

```toml
[[bin]]
name = "vex-vpn-helper"
path = "src/bin/helper.rs"
```

**Config struct additions (src/config.rs):**

```rust
// Add to Config struct:
#[serde(default)]
pub kill_switch_enabled: bool,
#[serde(default)]
pub kill_switch_allowed_ifaces: Vec<String>,
```

(These fields use `#[serde(default)]` for backward compatibility with existing config files.)

---

## 9. Files to Modify / Create — Implementation Checklist

### New files

| File | Purpose |
|------|---------|
| `src/ui_onboarding.rs` | F1: 5-step onboarding wizard |
| `src/ui_prefs.rs` | F6: PreferencesWindow |
| `src/bin/helper.rs` | F3: vex-vpn-helper binary (runs as root) |
| `assets/shortcuts.ui` | F6: ShortcutsWindow XML |

### Modified files

| File | Changes |
|------|---------|
| `Cargo.toml` | Add `oo7`, `notify-rust`, `libc`; add `[[bin]] vex-vpn-helper` |
| `src/secrets.rs` | F4: async load/save/delete; oo7 integration with plaintext fallback |
| `src/state.rs` | F5: prev_status tracking in poll_loop; notify_status_change fn |
| `src/config.rs` | F1/F6: add `kill_switch_enabled`, `kill_switch_allowed_ifaces` fields |
| `src/main.rs` | F1: async secrets check in spawn_future_local; F6: prefs/shortcuts actions + accelerators |
| `src/ui.rs` | F6: show_shortcuts_window fn; update build_primary_menu |
| `src/helper.rs` | F3: replace stub with pkexec invocation via call_helper |
| `src/dbus.rs` | F3: remove apply_kill_switch/remove_kill_switch (moved to helper.rs) |
| `src/ui_login.rs` | F4: add .await to secrets::save call |
| `nix/polkit-vex-vpn.policy` | F3: complete polkit XML (replaces stub) |
| `flake.nix` | F3: build/install helper binary + polkit action via postInstall |
| `nix/module-gui.nix` | F3: pathsToLink + polkit etc install; remove sudoers extraRules |

---

## 10. Testing Plan

### Unit tests

| Module | Tests to add |
|--------|-------------|
| `secrets.rs` | Convert existing `round_trip_in_temp_dir` to `#[tokio::test] async`; add `fallback_when_no_daemon` |
| `state.rs` | `notify_status_change_fires_on_connect`, `no_notify_on_same_status` |
| `config.rs` | `kill_switch_field_defaults_false`, `backward_compat_missing_kill_switch_field` |
| `helper.rs` | `is_valid_interface_*` (already tested in config.rs; port to helper) |

### Integration tests (manual during review)

1. **F1:** First run (delete credentials.toml + GNOME Keyring entry) → wizard appears →
   enter valid PIA credentials → wizard completes → main window active → server list loads
2. **F3:** Click kill switch toggle → polkit prompt appears → authenticate → nft table created
   (`nft list tables | grep pia_kill_switch`)
3. **F4:** Repeat F1 with GNOME Keyring running → verify `secret-tool lookup application vex-vpn`
   contains credentials; verify credentials.toml is deleted
4. **F4 fallback:** Kill GNOME Keyring service → credentials.toml written with 0600
5. **F5:** Connect VPN → notification appears; disconnect → notification appears; force service
   failure → Critical notification appears
6. **F6 Prefs:** Open Preferences (Ctrl+,) → change DNS provider → close → relaunch → verify
   persisted
7. **F6 Shortcuts:** Open Keyboard Shortcuts (Ctrl+?) → window shows correct sections

### Build validation (per copilot-instructions.md)

```bash
nix develop --command cargo clippy -- -D warnings
nix develop --command cargo build
nix develop --command cargo test
nix develop --command cargo build --release
nix build
```

---

## 11. Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| `oo7` 0.3 exact feature flags unknown | Medium | Low | Verify on docs.rs; if async runtime mismatch, use `tokio::task::block_in_place` wrapper |
| `oo7` pulls zbus 4.x → compile time increase | High | Low | Acceptable; both versions coexist safely in Cargo |
| `adw::SwitchRow` not in v1_4 feature | Low | Medium | Already confirmed added in libadwaita 1.4; Cargo.toml already has `features = ["v1_4"]` |
| `pkexec` path differs on non-NixOS | Medium | Medium | `helper_path()` fallback searches PATH; document non-NixOS install |
| `nft` not in PATH inside helper | Low | High | The helper runs as root; `/usr/sbin/nft` is the fallback; use absolute path |
| Secret Service daemon unavailable on minimal NixOS | High | Low | Plaintext fallback is the designed path; user sees no difference |
| Onboarding wizard shown over partially-loaded main window | Low | Low | Main window shows loading state gracefully; wizard is modal |
| `notify-rust` daemon unavailable (no notification daemon) | Medium | Low | Already wrapped in warn!; not a crash condition |
| Polkit action path differs between debug and release Nix builds | Medium | Medium | Use `substitute` in `postInstall` to bake in the correct store path |
| Config schema: `kill_switch_enabled` vs live kill switch state mismatch | Medium | Medium | `kill_switch_enabled` in Config is for "was enabled at last app close"; live state comes from poll via nft; keep them separate |

---

## 12. Implementation Notes for Phase 2

1. **F3 helper binary safety:** Add `nft` call via `std::process::Command::new("/usr/sbin/nft")`
   (absolute path) rather than relying on PATH, since the helper runs as root and PATH may differ.
   Validate interface name again inside the helper before any exec.

2. **oo7 API discrepancy:** The exact method signatures of `oo7 0.3` must be confirmed from
   `cargo doc --open` after adding the dependency. Key uncertainty: `create_item` signature
   (label, attributes, secret, replace) vs `store(label, attributes, secret)`.

3. **Onboarding: `adw::Carousel` construction:** Use
   `adw::Carousel::builder().allow_scroll_wheel(false).interactive(false).build()`.
   Navigation is button-driven only; swipe/wheel is disabled to prevent accidental page skips.

4. **PreferencesWindow: DNS ComboRow:** Use `gtk4::StringList::new(&["pia", "google", "cloudflare"])`
   as the model. Set `selected` from `Config::load().dns_provider` index.
   Connect `notify::selected` signal to update config.

5. **ShortcutsWindow XML:** Place in `assets/shortcuts.ui` and reference via
   `include_str!("../assets/shortcuts.ui")` from `src/ui.rs`. The `../` is relative to the
   crate root during compilation, which is the workspace root — confirmed correct for Rust's
   `include_str!` macro.

6. **Kill switch toggle in dashboard vs PreferencesWindow:** The existing dashboard toggle
   (`kill_switch_sw` in `LiveWidgets`) calls `dbus::apply_kill_switch`. After F3, these call
   sites must be updated to `helper::apply_kill_switch`. The PreferencesWindow toggle does the same.
   Ensure both code paths call the same function so state stays consistent.

7. **`src/bin/helper.rs` vs `src/helper.rs`:** Cargo treats `src/bin/helper.rs` as the
   vex-vpn-helper executable. The existing `src/helper.rs` is a module of the vex-vpn GUI crate.
   These are different files serving different purposes — confirm during implementation that
   Cargo.toml `[[bin]] path = "src/bin/helper.rs"` correctly separates them.

8. **Config `kill_switch_enabled` field:** This field records the user's preference (for restoring
   on next launch), not the live kill switch state. The live state comes from `poll_once` via
   `check_kill_switch()` in `state.rs`. Do not conflate the two.
