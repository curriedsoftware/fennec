# Tasks

Open work for Fennec. Keep `README.md` and `docs/` in sync with each change,
describing only what is implemented.

## Verify on hardware

- [x] Record a second time right after the chime, to confirm the microphone
      comes back after the speaker has used GPIO33. (2026-10-22: four
      button recordings in a row each ended on silence after speech and
      uploaded, with the chime after every upload.)
- [x] Flash the build with `wifi.channel` and check that the device joins the
      hidden network on the first attempt. (2026-10-22: connected in about
      2.5 s on the first attempt; without it, two timeouts and about 48 s.)

## Wake word speed

Done 2026-10-22: inference went from 172 ms to 72 ms on the Atom Echo with
the bundled `hey_fennec.fkw`, and the `task_wdt` warnings are gone. The
device's score on silence (0.0058) matches `fennec inspect`.

- [x] Build `fennec-core` with `opt-level = 3`. (No measurable effect on its
      own: 184 ms.)
- [x] Compute four convolution outputs at a time, summed over all input
      channels in registers (111 ms).
- [x] Use the ESP32's fused multiply-add `madd.s` (87 ms).
- [x] Run the flash at 80 MHz, in `sdkconfig.defaults` and in every image
      header `fennec flash`, `nix/firmware.nix` and `cargo run` write
      (72 ms).

Copying the model into RAM is no longer needed for speed, and 192 KB would
not fit next to Wi-Fi and a recording anyway.

## Wake word accuracy

First real test, one speaker, 1 m away: one detection at 0.94 (threshold
0.92) out of several attempts; the misses never reached half the threshold
(no `score peaked` lines). The bundled model is trained on synthetic voices
only.

Six button recordings of one speaker, scored with `fennec listen --scores`:
every take peaked between 0.92 and 0.98, but only 3 reached 0.92 on two
evaluations in a row. With `threshold = 0.88` (and `consecutive = 2`) 5 of 6
trigger; `fennec eval` puts that at about 1-2 false accepts per hour on the
synthetic stream (0 at 0.92). Setting `consecutive = 1` at 0.92 also gets
5 of 6, at 2 per hour.

- [ ] Try `wake_word.threshold = 0.88` on the device for a few days and note
      false wake-ups.
- [ ] Record more real "hey fennec" samples with the button (several
      people, distances, rooms) into `work/hey_fennec/positive-real/`; the
      CLI now reads the device's ADPCM WAVs.
- [ ] Add real recordings to training (positives and room background) and
      compare recall against the synthetic-only model.
- [ ] Reconsider the default threshold of 0.92 once there is real-voice
      data.
- [x] `fennec enroll`: record 3 takes plus a test take and 30 s of room,
      then train in one command. Training now varies every clip's speed
      (keeping pitch), voice (±3 semitones), distance, microphone and cut,
      and makes 9 negatives from each recorded take.
- [x] Run `fennec enroll` with real takes recorded through the Atom Echo
      and compare against the bundled model. (2026-10-22: 9 button
      recordings of one speaker, 3 trained on and 6 held out, plus 2 of
      "hey siri". The bundled model detects 1 of the 6 held-out takes; the
      enrolled model (threshold 0.96) all 6, at 0.999 or more, and scores
      the "hey siri" takes 0.01 and 0.06. The same takes with the previous
      augmentation also detect all 6 but score one "hey siri" 0.44.
      Button recordings start with two clicks (press and release), which
      training now leaves out.)
- [ ] Flash an enrolled model and note detections and false wake-ups over
      a few days, including far from the device and with the TV on.
      (2026-10-22: flashed `work/real/new-v2.fkw`, retrained on the
      enrolled dataset, at threshold 0.90: on 5 h of held-out audio 88.2%
      synthetic recall and 6 of 6 recorded takes at 0.6 false accepts/h,
      against 82.5% for the previous enrolled model at 0.95. The device had
      been running the bundled model, which detects none of the recorded
      takes at 0.95.)
- [x] A last layer that hears the whole phrase (1.25 s instead of 0.45 s;
      synthetic takes are 0.8 s median, 1.1 s p90) did not improve recall
      at the same false accept rate; not adopted.
- [x] `audio.button_preroll_ms`: button recordings start up to 5 s before
      the press, so missed wake words get uploaded.
- [x] `audio.wake_preroll_ms`: wake word recordings start up to 5 s before
      the detection, so false wake-ups upload what caused them.
      (2026-10-23: a false wake-up on distant sounds uploaded 4.5 s of
      faint noise that scored at most 0.04; the trigger itself was lost.
      The device ran threshold 0.90, set for `new-v2`, while `new-v3`
      stores 0.96.)
- [ ] Verify the wake word pre-roll on the device: the upload starts with
      the wake word, and a false wake-up is sent even without speech.
- [ ] `firmware/fennec.toml` has `wake_preroll_ms = 2000` while hunting
      false wake-ups; set it back to 0 (`fennec ota --settings`) once the
      wake word is stable.
