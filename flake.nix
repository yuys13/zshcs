{
  description = "zshcs - Zsh LSP server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts.url = "github:hercules-ci/flake-parts";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [ inputs.treefmt-nix.flakeModule ];
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
      perSystem =
        {
          config,
          self',
          inputs',
          pkgs,
          system,
          ...
        }:
        let
          pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ (import inputs.rust-overlay) ];
          };
          rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };
        in
        {
          treefmt.config = {
            projectRootFile = "flake.nix";
            programs = {
              nixfmt.enable = true;
              rustfmt = {
                enable = true;
                package = rustToolchain;
              };
              yamlfmt.enable = true;
            };
          };

          packages.default = rustPlatform.buildRustPackage {
            pname = "zshcs";
            version = "0.1.0";
            src = pkgs.lib.cleanSource ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            # Disable check phase because the tests launch a zsh subprocess
            # using zpty, which requires pty access not available in the Nix sandbox.
            doCheck = false;
          };

          apps.default = {
            type = "app";
            program = "${config.packages.default}/bin/zshcs";
          };

          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              rustToolchain
              openssl
              pkg-config
              zsh
            ];

            shellHook = ''
              echo "Rust development environment loaded"
              cargo --version
            '';
          };
        };
    };
}
