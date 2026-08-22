{
  description = "zshcs - Authentic Zsh completions for any LSP-compliant editor.";

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

            nativeBuildInputs = [
              pkgs.zsh
              pkgs.git
              pkgs.man-db
              pkgs.man-pages
              pkgs.coreutils
            ];

            preCheck = ''
              export HOME=$TMPDIR
              export MANPATH="${pkgs.coreutils}/share/man:${pkgs.man-pages}/share/man:$MANPATH"
            '';

          };

          apps.default = {
            type = "app";
            program = "${config.packages.default}/bin/zshcs";
            meta.description = "Authentic Zsh completions for any LSP-compliant editor.";
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
