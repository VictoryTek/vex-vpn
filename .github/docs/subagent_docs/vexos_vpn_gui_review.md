# Review: vexos_vpn_gui

Spec: `.github/docs/subagent_docs/vexos_vpn_gui_spec.md`
Date: 2026-10-03
Result: **PASS**

## Scope

- **Deleted (20):**
  - `src/backend/{mod,openvpn,wireguard}.rs`, `src/parser/{mod,openvpn,wireguard}.rs`
  - `src/{profile,ui_profiles,ui_import,helper,config,ui_prefs,history}.rs`, `src/bin/helper.rs`
  - `nix/{module-gui,module-vpn}.nix`, `nix/polkit-vex-vpn.policy`, `module.nix`
  - `assets/ca.rsa.4096.crt`, `tests/config_integration.rs`
- **New:**
  - `src/{vexos,cli,ui_regions,ui_settings,ui_login}.rs`
  - `nix/{package,module}.nix`
  - `tests/{backend_contract,login_stdin}.rs`, `tests/fixtures/*.json` (7)
- **Rewritten / modified:**
  - `src/{dbus,state,tray,ui,main,lib}.rs`
  - `Cargo.toml`, `Cargo.lock` (prune only: `configparser`)
  - `flake.nix`, `flake.lock`
  - `assets/shortcuts.ui`, `README.md`

## Findings fixed during review

| # | Severity | Finding | Fix |
|---|---|---|---|
| 1 | CRITICAL | The user's "Done when" list requires switching region (including Automatic) and protocol **from the tray**. The spec's tray only showed the current region. | Region submenu (radio group: Automatic, the 10 fastest plus the current region, and "All regions…") and Protocol submenu. Spec §5 updated. |
| 2 | CRITICAL | `nix build` failed: `login_stdin` tests hit `ETXTBSY`. A fake-pkexec script is written while a parallel test forks. | Spawning tests are serialized with a static `tokio::sync::Mutex`. This is test-only; the production pkexec is never a freshly written file. |
| 3 | RECOMMENDED | One failed status poll blanked the dashboard ("LOADING") and reset `prev_state`, which could drop a connect/disconnect notification. | The last good status is kept, `poll_error` is shown in the banner, and `prev_state` is retained. |
| 4 | RECOMMENDED | Connect (window and tray) opened the sign-in dialog even when credentials are sops-managed, where sign-in can only fail. | `AppState::can_sign_in()` requires `credentials_editable`. |
| 5 | MINOR | Re-selecting the current region or protocol re-ran the setter oneshot, which restarts the tunnel. | No-op guard in the Regions page and the tray radio groups. |
| 6 | MINOR | `run_oneshot` could read a stale `failed` ActiveState left by a previous run before the new job dispatched. | It now waits for the systemd Job object returned by StartUnit to disappear, then reads ActiveState. |

## Checklist

- **Spec compliance.** Every row of the user's full-control table is reachable:
  - Sign in / Change / Sign out / Test login: `ui_login.rs` (pkexec)
  - Connect / Disconnect / Reconnect: dashboard and tray (`StartUnit` / `StopUnit` / `RestartUnit`)
  - Region (including auto): Regions page and tray (`vexos-vpn-region@<id>`)
  - Protocol: Settings and tray (`vexos-vpn-protocol@<proto>`)
  - Kill switch: Settings and tray (start/stop `vexos-killswitch.service`); polkit `AUTH_ADMIN_KEEP` via `AllowInteractiveAuth`; cancel → `UnitError::Cancelled` toast
  - Refresh: Settings and the Regions empty state (pkexec)
  - Read-only autoconnect and mode: the Settings "System" group
  - `credentials_editable == false`: account buttons insensitive, with the reason given
  - `logged_in == false`: dashboard leads with the "Sign in to PIA" StatusPage
  - `last_error` banner with a "Sign in" action for missing credentials
  - Backend missing: the "vexos-vpn backend not installed" page with "Check again"
- **Security.**
  - Credentials reach the child only on stdin; this is proven by `tests/login_stdin.rs`, which checks argv and env.
  - The payload and owned strings are volatile-wiped, and the entry rows are cleared on response.
  - `Credentials` has no `Debug` or `Display`, and the only log line is the action name and exit code.
  - No root in-process, no netlink/nft/iptables, no file writes at all (history and config were dropped per the user's decision).
  - The module declares no polkit rules, system services, or `vexos*` names; this was verified by NixOS eval.
- **Performance.** No blocking work on the GTK thread. All process and D-Bus I/O runs on tokio via `Ctx::run`, and the widgets re-render on the broadcast. The poll runs every 2 s while the window is visible and every 10 s otherwise, and an action triggers an immediate re-poll.
- **Consistency.** It keeps the existing CSS and branding, the sidebar pattern, `anyhow`/`tracing`, the zbus 3 proxies, and the ksni 0.2 tray. No new dependencies; the locked versions are unchanged.
- **API currency.**
  - libadwaita 0.5/`v1_4`: `MessageDialog` (deprecated only from 1.6), `Banner` 1.3, `SwitchRow` 1.4, `PasswordEntryRow` 1.2.
  - tokio process stdin/EOF pattern per its docs; crane `mkLib`/`buildPackage` per its docs.
- **Known limitations (documented in code):**
  - GTK/GLib internal copies of the entry text are outside the app's control.
  - The tray depends on an SNI host (GNOME AppIndicator extension, DMS/Noctalia on Hyprland).
  - Pre-existing: `assets/shortcuts.ui` lists Ctrl+Return and Ctrl+S, which are not wired. Untouched apart from removing the deleted Preferences entry.

## Build validation (all inside `nix develop --command`)

| Command | Result |
|---|---|
| `cargo build` | OK, 0 warnings |
| `cargo clippy --all-targets -- -D warnings` | OK |
| `cargo fmt` | applied; `--check` runs in preflight |
| `cargo test` | 20 passed (2 unit + 12 backend_contract + 6 login_stdin) |
| `nix build` | OK; runs the same 20 tests in the sandbox. Output: `bin/vex-vpn`, `share/applications/vex-vpn.desktop` (`Categories=Network`, `Icon=vex-vpn`, `StartupWMClass=com.vex.vpn.nixos`), hicolor scalable/256/symbolic icons |
| `nix flake show` | `packages.x86_64-linux.default`, `overlays.default`, `nixosModules.default` (plus `devShells` and `checks`) |
| NixOS eval of `nixosModules.default` | `tray.autostart = true` → `systemd.user.services.vex-vpn-tray`, `ExecStart = …/vex-vpn --tray`, `wantedBy = graphical-session.target`; `false` → no service; the package is in `systemPackages`; zero `vexos*` units declared |

Not verifiable here, since this machine has no vexos-vpn (`/run/current-system/sw/bin/vexos-vpn` is absent): the on-device "Done when" items. The vexos desktop, stateless polkit prompt and sign-in-connects flows need a manual run on a vexos host.

## Scores

| Category | Score | Grade |
|----------|-------|-------|
| Specification Compliance | 96% | A |
| Best Practices | 92% | A- |
| Functionality | 93% | A |
| Code Quality | 92% | A- |
| Security | 96% | A |
| Performance | 94% | A |
| Consistency | 93% | A |
| Build Success | 100% | A+ |

**Overall Grade: A (95%)**
