{
  description = "vex-vpn — GTK4/libadwaita GUI for the vexos-vpn PIA backend";

  inputs = {
    # Same branch as vexos-nix; consumers override with
    # `inputs.vex-vpn.inputs.nixpkgs.follows = "nixpkgs"`.
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    # crane has no inputs of its own (nothing to follow).
    crane.url = "github:ipetkov/crane";
  };

  outputs = { self, nixpkgs, crane }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      mkVexVpn = pkgs: import ./nix/package.nix { inherit pkgs crane; src = ./.; };
    in
    {
      packages = forAllSystems (pkgs: rec {
        vex-vpn = mkVexVpn pkgs;
        default = vex-vpn;
      });

      overlays.default = final: _prev: { vex-vpn = mkVexVpn final; };

      nixosModules.default = import ./nix/module.nix { inherit mkVexVpn; };

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.default ];
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
            cargo-watch
          ];
          RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          shellHook = ''
            export GSK_RENDERER=cairo
          '';
        };
      });

      checks = forAllSystems (pkgs:
        let
          pkg = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
          inherit (pkg.passthru) craneLib commonArgs cargoArtifacts;
        in
        {
          vex-vpn = pkg;
          clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });
          fmt = craneLib.cargoFmt { src = craneLib.path ./.; };
        });
    };
}
