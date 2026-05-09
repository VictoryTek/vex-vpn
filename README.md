# vex-vpn

A native Rust/GTK4 GUI for Private Internet Access VPN on NixOS, built on top of the WireGuard-based systemd backend from [tadfisher/flake](https://github.com/tadfisher/flake/blob/main/nixos/modules/pia-vpn.nix).

## Features

- **Connect / Disconnect** — one-tap control via D-Bus → systemd
- **Kill switch** — nftables-based, toggleable at runtime and declarable in Nix config
- **Port forwarding** — enable/disable `pia-vpn-portforward.service` from the UI
- **Live stats** — rx/tx bytes from WireGuard interface, connected server, external IP
- **System tray** — KStatusNotifierItem tray icon (GNOME, KDE, XFCE, etc.)
- **Settings** — DNS provider, interface name, max latency, server filtering
- **Auto-connect** — systemd user service for graphical session autostart
- **Declarative** — all features expressible in `configuration.nix`

## Stack

| Layer | Technology |
|---|---|
| GUI | GTK4 + libadwaita (gtk4-rs bindings) |
| Async | Tokio |
| D-Bus | zbus (pure Rust) |
| Tray | ksni (KStatusNotifierItem) |
| VPN backend | WireGuard via systemd-networkd |
| Firewall | nftables (kill switch) |
| Build | Crane + Nix flake |

## Installation

### Quick Start (Nix Flake)

Run directly without installing:

```bash
nix run github:victorytek/vex-vpn
```

Install to your Nix profile:

```bash
nix profile add github:victorytek/vex-vpn
```

> **Note:** The quick-start options launch the GUI only. For kill switch, port forwarding, and autostart you need the full NixOS module setup below.

---

### Full NixOS Module Setup

### Step 1 — Add both flakes to your system flake

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

    # The WireGuard/systemd backend (tadfisher's module)
    tadfisher-flake.url = "github:tadfisher/flake";
    tadfisher-flake.inputs.nixpkgs.follows = "nixpkgs";

    # This GUI
    vex-vpn.url = "github:yourname/vex-vpn";  # or path:./vex-vpn
    vex-vpn.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = { nixpkgs, tadfisher-flake, vex-vpn, self }: {
    nixosConfigurations.mymachine = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [
        # Backend service
        "${tadfisher-flake}/nixos/modules/pia-vpn.nix"
        # GUI + NixOS module
        vex-vpn.nixosModules.default
        ./configuration.nix
      ];
    };
  };
}
```

### Step 2 — Configure in configuration.nix

```nix
{ config, ... }: {

  # ── VPN backend (tadfisher's module) ──────────────
  services.pia-vpn = {
    enable = true;
    interface = "wg0";
    maxLatency = 0.1;

    # CA cert from: https://raw.githubusercontent.com/pia-foss/manual-connections/master/ca.rsa.4096.crt
    certificateFile = ./ca.rsa.4096.crt;

    # Create this file with:
    #   echo "PIA_USER=your_username" > /run/secrets/pia
    #   echo "PIA_PASS=your_password" >> /run/secrets/pia
    # Better: use sops-nix or agenix
    environmentFile = "/run/secrets/pia";

    portForward.enable = true;
  };

  # ── GUI ───────────────────────────────────────────
  services.vex-vpn = {
    enable = true;
    autostart = true;   # launch on graphical login

    killSwitch.enable = true;
    killSwitch.allowedInterfaces = [ "lo" "eth0" ];  # allow LAN even when VPN drops

    dns.provider = "pia";  # use PIA's own DNS — overrides the 8.8.8.8 hardcode
  };
}
```

### Step 3 — Get the CA certificate

```bash
curl -o ca.rsa.4096.crt \
  https://raw.githubusercontent.com/pia-foss/manual-connections/master/ca.rsa.4096.crt
```

Place it next to your `configuration.nix`.

### Step 4 — Create credentials file

Using sops-nix (recommended):
```nix
sops.secrets.pia = {
  format = "dotenv";
  # file contents:
  # PIA_USER=your_username
  # PIA_PASS=your_password
};
services.pia-vpn.environmentFile = config.sops.secrets.pia.path;
```

Or manually (less secure):
```bash
sudo mkdir -p /run/secrets
sudo sh -c 'echo "PIA_USER=your_username" > /run/secrets/pia'
sudo sh -c 'echo "PIA_PASS=your_password" >> /run/secrets/pia'
sudo chmod 600 /run/secrets/pia
```

## Development

```bash
git clone https://github.com/yourname/vex-vpn
cd vex-vpn
nix develop          # drops into shell with Rust + GTK4 + all deps
cargo watch -x run   # live reload
```

## Kill Switch Details

The kill switch is implemented as an nftables table (`inet pia_kill_switch`). When enabled:

- All outbound traffic is **dropped by default**
- Traffic on the WireGuard interface (`wg0`) is **allowed**
- Loopback is **allowed**
- Configured `allowedInterfaces` and `allowedAddresses` are **allowed**
- Established/related connections are **allowed** to recover gracefully

The GUI toggle calls `nft` at runtime. The NixOS module option (`killSwitch.enable = true`) makes it declarative and persistent across reboots.

## Architecture

```
┌─────────────────────────────────────────┐
│              vex-vpn (Rust)             │
│                                         │
│  ┌──────────┐  ┌──────────┐            │
│  │  GTK4 UI │  │  Tray    │            │
│  │(libadw)  │  │  (ksni)  │            │
│  └────┬─────┘  └────┬─────┘            │
│       │              │                  │
│  ┌────▼──────────────▼────┐            │
│  │   AppState (Arc<RwLock>)│            │
│  │   + poll loop (Tokio)   │            │
│  └────────────┬────────────┘            │
│               │                         │
│  ┌────────────▼────────────┐            │
│  │   D-Bus (zbus)          │            │
│  │   nft (subprocess)      │            │
│  │   wg show (subprocess)  │            │
│  └────────────┬────────────┘            │
└───────────────┼─────────────────────────┘
                │ systemd D-Bus API
┌───────────────▼─────────────────────────┐
│         pia-vpn.service (systemd)       │
│         pia-vpn-portforward.service     │
│         (tadfisher's WireGuard scripts) │
└─────────────────────────────────────────┘
```

## License

MIT
