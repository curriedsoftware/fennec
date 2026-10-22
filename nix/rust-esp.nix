# Espressif's Rust fork, which adds the Xtensa targets the ESP32 needs. This
# is what `espup` installs, packaged from the same release binaries.
#
# Pinned to 1.98.0.0: the 1.99.0.0 standard library does not compile for
# ESP-IDF (`set_perm_nofollow` uses `libc::AT_FDCWD`, which the libc crate does
# not define for espidf). 1.98.1.0 has no aarch64 build.
#
# To update: bump `version`, refresh the hashes, and copy
# `lib/rustlib/src/rust/library/Cargo.lock` from the new rust-src into
# `nix/rust-std-<version>.Cargo.lock` (the firmware vendors std's
# dependencies from it, because it is built with `-Z build-std`).
{
  lib,
  stdenv,
  fetchurl,
  autoPatchelfHook,
  zlib,
}:

let
  version = "1.98.0.0";
  base = "https://github.com/esp-rs/rust-build/releases/download/v${version}";
  toolchain =
    {
      x86_64-linux = fetchurl {
        url = "${base}/rust-${version}-x86_64-unknown-linux-gnu.tar.xz";
        hash = "sha256-psPF7NUErUHdDw0vlcthl/lDucAsSO8IiM7cLy4RJ6Y=";
      };
      aarch64-linux = fetchurl {
        url = "${base}/rust-${version}-aarch64-unknown-linux-gnu.tar.xz";
        hash = "sha256-WIGmnhH8rjJVZSE4sfDKhfBy4gb1hQ/s36eBcAh3L/8=";
      };
    }
    .${stdenv.hostPlatform.system}
      or (throw "Espressif's Rust toolchain is not available for ${stdenv.hostPlatform.system}");
  rustSrc = fetchurl {
    url = "${base}/rust-src-${version}.tar.xz";
    hash = "sha256-B/bE0QwjttirhjqeIX3uUYV0T0+i/j1iA7qHdNJ4Zh4=";
  };
in
stdenv.mkDerivation {
  pname = "rust-esp";
  inherit version;

  srcs = [
    toolchain
    rustSrc
  ];
  sourceRoot = ".";

  nativeBuildInputs = [ autoPatchelfHook ];
  buildInputs = [
    zlib
    stdenv.cc.cc.lib
  ];

  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    for dir in rust-nightly-* rust-src-nightly; do
      patchShebangs "$dir/install.sh"
      "$dir/install.sh" --prefix="$out" --disable-ldconfig \
        --without=rust-docs,rust-docs-json-preview
    done
    rm -f "$out"/lib/rustlib/{install.log,uninstall.sh,manifest-*}
    runHook postInstall
  '';

  passthru.stdLock = ./rust-std-${version}.Cargo.lock;

  meta = {
    description = "Rust toolchain with Xtensa support, from esp-rs/rust-build";
    homepage = "https://github.com/esp-rs/rust-build";
    license = with lib.licenses; [
      mit
      asl20
    ];
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
  };
}
