{
  description = "PIA VPN GUI for NixOS — Rust/GTK4 frontend for the WireGuard-based PIA systemd service";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, crane }:
    let
      # NixOS module — works on all systems
      nixosModule = { config, lib, pkgs, ... }:
        import ./nix/module.nix { inherit config lib pkgs self; };

    in flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "clippy" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Build inputs required to compile gtk4-rs and libadwaita bindings
        nativeBuildInputs = with pkgs; [
          pkg-config
          wrapGAppsHook4
          gobject-introspection
        ];

        buildInputs = with pkgs; [
          gtk4
          libadwaita
          glib
          gdk-pixbuf
          pango
          cairo
          atk
          dbus
          openssl
        ];

        commonArgs = {
          src = craneLib.cleanCargoSource (craneLib.path ./.);
          inherit nativeBuildInputs buildInputs;
          # GTK4 needs GI_TYPELIB_PATH at build time for gobject-introspection
          PKG_CONFIG_PATH = pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" buildInputs;
        };

        # Build dependencies separately for faster rebuilds (Crane pattern)
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        pia-gui = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "pia-gui";

          postInstall = ''
            # Desktop entry
            mkdir -p $out/share/applications
            cat > $out/share/applications/pia-gui.desktop << EOF
            [Desktop Entry]
            Type=Application
            Name=Private Internet Access
            Comment=PIA VPN client for NixOS
            Exec=pia-gui
            Icon=network-vpn
            Categories=Network;VPN;
            StartupNotify=true
            EOF

            # Systemd user service (auto-start the GUI on login)
            mkdir -p $out/lib/systemd/user
            cat > $out/lib/systemd/user/pia-gui.service << EOF
            [Unit]
            Description=PIA VPN GUI
            After=graphical-session.target

            [Service]
            Type=simple
            ExecStart=%h/.nix-profile/bin/pia-gui
            Restart=on-failure
            RestartSec=3

            [Install]
            WantedBy=graphical-session.target
            EOF
          '';
        });

      in {
        packages = {
          inherit pia-gui;
          default = pia-gui;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ pia-gui ];
          packages = with pkgs; [
            rustToolchain
            rust-analyzer
            cargo-watch
            cargo-expand
          ];
          # Make GTK introspection available during `cargo run`
          shellHook = ''
            export GI_TYPELIB_PATH=${pkgs.libadwaita}/lib/girepository-1.0:${pkgs.gtk4}/lib/girepository-1.0
            export GSK_RENDERER=cairo
          '';
        };

        checks = {
          inherit pia-gui;
          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          fmt = craneLib.cargoFmt { src = craneLib.path ./.; };
        };
      }
    ) // {
      # Export NixOS module at the top level (not system-specific)
      nixosModules.default = nixosModule;
      nixosModules.pia-gui = nixosModule;
    };
}
