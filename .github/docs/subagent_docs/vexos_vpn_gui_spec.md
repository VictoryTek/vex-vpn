# Spec: vex-vpn → GUI for the vexos-vpn backend

Feature name: `vexos_vpn_gui`
Status: **Phase 1 approved — decisions in §10**
Date: 2026-10-03

---

## 1. Summary

vex-vpn stops being a "universal VPN client" with its own backends, parsers,
profiles, root helper and kill switch. It becomes a thin, unprivileged
GTK4/libadwaita front end for **vexos-vpn**, the PIA backend that ships in
vexos-nix (`modules/vpn.nix`, `pkgs/vexos-vpn/vexos-vpn.sh`, read at commit
`b70f25a`).

The GUI:

- reads state from `vexos-vpn status --json` and `vexos-vpn regions --json`
- drives units through systemd D-Bus (`StartUnit` / `StopUnit` / `RestartUnit`), where vexos-nix's polkit rules allow the `users` group
- runs `pkexec /run/current-system/sw/bin/vexos-vpn {login --stdin|logout|selftest|refresh}` for root-only actions, so polkit shows the admin password prompt
- never touches the network, firewall or VPN configs, never stores credentials, and never runs as root

The `vexos-vpn` CLI and the `just vpn-*` recipes stay as the backup.

---

## 2. Current state

| Area | Files | Fate |
|---|---|---|
| VPN backends (wg-quick / NM) | `src/backend/{mod,openvpn,wireguard}.rs` | **delete** |
| Config parsers | `src/parser/{mod,openvpn,wireguard}.rs` | **delete** |
| Profile model + UI | `src/profile.rs`, `src/ui_profiles.rs`, `src/ui_import.rs` | **delete** |
| Root helper | `src/bin/helper.rs`, `src/helper.rs`, `[[bin]] vex-vpn-helper` | **delete** |
| systemd + NetworkManager D-Bus | `src/dbus.rs` (NM proxies, wg-quick helpers) | **rewrite** (systemd only) |
| Poll loop / watchers | `src/state.rs` (NM watcher, wg watchdog, profile status) | **rewrite** |
| Main window | `src/ui.rs` (sidebar, round connect button, history page, CSS) | **rework**, keep branding; history page removed |
| Tray | `src/tray.rs` (ksni 0.2) | **rework**. The current icon and menu never refresh, because `Handle::update` is never called. |
| Config | `src/config.rs` (profiles, NM `auto_reconnect`, `kill_switch_service`) | **delete**, see Q2 |
| Preferences | `src/ui_prefs.rs` (unwired toggles, a no-op log level) | **delete**, see Q2 |
| History | `src/history.rs` (JSONL under `$XDG_STATE_HOME`) | **delete** (Q1) |
| Nix | `nix/module-vpn.nix`, `nix/polkit-vex-vpn.policy`, `nix/module-gui.nix` (iptables kill switch, polkit, `wg` wrapper), `module.nix` (stale stub) | delete / rewrite (see §6) |
| Flake | nixos-unstable, flake-utils (includes darwin), rust-overlay toolchain, helper and polkit install | rewrite (see §6) |
| Tests | `tests/config_integration.rs` (profiles/TOML) | **replace** |
| Asset | `assets/ca.rsa.4096.crt` (no longer referenced by any code) | **delete** |

Locked crate versions, which stay unchanged: gtk4 0.7.3 (`v4_10`), libadwaita 0.5.3 (`v1_4`), glib 0.18.5, zbus 3.15.2, tokio 1.52.1, ksni 0.2.2, serde_json 1.0.149.

---

## 3. Backend contract (verified against vexos-vpn.sh)

### 3.1 `vexos-vpn status --json` (unprivileged)

It is built by `cmd_status` from `/run/vexos-vpn/status.json` merged with live fields. Details that matter to the parser:

