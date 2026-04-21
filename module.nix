{ config, lib, pkgs, self, ... }:

let
  cfg = config.services.pia-gui;
  vpnCfg = config.services.pia-vpn;
in

with lib;

{
  options.services.pia-gui = {
    enable = mkEnableOption "PIA VPN GUI — GTK4/Rust frontend for the pia-vpn WireGuard service";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.system}.pia-gui;
      description = "The pia-gui package to use.";
    };

    autostart = mkOption {
      type = types.bool;
      default = false;
      description = ''
        Whether to autostart the GUI on graphical login via a systemd user service.
        Requires a graphical session (GNOME, KDE, etc.).
      '';
    };

    killSwitch = {
      enable = mkEnableOption "network kill switch — block all traffic if VPN tunnel drops";

      allowedInterfaces = mkOption {
        type = types.listOf types.str;
        default = [ "lo" ];
        description = ''
          Additional interfaces allowed to pass traffic even when kill switch is active.
          The VPN interface itself is always allowed. Loopback is allowed by default.
          Add your LAN interface here if you want LAN access to survive a VPN drop.
        '';
      };

      allowedAddresses = mkOption {
        type = types.listOf types.str;
        default = [];
        description = ''
          IP addresses or CIDR ranges to always allow, even with the kill switch active.
          Useful for allowing the WireGuard handshake endpoint through.
        '';
      };
    };

    dns = {
      provider = mkOption {
        type = types.enum [ "pia" "google" "cloudflare" "custom" ];
        default = "pia";
        description = "DNS provider to use when connected.";
      };

      customServers = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Custom DNS servers to use when provider is set to 'custom'.";
      };
    };
  };

  config = mkIf cfg.enable {

    # Ensure the pia-vpn backend service is available
    # (user is expected to also enable services.pia-vpn from tadfisher's module)
    assertions = lib.mkIf cfg.enable [
      {
        assertion = config.services.pia-vpn.enable or false;
        message = ''
          services.pia-gui requires services.pia-vpn to be enabled.
          Add the pia-vpn module from github:tadfisher/flake and set:
            services.pia-vpn.enable = true;
        '';
      }
    ];

    # Install the GUI package system-wide
    environment.systemPackages = [ cfg.package ];

    # Kill switch via nftables
    # This declarative ruleset is separate from the runtime toggle in the GUI.
    # The GUI's toggle calls nft at runtime; this ensures the base policy is in place.
    networking.nftables.enable = mkIf cfg.killSwitch.enable true;

    networking.nftables.tables.pia_kill_switch = mkIf cfg.killSwitch.enable {
      family = "inet";
      content = let
        iface = vpnCfg.interface or "wg0";
        allowedIfaces = [ iface ] ++ cfg.killSwitch.allowedInterfaces;
        ifaceRules = concatMapStrings (i: "    oifname \"${i}\" accept\n    iifname \"${i}\" accept\n") allowedIfaces;
        addrRules = concatMapStrings (a: "    ip daddr ${a} accept\n") cfg.killSwitch.allowedAddresses;
      in ''
        chain output {
          type filter hook output priority 0; policy drop;
          ct state established,related accept
          ${ifaceRules}
          ${addrRules}
        }
        chain input {
          type filter hook input priority 0; policy drop;
          ct state established,related accept
          ${ifaceRules}
        }
      '';
    };

    # DNS configuration based on provider setting
    networking.nameservers = mkIf (cfg.dns.provider != "pia") (
      {
        google = [ "8.8.8.8" "8.8.4.4" ];
        cloudflare = [ "1.1.1.1" "1.0.0.1" ];
        custom = cfg.dns.customServers;
      }.${cfg.dns.provider}
    );

    # PIA's own DNS (only active when connected) — override the pia-vpn module's
    # hardcoded 8.8.8.8 with PIA's DNS server
    # This patches the pia-vpn service to use PIA DNS when provider = "pia"
    systemd.services.pia-vpn = mkIf (cfg.dns.provider == "pia") {
      serviceConfig.Environment = [
        "PIA_DNS=10.0.0.241"
      ];
    };

    # Autostart GUI for all users via systemd user service template
    systemd.user.services.pia-gui = mkIf cfg.autostart {
      description = "PIA VPN GUI";
      after = [ "graphical-session.target" ];
      wantedBy = [ "graphical-session.target" ];
      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/pia-gui";
        Restart = "on-failure";
        RestartSec = 3;
        Environment = [
          "GSK_RENDERER=cairo"
        ];
      };
    };

    # Policy-kit rule so the GUI can control pia-vpn.service without sudo
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (
          action.id == "org.freedesktop.systemd1.manage-units" &&
          action.lookup("unit") in [
            "pia-vpn.service",
            "pia-vpn-portforward.service"
          ] &&
          subject.isInGroup("wheel")
        ) {
          return polkit.Result.YES;
        }
      });
    '';

    # Allow users in the 'wheel' group to run nft for kill switch management
    security.sudo.extraRules = [
      {
        groups = [ "wheel" ];
        commands = [
          {
            command = "${pkgs.nftables}/bin/nft";
            options = [ "NOPASSWD" ];
          }
        ];
      }
    ];

    # Make wg available without sudo for reading transfer stats
    # wg show is read-only and safe to run as user
    security.wrappers.wg = {
      source = "${pkgs.wireguard-tools}/bin/wg";
      capabilities = "cap_net_admin+pe";
      owner = "root";
      group = "wheel";
      permissions = "u+rx,g+rx";
    };
  };
}
