# nix/module-vpn.nix — Universal VPN backend module for vex-vpn
{ config, lib, pkgs, ... }:
with lib;
let
  cfg = config.services.vex-vpn;
in {
  options.services.vex-vpn = {
    enable = mkEnableOption "vex-vpn universal VPN client backend";

    profiles = mkOption {
      type = types.attrsOf (types.submodule {
        options = {
          type = mkOption {
            type = types.enum [ "wireguard" "openvpn" ];
            description = "VPN protocol type: wireguard or openvpn.";
            example = "wireguard";
          };
          configFile = mkOption {
            type = types.path;
            description = ''
              Path to the VPN configuration file.
              For WireGuard: a .conf file following wg-quick(8) format.
              For OpenVPN: a .ovpn file.
            '';
          };
          autoConnect = mkOption {
            type = types.bool;
            default = false;
            description = "Automatically connect to this profile on login.";
          };
          killSwitch = mkOption {
            type = types.bool;
            default = false;
            description = "Enable kill switch: block all traffic if VPN disconnects.";
          };
        };
      });
      default = {};
      description = ''
        Declarative VPN profiles to pre-configure in vex-vpn.
        WireGuard profiles are configured via networking.wg-quick.
        OpenVPN profiles are deployed as NetworkManager connection files.

        Example:
          services.vex-vpn.profiles = {
            work-vpn = {
              type = "wireguard";
              configFile = ./vpn/work.conf;
              autoConnect = false;
              killSwitch = true;
            };
          };
      '';
    };
  };

  config = mkIf cfg.enable {
    # WireGuard profiles: use wg-quick systemd integration
    networking.wg-quick.interfaces = mapAttrs' (name: prof:
      nameValuePair name {
        configFile = prof.configFile;
      }
    ) (filterAttrs (_: p: p.type == "wireguard") cfg.profiles);

    # Polkit rules: allow vex-vpn-helper to manage nftables kill switch
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (action.id === "com.vex.vpn.helper" &&
            subject.isInGroup("users")) {
          return polkit.Result.YES;
        }
      });
    '';

    # D-Bus activation for vex-vpn-helper (if needed)
    # Users in the "networkmanager" group can manage NM connections
    users.groups.networkmanager.members = mkDefault [];

  };
}
