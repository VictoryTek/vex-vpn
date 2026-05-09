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
      # ── NixOS modules ───────────────────────────────────────────────────────
      # pia-vpn backend: the WireGuard/systemd service (vendored from tadfisher/flake,
      # with DNS and iproute2 fixes). Can be used standalone without the GUI.
      vpnModule = ./nix/module-vpn.nix;

      # vex-vpn frontend: the GTK4/Rust GUI. Requires pia-vpn to be enabled.
      guiModule = { config, lib, pkgs, ... }:
        import ./nix/module-gui.nix { inherit config lib pkgs self; };

      # Combined module — the recommended entry point for most users.
      # Imports both vpn + gui so users only need one line in their system config.
      combinedModule = { config, lib, pkgs, ... }: {
        imports = [
          vpnModule
          (import ./nix/module-gui.nix { inherit config lib pkgs self; })
        ];
      };

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
          src = let
            certFilter = path: type:
              type == "directory" || builtins.match ".*\\.crt$" path != null;
            srcFilter = path: type:
              (certFilter path type) || (craneLib.filterCargoSources path type);
          in pkgs.lib.cleanSourceWith {
            src = craneLib.path ./.;
            filter = srcFilter;
          };
          inherit nativeBuildInputs buildInputs;
          PKG_CONFIG_PATH = pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" buildInputs;
        };

        # Build dependencies separately for faster rebuilds (Crane pattern)
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          preBuild = ''
            export GI_TYPELIB_PATH=${pkgs.gtk4}/lib/girepository-1.0:${pkgs.libadwaita}/lib/girepository-1.0:${pkgs.glib}/lib/girepository-1.0:${pkgs.pango}/lib/girepository-1.0:${pkgs.cairo}/lib/girepository-1.0:${pkgs.atk}/lib/girepository-1.0:${pkgs.gdk-pixbuf}/lib/girepository-1.0
          '';
        });

        vex-vpn = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "vex-vpn";

          preBuild = ''
            export GI_TYPELIB_PATH=${pkgs.gtk4}/lib/girepository-1.0:${pkgs.libadwaita}/lib/girepository-1.0:${pkgs.glib}/lib/girepository-1.0:${pkgs.pango}/lib/girepository-1.0:${pkgs.cairo}/lib/girepository-1.0:${pkgs.atk}/lib/girepository-1.0:${pkgs.gdk-pixbuf}/lib/girepository-1.0
          '';

          postInstall = ''
            # PIA CA certificate for NixOS module
            mkdir -p $out/share/pia
            cp assets/ca.rsa.4096.crt $out/share/pia/

            # Desktop entry
            mkdir -p $out/share/applications
            cat > $out/share/applications/vex-vpn.desktop << EOF
            [Desktop Entry]
            Type=Application
            Name=Private Internet Access
            Comment=PIA VPN client for NixOS
            Exec=vex-vpn
            Icon=network-vpn
            Categories=Network;VPN;
            StartupNotify=true
            EOF

            # Systemd user service (auto-start the GUI on login)
            mkdir -p $out/lib/systemd/user
            cat > $out/lib/systemd/user/vex-vpn.service << EOF
            [Unit]
            Description=PIA VPN GUI
            After=graphical-session.target

            [Service]
            Type=simple
            ExecStart=%h/.nix-profile/bin/vex-vpn
            Restart=on-failure
            RestartSec=3

            [Install]
            WantedBy=graphical-session.target
            EOF
          '';
        });

      in {
        packages = {
          inherit vex-vpn;
          default = vex-vpn;
        };

        devShells.default = pkgs.mkShell {
          inputsFrom = [ vex-vpn ];
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
          inherit vex-vpn;
          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          fmt = craneLib.cargoFmt { src = craneLib.path ./.; };
        };
      }
    ) // {
      # ── NixOS modules (system-independent) ──────────────────────────────────
      # Most users: import nixosModules.default — gets both vpn backend + gui.
      # Advanced:   import nixosModules.pia-vpn alone (headless/server use).
      #             import nixosModules.vex-vpn alone (if you manage pia-vpn separately).
      nixosModules = {
        default  = combinedModule;
        pia-vpn  = vpnModule;
        vex-vpn  = guiModule;
      };
    };
}
