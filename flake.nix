{
  description = "zshcs - Authentic Zsh completions for any LSP-compliant editor.";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
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
          toolchainToml = builtins.fromTOML (builtins.readFile ./rust-toolchain.toml);
          rustDevToolchain = pkgs.rust-bin.fromRustupToolchain toolchainToml.toolchain;
          rustBuildToolchain = pkgs.rust-bin.fromRustupToolchain {
            inherit (toolchainToml.toolchain) channel;
            profile = "minimal";
          };
          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustBuildToolchain;
            rustc = rustBuildToolchain;
          };
          testInputs = with pkgs; [
            git
            man-db
            man-pages
            zsh
          ];
        in
        {
          _module.args.pkgs = import inputs.nixpkgs {
            inherit system;
            overlays = [ (import inputs.rust-overlay) ];
          };

          treefmt.config = {
            projectRootFile = "flake.nix";
            settings.global.excludes = [
              ".agents/skills/gh-actions-lock/**"
            ];
            programs = {
              nixfmt.enable = true;
              rustfmt = {
                enable = true;
                package = rustDevToolchain;
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

            nativeBuildInputs = testInputs;

            preCheck = ''
              export HOME=$TMPDIR
              export MANPATH="${pkgs.git}/share/man:${pkgs.man-pages}/share/man:$MANPATH"
            '';
          };

          apps.default = {
            type = "app";
            program = "${config.packages.default}/bin/zshcs";
            meta.description = "Authentic Zsh completions for any LSP-compliant editor.";
          };

          devShells.default = pkgs.mkShell {
            packages = [ rustDevToolchain ] ++ testInputs;

            shellHook = ''
              export MANPATH="${pkgs.git}/share/man:${pkgs.man-pages}/share/man:$MANPATH"
              echo "Rust development environment loaded"
              cargo --version
            '';
          };
        };
    };
}
