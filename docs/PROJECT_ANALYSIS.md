# vex-vpn — Project Analysis

vex-vpn is the GTK4/libadwaita GUI for **vexos-vpn**, the PIA VPN and kill
switch that ships in vexos-nix. This document describes how the app is put
together. The source of truth for the backend contract is vexos-nix
`modules/vpn.nix` and `pkgs/vexos-vpn/vexos-vpn.sh`. The design and review for
this rework are in `.github/docs/subagent_docs/vexos_vpn_gui_spec.md` and
`vexos_vpn_gui_review.md`.

An earlier version of this file reviewed the previous multi-provider client
(own backends, profiles, root helper, NetworkManager). That code is gone; see
git history before the `vexos_vpn_gui` change if you need it.

## Principles

- **Unprivileged.** vex-vpn runs as the normal user. It never touches the
  network, firewall or VPN configs, and writes no files.
- **No credentials at rest.** PIA credentials are typed into a dialog and
  written only to the stdin of `pkexec vexos-vpn login --stdin`. They never go
  into argv, the environment, logs or disk, and vex-vpn's own buffers are wiped
  afterwards.
- **vexos-vpn owns the VPN.** The GUI reads its state and asks systemd (or
  pkexec) to act. It never reimplements any of it.

## How it talks to the backend

| Need | Mechanism |
|---|---|
| Read state | `vexos-vpn status --json` and `regions --json` (unprivileged). Polled every 2 s while the window is visible and every 10 s otherwise; an action triggers an immediate re-poll. |
| Connect / disconnect / reconnect | `StartUnit` / `StopUnit` / `RestartUnit` on `vexos-vpn.service` |
| Region, protocol | `vexos-vpn-region@<id>.service`, `vexos-vpn-protocol@<proto>.service` (oneshot; completion is tracked through the systemd Job) |
| Kill switch | start / stop `vexos-killswitch.service`. In `always` mode, stop triggers a polkit admin prompt. A dismissed prompt is shown as "cancelled", not an error. |
| Sign in, sign out, test login, refresh server list | `pkexec vexos-vpn login --stdin` / `logout` / `selftest` / `refresh`. pkexec exit 126 or 127 means cancelled. |

The polkit rules that allow the unit actions for the `users` group are shipped
by vexos-nix, not by this repository.

## Code map

| Path | Role |
|---|---|
| `src/vexos.rs` | serde model of the status and regions JSON, the connect state, byte-rate maths (library crate, no GTK) |
| `src/cli.rs` | finds and runs `vexos-vpn`; the pkexec runner and `Credentials` (library crate) |
| `src/dbus.rs` | zbus 3 proxies for systemd Manager, Unit and Job; error classification (`UnitError`) |
| `src/state.rs` | `AppState`, the shared `Ctx`, the poll loop, desktop notifications |
| `src/ui.rs` | window, sidebar, dashboard (connect toggle, sign-in call to action, error banner, live rates) |
| `src/ui_regions.rs`, `src/ui_settings.rs`, `src/ui_login.rs` | region list, settings and account page, sign-in/out dialogs and pkexec actions |
| `src/tray.rs` | ksni tray with connect, kill switch, region and protocol menus |
| `nix/package.nix`, `nix/module.nix`, `flake.nix` | package (desktop file and icons), module (`programs.vex-vpn.*` only), flake outputs |
| `tests/` | `backend_contract.rs` (JSON fixtures in `tests/fixtures/`) and `login_stdin.rs` (fake pkexec: credentials on stdin only) |

## Threads and state

The GTK main thread owns every widget. One Tokio runtime runs the poll loop,
D-Bus calls and subprocesses; the UI awaits them through `Ctx::run` from
`glib::spawn_future_local`, so nothing blocks the main loop. The ksni tray
runs on its own thread and is refreshed with `Handle::update` from the poll's
broadcast channel. Shared state is `Arc<RwLock<AppState>>`.

## Known limitations

- GTK and GLib keep their own internal copy of the text in the password
  entry. vex-vpn clears the entry and drops the value immediately, but cannot
  wipe GTK's copy.
- The tray needs a StatusNotifier host: the GNOME AppIndicator extension, or
  the shell on Hyprland (DankMaterialShell, Noctalia).
- The tray autostart is a systemd user service bound to
  `graphical-session.target`, so it assumes a session that reaches that target
  (GNOME, or Hyprland under UWSM).
- `assets/shortcuts.ui` lists Ctrl+Return and Ctrl+S, which are not wired up.
- The on-device flows (connect, region, protocol, kill switch, sign-in with the
  polkit prompt, the stateless password prompt) can only be exercised on a
  vexos host that has `vexos-vpn`.
