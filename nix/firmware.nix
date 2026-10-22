# The Fennec firmware, built without network access: ESP-IDF and its tools
# come from nixpkgs-esp-dev, the Xtensa Rust toolchain from ./rust-esp.nix,
# and all crates (including the standard library's dependencies, needed by
# `-Z build-std`) are vendored from the lock files.
#
# The output holds everything `fennec flash` and `fennec ota` need:
#
#   fennec-firmware.elf   flashed over USB
#   fennec-firmware.bin   app image for over-the-air updates
#   bootloader.bin        ESP-IDF bootloader with rollback support
#   partitions.csv        flash layout
{
  lib,
  stdenv,
  rustPlatform,
  symlinkJoin,
  rust-esp,
  esp-libclang,
  esp-idf,
  ldproxy,
  espflash,
  cmake,
  ninja,
  git,
  srcOnly,
  libopus,
}:

let
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../firmware/Cargo.toml
      ../firmware/Cargo.lock
      ../firmware/build.rs
      ../firmware/src
      ../firmware/components
      ../firmware/.cargo
      ../firmware/sdkconfig.defaults
      ../firmware/partitions.csv
      ../firmware/espflash.toml
      # fennec-core inherits its package fields from the workspace.
      ../Cargo.toml
      ../crates
    ];
  };

  vendor = symlinkJoin {
    name = "fennec-firmware-vendor";
    paths = [
      (rustPlatform.importCargoLock { lockFile = ../firmware/Cargo.lock; })
      (rustPlatform.importCargoLock { lockFile = rust-esp.stdLock; })
    ];
  };
in
stdenv.mkDerivation {
  pname = "fennec-firmware";
  version = (lib.importTOML ../firmware/Cargo.toml).package.version;
  inherit src;

  nativeBuildInputs = [
    rust-esp
    esp-idf
    ldproxy
    espflash
    cmake
    ninja
    git
  ];

  dontUseCmakeConfigure = true;
  dontUseNinjaBuild = true;
  dontUseNinjaInstall = true;

  # Use the ESP-IDF from the environment instead of downloading one.
  ESP_IDF_TOOLS_INSTALL_DIR = "fromenv";
  ESP_IDF_COMPONENT_MANAGER = "off";
  LIBCLANG_PATH = "${esp-libclang}/lib";
  # libopus for the `opus` feature (voice notes), built as an ESP-IDF
  # component; see firmware/components/opus.
  FENNEC_OPUS_SRC = srcOnly libopus;

  buildPhase = ''
    runHook preBuild

    export HOME=$TMPDIR
    export CARGO_HOME=$TMPDIR/cargo
    mkdir -p $CARGO_HOME
    cat > $CARGO_HOME/config.toml <<EOF
    [source.crates-io]
    replace-with = "vendored-sources"

    [source.vendored-sources]
    directory = "${vendor}"
    EOF

    cd firmware
    cargo build --release --offline --locked

    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    mkdir -p $out
    elf=target/xtensa-esp32-espidf/release/fennec-firmware
    cp $elf $out/fennec-firmware.elf
    cp partitions.csv $out/
    cp target/xtensa-esp32-espidf/release/build/esp-idf-sys-*/out/build/bootloader/bootloader.bin $out/
    espflash save-image --chip esp32 --flash-freq 80mhz --partition-table partitions.csv $elf $out/fennec-firmware.bin

    runHook postInstall
  '';

  dontStrip = true;
  dontPatchELF = true;
  dontFixup = true;

  meta = {
    description = "Fennec firmware for the M5Stack Atom Echo";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
  };
}
