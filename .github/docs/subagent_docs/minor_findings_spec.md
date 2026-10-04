# Minor findings — spec

## Items
1. `.gitignore` lists `flake.lock`, which is tracked → remove the line.
2. pkexec exit 127 is mapped to `Cancelled`, but pkexec also exits 127 with
   "No such file or directory" when `vexos-vpn` is missing → in `Privileged::run`,
   only treat 126/127 as `Cancelled` when stderr does not contain that text;
   otherwise fall through to `Failed` (shows pkexec's message). Add a test.
3. Kill switch on (Settings switch, tray item) cuts the network instantly when
   the VPN is not connected → show an `AdwMessageDialog` ("Turn on kill switch?",
   Cancel / Turn on) when state != Connected. Dialog lives in
   `ui_settings::enable_kill_switch`; tray sends new `TrayMessage::EnableKillSwitch`
   which activates the window and its new `win.enable-kill-switch` action.
   Turning it off is unchanged. No new dependencies.
4. Fixtures are hand-written → needs a real `vexos-vpn status|regions --json`
   capture from a machine with the backend; not possible here. Not changed.

## Validation
`bash scripts/preflight.sh` (fmt, clippy -D warnings, build, test, release, nix build).

## Risks
Switch must not stay "on" after Cancel → `ctx.poke()` re-syncs it from status.