- `state` **can be missing.** If the daemon has never run, `status.json` does not exist and the base object is `{}`. A missing state means `disconnected`.
- If the unit is not active, a stale state becomes `disconnected`, except `error`, which stays visible while systemd waits to retry (`RestartSec=10`).
- These fields are written with `jq --arg`, so they are always **strings** when present, and may be empty: `state`, `protocol`, `region`, `region_name`, `server_cn`, `endpoint_ip`, `interface`, `since` (from `date -Iseconds`, for example `2026-10-03T12:00:00+02:00`), `last_error`, `region_setting` (the last of these is set by the daemon).
- These fields are always present:
  - `killswitch` (bool), `killswitch_mode` (`off|manual|always`)
  - `rx_bytes`, `tx_bytes` (numbers; 0 unless the interface exists)
  - `protocol_setting`, `region_setting` (strings)
  - `logged_in`, `credentials_editable`, `autoconnect` (bools)
- Missing credentials show up as `logged_in: false` and, after a connect attempt, `state: "error"` with `last_error` starting `"no PIA credentials"` or `"credentials file is malformed"`.

Rust model (`src/vexos.rs`, exported from `lib.rs` for tests):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VpnState { #[default] Disconnected, Connecting, Connected, Error,
                    #[serde(other)] Unknown }   // future-proof; rendered like Disconnected

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KillSwitchMode { Off, #[default] Manual, Always }

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Status {
    pub state: VpnState,
    pub protocol: String, pub region: String, pub region_name: String,
    pub server_cn: String, pub endpoint_ip: String, pub interface: String,
    pub since: String, pub last_error: String,
    pub rx_bytes: u64, pub tx_bytes: u64,
    pub killswitch: bool, pub killswitch_mode: KillSwitchMode,
    pub protocol_setting: String, pub region_setting: String,
    pub logged_in: bool, pub credentials_editable: bool, pub autoconnect: bool,
}
impl Status {
    pub fn parse(json: &str) -> anyhow::Result<Self>;
    /// logged_in == false, or last_error names missing/malformed credentials.
    pub fn needs_sign_in(&self) -> bool;
}
```

### 3.2 `vexos-vpn regions --json` (unprivileged)

It returns `[{id, name, country, port_forward, latency_s|null}]`, sorted by name, with offline regions removed.

If there is no cached server list, it **exits 1** with `no cached server list yet (connect once, or: sudo vexos-vpn refresh)` on stderr. The GUI shows this as an empty state with a "Refresh server list" button.

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct Region { pub id: String, pub name: String, pub country: String,
                    #[serde(default)] pub port_forward: bool,
                    pub latency_s: Option<f64> }
pub fn parse_regions(json: &str) -> anyhow::Result<Vec<Region>>;
pub fn is_valid_unit_arg(id: &str) -> bool;   // ^[a-z0-9_-]+$, same as the polkit regex
```

### 3.3 Actions

| Action | Mechanism | Auth |
|---|---|---|
| Connect / Disconnect / Reconnect | `StartUnit` / `StopUnit` / `RestartUnit("vexos-vpn.service","replace")` | none (polkit YES) |
| Region | `StartUnit("vexos-vpn-region@<id>.service")`, where `auto` means Automatic | none |
| Protocol | `StartUnit("vexos-vpn-protocol@{wireguard,openvpn}.service")` | none |
| Kill switch on | `StartUnit("vexos-killswitch.service")` | none |
| Kill switch off | `StopUnit("vexos-killswitch.service")` | none in `manual`; `AUTH_ADMIN_KEEP` prompt in `always` |
| Sign in / Change | `pkexec vexos-vpn login --stdin` with `"user\npass\n"` on stdin | admin prompt |
| Sign out | `pkexec vexos-vpn logout` (removes the credentials and stops the VPN) | admin prompt |
| Test login | `pkexec vexos-vpn selftest` (stdout holds OK/FAILED lines only) | admin prompt |
| Refresh server list | `pkexec vexos-vpn refresh` | admin prompt |

All D-Bus calls use `MethodFlags::AllowInteractiveAuth`. This is already done in the current `dbus.rs` and is verified against zbus 3.15.

`StartUnit` only *enqueues* a job (systemd D-Bus API, `man org.freedesktop.systemd1`). Region and protocol are `Type=oneshot` units, so failures would otherwise be invisible. After starting one, the GUI polls that unit's `ActiveState` every 250 ms, for up to 30 s, until it leaves `activating`. If it ends up `failed`, the GUI shows a toast: "Couldn't change region — see `journalctl -u vexos-vpn-region@<id>`".

D-Bus error classification (`dbus::UnitError`):

- `org.freedesktop.DBus.Error.AccessDenied` or `…InteractiveAuthorizationRequired` → **Cancelled**: an info toast, and the switch state is re-synced from status. It is not an error dialog.
- `org.freedesktop.systemd1.NoSuchUnit` → **NotInstalled**. For the kill switch, the row is hidden when `killswitch_mode == off`, so this is only a safety net.
- Anything else → **Failed(String)**: an error toast.

pkexec outcome (`man pkexec`: 127 means not authorized, an error, or no agent; 126 means the dialog was dismissed):

```rust
pub enum PrivOutcome { Ok { stdout: String },
                       Cancelled,                         // exit 126 / 127
                       Failed { code: i32, stdout: String, stderr: String } }
```

On 127, if stderr contains `No authentication agent`, the GUI shows "No polkit authentication agent is running" rather than a silent "cancelled". vexos GNOME and Hyprland (`hyprpolkitagent`) both run an agent, so this is only a safety net.

---

## 4. Credential handling (security-critical)

`src/cli.rs` holds a `Privileged { pkexec: PathBuf, cli: PathBuf }`.

- Defaults: `/run/wrappers/bin/pkexec` and `/run/current-system/sw/bin/vexos-vpn`.
- Tests inject fake paths.

`login(user: &str, pass: &str)`:

1. **Validate first.**
   - Reject empty values and any `\n`, `\r` or `\0`, since these would break vexos-vpn's line protocol.
   - The dialog disables "Sign in" until both fields are valid.
2. **Build the command:** `Command::new(pkexec).arg(cli).args(["login","--stdin"])`, with stdin, stdout and stderr all piped.
   - **No credential ever goes into argv or env.** The command is built before the payload exists.
   - `kill_on_drop(false)`, so a login is never killed halfway.
3. **Build the payload:** `Vec<u8>` with exact capacity (so it never reallocates and leaves copies), then `user\npass\n`.
4. **Write and close:** `write_all`, then drop stdin to send EOF, then `wait_with_output()`. This follows the tokio `process` docs pattern verified via Context7.
5. **Zero the buffers:** after writing (on success *and* on error), overwrite the payload and the owned `String` copies with `std::ptr::write_volatile` followed by `compiler_fence(SeqCst)`. This is a 6-line `wipe()` helper, so no new dependency is needed.
6. **Clear the dialog:** in the UI, clear the entry rows' text the moment their values are read, then close the dialog.
7. **Keep secrets out of logs:** the credential carrier has no `Debug` and no `Display`. `tracing` lines record only the action name and exit code, never stdin, and never the stdout of `login`.

Known limit, to be documented in code: copies held inside GTK and GLib (the entry's text buffer and the `GString` returned by `text()`) are outside our control. We clear the widget and drop the `GString` immediately.

**After a successful sign-in:**

- `vexos-vpn login` already runs `systemctl try-restart vexos-vpn.service`.
- If the user was previously **signed out** (`logged_in == false`), the GUI also calls `StartUnit("vexos-vpn.service")`. This is the path that clears the error banner and connects without a terminal (see Q4).
- "Change PIA account" does not start anything extra.

---

## 5. Application architecture

```
main.rs ── tokio Runtime ── state::poll_loop ──► AppState (Arc<RwLock>) ──► broadcast<()>
   │                          ▲   (status --json, 2 s visible / 10 s hidden;            │
   │                          │    poke() after every action → immediate re-poll)       │
   │                          │                                                         ├─► tray (ksni Handle::update)
   └─ adw::Application ── ui.rs window ── ui_regions.rs / ui_settings.rs / ui_login.rs ─┘
                                 │
                                 └─ actions: dbus.rs (systemd) · cli.rs (pkexec / status / regions)
```

### Modules

| File | Responsibility |
|---|---|
| `src/vexos.rs` (new) | `Status`, `Region`, enums, parsing, `needs_sign_in`, `is_valid_unit_arg`, `Rates::update(prev, now, dt)` for rx/tx bytes per second (resets when the counter goes down or the interface changes). Pure, so it is unit-testable. |
| `src/cli.rs` (new) | `find_backend()`: looks up `vexos-vpn` on `PATH`, falling back to `/run/current-system/sw/bin`. `status()` and `regions()` use `tokio::process` with a 5 s timeout. Also holds the `Privileged` runner, `login`, `logout`, `selftest`, `refresh`, and `wipe`. |
| `src/dbus.rs` (rewrite) | systemd proxies only: `start_unit`, `stop_unit`, `restart_unit`, `wait_oneshot`, `UnitError`. NetworkManager code removed. |
| `src/state.rs` (rewrite) | `AppState { backend_present, status: Option<Status>, rates, regions: Option<Result<Vec<Region>,String>>, poll_error }`. The poll loop reads visibility from a `tokio::sync::watch<bool>` and is woken early by a `Notify` after each action. It refreshes regions on the transition into `connected`, because latency is filled in after an auto connect. It keeps the desktop notifications (connected / disconnected / error). |
| `src/ui.rs` (rework) | Window, sidebar (Dashboard, Regions, Settings) and CSS. The dashboard has the round connect toggle (the existing branded button acts as the "large connect switch"). It also shows: state pill, region and protocol, "Connected since HH:MM · 1h 5m" (parsed with `glib::DateTime::from_iso8601`, so no new dependency), live ↓/↑ rate cards, a kill-switch chip, a Reconnect button, an `adw::Banner` for `last_error` (with a **Sign in** button when `needs_sign_in`), and a prominent "Sign in to PIA" `adw::StatusPage` call to action when `logged_in == false`. When the backend is missing, it shows a "vexos-vpn backend not installed" StatusPage with a **Check again** button. |
| `src/ui_regions.rs` (new) | A `gtk::SearchEntry` plus a `ListBox` of `adw::ActionRow`s, filtering on name, country and id. "Automatic (fastest)" is pinned at the top. Each row shows latency ("42 ms" or nothing) and a checkmark on `region_setting`. Activating a row starts the region unit. When the server list is missing, an empty state offers "Refresh server list". |
| `src/ui_settings.rs` (new) | `adw::PreferencesPage` groups. **Connection:** a protocol `ComboRow` (WireGuard / OpenVPN), with programmatic updates guarded so they don't re-fire. **Kill switch:** `SwitchRow` plus a mode explanation; hidden with a note when mode is `off`; in `always` mode the subtitle reads "Turning it off asks for your password and lasts until reboot". **Account:** signed in/out, Sign in / Change / Sign out / Test login; when `credentials_editable == false`, these are disabled with "Credentials are managed by your vexos NixOS config (sops)". **Server list:** Refresh. **System (read-only):** autoconnect and kill-switch mode, each with the note "Set in your vexos NixOS config". |
| `src/ui_login.rs` (new) | The sign-in / change dialog: `adw::MessageDialog` with an `EntryRow` for the username and a `PasswordEntryRow`. Also the sign-out confirmation, which warns that internet access stays blocked if the kill switch is on. Also the selftest result dialog, which shows stdout. libadwaita 0.5 / `v1_4` has `MessageDialog`. `AlertDialog` needs 1.5 and a crate bump, so it is not used. |
| `src/tray.rs` (rework) | The tray keeps a *snapshot*, pushed with `Handle::update(\|t\| t.snap = …)` on each broadcast (this fixes the never-refreshing icon, and removes `block_on`). Icon per state: connected → `network-vpn-symbolic`, connecting → `-acquiring-`, error → `-no-route-`, otherwise `-disabled-`. Menu, top to bottom: a disabled status label; Connect/Disconnect; a kill switch `CheckmarkItem` (omitted when the mode is `off`); "Region: <name>", which opens the window on the Regions page; Open vex-vpn; Quit. Actions run on the main tokio handle. |
| `src/main.rs` (rework) | `vex-vpn --tray` starts without a window and calls `app.hold()`. The flag is stripped from argv before `app.run_with_args`, so GApplication doesn't reject it. A second launch from the menu is forwarded by GApplication uniqueness (`com.vex.vpn.nixos`) and presents the window. When started with `--tray`, the window is `hide_on_close`. The visibility watch is fed from the window's `map` and `unmap` signals. |
| `src/lib.rs` | `pub mod vexos; pub mod cli;`. These are GTK-free, so tests link fast. |

Nothing blocks the GTK main thread. All I/O runs as `tokio` tasks, and widgets are updated from `glib::spawn_future_local`. This follows the existing pattern: `rt.enter()` guard plus awaiting tokio futures from glib futures, which already works in this codebase.

Busy handling: while an action is in flight, its controls are insensitive and show a spinner. The UI re-syncs from the next status poll, never from optimistic guesses.

---

## 6. Nix

### 6.1 `flake.nix` (rewrite)

```nix
inputs = {
  nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";   # same branch as vexos-nix
  crane.url   = "github:ipetkov/crane";               # crane has no nixpkgs input any more; nothing to follow
};
outputs = { self, nixpkgs, crane }:
  let
    systems = [ "x86_64-linux" "aarch64-linux" ];
    forAll  = nixpkgs.lib.genAttrs systems;
    mkVexVpn = pkgs: import ./nix/package.nix { inherit pkgs crane; src = ./.; };
  in {
    packages    = forAll (s: rec { vex-vpn = mkVexVpn nixpkgs.legacyPackages.${s}; default = vex-vpn; });
    overlays.default = final: _prev: { vex-vpn = mkVexVpn final; };
    nixosModules.default = import ./nix/module.nix { inherit mkVexVpn; };
    devShells = forAll (s: …);   # nixpkgs cargo/rustc/clippy/rustfmt/rust-analyzer + inputsFrom package
    checks    = forAll (s: { package; clippy; fmt; });
  };
```

- **flake-utils is dropped.** It generated darwin outputs that can never build.
- **rust-overlay is dropped** (see Q3). The package is built with crane over the *consumer's* nixpkgs Rust toolchain:
  - that is the only way `overlays.default` can build against `final` without forcing rust-overlay onto vexos-nix
  - the devshell uses the same toolchain, so preflight `cargo` and `nix build` agree
- Crane's `mkLib pkgs` is verified via Context7. `buildPackage` defaults to `cargoExtraArgs = "--locked"` and `doCheck = true`, so `nix build` also runs `cargo test`.
- **Crane inputs:** this crane version may not take a `nixpkgs` input. If the locked crane still has one, keep `inputs.nixpkgs.follows = "nixpkgs"`.
- **Phase 2 check:** run `nix flake metadata` to verify which inputs exist.

### 6.2 `nix/package.nix` (new, replacing the inline `postInstall`)

- `nativeBuildInputs`: `pkg-config`, `wrapGAppsHook4`, `copyDesktopItems`, `glib`
- `buildInputs`: `gtk4`, `libadwaita`, `glib`, `dbus`
- Install:
  - `bin/vex-vpn`
  - `share/icons/hicolor/scalable/apps/vex-vpn.svg` and `256x256/apps/vex-vpn.png`
  - the 4 symbolic tray icons under `hicolor/symbolic/apps`
- `makeDesktopItem`:
  - `name = "vex-vpn"`, `exec = "vex-vpn"`, `icon = "vex-vpn"`, `desktopName = "Vex VPN"`
  - `comment = "PIA VPN and kill switch for vexos"`, `categories = [ "Network" ]`
  - `startupWMClass = "com.vex.vpn.nixos"`, so GNOME and Hyprland match the running window to the launcher entry
- The `src` filter keeps Cargo sources plus `*.svg`, `*.png`, `*.ui` and `*.gresource.xml`. `.crt` and `.policy` are dropped.
- No helper, no polkit file, no `lib/systemd/user` unit.

### 6.3 `nix/module.nix` (replaces `module-gui.nix`, `module-vpn.nix`, root `module.nix`)

```nix
{ mkVexVpn }:
{ config, lib, pkgs, ... }:
let cfg = config.programs.vex-vpn; pkg = mkVexVpn pkgs; in {
  options.programs.vex-vpn = {
    enable = lib.mkEnableOption "vex-vpn, the GTK GUI for vexos-vpn";
    tray.autostart = lib.mkOption { type = lib.types.bool; default = true;
      description = "Start the vex-vpn tray icon with the graphical session."; };
  };
  config = lib.mkIf cfg.enable (lib.mkMerge [
    { environment.systemPackages = [ pkg ]; }
    (lib.mkIf cfg.tray.autostart {
      systemd.user.services.vex-vpn-tray = {
        description = "vex-vpn tray";
        partOf   = [ "graphical-session.target" ];
        after    = [ "graphical-session.target" ];
        wantedBy = [ "graphical-session.target" ];
        serviceConfig = { ExecStart = "${pkg}/bin/vex-vpn --tray"; Restart = "on-failure"; RestartSec = 3; };
      };
    })
  ]);
}
```

What this module does and does not do:

- **Builds with the importing system's own nixpkgs** (`mkVexVpn pkgs`), so it does not depend on the overlay being applied.
- **Declares nothing else:** no polkit rules, no system services, no firewall, and nothing named `vexos-vpn*` or `vexos-killswitch*`.

Why a systemd user service and not XDG autostart:

- vexos GNOME reaches `graphical-session.target`.
- vexos Hyprland runs under **UWSM** (`modules/hyprland-desktop.nix`, `withUWSM = true`, default session `hyprland-uwsm`), so it reaches that target too.
- Plain XDG autostart is not processed by Hyprland itself.

Tray hosts: GNOME has the `appindicatorsupport` extension enabled in vexos; DMS and Noctalia provide an SNI host on Hyprland.

### 6.4 Other

- **`Cargo.toml`:**
  - Remove the `vex-vpn-helper` bin.
  - Remove the now-unused dependencies: `configparser`, `uuid`, `async-trait`, `libc`, `thiserror` (currently unused), `toml` (if Q2 is accepted), `futures-util` (no more signal streams).
  - Update `description`.
  - **No dependency is added.**
  - `Cargo.lock` changes only by pruning, which the first `nix develop --command cargo build` does offline. `cargo update` is never run.
- **`build.rs` and the gresource bundle:** unchanged.
- **README:** rewritten.
  - vex-vpn is the GUI for vexos-vpn, and the CLI / `just vpn-*` is the backup.
  - Covers the vexos-nix flake input with `follows`, `programs.vex-vpn.enable` and `tray.autostart`, and a feature tour.
  - The tadfisher/flake, `services.vex-vpn` and nftables sections are dropped.

---

## 7. Tests

### `tests/backend_contract.rs`

This replaces `config_integration.rs`. Fixtures in `tests/fixtures/` were hand-derived from `cmd_status` / `cmd_regions` jq output:

| Fixture | Covers |
|---|---|
| `status_connected.json` | `state=connected`, all string fields, rx/tx, killswitch true, `mode=manual` |
| `status_connecting.json` | `connecting`, `region_setting=auto` |
| `status_disconnected.json` | **no `state` key** (the daemon never ran) → `Disconnected` |
| `status_error_no_credentials.json` | `error`, `last_error="no PIA credentials — run: sudo vexos-vpn login"`, `logged_in=false` → `needs_sign_in()` |
| `status_sops.json` | `credentials_editable=false`, `logged_in=true`, `mode=always`, `autoconnect=true` |
| `status_unknown_state.json` | a future `state` value → `Unknown`, parses without error |
| `regions.json` | mixed `latency_s` number and **`null`**, `port_forward` true/false |

There are also tests for `is_valid_unit_arg` (accepts `auto`, `us_east`, `de-frankfurt`; rejects `../x`, `a b`, `A`, empty) and for `Rates::update` (normal, counter reset, interface change).

### `tests/login_stdin.rs`

This uses `#[tokio::test]` (tokio `full` already includes the macros) and a fake pkexec, a `/bin/sh` script written to a tempdir. `/bin/sh` exists in the Nix build sandbox, so this also runs under `nix build`. The script records `"$@"` to `argv`, `env` to `env`, and `cat` to `stdin`, then exits with a configurable code.

- `login("alice","s3cr3t-pw")`:
  - `stdin == "alice\ns3cr3t-pw\n"`
  - argv is exactly `[<cli>, "login", "--stdin"]`
  - neither `alice` nor `s3cr3t-pw` appears anywhere in argv or env
- exit 126 → `Cancelled`; exit 127 → `Cancelled`; exit 1 with stderr → `Failed{stderr}`; exit 0 with stdout (selftest) → `Ok{stdout}`
- usernames or passwords containing `\n` or `\r` are rejected **before** spawning; asserted because no `argv` file is created

The existing `format_bytes` unit test is kept (moved to `vexos.rs`).

---

## 8. Implementation steps (Phase 2)

1. **Delete** the files in §2 marked delete: `src/backend/`, `src/parser/`, `profile.rs`, `ui_profiles.rs`, `ui_import.rs`, `src/bin/helper.rs`, `src/helper.rs`, `config.rs`, `ui_prefs.rs`, `history.rs`, `nix/module-vpn.nix`, `nix/polkit-vex-vpn.policy`, `nix/module-gui.nix`, `module.nix`, `assets/ca.rsa.4096.crt`, `tests/config_integration.rs`.
   - Use `git rm`? **No.** CLAUDE.md forbids staging, and `rm -rf` is forbidden.
   - Files are removed with plain `rm <file>` (directories via `rm <files>` then `rmdir`). The user stages the deletions.
   - → verify: `git status` shows only the expected `D` entries.
2. Add `vexos.rs` and `cli.rs` plus their tests. → verify: `nix develop --command cargo test --test backend_contract --test login_stdin`.
3. Rewrite `dbus.rs` and `state.rs`; rework `main.rs` and `tray.rs`. → verify: `cargo build` in the devshell.
4. Add the UI: `ui.rs` rework, `ui_regions.rs`, `ui_settings.rs`, `ui_login.rs`. → verify: `cargo build` plus `cargo clippy -- -D warnings`.
5. Nix: `flake.nix`, `nix/package.nix`, `nix/module.nix`, and update `flake.lock` for the input changes (`nix flake lock`). → verify:
   - `nix build`
   - `nix flake show` lists `packages.x86_64-linux.default`, `overlays.default`, `nixosModules.default`
   - `ls result/share/applications result/share/icons/hicolor/*/apps`
6. Rewrite the README and update `Cargo.toml` metadata. → verify: `bash scripts/preflight.sh` exits 0.

### Commands approved for Phases 2–6

All `cargo` commands run inside the devshell:

- `nix develop --command cargo build`, `cargo build --release`, `cargo test`, `cargo fmt` / `cargo fmt --check`, `cargo clippy -- -D warnings`
- `nix build`, `nix flake show`, `nix flake metadata`
- `nix flake lock`: needed because inputs change. It only re-locks changed inputs.
- `bash scripts/preflight.sh`

Not used: bare `cargo …`, `nix build --rebuild`, `rm -rf`, `cargo update`, any git write.

---

## 9. Risks and mitigations

| Risk | Mitigation |
|---|---|
| The nixpkgs 26.05 rustc/clippy is newer than the old rust-overlay pin, so new clippy lints appear | Fix them in Phase 2; the zero-warning gate stays. |
| gtk4-rs 0.7 / libadwaita-rs 0.5 against the GTK and libadwaita C libraries in 26.05 | The C ABI is backwards-compatible and the features are capped at `v4_10` / `v1_4`. `nix build` is the proof. |
| `--locked` fails after dependency removal | The devshell `cargo build` prunes `Cargo.lock` before `nix build`, and preflight runs in that order. |
| A user dismisses polkit vs a real failure | 126/127 → Cancelled toast; the "no agent" stderr is special-cased; the UI always re-syncs from status. |
| No polkit agent / no SNI host on some future session | An error message names the cause; the window still works without the tray. |
| The region/protocol oneshot fails silently | `wait_oneshot` polls `ActiveState` and shows a toast on `failed`. |
| Credentials left in memory | Payload and owned strings are volatile-wiped; widgets are cleared at once; the GTK-internal copy is documented as out of scope. |
| The kill switch blocks the sign-in / refresh network path | It is handled by vexos-vpn itself (root-only bootstrap holes); the GUI only shows stderr. |
| `GSK_RENDERER=cairo` was set in the old user service | Dropped. If rendering issues show up on vexos hardware, add it to the module's service `Environment`. |
| The project docs (`CLAUDE.md` Project Context, `.github/copilot-instructions.md`, `docs/PROJECT_ANALYSIS.md`) still describe the helper, profiles and NM | Not edited in this change; see the follow-up in §11. |

---

## 10. Decisions (approved by user 2026-10-03)

- **Q1 — History: DROPPED.** `src/history.rs` and the History page are deleted; vex-vpn writes no files at all.
- **Q2 — Preferences window + `config.rs`: DELETED**, along with the "Preferences" menu entry and the `toml` dependency.
- **Q3 — rust-overlay: DROPPED.** The nixpkgs Rust toolchain is used for both the package and the devshell.
- **Q4 — First sign-in connects: YES** (required by "Signing in from the GUI connects"). "Change PIA account" never auto-connects.

## 11. Out of scope / follow-ups

- **Project docs.** `CLAUDE.md` "Project Context" (two binaries, helper and polkit), `.github/copilot-instructions.md` and `docs/PROJECT_ANALYSIS.md` become outdated. I'll update the factual lines in a follow-up if you want.
- **vexos-nix side.** Adding the `vex-vpn` input and `programs.vex-vpn.enable` to vexos-nix `modules/vpn.nix` is that repo's change, not this one.
- **inotify watcher on `/run/vexos-vpn/status.json`.** Polling at 2 s / 10 s meets the requirement; inotify can be added later if needed.

## 12. Sources

1. vexos-nix `pkgs/vexos-vpn/vexos-vpn.sh` @ b70f25a: the status/regions JSON shape, login `--stdin`, and exit behavior
2. vexos-nix `modules/vpn.nix` @ b70f25a: unit names, polkit rules (`AUTH_ADMIN_KEEP` in `always` mode), config keys
3. vexos-nix `modules/hyprland-desktop.nix` / `home/dank-material-shell.nix`: UWSM session and `hyprpolkitagent`
4. `pkexec(1)` man page: exit codes 126 (dismissed) and 127 (not authorized / error)
5. `org.freedesktop.systemd1(5)` man page: `StartUnit` enqueues a job; mode strings
6. tokio docs (Context7 `/websites/rs_tokio_tokio`): piped stdin, `write_all` + drop for EOF, `wait_with_output`, `kill_on_drop`
7. crane docs (Context7 `/ipetkov/crane`): `mkLib pkgs`, `buildPackage` defaults (`--locked`, `doCheck`)
8. libadwaita docs (Context7): `PasswordEntryRow` 1.2, `Banner` 1.3, `SwitchRow` / `NavigationSplitView` 1.4, `MessageDialog` 1.2 (deprecated in 1.6, which is fine for `v1_4`)
9. ksni 0.2.2 docs (docs.rs): `TrayService::handle`, `Handle::update<R, F: FnOnce(&mut T) -> R>`
