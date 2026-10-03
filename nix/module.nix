# NixOS module for vex-vpn, the desktop GUI for vexos-vpn.
#
# Installs the app and (optionally) starts its tray icon with the graphical
# session. Deliberately nothing else: the VPN units, kill switch and polkit
# rules all belong to vexos-nix `modules/vpn.nix`.
{ mkVexVpn }:
{ config, lib, pkgs, ... }:

let
  cfg = config.programs.vex-vpn;
  package = mkVexVpn pkgs;
in
{
  options.programs.vex-vpn = {
    enable = lib.mkEnableOption "vex-vpn, the GTK GUI for vexos-vpn";

    tray.autostart = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = ''
        Start the vex-vpn tray icon with the graphical session (GNOME, or
        Hyprland under UWSM), via a systemd user service bound to
        graphical-session.target.
      '';
    };
  };

  config = lib.mkIf cfg.enable (lib.mkMerge [
    { environment.systemPackages = [ package ]; }

    (lib.mkIf cfg.tray.autostart {
      systemd.user.services.vex-vpn-tray = {
        description = "vex-vpn tray icon";
        partOf = [ "graphical-session.target" ];
        after = [ "graphical-session.target" ];
        wantedBy = [ "graphical-session.target" ];
        serviceConfig = {
          ExecStart = "${package}/bin/vex-vpn --tray";
          Restart = "on-failure";
          RestartSec = 3;
        };
      };
    })
  ]);
}
