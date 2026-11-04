{
  description = "foldl: run code when a shell enters or leaves a folder";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Only for checking the home-manager module.
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      crane,
      rust-overlay,
      home-manager,
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
              # The docs test checks the README covers every command.
              ./README.md
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
            # Holders describe their sessions with ps.
            pkgs.unixtools.ps
            # The importer's parity check runs direnv.
            pkgs.direnv
          ]
          # script(1) for terminal tests; macOS uses its own /usr/bin/script.
          ++ lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.util-linux;
          testEnv = {
            FOLDL_TEST_BASH3 = "${bash3}/bin/bash3";
            FOLDL_TEST_BASH5 = "${pkgs.bashInteractive}/bin/bash";
            FOLDL_TEST_ZSH = "${pkgs.zsh}/bin/zsh";
          };

          commonArgs = {
            inherit src;
            strictDeps = true;
            pname = "foldl";
            # One version for everything, from the workspace.
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
          };
          cargoArtifacts = craneLib.buildDepsOnly commonArgs;

          # A static Linux binary, cross-built with zig as the linker so it
          # also builds on macOS; it needs nothing at run time.
          linuxTarget =
            {
              "aarch64-darwin" = "aarch64-unknown-linux-musl";
              "aarch64-linux" = "aarch64-unknown-linux-musl";
              "x86_64-darwin" = "x86_64-unknown-linux-musl";
              "x86_64-linux" = "x86_64-unknown-linux-musl";
            }
            .${pkgs.stdenv.hostPlatform.system};
          staticToolchain = toolchain.override { targets = [ linuxTarget ]; };
          staticCrane = (crane.mkLib pkgs).overrideToolchain (_: staticToolchain);
          staticArgs = commonArgs // {
            CARGO_BUILD_TARGET = linuxTarget;
            nativeBuildInputs = [
              pkgs.cargo-zigbuild
              pkgs.zig
            ];
            preBuild = ''
              export HOME=$TMPDIR
              export ZIG_GLOBAL_CACHE_DIR=$TMPDIR/zig-cache
            '';
            cargoBuildCommand = "cargo zigbuild --release";
            cargoCheckCommand = "true";
            doCheck = false;
          };
          foldlLinuxStatic = staticCrane.buildPackage (
            staticArgs
            // {
              cargoArtifacts = staticCrane.buildDepsOnly staticArgs;
              cargoExtraArgs = "-p foldl";
              postInstall = installNotices;
            }
          );

          # THIRD_PARTY_LICENSES.md, generated from Cargo.lock offline with the
          # vendored crates, so `scripts/licenses` and the `licenses` check
          # produce the same bytes.
          thirdPartyLicenses =
            pkgs.runCommand "foldl-third-party-licenses"
              {
                nativeBuildInputs = [
                  toolchain
                  pkgs.cargo-about
                ];
                src = lib.fileset.toSource {
                  root = ./.;
                  fileset = lib.fileset.unions [
                    ./Cargo.toml
                    ./Cargo.lock
                    ./crates
                    ./packaging/licenses
                  ];
                };
                vendor = craneLib.vendorCargoDeps { inherit src; };
              }
              ''
                export HOME=$TMPDIR CARGO_HOME=$TMPDIR/cargo
                mkdir -p $CARGO_HOME
                cp $vendor/config.toml $CARGO_HOME/config.toml
                cd $src
                cargo about generate --frozen \
                  -c packaging/licenses/about.toml \
                  -o $out packaging/licenses/about.hbs
              '';

          # Our license and the notices of the crates compiled in, which
          # MIT and Apache-2.0 require to travel with the binary.
          installNotices = ''
            install -Dm644 ${./LICENSE} $out/share/doc/foldl/LICENSE
            install -Dm644 ${thirdPartyLicenses} $out/share/doc/foldl/THIRD_PARTY_LICENSES.md
          '';

          foldl = craneLib.buildPackage (
            commonArgs
            // {
              inherit cargoArtifacts;
              postInstall = installNotices;
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
            foldlLinuxStatic
            thirdPartyLicenses
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
          foldl-linux-static = s.foldlLinuxStatic;
          third-party-licenses = s.thirdPartyLicenses;
          bash3 = s.bash3;
        }
      );

      overlays.default = final: _: {
        foldl = self.packages.${final.stdenv.hostPlatform.system}.default;
      };
      homeManagerModules.default = import ./nix/hm-module.nix self;
      nixosModules.default = import ./nix/nixos-module.nix self;

      checks = forAllSystems (
        pkgs:
        let
          s = perSystem pkgs;
          inherit (s) craneLib commonArgs cargoArtifacts;
        in
        {
          build = s.foldl;

          # THIRD_PARTY_LICENSES.md matches Cargo.lock.
          licenses = pkgs.runCommand "foldl-licenses-up-to-date" { } ''
            if ! diff -u ${./THIRD_PARTY_LICENSES.md} ${s.thirdPartyLicenses}; then
              echo "THIRD_PARTY_LICENSES.md is out of date; run scripts/licenses" >&2
              exit 1
            fi
            touch $out
          '';

          # The home-manager module puts the hook in bash and zsh and writes
          # the config.
          home-manager-module =
            let
              home = home-manager.lib.homeManagerConfiguration {
                inherit pkgs;
                modules = [
                  self.homeManagerModules.default
                  {
                    home.username = "test";
                    home.homeDirectory = if pkgs.stdenv.isDarwin then "/Users/test" else "/home/test";
                    home.stateVersion = "25.05";
                    programs.bash.enable = true;
                    programs.zsh.enable = true;
                    programs.foldl.enable = true;
                    programs.foldl.settings.verbosity = "quiet";
                  }
                ];
              };
              files = "${home.activationPackage}/home-files";
            in
            pkgs.runCommand "foldl-home-manager-module" { } ''
              grep -q 'foldl hook bash' ${files}/.bashrc
              grep -q 'foldl hook zsh' ${files}/.zshrc
              grep -q 'verbosity = "quiet"' ${files}/.config/foldl/config.toml
              touch $out
            '';

          # The NixOS module, evaluated (not built) for Linux.
          nixos-module =
            let
              system = nixpkgs.lib.nixosSystem {
                system = "aarch64-linux";
                modules = [
                  self.nixosModules.default
                  {
                    programs.foldl.enable = true;
                    boot.isContainer = true;
                    system.stateVersion = "25.05";
                  }
                ];
              };
              # Evaluated only: the Linux package isn't built from here.
              text = t: pkgs.writeText "init" (builtins.unsafeDiscardStringContext t);
              bash = text system.config.programs.bash.interactiveShellInit;
              zsh = text system.config.programs.zsh.interactiveShellInit;
            in
            pkgs.runCommand "foldl-nixos-module" { } ''
              grep -q 'foldl hook bash' ${bash}
              grep -q 'foldl hook zsh' ${zsh}
              touch $out
            '';

          # The Homebrew formula parses, and its test block's steps pass
          # against the built binary (brew itself isn't in nixpkgs).
          homebrew-formula =
            pkgs.runCommand "foldl-homebrew-formula"
              {
                nativeBuildInputs = [
                  pkgs.ruby
                  s.foldl
                ];
              }
              ''
                ruby -c ${./packaging/homebrew/foldl.rb}
                # What the formula's install puts in doc/ must exist.
                test -s ${./THIRD_PARTY_LICENSES.md}
                export HOME=$TMPDIR
                foldl --version | grep -q foldl
                mkdir -p p/.foldl/1-env
                printf 'version = 1\n[env]\nGREETING = "hi"\n' > p/.foldl/1-env/manifest.toml
                foldl allow p 2>/dev/null
                test "$(foldl exec p -- sh -c 'echo $GREETING')" = hi
                touch $out
              '';

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
              # The text table names any uncovered lines; the lcov file is the
              # output.
              cargoLlvmCovExtraArgs = "--workspace --fail-under-lines 100 --show-missing-lines";
              postInstall = "cargo llvm-cov report --release --lcov --output-path $out";
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
                pkgs.asciinema-agg
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
