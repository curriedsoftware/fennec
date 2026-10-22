fn main() {
    embuild::espidf::sysenv::output();

    // libopus is an ESP-IDF component (components/opus), built only when its
    // source is available; a missing source would otherwise surface as
    // undefined references at link time.
    println!("cargo:rerun-if-env-changed=FENNEC_OPUS_SRC");
    if std::env::var_os("CARGO_FEATURE_OPUS").is_some()
        && std::env::var_os("FENNEC_OPUS_SRC").is_none()
    {
        println!(
            "cargo::error=the `opus` feature needs the libopus source in FENNEC_OPUS_SRC: \
             build from the flake's development shell (`nix develop`), or build without it \
             (`--no-default-features --features telegram`) to upload WAV files"
        );
    }
}
