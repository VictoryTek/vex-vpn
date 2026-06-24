# GUI module for vex-vpn — GTK4/Rust universal VPN client.
# This module is designed to be used alongside nix/module-vpn.nix (or via the
# combined nixosModules.default which imports both).
{ config, lib, pkgs, self, ... }:

let
  cfg = config.services.vex-vpn;

  iptables  = "${pkgs.iptables}/bin/iptables";
  ip6tables = "${pkgs.iptables}/bin/ip6tables";

  ks-start = pkgs.writeShellScript "vex-vpn-ks-start" ''
    # Create kill switch chains (flush if they already exist).
    ${iptables}  -N VEX_KS_OUT 2>/dev/null || ${iptables}  -F VEX_KS_OUT
    ${iptables}  -N VEX_KS_IN  2>/dev/null || ${iptables}  -F VEX_KS_IN
    ${ip6tables} -N VEX_KS_OUT 2>/dev/null || ${ip6tables} -F VEX_KS_OUT
    ${ip6tables} -N VEX_KS_IN  2>/dev/null || ${ip6tables} -F VEX_KS_IN

    # ── OUTPUT rules ─────────────────────────────────────────────────────────
    ${iptables}  -A VEX_KS_OUT -o lo                                         -j ACCEPT
    ${iptables}  -A VEX_KS_OUT -m conntrack --ctstate ESTABLISHED,RELATED    -j ACCEPT
    ${iptables}  -A VEX_KS_OUT -p udp --dport 67                             -j ACCEPT  # DHCP
    ${iptables}  -A VEX_KS_OUT -p udp --dport 1194                           -j ACCEPT  # OpenVPN UDP
    ${iptables}  -A VEX_KS_OUT -p tcp --dport 443                            -j ACCEPT  # OpenVPN/WG TCP
    ${iptables}  -A VEX_KS_OUT -p udp --dport 51820                          -j ACCEPT  # WireGuard
    ${iptables}  -A VEX_KS_OUT -o tun+                                       -j ACCEPT
    ${iptables}  -A VEX_KS_OUT -o wg+                                        -j ACCEPT
    ${iptables}  -A VEX_KS_OUT -o nordlynx  -j ACCEPT 2>/dev/null            || true
    ${iptables}  -A VEX_KS_OUT -o tailscale0 -j ACCEPT 2>/dev/null           || true
    ${iptables}  -A VEX_KS_OUT                                               -j DROP

    ${ip6tables} -A VEX_KS_OUT -o lo                                         -j ACCEPT
    ${ip6tables} -A VEX_KS_OUT -m conntrack --ctstate ESTABLISHED,RELATED    -j ACCEPT
    ${ip6tables} -A VEX_KS_OUT -o tun+                                       -j ACCEPT
    ${ip6tables} -A VEX_KS_OUT -o wg+                                        -j ACCEPT
    ${ip6tables} -A VEX_KS_OUT -o nordlynx  -j ACCEPT 2>/dev/null            || true
    ${ip6tables} -A VEX_KS_OUT -o tailscale0 -j ACCEPT 2>/dev/null           || true
    ${ip6tables} -A VEX_KS_OUT                                               -j DROP

    # ── INPUT rules ──────────────────────────────────────────────────────────
    ${iptables}  -A VEX_KS_IN -i lo                                          -j ACCEPT
    ${iptables}  -A VEX_KS_IN -m conntrack --ctstate ESTABLISHED,RELATED     -j ACCEPT
    ${iptables}  -A VEX_KS_IN -p udp --dport 68                              -j ACCEPT  # DHCP
    ${iptables}  -A VEX_KS_IN -i tun+                                        -j ACCEPT
    ${iptables}  -A VEX_KS_IN -i wg+                                         -j ACCEPT
    ${iptables}  -A VEX_KS_IN -i nordlynx  -j ACCEPT 2>/dev/null             || true
    ${iptables}  -A VEX_KS_IN -i tailscale0 -j ACCEPT 2>/dev/null            || true
    ${iptables}  -A VEX_KS_IN                                                -j DROP

    ${ip6tables} -A VEX_KS_IN -i lo                                          -j ACCEPT
    ${ip6tables} -A VEX_KS_IN -m conntrack --ctstate ESTABLISHED,RELATED     -j ACCEPT
    ${ip6tables} -A VEX_KS_IN -i tun+                                        -j ACCEPT
    ${ip6tables} -A VEX_KS_IN -i wg+                                         -j ACCEPT
    ${ip6tables} -A VEX_KS_IN -i nordlynx  -j ACCEPT 2>/dev/null             || true
    ${ip6tables} -A VEX_KS_IN -i tailscale0 -j ACCEPT 2>/dev/null            || true
    ${ip6tables} -A VEX_KS_IN                                                -j DROP

    # ── Hook into built-in chains ─────────────────────────────────────────────
    ${iptables}  -I OUTPUT 1 -j VEX_KS_OUT
    ${iptables}  -I INPUT  1 -j VEX_KS_IN
    ${ip6tables} -I OUTPUT 1 -j VEX_KS_OUT
    ${ip6tables} -I INPUT  1 -j VEX_KS_IN
  '';

  ks-stop = pkgs.writeShellScript "vex-vpn-ks-stop" ''
    ${iptables}  -D OUTPUT -j VEX_KS_OUT 2>/dev/null || true
    ${iptables}  -D INPUT  -j VEX_KS_IN  2>/dev/null || true
    ${ip6tables} -D OUTPUT -j VEX_KS_OUT 2>/dev/null || true
    ${ip6tables} -D INPUT  -j VEX_KS_IN  2>/dev/null || true

    ${iptables}  -F VEX_KS_OUT 2>/dev/null || true
    ${iptables}  -F VEX_KS_IN  2>/dev/null || true
    ${ip6tables} -F VEX_KS_OUT 2>/dev/null || true
    ${ip6tables} -F VEX_KS_IN  2>/dev/null || true

    ${iptables}  -X VEX_KS_OUT 2>/dev/null || true
    ${iptables}  -X VEX_KS_IN  2>/dev/null || true
    ${ip6tables} -X VEX_KS_OUT 2>/dev/null || true
    ${ip6tables} -X VEX_KS_IN  2>/dev/null || true
  '';
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

      serviceName = mkOption {
        type = types.str;
        default = "vex-vpn-killswitch";
        description = ''
          Name of the systemd service used to manage the kill switch.
          Set to "vpn-kill-switch" on vexos-nix to use the system-provided
          service instead of the vex-vpn-managed one.
        '';
      };

      vpnInterface = mkOption {
        type = types.str;
        default = "tun0";
        description = ''
          The VPN interface to allow through the kill switch (e.g. "tun0" for
          OpenVPN via NetworkManager, or "wg0" for a WireGuard profile).
          The systemd service already allows all tun+ and wg+ prefixes, so
          this option is retained for documentation purposes.
        '';
      };

      allowedInterfaces = mkOption {
        type = types.listOf types.str;
        default = [ "lo" ];
        description = ''
          Additional interfaces allowed to pass traffic even when the kill switch
          is active. Loopback is always allowed. The systemd service already
          allows tun+ and wg+ via prefix matching.
        '';
      };

      allowedAddresses = mkOption {
        type = types.listOf types.str;
        default = [];
        description = ''
          IP addresses or CIDR ranges to always allow, even with the kill switch
          active. Useful for allowing the VPN handshake endpoint through.
        '';
      };
    };
  };

  config = mkIf cfg.enable {

    # Install the GUI package system-wide.
    environment.systemPackages = [ cfg.package ];

    # Expose /libexec so pkexec can find vex-vpn-helper via the system profile.
    environment.pathsToLink = [ "/libexec" ];

    # Kill switch systemd service (iptables-based, works on any firewall backend).
    systemd.services.vex-vpn-killswitch = mkIf cfg.killSwitch.enable {
      description = "vex-vpn network kill switch (iptables)";
      after = [ "network.target" ];
      # wantedBy is intentionally empty — the app toggles this at runtime.
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${ks-start}";
        ExecStop  = "${ks-stop}";
      };
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

    # Polkit rules:
    # 1. Allow wheel users to manage vex-vpn systemd units without a password.
    # 2. Allow active local users (or wheel group) to toggle the kill switch service.
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

      polkit.addRule(function(action, subject) {
        if (
          action.id === "org.freedesktop.systemd1.manage-units" &&
          action.lookup("unit") === "${cfg.killSwitch.serviceName}.service" &&
          (subject.isInGroup("wheel") || (subject.local && subject.active))
        ) {
          return polkit.Result.YES;
        }
      });
    '';

    # Install the polkit action file for vex-vpn-helper.
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
