# GUI module for vex-vpn — GTK4/Rust frontend for the pia-vpn WireGuard service.
# This module is designed to be used alongside nix/module-vpn.nix (or via the
# combined nixosModules.default which imports both).
{ config, lib, pkgs, self, ... }:

let
  cfg = config.services.vex-vpn;
  # vpnCfg is only referenced inside mkIf cfg.enable blocks so it is safe even if
  # services.pia-vpn is not in scope (standalone vex-vpn usage gets caught by the
  # assertion below).
  vpnCfg = config.services.pia-vpn;
in

with lib;

{
  options.services.vex-vpn = {
    enable = mkEnableOption "vex-vpn GUI — GTK4/Rust frontend for the pia-vpn WireGuard service";

    package = mkOption {
      type = types.package;
      default = self.packages.${pkgs.system}.vex-vpn;
      description = "The vex-vpn package to use.";
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
          Additional interfaces allowed to pass traffic even when the kill switch is active.
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
        description = ''
          DNS provider to use when the VPN is connected.
          "pia" uses PIA's own DNS (10.0.0.241), which is the recommended default.
        '';
      };

      customServers = mkOption {
        type = types.listOf types.str;
        default = [];
        description = "Custom DNS servers to use when provider is set to 'custom'.";
      };
    };
  };

  config = mkIf cfg.enable {

    # Guard: if this module is imported without the VPN module, give a clear error.
    assertions = [
      {
        assertion = config.services.pia-vpn.enable or false;
        message = ''
          services.vex-vpn requires services.pia-vpn to be enabled.
          Use nixosModules.default (which includes both) or add nixosModules.pia-vpn
          to your imports and set services.pia-vpn.enable = true.
        '';
      }
    ];

    # Install the GUI package system-wide.
    environment.systemPackages = [ cfg.package ];

    # Wire the GUI's dns.provider choice into the VPN module's dnsServers option.
    # This overrides the default PIA DNS only if the user explicitly chose otherwise.
    services.pia-vpn.dnsServers = {
      pia        = [ "10.0.0.241" "10.0.0.242" ];
      google     = [ "8.8.8.8" "8.8.4.4" ];
      cloudflare = [ "1.1.1.1" "1.0.0.1" ];
      custom     = cfg.dns.customServers;
    }.${cfg.dns.provider};

    # Kill switch via nftables (declarative ruleset, separate from the runtime toggle).
    networking.nftables.enable = mkIf cfg.killSwitch.enable true;

    networking.nftables.tables.pia_kill_switch = mkIf cfg.killSwitch.enable {
      family = "inet";
      content =
        let
          # Wire interface directly from vpn module config.
          iface = vpnCfg.interface;
          allowedIfaces = [ iface ] ++ cfg.killSwitch.allowedInterfaces;
          ifaceRules = concatMapStrings
            (i: "    oifname \"${i}\" accept\n    iifname \"${i}\" accept\n")
            allowedIfaces;
          addrRules = concatMapStrings
            (a: "    ip daddr ${a} accept\n")
            cfg.killSwitch.allowedAddresses;
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

    # Autostart GUI for the graphical session via systemd user service.
    systemd.user.services.vex-vpn = mkIf cfg.autostart {
      description = "vex-vpn GUI";
      after = [ "graphical-session.target" ];
      wantedBy = [ "graphical-session.target" ];
      serviceConfig = {
        Type = "simple";
        ExecStart = "${cfg.package}/bin/vex-vpn";
        Restart = "on-failure";
        RestartSec = 3;
        Environment = [ "GSK_RENDERER=cairo" ];
      };
    };

    # Policy-kit rule so the GUI can control pia-vpn.service without sudo.
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

    # Allow users in 'wheel' to run specific nft commands for kill-switch management.
    # Narrowed from full nft access to only the two commands the GUI actually uses.
    security.sudo.extraRules = [
      {
        groups = [ "wheel" ];
        commands = [
          {
            command = "${pkgs.nftables}/bin/nft -f -";
            options = [ "NOPASSWD" ];
          }
          {
            command = "${pkgs.nftables}/bin/nft delete table inet pia_kill_switch";
            options = [ "NOPASSWD" ];
          }
        ];
      }
    ];

    # wg show is read-only; give it cap_net_admin so users can read transfer stats.
    security.wrappers.wg = {
      source = "${pkgs.wireguard-tools}/bin/wg";
      capabilities = "cap_net_admin+pe";
      owner = "root";
      group = "wheel";
      permissions = "u+rx,g+rx";
    };
  };
}
