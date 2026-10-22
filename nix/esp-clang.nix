# libclang from Espressif's LLVM fork. bindgen (in esp-idf-sys) needs it to
# parse ESP-IDF's headers for the Xtensa target.
{
  lib,
  stdenv,
  fetchurl,
  autoPatchelfHook,
  zlib,
  libxml2,
}:

let
  version = "esp-20.1.1_20250829";
  base = "https://github.com/espressif/llvm-project/releases/download/${version}";
in
stdenv.mkDerivation {
  pname = "esp-libclang";
  inherit version;

  src =
    {
      x86_64-linux = fetchurl {
        url = "${base}/libs-clang-${version}-x86_64-linux-gnu.tar.xz";
        hash = "sha256-ASjiqGsmLj7fzgJgNO3gfi9FcysQxsuKxSLI9Mcr79g=";
      };
      aarch64-linux = fetchurl {
        url = "${base}/libs-clang-${version}-aarch64-linux-gnu.tar.xz";
        hash = "sha256-zKWYhh+Zm/1AhJH0KIwvOBtEj53b2sL7QUR0cymHhAU=";
      };
    }
    .${stdenv.hostPlatform.system}
      or (throw "esp-clang is not available for ${stdenv.hostPlatform.system}");

  nativeBuildInputs = [ autoPatchelfHook ];
  buildInputs = [
    zlib
    libxml2
    stdenv.cc.cc.lib
  ];
  # Optional dependencies of tools we do not use.
  autoPatchelfIgnoreMissingDeps = true;

  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp -r lib $out/
    runHook postInstall
  '';

  meta = {
    description = "libclang with Xtensa support, from Espressif's LLVM fork";
    homepage = "https://github.com/espressif/llvm-project";
    license = lib.licenses.asl20;
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
    ];
  };
}
