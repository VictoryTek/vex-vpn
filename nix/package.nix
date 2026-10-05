# vex-vpn package — built with crane over the given nixpkgs' own Rust
# toolchain, so `overlays.default` and the NixOS module build against the
# consumer's nixpkgs (vexos-nix: NixOS 26.05) without extra overlays.
{ pkgs, crane, src }:

let
  craneLib = crane.mkLib pkgs;

  commonArgs = {
    src = pkgs.lib.cleanSourceWith {
      src = craneLib.path src;
      filter = path: type:
        (craneLib.filterCargoSources path type)
        || builtins.match ".*\\.(ui|svg|png|gresource\\.xml)$" path != null
        || builtins.match ".*/tests/fixtures/.*\\.json$" path != null;
    };
    strictDeps = true;

    nativeBuildInputs = with pkgs; [
      pkg-config
      wrapGAppsHook4
      glib # glib-compile-resources for build.rs
    ];
    buildInputs = with pkgs; [
      gtk4
      libadwaita
      glib
      dbus
    ];
  };

  cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
    pname = "vex-vpn-deps";
    version = "0.1.0";
  });

  desktopItem = pkgs.makeDesktopItem {
    name = "vex-vpn";
    desktopName = "Vex VPN";
    genericName = "VPN";
    comment = "PIA VPN and kill switch for vexos";
    exec = "vex-vpn";
    icon = "vex-vpn";
    categories = [ "Network" ];
    keywords = [ "vpn" "pia" "wireguard" "openvpn" "kill switch" ];
    startupNotify = true;
    # GTK sets the Wayland app_id / X11 WM_CLASS to the application id.
    startupWMClass = "com.vex.vpn.nixos";
  };
in
craneLib.buildPackage (commonArgs // {
  inherit cargoArtifacts;
  pname = "vex-vpn";

  passthru = { inherit cargoArtifacts commonArgs craneLib; };

  postInstall = ''
    install -Dm644 assets/icons/hicolor/256x256/apps/vex-vpn.png \
      $out/share/icons/hicolor/256x256/apps/vex-vpn.png
    for icon in network-vpn-symbolic network-vpn-disabled-symbolic \
                network-vpn-acquiring-symbolic network-vpn-no-route-symbolic; do
      install -Dm644 assets/icons/hicolor/symbolic/apps/$icon.svg \
        $out/share/icons/hicolor/symbolic/apps/$icon.svg
    done
    install -Dm644 ${desktopItem}/share/applications/vex-vpn.desktop \
      $out/share/applications/vex-vpn.desktop
  '';

  meta = {
    description = "GTK4/libadwaita GUI for the vexos-vpn PIA backend";
    homepage = "https://github.com/victorytek/vex-vpn";
    license = pkgs.lib.licenses.mit;
    mainProgram = "vex-vpn";
    platforms = pkgs.lib.platforms.linux;
  };
})
