# GUI module for vex-vpn — GTK4/Rust universal VPN client.
# This module is designed to be used alongside nix/module-vpn.nix (or via the
# combined nixosModules.default which imports both).
{ config, lib, pkgs, self, ... }:

let
  cfg = config.services.vex-vpn;
in

with lib;

{
  options.services.vex-vpn = {
    enable = mkEnableOption "vex-vpn GUI — GTK4/Rust universal VPN client";

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

      vpnInterface = mkOption {
        type = types.str;
        default = "wg0";
        description = ''
          The VPN interface to allow through the kill switch (e.g. "wg0" for
          WireGuard or the tun interface name for OpenVPN).
        '';
      };

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
          Useful for allowing the VPN handshake endpoint through.
        '';
      };
    };
  };

  config = mkIf cfg.enable {

    # Install the GUI package system-wide.
    environment.systemPackages = [ cfg.package ];

    # Expose /libexec so pkexec can find vex-vpn-helper via the system profile.
    environment.pathsToLink = [ "/libexec" ];

    # Kill switch via nftables (declarative ruleset, separate from the runtime toggle).
    networking.nftables.enable = mkIf cfg.killSwitch.enable true;

    networking.nftables.tables.vex_kill_switch = mkIf cfg.killSwitch.enable {
      family = "inet";
      content =
        let
          allowedIfaces = [ cfg.killSwitch.vpnInterface ] ++ cfg.killSwitch.allowedInterfaces;
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

    # Polkit rule: allow members of 'wheel' group to manage vex-vpn systemd units
    # without a password prompt.
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (
          action.id == "org.freedesktop.systemd1.manage-units" &&
          action.lookup("unit") == "vex-vpn.service" &&
          subject.isInGroup("wheel")
        ) {
          return polkit.Result.YES;
        }
      });
    '';

    # Install the polkit action file for vex-vpn-helper (nftables kill switch).
    environment.etc."polkit-1/actions/org.vex-vpn.helper.policy".source =
      "${cfg.package}/share/polkit-1/actions/org.vex-vpn.helper.policy";

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
