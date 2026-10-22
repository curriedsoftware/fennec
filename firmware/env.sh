# Source this file inside the firmware development shell (see README.md):
#
#     . ./env.sh
#
# It sets up the Espressif Rust toolchain installed by `espup install`.

if [ ! -f "$HOME/export-esp.sh" ]; then
    echo "~/export-esp.sh not found; run 'espup install --targets esp32 --std' first" >&2
    return 1 2>/dev/null || exit 1
fi
. "$HOME/export-esp.sh"

# bindgen loads esp-clang's libclang, which needs a libstdc++ that Nix does
# not put on the default library path.
LD_LIBRARY_PATH="$(dirname "$(gcc -print-file-name=libstdc++.so.6)")${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export LD_LIBRARY_PATH