- [ ] Check why faint sounds 10-14 dB above the room count as speech for
      the endpointer (the false wake-up above was uploaded, not dropped).
- [ ] Verify the pre-roll on the device: press twice after a miss, check
      the upload holds the take, the heap during and after the recording,
      and that the live audio after the press has no gaps.

## Recording length and memory

A button-triggered recording stopped at 6.1 s (`MaxLength`) with 66 KB of
heap free, well under `audio.max_recording_seconds = 12`. That was after
about 9 minutes of uptime. Right after a fresh boot, the heap left beside a
recording was about 116 KB on the first recording and about 108-111 KB on the
next three: a one-time drop of about 8 KB after the first upload, then
stable, so not a per-recording leak.

- [ ] Check whether free heap shrinks with uptime (66 KB after 9 minutes vs.
      about 90 KB on a fresh boot), e.g. log it every minute while idle.
- [ ] Find where the heap goes (Wi-Fi, TLS, OTA server, I2S buffers) and
      whether the 64 KB reserve can shrink.
- [ ] Check why the endpointer did not end the recording on silence (room
      noise vs. `audio.speech_margin_db`).

## Replies played on the device

Flow: wake word → record → upload → wait for the main bot's reply → play
it → back to wake word. The speaker side works (`firmware/src/speaker.rs`,
`firmware/src/audio.rs`).

- [ ] `telegram::send`: return the `message_id` of the uploaded recording.
- [ ] Wait for the reply: long-poll `getUpdates` with the device bot's token
      and accept only messages whose `reply_to_message.message_id` matches.
      Track the update `offset`.
- [ ] Download the reply (`getFile`, then the file URL) and stream it into
      the speaker: WAV, 16 kHz mono, PCM or IMA ADPCM (`adpcm::decode`
      exists in `fennec-core`).
- [ ] Text replies: play a short "text reply" chime, or ignore them.
- [ ] New LED states: waiting for a reply, playing.
- [ ] The button cancels the wait or stops playback.
- [ ] `[reply]` configuration: `enabled`, `timeout_s` (about 30), `volume`
      (replaces the hard-coded `SPEAKER_VOLUME`). Add it to
      `fennec-core::config`, the CLI and `fennec.example.toml`.
- [ ] Replace the test chime after uploads with the reply flow (or keep it
      as the "uploaded" sound).
- [ ] Optional: after playing a reply, listen for a follow-up without the
      wake word for a few seconds.
- [ ] Docs: `docs/telegram.md` says the device never reads messages and
      device bots need no settings. Device bots then need Bot-to-Bot
      Communication Mode to see the main bot's replies. Add an example main
      bot that answers with a WAV (TTS on the bot side, e.g. Piper).

## Voice notes instead of WAV documents

Built 2026-10-22 behind the `opus` feature (on by default; `telegram` is the
other feature). Measured on the Atom Echo, fixed-point libopus, thread
pinned to the second core: 6.8 ms per 20 ms frame at complexity 0, 8.9 ms at
2 (used), 12.7 ms at 5; 15 KB heap and about 24 KB stack. The float build
was much slower (36 ms per frame) and needs 32 KB of heap.

- [x] Spike: build libopus for the ESP32 (`firmware/components/opus`, source
      from nixpkgs through `FENNEC_OPUS_SRC`), measure encode time and heap.
- [x] Encode while recording on a thread on the second core, write Ogg pages
      in `fennec-core::ogg`, upload with `sendVoice`.
- [x] Keep WAV for builds without the `opus` feature.
- [x] Docs: `docs/telegram.md` (`message.voice`, example bot accepts both),
      `docs/firmware.md` (features, voice notes), README, `wake-word.md`;
      the CLI reads `.ogg` recordings through ffmpeg.
- [x] Confirm on the device: recordings of 4.5 s and 3.7 s ended on silence
      and uploaded with `sendVoice` in 0.9-2 s; the downloaded files are
      byte-identical to what the device built (13,309 and 10,794 bytes),
      `ffprobe` reads Opus, mono, 23.6 kbit/s with the right durations, and
      ffmpeg decodes them without errors. The first version stopped every
      recording after 1 s: the heap check counted the encoder thread's 48 KB
      against the upload reserve, although it is freed before the upload.
- [ ] Optional: skip or fade in the first ~150 ms of button-triggered
      recordings. The button click reaches full scale there (also in the WAV
      recordings, so not caused by Opus).
- [ ] Flash space: the app is 1.57 MB of its 1.70 MB slot (92%). The model
      partition has 576 KB for a 192 KB model; shrinking it would leave room
      to grow both app slots.

## Housekeeping

- [ ] `firmware/target/` has a CMake cache from the `.embuild` ESP-IDF; it
      fails inside the flake shell until
      `target/xtensa-esp32-espidf/release/build/esp-idf-sys-*` is removed.
- [x] Update the module comment in `firmware/src/main.rs` to mention the
      speaker.
