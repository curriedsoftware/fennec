# Firmware

The firmware runs on the Atom Echo's ESP32-PICO-D4 (240 MHz dual core,
520 KB RAM, no PSRAM, 4 MB flash) on top of ESP-IDF v5.5, using the
`esp-idf-svc` Rust bindings. The Atom Echo is the only supported device
today; see [Other devices](#other-devices).

## What it does

```
                     +-----------------------------------------------+
 PDM microphone ---> | high-pass, gain                               |
 (GPIO33 clk,        |    |                                          |
  GPIO23 data)       |    +--> log-mel front end --> wake word model |--+ wake word
                     |    |    (every 10 ms)        (every 100 ms)   |  |
                     |    +--> noise floor estimate                  |  |
 button (GPIO39) --->|---------------------------------------------- |--+ or press
                     +-----------------------------------------------+  |
                                                                        v
                     record until silence, button, or limit: 20 ms frames go to
                     the Opus encoder on the second core, Ogg pages stay in RAM
                                                                        |
                                                                        v
                     upload over HTTPS to the messaging service
                     (Telegram: POST api.telegram.org/bot<token>/sendVoice)
```

- **Wake word**: the model lives in its own flash partition and runs
  straight from flash, on the main core. On startup the log shows how long one
  inference takes; it must stay well under 100 ms. The bundled
  `hey_fennec.fkw` (192 KB, about 1.5 million multiply-adds) takes about
  72 ms on an Atom Echo. Three things keep it there: the convolutions compute
  four outputs at a time with the ESP32's fused multiply-add (`madd.s`), the
  flash runs at 80 MHz (`CONFIG_ESPTOOLPY_FLASHFREQ_80M`, which `fennec flash`
  also writes into the image headers), and `fennec-core` is built with
  `opt-level = 3`. If a larger model doesn't fit, train a smaller one with
  `fennec train --width 0.7`. The startup line also prints the score on
  silence; `fennec inspect` prints the same number for the model on your
  computer, so a difference means the device computes something else.
- **End of speech**: frames more than `audio.speech_margin_db` above the
  background noise count as speech. The noise level is tracked continuously
  while idle. The recording ends `audio.silence_ms` after speech stops.
- **Voice notes**: recordings are encoded while they are made, as Ogg Opus
  (RFC 7845) at 24 kbit/s: about 3 KB per second, the format Telegram shows as
  a voice note. libopus is built in fixed point as an ESP-IDF component
  (`firmware/components/opus`) and runs on its own thread pinned to the
  second core, which the rest of the firmware leaves idle. At complexity 2 it
  takes about 9 ms per 20 ms of audio there. The encoder needs about 15 KB of
  heap and 24 KB of stack (libopus keeps its scratch buffers on the stack);
  its thread only exists while recording. `fennec-core::ogg` writes the Ogg
  pages.
- **Memory**: recording stops early rather than eat into the 64 KB of heap
  kept free for Wi-Fi and TLS. Without the `opus` feature, recordings are
  IMA ADPCM WAV files (4:1, about 8 KB per second).
- **Upload**: one HTTPS request per recording, to the messaging service's
  API (currently Telegram's Bot API). The server certificate is
  checked against ESP-IDF's bundle of root certificates. On failure, Fennec
  reconnects and retries once.
- **Speaker**: an NS4168 amplifier on I2S (GPIO19 bit clock, GPIO33 word
  select, GPIO22 data) plays a short chime after each successful upload. Its
  word select shares GPIO33 with the microphone clock, so the two take turns:
  the firmware closes the microphone, plays through the speaker, and opens
  the microphone again before listening for the wake word.

## LED

| Colour          | Meaning                                     |
|-----------------|---------------------------------------------|
| orange          | connecting to Wi-Fi (flashes red on failure; it keeps retrying) |
| dim blue        | listening for the wake word                 |
| cyan            | recording                                   |
| purple          | uploading                                   |
| white           | receiving an over-the-air update            |
| green flash     | uploaded (followed by a chime)              |
| red flash       | upload failed (details in the serial log)   |
| off for a moment | nothing was said; nothing is uploaded      |
| two red blinks, repeating | no configuration on the device; run `fennec flash` |

## Button

- Press to start recording without the wake word.
- Press while recording to stop and send right away.

## Configuration reference

All settings live in a TOML file (start from `firmware/fennec.example.toml`).
The firmware image is the same for every device: `fennec flash` and
`fennec ota --settings` encode the file and write it to the device's
`fennec_cfg` partition, after checking it and stopping with an explanation
if something is missing or invalid.

| Setting                         | Default                     | Notes |
|---------------------------------|-----------------------------|-------|
| `wifi.ssid`                     | required                    | 2.4 GHz networks only (ESP32 limitation) |
| `wifi.password`                 | required                    | Empty for open networks |
| `wifi.channel`                  | none (scan every channel)   | 1-13; scanned first, helps with hidden networks |
| `telegram.bot_token`            | required                    | The device's own bot, see [telegram.md](telegram.md) |
| `telegram.chat_id`              | required                    | Number (`-100...`) or `"@channel"` |
| `telegram.message_thread_id`    | none                        | Topic in a forum group |
| `device.name`                   | `atom-echo`                 | Used in captions and file names |
| `device.led_brightness`         | 40                          | 0–255 |
| `wake_word.enabled`             | true                        | false: button only |
| `wake_word.model`               | `../models/hey_fennec.fkw`  | Relative to the config file |
| `wake_word.threshold`           | from the model              | 0–1; higher means fewer false wake-ups |
| `wake_word.consecutive`         | 2                           | Evaluations in a row above the threshold |
| `audio.mic_gain_db`             | 12                          | 0–40 |
| `audio.mic_channel`             | `right`                     | `left` or `right` |
| `audio.silence_ms`              | 1200                        | Pause that ends a recording |
| `audio.no_speech_ms`            | 4000                        | Give up if nothing is said |
| `audio.max_recording_seconds`   | 12                          | 1–30, also limited by free RAM |
| `audio.speech_margin_db`        | 9                           | Raise it in noisy rooms if recordings never end |
| `audio.button_preroll_ms`       | 0                           | 0–5000; button recordings start this long before the press, to capture missed wake words (see [wake-word.md](wake-word.md#capturing-missed-wake-words)) |
| `audio.wake_preroll_ms`         | 0                           | 0–5000; wake word recordings start this long before the detection, to capture false wake-ups (see [wake-word.md](wake-word.md#capturing-false-wake-ups)) |
| `ota.password`                  | empty                       | Enables over-the-air updates; see below |

### Several devices

Keep one configuration file per device. Files named `fennec.toml` or
`*.toml.local` are ignored by git, which keeps tokens and passwords out of the
repository:

```bash
nix run .#flash -- --config firmware/kitchen.toml.local
nix run .#flash -- --config firmware/office.toml.local
```

## Flash layout

| Partition      | Offset     | Size     | Contents                                  |
|----------------|------------|----------|-------------------------------------------|
| `nvs`          | `0x9000`   | 24 KB    | Wi-Fi calibration data (ESP-IDF)          |
| `otadata`      | `0xf000`   | 8 KB     | Which app slot boots                      |
| `phy_init`     | `0x11000`  | 4 KB     | Radio calibration (ESP-IDF)               |
| `ota_0`        | `0x20000`  | 1.6 MB   | App slot 1                                |
| `ota_1`        | `0x1c0000` | 1.6 MB   | App slot 2                                |
| `fennec_cfg`   | `0x360000` | 64 KB    | Device configuration                      |
| `fennec_model` | `0x370000` | 576 KB   | Wake word model (`.fkw`), used in place   |

`partitions.csv` is the source of truth; `fennec` reads the offsets from it.

## Building and flashing

From the flake's development shell (`nix develop`, or direnv):

```bash
cd firmware
cargo build --release
cd ..
./target/release/fennec flash --config firmware/fennec.toml
```

Or let Nix build the firmware: `nix run .#flash -- --config firmware/fennec.toml`.

`fennec flash` writes the bootloader, partition table and firmware, then the
configuration and the wake word model, and opens the serial monitor
(`--no-monitor` skips it). `espflash` finds the Atom Echo automatically when
it is the only serial device; otherwise pass `--port /dev/ttyUSB0`. Use
`--model` to flash a different model than the configuration names.

After that first flash, `cargo run --release` in `firmware/` is the quickest
way to try firmware changes: it rewrites only the app (and espflash's default
bootloader, which lacks rollback) and keeps the configuration and model.

ESP-IDF settings are in `sdkconfig.defaults`. `firmware/.cargo/config.toml`
points esp-idf-sys at it with a path relative to `firmware/`, so builds
with a custom `CARGO_TARGET_DIR` use it too. To check what a build used, look
at `out/sdkconfig` under `<target>/xtensa-esp32-espidf/release/build/esp-idf-sys-*/`.

## Build features

| Feature    | Default | What it does |
|------------|---------|--------------|
| `telegram` | yes     | Uploads recordings to Telegram. Without it, recordings are only logged; a starting point for other messaging services. |
| `opus`     | yes     | Uploads voice notes (Ogg Opus, `sendVoice`) instead of WAV documents (`sendDocument`). Implies `telegram`. |

The `opus` feature compiles libopus from the source tree in
`FENNEC_OPUS_SRC`, which the flake's development shell and `nix build
.#firmware` set (the nixpkgs `libopus` source, no network access needed).
Outside the flake, point it at an unpacked libopus 1.5 or newer, or build
without it:

```bash
cargo build --release --no-default-features --features telegram   # WAV uploads
```

libopus adds about 220 KB to the firmware: with it, the app takes about
1.57 MB of its 1.70 MB slot.

## Over-the-air updates

Set `ota.password` in the configuration and flash it once over USB. From
then on, the device runs a small HTTP server on port 80:

- `GET /` returns its name, firmware version, wake word, uptime and free
  memory as JSON, without a password.
- `POST /update/firmware`, `/update/config` and `/update/model` take the new
  contents with `Authorization: Bearer <ota.password>`.

`fennec ota` drives it:

```bash
fennec ota --host 192.168.1.42 --config firmware/fennec.toml --firmware
fennec ota --host fennec-kitchen --config firmware/fennec.toml --settings --model
fennec ota --host fennec-kitchen --config firmware/fennec.toml --all
```

The device checks what it receives (an ESP32 app image, a valid
configuration, a valid `.fkw` model), writes it, and restarts. `fennec ota`
waits for it to come back and prints its status.

New firmware is written to the app slot that is not running. The bootloader
boots it once; the firmware marks itself good after it joins Wi-Fi. If it
crashes or hangs before that, the next reset goes back to the previous
firmware. (This needs the bootloader `fennec flash` installs; espflash's
default one, written by `cargo run`, boots updates without rollback.)

The password travels in plain HTTP, so treat your local network as the trust
boundary: anyone who can see the traffic or guess the password can replace the
firmware. Without `ota.password`, updates over Wi-Fi are refused.

## Troubleshooting

**`fennec flash`: `no firmware at ...`**: build it first (`cargo build
--release` in `firmware/`), or use `nix run .#flash`, which builds it.

**Build outside the flake: `Unable to find libclang` or `libstdc++.so.6:
cannot open shared object file`**: run `. ./env.sh` in the shell you build
from (only needed with the espup-based nixomatic shell).

**Build outside the flake fails on NixOS with errors about
`xtensa-esp-elf-gcc` or `No such file or directory` for downloaded tools**:
enable nix-ld (`programs.nix-ld.enable = true;`), or use the flake, which
needs neither.

**`fennec ota`: `cannot reach the device`**: check the address in your
router, or in the serial log (`connected, address ...`). The device and your
computer must be on the same network.

**`fennec ota`: `updates are disabled`**: the configuration on the device has
no `ota.password`. Set one and run `fennec flash` once.

**Build: `CMake Error: The source ".../esp-idf-v5.5.5/components/bootloader/subproject/CMakeLists.txt"
does not match the source ".../.embuild/espressif/esp-idf/v5.5.5/..." used to
generate cache`**: the target directory was built before with a different
ESP-IDF checkout (the flake's copy in the Nix store versus the one esp-idf-sys
downloads into `.embuild/`). Remove the stale esp-idf-sys build, for example
`rm -rf target/xtensa-esp32-espidf/release/build/esp-idf-sys-*` in `firmware/`,
and build again.

**Flashing: no serial port appears (no `/dev/ttyUSB0`)**: the Atom Echo
shows up as an FTDI USB-serial adapter (`0403:6001`, product "M5stack") on
`/dev/ttyUSB0`. If `ls /sys/class/tty | grep USB` shows nothing while the
device is plugged in, the USB-C cable is most likely charge-only. Use one
that carries data, and plug it into the computer directly rather than through
a hub.

**Flashing: `Permission denied` on `/dev/ttyUSB0`**: add your user to the
`dialout` group (on NixOS: `users.users.<you>.extraGroups = [ "dialout" ];`),
then log in again.

**Boot loop with `***ERROR*** A stack overflow in task main has been
detected.`** right after `device "<name>"` in the serial log: the firmware
was built without `sdkconfig.defaults`, so the main task got ESP-IDF's
default 3.5 KB stack instead of 24 KB. Check `CONFIG_ESP_MAIN_TASK_STACK_SIZE`
in the build's `sdkconfig` (see [Building and flashing](#building-and-flashing)).
If you set `ESP_IDF_SDKCONFIG_DEFAULTS` yourself, use an absolute path. Such
a build also lacks rollback in its bootloader, so reflash with `fennec flash`
once it is fixed.

**The LED stays orange**: the device can't join the Wi-Fi network. Check the
SSID and password, and that the network is 2.4 GHz. The serial log has the
details.

**`cannot connect to Wi-Fi network ...: ESP_ERR_TIMEOUT` a few times, then it
connects**: typical of hidden networks, which only answer probes for their
name. The device keeps retrying and gets there, but setting `wifi.channel`
to the network's channel makes it connect on the first try. The serial log
shows the channel once connected (`wifi:connected with ..., channel 1`).

**`task_wdt: Task watchdog got triggered ... IDLE0 (CPU 0) ... CPU 0: main`
every 5 seconds**: wake word inference is slower than the 100 ms it runs
every (see `wake word inference takes ... ms` at startup), so the main task
never lets the idle task run. The device keeps working, but detections lag.
Check that the flash runs at 80 MHz (`SPI Speed : 80MHz` in the boot log):
at 40 MHz the bundled model takes about 87 ms instead of 72 ms. Otherwise,
use a smaller model (`fennec train --width 0.7`).

**`i2s_channel_register_event_callback(...): on_recv callback not in IRAM`**
at startup: the firmware was built with `CONFIG_I2S_ISR_IRAM_SAFE=y`, which
the I2S driver in esp-idf-hal does not support. Remove it from
`sdkconfig.defaults` and rebuild.

**Red flash after recording**: the upload failed. Look for
`Telegram answered ...` in the serial log:
- `401 Unauthorized`: wrong `bot_token`.
- `400 Bad Request: chat not found`: wrong `chat_id`, or the bot is not in
  the group.
- `403 Forbidden`: the bot was removed from the group.

**Recordings are silent or very quiet**: try `audio.mic_channel = "left"`.
If they are just quiet, raise `audio.mic_gain_db`.

**Recordings never end before the time limit**: the room is louder than
expected. Raise `audio.speech_margin_db`, or press the button to finish.

**It does not hear the wake word**: watch the serial log while you say it.
`wake word score peaked at 0.71 (threshold 0.92)` means it came close: lower
`wake_word.threshold` a little (try -0.05 at a time; it raises false
accepts). A peak *above* the threshold, such as `peaked at 0.97`, means it
was reached on fewer than `wake_word.consecutive` evaluations in a row,
often a single 100 ms spike; lowering the threshold usually catches those
too. No such line at all means the model scored below half the threshold.
`fennec eval --consecutive` shows what each setting costs in false accepts.
The bundled model is trained on synthetic voices only, so it hears some
people and microphones better than others; record yourself with the button
and check the recording with `fennec listen --scores models/hey_fennec.fkw
recording.ogg`, or train with your own recordings (see
[wake-word.md](wake-word.md#improving-a-model)).

**It wakes up by itself**: raise `wake_word.threshold` (try +0.05 at a time).
Better still, train with background recordings from the room; see
[wake-word.md](wake-word.md#improving-a-model).

## Other devices

Fennec only runs on the M5Stack Atom Echo today, but most of it is not tied
to that board:

- `fennec-core` (front end, wake word, end of speech detection, ADPCM, Ogg)
  is `no_std` Rust with no hardware dependencies.
- The firmware uses ESP-IDF through `esp-idf-svc`, so other ESP32 chips build
  with a different `MCU` and target in `firmware/.cargo/config.toml`.

What a port has to change:

- `mic.rs`: the Atom Echo has a PDM microphone; many boards use an I2S MEMS
  microphone (e.g. INMP441) instead.
- `speaker.rs` and `audio.rs`: the speaker's pins, and the pin it shares with
  the microphone on the Atom Echo. Boards without that limitation can keep
  both open.
- `led.rs`: one SK6812 LED on GPIO27.
- The button on GPIO39 in `main.rs`.
- `partitions.csv` and `CONFIG_ESPTOOLPY_FLASHSIZE_4MB`, for other flash
  sizes.
- Timings: wake word inference (72 ms here) and Opus encoding (9 ms per 20 ms
  on the second core) were measured on a 240 MHz dual-core ESP32. Single-core
  chips such as the ESP32-C3 would run the encoder on the same core as the
  wake word, and chips with PSRAM can afford longer recordings.
