{
  description = "Slopbox development sandbox";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      lib = nixpkgs.lib;
      linuxSystems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      systems = linuxSystems ++ [ "aarch64-darwin" ];
      forAllSystems = lib.genAttrs systems;
      darwinRustEnv = {
        # Copied workers may load only system libraries under Seatbelt.
        RUSTFLAGS = "-C link-arg=-Wl,-dead_strip_dylibs";
      };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        rec {
          default = slopbox;
          slopbox = pkgs.rustPlatform.buildRustPackage {
            pname = "slopbox";
            version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;
            src = lib.cleanSource ./.;
            cargoLock.lockFile = ./Cargo.lock;
            env = lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin darwinRustEnv;
            nativeBuildInputs = lib.optional pkgs.stdenv.hostPlatform.isLinux pkgs.makeWrapper;
            nativeCheckInputs = [
              pkgs.bash
              pkgs.git
            ];
            # Fake HTTP upstreams bind loopback; no external test networking is needed.
            __darwinAllowLocalNetworking = true;
            # These exercise host OS facilities, not the Nix build sandbox.
            checkFlags = lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
              "--skip=backend::macos::engine::coalition::tests::the_host_coalition_cannot_be_terminated"
              "--skip=backend::macos::engine::coalition::tests::a_stale_process_version_cannot_signal_a_live_process"
              "--skip=command::tests::native_diff_accepts_stage_labels_without_using_path"
              # The build sandbox strips set-id bits; host enforcement tests cover them.
              "--skip=backend::macos::runtime::selected::tests::selected_native_runtime_rejects_setid_executables"
              "--skip=terminal::tests::private_pty_has_cloexec_descriptors_and_reports_resize_and_eof"
            ];
            postInstall = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
              wrapProgram "$out/bin/slopbox" \
                --prefix PATH : ${
                  lib.makeBinPath [
                    pkgs.bash
                    pkgs.bubblewrap
                    pkgs.coreutils
                    pkgs.diffutils
                    pkgs.wl-clipboard
                  ]
                }
            '';
            doInstallCheck = true;
            installCheckPhase = ''
              runHook preInstallCheck
              "$out/bin/slopbox" --version
              "$out/bin/slopbox" --help > /dev/null
              ${lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
                OTOOL=/usr/bin/otool \
                  ${pkgs.bash}/bin/bash ${./tests/native/check-system-dependencies.sh} "$out/bin/slopbox"
              ''}
              runHook postInstallCheck
            '';
            meta = {
              mainProgram = "slopbox";
              platforms = systems;
            };
          };
        }
      );

      checks = forAllSystems (
        system:
        {
          package = self.packages.${system}.slopbox;
        }
        // lib.optionalAttrs (lib.elem system linuxSystems) {
          e2e = import ./tests/nixos.nix {
            pkgs = nixpkgs.legacyPackages.${system};
            e2e = self.apps.${system}.e2e.program;
            devShell = self.devShells.${system}.default;
            nixpkgsSource = nixpkgs.outPath;
          };
        }
      );

      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);

      apps = lib.genAttrs linuxSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          e2e = pkgs.writeShellApplication {
            name = "slopbox-e2e";
            inheritPath = false;
            runtimeInputs = with pkgs; [
              bash
              bubblewrap
              coreutils
              curl
              diffutils
              findutils
              gawk
              git
              gnused
              nix
              nodejs
              openssh
              openssl
              pi-coding-agent
              ripgrep
              util-linux
            ];
            text = ''
              exec ${pkgs.bash}/bin/bash ${./tests/e2e.sh} ${
                self.packages.${system}.slopbox
              }/bin/slopbox ${self} ${./tests}
            '';
          };
        in
        {
          e2e = {
            type = "app";
            program = "${e2e}/bin/slopbox-e2e";
            meta.description = "Run Slopbox end-to-end security checks";
          };
        }
      );

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            env = lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin darwinRustEnv;
            packages =
              with pkgs;
              [
                bash
                cargo
                clippy
                git
                nixfmt
                rustc
                rustfmt
              ]
              ++ lib.optionals stdenv.hostPlatform.isLinux [
                bubblewrap
                wl-clipboard
              ];
          };
        }
      );
    };
}
