{
  description = "cx";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        rustPlatform = pkgs.makeRustPlatform {
          cargo = toolchain;
          rustc = toolchain;
        };
        cx = rustPlatform.buildRustPackage {
          pname = "cx";
          version = (pkgs.lib.importTOML ./Cargo.toml).workspace.package.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeCheckInputs = [ pkgs.git ];
        };
      in
      {
        packages = {
          default = cx;
          docker = pkgs.dockerTools.buildLayeredImage {
            name = "cx";
            tag = "latest";
            config.Entrypoint = [ "${cx}/bin/cx" ];
          };
        };

        apps.default = {
          type = "app";
          program = "${cx}/bin/cx";
        };

        devShells.default = pkgs.mkShell {
          packages = [
            toolchain
            pkgs.rust-analyzer
          ];
        };
      });
}
