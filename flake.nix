{
  description = "foldl: run code when a shell enters or leaves a folder";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
    }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f (pkgsFor system));
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [
            (import rust-overlay)
            # cargo-llvm-cov's own test suite fails outside a Linux sandbox;
            # we only need the tool.
            (_: prev: {
              cargo-llvm-cov = prev.cargo-llvm-cov.overrideAttrs { doCheck = false; };
            })
          ];
        };

      perSystem =
        pkgs:
        let
          inherit (pkgs) lib;
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          craneLib = (crane.mkLib pkgs).overrideToolchain (_: toolchain);

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./deny.toml
              ./crates
            ];
          };

          bash3 = pkgs.callPackage ./nix/bash3.nix { };
          cargoLlvmCov = pkgs.cargo-llvm-cov;

          # Shells and tools the integration tests drive. Pinned here so the
          # escaping and hook tests run against known versions.
          testTools = [
            bash3
            pkgs.bashInteractive
            pkgs.zsh
            pkgs.git
            pkgs.coreutils
          ]
          # script(1) for terminal tests; macOS uses its own /usr/bin/script.
          ++ lib.optional pkgs.stdenv.isLinux pkgs.util-linux;
          testEnv = {
            FOLDL_TEST_BASH3 = "${bash3}/bin/bash3";
            FOLDL_TEST_BASH5 = "${pkgs.bashInteractive}/bin/bash";
            FOLDL_TEST_ZSH = "${pkgs.zsh}/bin/zsh";
          };

          commonArgs = {
            inherit src;
            strictDeps = true;
            pname = "foldl";
            version = "0.1.0";
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          foldl = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              # Tests run in the checks below, with the pinned shells.
              doCheck = false;
              meta = {
                description = "Run code when a shell enters or leaves a folder";
                license = lib.licenses.mit;
                mainProgram = "foldl";
              };
            }
          );
        in
        {
          inherit
            craneLib
            commonArgs
            cargoArtifacts
            foldl
            toolchain
            testTools
            testEnv
            bash3
            cargoLlvmCov
            ;
        };
    in
    {
      packages = forAllSystems (
        pkgs:
        let
          s = perSystem pkgs;
        in
        {
          default = s.foldl;
          foldl = s.foldl;
          bash3 = s.bash3;
        }
      );

      checks = forAllSystems (
        pkgs:
        let
          s = perSystem pkgs;
          inherit (s) craneLib commonArgs cargoArtifacts;
        in
        {
          build = s.foldl;

          fmt = craneLib.cargoFmt { inherit (commonArgs) src pname version; };

          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );

          deny = craneLib.cargoDeny {
            inherit (commonArgs) src pname version;
            cargoDenyChecks = "bans licenses sources";
          };

          # Runs the whole test suite once, instrumented, and fails below
          # 100% line coverage. This is also the test run.
          coverage = craneLib.cargoLlvmCov (
            commonArgs
            // s.testEnv
            // {
              inherit cargoArtifacts;
              nativeBuildInputs = s.testTools;
              cargoLlvmCovExtraArgs = "--workspace --fail-under-lines 100 --show-missing-lines --lcov --output-path $out";
            }
          );
        }
      );

      devShells = forAllSystems (
        pkgs:
        let
          s = perSystem pkgs;
        in
        {
          default = pkgs.mkShell (
            s.testEnv
            // {
              packages = [
                s.toolchain
                s.cargoLlvmCov
                pkgs.cargo-deny
                pkgs.tmux
                pkgs.nixfmt
              ]
              ++ s.testTools;
            }
          );
        }
      );

      formatter = forAllSystems (pkgs: pkgs.nixfmt);
    };
}
