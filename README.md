# vex-vpn

The desktop app for **vexos-vpn**, the Private Internet Access VPN and kill
switch that ships with [vexos-nix](https://github.com/victorytek/vexos-nix).
It is a native Rust GTK4/libadwaita GUI with a system tray icon.

vex-vpn is only a front end. vexos-vpn owns the tunnel, the firewall and the
credentials. vex-vpn runs as your normal user, never touches the network,
firewall or VPN configuration, and never stores your password. If the GUI is
not available, the `vexos-vpn` command and the `just vpn-*` recipes in
vexos-nix do everything it does.

## Features

- **Connect, disconnect and reconnect**, from the main window or the tray.
- **Regions**: a searchable list with measured latency. "Automatic (fastest)"
  is pinned at the top.
- **Protocol**: WireGuard (recommended) or OpenVPN (backup).
- **Kill switch**: turn it on or off. In `always` mode, turning it off asks
  for your password and lasts until reboot.
- **PIA account**:
  - sign in and change account by typing your username and password into the GUI
  - sign out
  - test login
- **Refresh the PIA server list.**
- **Live status**: state, region, protocol, connected-since time,
  download/upload rate, kill switch state and the last error.
- **Tray icon**:
  - per-state icon
  - connect/disconnect
  - kill switch toggle
  - current region
  - open window

Two settings are shown read-only because they live in your vexos NixOS config
(`vexos.vpn.*`): **connect at boot** and **kill switch mode**.

## How it works

| What | How |
|---|---|
| Read state | `vexos-vpn status --json` and `vexos-vpn regions --json`, as your user. Polled every 2 s while the window is open, every 10 s otherwise. |
| Connect, disconnect, region, protocol, kill switch | systemd units started and stopped over D-Bus: `vexos-vpn.service`, `vexos-vpn-region@<id>.service`, `vexos-vpn-protocol@<proto>.service`, `vexos-killswitch.service`. vexos-nix's polkit rules allow these for the `users` group without a password. |
| Sign in, sign out, test login, refresh server list | `pkexec vexos-vpn login --stdin` / `logout` / `selftest` / `refresh`. Polkit asks for your administrator password. |

When you sign in, the username and password are written only to the stdin of
`pkexec vexos-vpn login --stdin`. They never go on a command line, into an
environment variable, into a log or onto disk. vex-vpn's own copies are wiped
from memory straight after. vexos-vpn stores them in a file only root can read.

If your credentials come from sops-nix (`vexos.vpn.credentialsFile`), the
account buttons are disabled and the GUI says why.

## Installation (vexos-nix)

Add the flake input, following vexos-nix's nixpkgs:

```nix
# flake.nix
inputs.vex-vpn = {
  url = "github:victorytek/vex-vpn";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Then enable it in a module that is imported alongside `modules/vpn.nix`:

```nix
{ inputs, ... }: {
  imports = [ inputs.vex-vpn.nixosModules.default ];

  programs.vex-vpn = {
    enable = true;
    tray.autostart = true;   # default
  };
}
```

`tray.autostart` adds a systemd user service, `vex-vpn-tray`, bound to
`graphical-session.target`. GNOME and Hyprland under UWSM both start it.

The module only installs the app and that user service. The VPN units, kill
switch and polkit rules all come from vexos-nix.

### Flake outputs

| Output | Contents |
|---|---|
| `packages.x86_64-linux.default` | The GUI, with its `.desktop` file (`Categories=Network;`) and icons |
| `overlays.default` | Adds `pkgs.vex-vpn` |
| `nixosModules.default` | `programs.vex-vpn.enable` and `programs.vex-vpn.tray.autostart` |

## Backup: the command line

Everything in the GUI is also available from a terminal:

```bash
vexos-vpn status            # or: just vpn-status
vexos-vpn up | down         # just vpn-up / just vpn-down
vexos-vpn regions           # just vpn-regions
vexos-vpn region <id|auto>  # just vpn-region <id>
vexos-vpn protocol wireguard|openvpn
vexos-vpn killswitch on|off
sudo vexos-vpn login        # just vpn-login
sudo vexos-vpn selftest     # just vpn-selftest
```

## Development

```bash
git clone https://github.com/victorytek/vex-vpn
cd vex-vpn
nix develop                  # nixpkgs Rust toolchain + GTK4/libadwaita
cargo run                    # inside the dev shell
bash scripts/preflight.sh    # fmt, clippy, build, test, release build, nix build
```

The GUI needs a vexos system with `vexos-vpn` installed. Without it, the GUI
shows a "vexos-vpn backend not installed" page.

## License

MIT
