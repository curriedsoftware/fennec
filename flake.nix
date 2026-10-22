{
  description = "Fennec: a wake word voice remote for chat bots on the M5Stack Atom Echo";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # Piper 1.7.0. The piper-tts package in newer nixpkgs fails to build.
    nixpkgs-piper.url = "github:NixOS/nixpkgs/8150a7429783f648af04357ff3ed1f2e32d2ac98";
    # ESP-IDF and its toolchains, packaged for Nix.
    esp-dev.url = "github:mirrexagon/nixpkgs-esp-dev";
  };

  outputs =
    {
      self,
      nixpkgs,
      nixpkgs-piper,
      esp-dev,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f system);

      perSystem =
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          piperPkgs = nixpkgs-piper.legacyPackages.${system};
          lib = pkgs.lib;

          rust-esp = pkgs.callPackage ./nix/rust-esp.nix { };
          esp-libclang = pkgs.callPackage ./nix/esp-clang.nix { };
          # Only the ESP32's Xtensa toolchain, at the ESP-IDF release
          # esp-idf-sys supports best.
          esp-idf = esp-dev.packages.${system}.esp-idf-xtensa.override {
            rev = "v5.5.5";
            sha256 = "sha256-/xVsusdmXFMLImfKryoB5ulR09AJefzoIUFmTAb3n/s=";
            toolsToInclude = [
              "xtensa-esp-elf"
              "esp-rom-elfs"
            ];
          };

          # A Python that can `import piper`, for `fennec synth`. It runs with
          # PYTHONPATH cleared, so ESP-IDF's Python packages (a different
          # Python version) in the same shell cannot leak into it.
          piperPythonEnv = piperPkgs.python3.withPackages (ps: [ (ps.toPythonModule piperPkgs.piper-tts) ]);
          piperPython = pkgs.writeShellScriptBin "fennec-python" ''
            unset PYTHONPATH
            exec ${piperPythonEnv}/bin/python3 "$@"
          '';
          piper = pkgs.writeShellScriptBin "piper" ''
            unset PYTHONPATH
            exec ${piperPkgs.piper-tts}/bin/piper "$@"
          '';

          fennec = pkgs.rustPlatform.buildRustPackage {
            pname = "fennec";
            version = (lib.importTOML ./Cargo.toml).workspace.package.version;
            src = lib.fileset.toSource {
              root = ./.;
              fileset = lib.fileset.unions [
                ./Cargo.toml
                ./Cargo.lock
                ./crates
                ./firmware/fennec.example.toml
                ./firmware/partitions.csv
                ./models
              ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [
              "-p"
              "fennec-cli"
            ];
            cargoTestFlags = [ "--workspace" ];
            nativeBuildInputs = [ pkgs.makeWrapper ];
            postInstall = ''
              wrapProgram $out/bin/fennec \
                --set-default FENNEC_PYTHON ${piperPython}/bin/fennec-python \
                --prefix PATH : ${
                  lib.makeBinPath [
                    pkgs.espflash
                    pkgs.alsa-utils
                  ]
                }
            '';
            meta = {
              description = "Synthesize data for, train, test and flash Fennec wake word models";
              mainProgram = "fennec";
            };
          };

          firmware = pkgs.callPackage ./nix/firmware.nix {
            inherit rust-esp esp-libclang esp-idf;
          };

          # `fennec flash`/`fennec ota` with the firmware built by Nix.
          withFirmware =
            name: command:
            pkgs.writeShellApplication {
              inherit name;
              runtimeInputs = [ fennec ];
              text = ''
                export FENNEC_FIRMWARE_DIR=${firmware}
                exec fennec ${command} "$@"
              '';
            };
        in
        {
          packages = {
            inherit
              fennec
              firmware
              rust-esp
              esp-libclang
              ;
            default = fennec;
            flash = withFirmware "fennec-flash" "flash";
            ota = withFirmware "fennec-ota" "ota";
          };

          devShells.default = pkgs.mkShell {
            name = "fennec";
            packages = [
              # One toolchain for everything: it builds the firmware for the
              # ESP32 and the host tools alike.
              rust-esp
              esp-idf
              pkgs.ldproxy
              pkgs.espflash
              pkgs.cmake
              pkgs.ninja
              pkgs.git
              pkgs.pkg-config

              piper
              pkgs.alsa-utils
              pkgs.sox
              pkgs.ffmpeg
              pkgs.jq
              pkgs.curl
            ];
            ESP_IDF_TOOLS_INSTALL_DIR = "fromenv";
            ESP_IDF_COMPONENT_MANAGER = "off";
            LIBCLANG_PATH = "${esp-libclang}/lib";
            # libopus for the firmware's `opus` feature (voice notes).
            FENNEC_OPUS_SRC = pkgs.srcOnly pkgs.libopus;
            FENNEC_PYTHON = "${piperPython}/bin/fennec-python";
            shellHook = ''
              echo "fennec development shell: cargo builds both the CLI (repository root) and the firmware (firmware/)"
            '';
          };

          apps = {
            flash = {
              type = "app";
              program = lib.getExe self.packages.${system}.flash;
              meta.description = "Flash firmware, configuration and wake word model over USB";
            };
            ota = {
              type = "app";
              program = lib.getExe self.packages.${system}.ota;
              meta.description = "Update a device over Wi-Fi";
            };
            default = {
              type = "app";
              program = lib.getExe fennec;
              meta.description = "The fennec CLI";
            };
          };

          checks = {
            inherit fennec firmware;
          };
        };

      all = forAllSystems perSystem;
    in
    {
      packages = nixpkgs.lib.mapAttrs (_: s: s.packages) all;
      devShells = nixpkgs.lib.mapAttrs (_: s: s.devShells) all;
      apps = nixpkgs.lib.mapAttrs (_: s: s.apps) all;
      checks = nixpkgs.lib.mapAttrs (_: s: s.checks) all;
      formatter = forAllSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
