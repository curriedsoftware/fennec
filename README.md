<p align="center">
  <img src="images/fennec.svg" alt="Fennec logo" width="160">
</p>

# Fennec

> *Big ears, tiny fox.* Like its namesake, which has the largest ears
> for its size of any fox, Fennec is a tiny listener with a great
> sense of hearing.  It fits in a 24 mm cube and still catches every
> "hey fennec" you throw at it.  Not a fan of the name? Teach it a new
> trick: the wake word can be [retrained](#your-own-wake-word) to
> whatever you like.

Fennec turns an [M5Stack Atom Echo](https://docs.m5stack.com/en/atom/atomecho)
into a voice remote for your own chat bot. Say the wake word (or press
the button), say what you want, and the recording lands in a chat on a
messaging service as an audio file. Your bot picks it up there, like any
other message, and does whatever it does: talk to Home Assistant, a local
LLM, book train tickets...

Fennec talks to [Telegram](docs/telegram.md) today, where recordings show up
as native voice notes. The rest of the device (wake word, recording,
encoding) doesn't depend on the messaging service, so others, like WhatsApp,
can be added alongside it.

```
 "hey fennec, turn off the        +-------------+  messaging    +------------------+
  lights and book me a      ----> |  Atom Echo  | ------------> |  group chat      |
  train to Madrid on Friday"      |  (Fennec)   |  service API  |  (e.g. Telegram) |
                                  +-------------+               |                  |
                                   wake word, VAD,              |  your main bot   |
                                   Opus, Wi-Fi                  |  reads the audio,|
                                                                |  transcribes it, |
                                                                |  acts, replies   |
                                                                +------------------+
```

Everything is written in Rust:

| Path                  | What it is                                                                                                            |
|-----------------------|-----------------------------------------------------------------------------------------------------------------------|
| `firmware/`           | ESP-IDF firmware for the Atom Echo                                                                                    |
| `crates/fennec-core/` | `no_std` audio front end, wake word inference, end of speech detection, ADPCM and Ogg encoding, shared by firmware and CLI |
| `crates/fennec-cli/`  | `fennec`: synthesizes training data, trains, evaluates and tests wake word models                                     |
| `models/`             | Ready-to-use wake word models                                                                                         |
| `docs/`               | Guides                                                                                                                |

Because the firmware and the training tools share `fennec-core`, a model is
evaluated during training with exactly the code that runs on the device.

### Supported devices

The [M5Stack Atom Echo](https://docs.m5stack.com/en/atom/atomecho) is the
only device Fennec runs on today. Nothing ties the design to it: the wake
word, end of speech detection and encoding in `fennec-core` are `no_std`
Rust without hardware dependencies, and the firmware uses ESP-IDF, so other
ESP32 boards with a microphone (and optionally a speaker) are natural next
targets. What is specific to the Atom Echo lives in a few firmware modules:
the pins and drivers in `mic.rs`, `speaker.rs`, `audio.rs` and `led.rs`, and
the flash layout in `partitions.csv`. See
[docs/firmware.md](docs/firmware.md#other-devices).

### Non-goals

Fennec's job ends when the recording lands in the chat. It hears the wake
word, records you, and gets the audio to the messaging service as quickly and
reliably as it can. Everything that happens after that is up to your bot, and
Fennec deliberately leaves it out:

- **Speech to text.** The device sends audio, never text. Your bot transcribes
  it with whatever it likes (whisper.cpp, faster-whisper, a cloud API).
- **Understanding and acting.** Fennec has no LLM, no intent matching and no
  integrations. Talking to Home Assistant, deciding which requests go to a
  local intent matcher and which to an LLM, or splitting a request like "turn
  off the lights and tell me the exchange rate" are all your bot's job.
- **A bot.** This repository contains no main bot, only the short example
  handler in [docs/telegram.md](docs/telegram.md#handling-recordings-in-your-bot).
  `fennec.toml` configures the device and nothing else.

Keeping that line means Fennec works with any bot and any backend, and the
bot can change without reflashing anything.

## How it behaves

1. The LED glows dim blue while Fennec listens for the wake word.
2. When it hears the wake word, or you press the button, the LED turns cyan and
   recording starts.
3. Recording stops after you have been quiet for a moment (1.2 s by default),
   when you press the button again, or after 12 s.
4. The LED turns purple while the recording uploads, then flashes green and
   chimes on success, or flashes red on failure. If you said nothing, it
   blinks off and goes back to listening (unless `audio.button_preroll_ms`
   or `audio.wake_preroll_ms` is set for what started the recording: then
   the audio from before it is uploaded, see
   [Capturing missed wake words](docs/wake-word.md#capturing-missed-wake-words)
   and [false wake-ups](docs/wake-word.md#capturing-false-wake-ups)).

Each upload is a voice note (Ogg Opus, 16 kHz mono, 24 kbit/s) whose caption
your bot can parse. Firmware built without the `opus` feature uploads WAV
files (IMA ADPCM) instead; see
[docs/firmware.md](docs/firmware.md#build-features).

```
#fennec device=kitchen trigger=wake_word score=0.97 duration=3.4s
```

## Quick start

You need an Atom Echo, a USB-C cable, a Linux machine with
[Nix](https://nixos.org/download/), and an account on a supported messaging
service (currently Telegram).

### 1. Set up the messaging service

The device posts as its own bot into a group chat that your main bot is also
in. For Telegram, create a bot for the device, a group with it and your main
bot, and enable bot-to-bot communication so your main bot sees the device's
messages. This takes about five minutes; follow
[docs/telegram.md](docs/telegram.md), which starts by explaining
[how the two bots talk to each other](docs/telegram.md#how-bot-to-bot-communication-works).
At the end you have a **bot token** and a **chat id**.

### 2. Configure

The firmware image is the same for every device. Your settings live in a
small file that `fennec` writes to its own flash partition, so they never end
up in the firmware or in the Nix store.

```bash
cp firmware/fennec.example.toml firmware/fennec.toml
$EDITOR firmware/fennec.toml
```

Fill in your Wi-Fi network, the bot token, the chat id, and a device name.
Set `ota.password` too if you want to update the device over Wi-Fi later
(`openssl rand -hex 16` makes a good one). The other settings have sensible
defaults; [docs/firmware.md](docs/firmware.md) explains all of them.

### 3. Flash

Plug in the Atom Echo and run:

```bash
nix run .#flash -- --config firmware/fennec.toml
```

This builds the firmware with Nix (the first time takes a while: it fetches
ESP-IDF and Espressif's Rust toolchain), flashes it together with your
configuration and the `hey fennec` model, and then shows the device's log.
You should see something like:

```
I (1234) fennec_firmware: connected, address 192.168.1.42
I (1300) fennec_firmware: wake word inference takes 72 ms (score on silence 0.0058)
I (1301) fennec_firmware: listening for "hey fennec" (threshold 0.92)
```

Press the button, say something, and the recording appears in your group.
Then try the wake word: "hey fennec".

> To flash, your user needs access to the serial port (the `dialout` group
> on most distributions).

### 4. Update over Wi-Fi

With `ota.password` set, later changes don't need the USB cable:

```bash
nix run .#ota -- --host 192.168.1.42 --config firmware/fennec.toml --firmware   # new firmware
nix run .#ota -- --host 192.168.1.42 --config firmware/fennec.toml --settings   # changed fennec.toml
nix run .#ota -- --host 192.168.1.42 --config firmware/fennec.toml --model      # new wake word model
```

Instead of the IP address, `fennec-<device name>` works if your router
resolves DHCP host names. A firmware update that doesn't come up and join
Wi-Fi is rolled back automatically on the next reset. See
[docs/firmware.md](docs/firmware.md#over-the-air-updates).

### 5. Make your bot listen

Your main bot receives each recording as a voice note (`audio/ogg`, Opus).
Download it, transcribe it (e.g. with whisper.cpp or
faster-whisper) and handle the text as if you had typed it. For Telegram, see
[docs/telegram.md](docs/telegram.md#handling-recordings-in-your-bot) for the
details and a short example.

## Your own wake word

`models/hey_fennec.fkw` responds to "hey fennec". You can train a model for
any phrase on a Linux machine, without recording a single sample: `fennec`
synthesizes thousands of examples with [Piper](https://github.com/OHF-Voice/piper1-gpl)
voices covering more than a thousand speakers, mixes them with noise, trains
a small network, and measures how often it wakes up by mistake.

```bash
nix develop        # or let direnv load it, see below

cargo build --release -p fennec-cli
./target/release/fennec voices --dir work/voices
./target/release/fennec synth --phrase "hey jarvis" --out work/hey_jarvis --voices work/voices
./target/release/fennec train --data work/hey_jarvis --out models/hey_jarvis.fkw
```

To tune a model to your voice, `fennec enroll` does everything in one go. It
synthesizes the dataset if it is missing, asks for three takes of the wake
word plus one to test with, records 30 s of your room, and trains. Training
turns the takes into many variations (speed, pitch, distance, microphone,
noise, cut off a little) and into negatives (backwards, words swapped). With
three takes recorded on the Atom Echo, an enrolled "hey fennec" model detected
6 of 6 held-out takes of the same speaker, against 1 of 6 for the bundled
model; [the recipe](docs/wake-word.md#a-recipe-that-works) shows how:

```bash
./target/release/fennec enroll --phrase "hey jarvis" --data work/hey_jarvis \
    --voices work/voices --out models/hey_jarvis.fkw
```

Then set `wake_word.model = "../models/hey_jarvis.fkw"` in `fennec.toml` and
send it with `fennec ota ... --model` (or `fennec flash` over USB).
[docs/wake-word.md](docs/wake-word.md) covers the details: picking
a good phrase, adding your own voice and your home's background noise, reading
the evaluation report, [`fennec enroll`](docs/wake-word.md#the-quick-way-fennec-enroll),
and testing a model live on your computer. It also
explains how to [retrain](docs/wake-word.md#retraining-a-model) `hey fennec`
with more data, and how to
[augment](docs/wake-word.md#augmenting-the-dataset-with-nixomatic) a dataset
with public noise corpora and your own audio, using tools from
[nixomatic](https://nixomatic.com).

## Development Environment

### The flake

`flake.nix` has one development shell with everything: Espressif's Rust
toolchain (which builds both the host tools and the firmware), ESP-IDF
v5.5.5, `espflash`, Piper for synthetic training data, `arecord`, sox and
ffmpeg. Nothing is downloaded at build time, so on NixOS there is no need for
nix-ld.

```bash
nix develop
cargo build --release -p fennec-cli          # the CLI, from the repository root
(cd firmware && cargo build --release)       # the firmware
./target/release/fennec flash --config firmware/fennec.toml   # flash what you just built
```

With [direnv](https://direnv.net/) and
[nix-direnv](https://github.com/nix-community/nix-direnv), the shell loads by
itself when you enter the directory. `.envrc` contains `use flake`; allow it
once:

```bash
direnv allow
```

The flake also builds the pieces directly:

| Command                                                       | What it does                                                                                             |
|---------------------------------------------------------------|----------------------------------------------------------------------------------------------------------|
| `nix build .#firmware`                                        | Firmware: `fennec-firmware.elf`, the OTA image `fennec-firmware.bin`, `bootloader.bin`, `partitions.csv` |
| `nix build` / `nix build .#fennec`                            | The `fennec` CLI, with Piper, `espflash` and `arecord` wired in                                          |
| `nix run .#flash -- --config <file>`                          | Build the firmware and flash it, the configuration and the model over USB                                |
| `nix run .#ota -- --host <device> --config <file> --firmware` | Build the firmware and send it over Wi-Fi                                                                |
| `nix flake check`                                             | Build everything and run the tests                                                                       |

Version notes:

- Espressif's Rust toolchain is pinned to 1.98.0.0: the standard library in
  1.99.0.0 does not compile for ESP-IDF (it uses `libc::AT_FDCWD`, which the
  `libc` crate does not define for that target).
- `piper-tts` comes from a pinned nixpkgs revision (`8150a74`, Piper 1.7.0)
  because the newer nixpkgs package fails to build at the moment.

### Without the flake: nixomatic

The CLI and the training tools also work from a
[nixomatic](https://nixomatic.com) shell. The firmware works too, with
Espressif's toolchain installed by `espup` (which downloads prebuilt
binaries, so NixOS needs [nix-ld](https://wiki.nixos.org/wiki/Nix-ld)).

### Host tools and wake word training

#### Using Nix

```bash
nix \
    --extra-experimental-features 'nix-command flakes' \
    develop 'https://nixomatic.com/?p=rustc,cargo,piper-tts:8150a74,alsa-utils' \
      --accept-flake-config
```

#### Using Docker

```bash
docker run -v nix-store:/nix -v "$PWD:/workspace" -w /workspace --rm -it nixos/nix nix \
    --extra-experimental-features 'nix-command flakes' \
    develop 'https://nixomatic.com/?p=rustc,cargo,piper-tts:8150a74,alsa-utils' \
      --accept-flake-config
```

Run the tests with `cargo test --workspace`.

#### Training on a GPU

`fennec train` uses the first NVIDIA GPU when `fennec` is built with the
`cuda` feature, and falls back to the CPU when there is no usable GPU. This
is about 25 times faster on an RTX 4090. The CUDA toolkit is unfree, so Nix
needs `NIXPKGS_ALLOW_UNFREE=1` and `--impure`:

```bash
NIXPKGS_ALLOW_UNFREE=1 nix \
    --extra-experimental-features 'nix-command flakes' \
    develop --impure 'https://nixomatic.com/?p=rustc,cargo,piper-tts:8150a74,alsa-utils,cudaPackages_12.cudatoolkit' \
      --accept-flake-config

export CUDA_COMPUTE_CAP=89   # your GPU's compute capability without the dot
export LIBRARY_PATH="$(dirname "$(dirname "$(which nvcc)")")/lib/stubs:$LIBRARY_PATH"
cargo build --release -p fennec-cli --features cuda
LD_LIBRARY_PATH=/run/opengl-driver/lib ./target/release/fennec train ...
```

- `CUDA_COMPUTE_CAP` is detected with `nvidia-smi` when unset; look up your
  GPU's value in [NVIDIA's list](https://developer.nvidia.com/cuda-gpus)
  (8.9 → `89` for the RTX 40 series) if that fails.
- `libcuda` ships with the NVIDIA driver, not the toolkit, so the build links
  against the toolkit's stub, and `LD_LIBRARY_PATH` points the binary at the
  real one when it runs. `/run/opengl-driver/lib` is where NixOS keeps it; on
  other distributions use the directory holding `libcuda.so.1`. Without it,
  training prints `no usable GPU` and runs on the CPU.
- The driver must support the toolkit's CUDA version, or training fails with
  `CUDA_ERROR_UNSUPPORTED_PTX_VERSION`. `cudaPackages_12` (CUDA 12.9) needs
  driver 575 or newer; `cudatoolkit` alone is the newest CUDA, which may be
  ahead of your driver.

### Firmware

#### Using Nix

```bash
nix \
    --extra-experimental-features 'nix-command flakes' \
    develop 'https://nixomatic.com/?p=rustup,espup,espflash,ldproxy,python3,cmake,ninja,git' \
      --accept-flake-config
```

#### Using Docker

```bash
docker run -v nix-store:/nix -v "$PWD:/workspace" -w /workspace --rm -it nixos/nix nix \
    --extra-experimental-features 'nix-command flakes' \
    develop 'https://nixomatic.com/?p=rustup,espup,espflash,ldproxy,python3,cmake,ninja,git' \
      --accept-flake-config
```

In Docker, flashing needs the serial port passed through (e.g.
`--device /dev/ttyUSB0`). Then, inside the shell, from `firmware/`:
`espup install --targets esp32 --std` once, `. ./env.sh`, and
`cargo build --release`.

### Iterating on the firmware

Flash a device once with `fennec flash`, which writes the bootloader,
partition table, firmware, configuration and model. After that, `cargo run
--release` in `firmware/` (with either shell) reflashes just the firmware and
opens the serial monitor; the configuration and the model stay on the device.
