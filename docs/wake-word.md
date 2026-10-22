# Training a wake word

Fennec's wake word models are small convolutional networks (about 200 KB)
that the Atom Echo evaluates every 100 ms on the last 1.5 s of audio. You train
them on Linux with the `fennec` CLI. No GPU and no recordings are needed:
`fennec` synthesizes the training data with Piper text-to-speech. Your own
recordings make a model better, though, and adding a few takes minutes:
[`fennec enroll`](#the-quick-way-fennec-enroll) does it in one command from
three takes of your voice.

The pipeline:

```
fennec voices   download Piper voices (once)
fennec synth    say the wake word and lots of other things with ~1000 voices
fennec record   (optional) add your voice and your home's background noise
fennec train    augment, train, evaluate, pick a threshold, write the .fkw
fennec enroll   all of the above in one go, from a few takes of your voice
fennec listen   try the model live with your computer's microphone
```

On a 16-core machine the whole run takes well under an hour, mostly training.

## Environment

The flake's development shell has everything. From the repository root:

```bash
nix develop        # or `direnv allow` once

cargo build --release -p fennec-cli
alias fennec=$PWD/target/release/fennec
```

`nix run . -- <command>` runs the same CLI without building it yourself.

- Piper provides the text-to-speech engine. The shell points `FENNEC_PYTHON`
  at a Python that can `import piper`, which `fennec synth` drives. Piper is
  pinned to nixpkgs revision `8150a74` (Piper 1.7.0) because the current
  nixpkgs package fails to build.
- `arecord` (alsa-utils) is used by `fennec record` and for live testing.

Without the flake, a nixomatic shell works too:

```bash
nix --extra-experimental-features 'nix-command flakes' \
    develop 'https://nixomatic.com/?p=rustc,cargo,piper-tts:8150a74,alsa-utils' \
    --accept-flake-config
```

## Choosing a phrase

Good wake words:

- have **three or more syllables**: "hey fennec" (3), "okay computer" (5).
  Single words like "fennec" trigger by accident much more often.
- **don't sound like everyday words or names**. "Hey Siri" works because
  nobody says "siri" otherwise.
- are **easy for Piper to pronounce**. Run `fennec synth` with
  `--positives 20 --negatives 0`, listen to a few files in `positive/`, and
  see the tips below if they sound wrong.

Piper reads the phrase in English. For an unusual word, a phonetic spelling
often sounds better ("hey fen-ick"). Check the clips before training.

## The quick way: `fennec enroll`

`fennec enroll` retrains a wake word for your voice from three short takes,
in one command:

```bash
fennec enroll --phrase "hey fennec" --data work/hey_fennec --voices work/voices \
    --out work/hey_fennec_me.fkw
```

It walks you through everything, and you only have to be at the computer
for the first minute or two:

1. **The dataset.** If `work/hey_fennec/` doesn't exist yet, it downloads the
   default voices into `--voices` (if they aren't there) and runs
   `fennec synth` with the default sizes and your `--confusable` phrases.
   That takes a few minutes. An existing dataset is used as it is;
   `--phrase` is then optional, and must match it.
2. **Your takes.** It asks for three takes of the wake word to train on
   (`--takes`) and one more to test with (`--test-takes`), 3 s each. Press
   Enter, then say the wake word the way you normally would. Each take is
   checked, and you are asked to record it again if it clips, is very quiet,
   is cut off at either end, is too short or too long, has too much room noise
   compared to your voice, or is much longer or shorter than your other takes
   (often a sign that it wasn't the wake word, said once). Answer `n` to keep
   it anyway.
3. **Your room.** 30 s of the room as it usually sounds, without the wake
   word (`--room-seconds`, 0 to skip).
4. **Training**, exactly as [`fennec train`](#4-train) with its default
   settings (`--steps` and `--seed` can be changed), followed by its usual
   report.
5. **Your test take**, scored by the new model in a quiet room and with
   noise 10 dB below your voice:

   ```
   your test takes, threshold 0.92 ("recall (recorded)" above mixes noise at 10 to 20 dB):
       quiet  noise at 10 dB
     0.981 ok     0.947 ok      work/hey_fennec/positive-test/0000.wav
   ```

The takes land in `positive-real/`, the test take in `positive-test/` and
the room in `background/`, so they are kept for the next training run. Each
`fennec enroll` adds new takes and never replaces old ones; running it again
with another person in the house adds their voice.

To use recordings you already have instead of recording new ones, for
example voice notes the Atom Echo uploaded, pass them as files. The last
`--test-takes` of them are kept for testing, and the checks above only warn:

```bash
fennec enroll --data work/hey_fennec --out work/hey_fennec_me.fkw \
    ~/Downloads/voice-1.ogg ~/Downloads/voice-2.ogg ~/Downloads/voice-3.ogg ~/Downloads/voice-4.ogg
```

Recordings made with the Atom Echo's button start with its click, once when
it is pressed and once when it is released. You can use them as they are: in
every recording in `positive-real/` and `positive-test/`, training looks for
the wake word itself. That is the loudest stretch well above the room's
noise, leaving out bursts shorter than 80 ms (like clicks) and joining
sounds less than 0.4 s apart (like two words). Other devices may need more
or less cleaning; see
[Recordings from other devices](#recordings-from-other-devices).

### What happens to three takes

Three takes are not much, so training makes the most of them. The synthetic
clips stay the base of the model: they cover the many ways a wake word can be
said, which three takes of one person can't. Your takes teach it what your
voice and your microphone sound like. They are picked for 30% of the wake
word windows, and every time one is picked, it changes:

| Change       | How                                                                     |
|--------------|-------------------------------------------------------------------------|
| Speed        | 85% to 115%, keeping the pitch                                          |
| Voice        | up to 3 semitones higher or lower, at the same speed; pitch and formants move together, like a smaller or larger speaker |
| Distance     | across the room (duller, more echo than voice), close up (more bass, no echo), or a room's echo at a normal distance |
| Microphone   | a high-pass filter and a boost or cut somewhere between 300 and 4000 Hz |
| Cut          | sometimes up to 8% missing at the start or 5% at the end: still the wake word |
| Level, noise | as for every window (see [Train](#4-train))                             |

Every clip, synthetic or recorded, wake word or not, gets the same changes,
so that a changed sound never gives the wake word away by itself.

Your takes also become **negatives**: the same voice and microphone saying
something that is *not* the wake word. From each take, `fennec` makes nine:
the take played backwards, its words swapped ("fennec hey"), its first word
twice ("hey hey"), its first word followed by other speech, and other
speech followed by its last word. It splits a take at the quietest point
between 25% and 70% of its length, usually the gap between the words. These
join the other negatives, so hard-negative mining replays the ones that fool
the model. Cutting a take more than a little, at the start or at the end, is
a negative too, as for synthetic clips.

### How many takes

Three is enough to make a difference, and five is better. Ten or more,
recorded on different days and at different distances, is better still:
however much training changes three takes, it can't invent the way you say
the wake word when you're tired, in a hurry, or across the room. Run
`fennec enroll` again later to add more.

Make the test take count. With fewer than four takes in `positive-real/`,
none of them is held out, and `positive-test/` is the only check of how the
model does on your voice. A take you have recorded is the most honest
number there is; also try the model live with `fennec listen` (see
[Test it on your computer](#5-test-it-on-your-computer)).

### A recipe that works

This is the exact procedure behind the numbers below, for "hey fennec"
recorded with the Atom Echo. One speaker, nine button recordings: three to
train on and six held out. On the six held-out takes:

| Model                                  | Held-out takes detected | Peak scores (`fennec listen`) |
|----------------------------------------|-------------------------|-------------------------------|
| `models/hey_fennec.fkw` (synthetic only) | 1 of 6                | 0.08 to 0.93  |
| Enrolled with 3 takes (threshold 0.96) | 6 of 6                  | 0.999 to 1.000 |

The enrolled model scored two recordings of "hey siri" 0.01 and 0.06, and
had no false accepts in an hour of held-out synthetic speech and noise at
its threshold.

1. **Build the same dataset as the bundled model**, with 10000 negatives
   and its confusable phrases. `fennec enroll` would otherwise create one
   with the default 6000 negatives and none of the confusables:

   ```bash
   fennec voices --dir work/voices
   fennec synth --phrase "hey fennec" --out work/hey_fennec --voices work/voices \
       --negatives 10000 \
       --confusable "hey fenwick" --confusable "hey phoenix" --confusable "fennel" \
       --confusable "hey phoebe" --confusable "hey venice" --confusable "hey finn"
   ```

2. **Record nine takes with the device.** With the firmware and settings
   you normally use, press the button, say "hey fennec" the way you
   normally would, and let it upload. Say three close to the device, three
   at about a metre and three from across the room. Also record two or three
   near misses, like "hey siri" or a name that sounds alike. Save the voice
   notes from the chat into one directory, e.g. `~/fennec-takes/`.

3. **Enroll with three takes and test with six.** Pass one close, one
   middle and one far take first; the last six files are only used for
   testing:

   ```bash
   fennec enroll --data work/hey_fennec --out work/hey_fennec_me.fkw --test-takes 6 \
       close-1.ogg middle-1.ogg far-1.ogg \
       close-2.ogg close-3.ogg middle-2.ogg middle-3.ogg far-2.ogg far-3.ogg
   ```

   The import shows how much speech it found in each file. "Hey fennec" said
   at a normal pace is about half a second, clicks left out. A take with much
   more than the others holds something else too; leave it out.

   Training took 22 minutes with 60 threads; the default 6000 steps and seed
   are what was used. At the end, every test take should score close to 1,
   both quiet and with noise:

   ```
   your test takes, threshold 0.96 ("recall (recorded)" above mixes noise at 10 to 20 dB):
       quiet  noise at 10 dB
     0.994 ok     0.999 ok      work/hey_fennec/positive-test/0000.wav
     ...
   ```

4. **Check the near misses.** They should stay far below the threshold:

   ```bash
   fennec listen --scores work/hey_fennec_me.fkw ~/fennec-takes/hey-siri-1.ogg
   ```

5. **Send it to the device** (see [Use it on the device](#6-use-it-on-the-device)),
   and keep the voice notes of any misses or false wake-ups: they are the
   best material for the next `fennec enroll`.

If a test take is missed, add more takes rather than moving that one into
training, so that the test stays honest: run `fennec enroll` again with three
new recordings and `--test-takes 0`. The existing test takes are scored by
the report's "recall (recorded)" column.

### Capturing missed wake words

A wake word the device misses is never uploaded, yet it is the best take to
train on. With `audio.button_preroll_ms = 3000` in `fennec.toml`, the device
keeps the last 3 s of audio while it listens (about 24 KB of RAM), and a
recording started with the button begins that long before the press. When
it misses "hey fennec", press the button twice (start and stop): the upload
holds the take it missed, with `preroll=3.0s` in its caption, and is sent
even if you say nothing after the press.

Pass these voice notes to `fennec enroll` like any other take. Training
looks for the wake word in each recording and leaves the button's clicks
out; if you also spoke after the press, check how much speech the import
found (see below), since the loudest stretch is kept. Pre-roll audio is
stored as IMA ADPCM before it is encoded as Opus, so it sounds very
slightly worse than the rest of the recording.

### Capturing false wake-ups

A recording started by the wake word begins after the detection, so a
false wake-up uploads whatever came next, not the sound that fooled the
model. With `audio.wake_preroll_ms = 2000`, wake word recordings begin 2 s
before the detection: enough for the 1.5 s the model hears. A false
wake-up is then uploaded with its cause, with `preroll=2.0s` in the
caption, even if nothing is said after it. Real wake-ups start with the
wake word. Both settings share one buffer, as long as the longer of the
two.

Score the upload with the model on the device to see what fired (add
`--threshold` if `wake_word.threshold` overrides the model's):

```bash
fennec listen --scores work/hey_fennec_me.fkw kitchen-1234.ogg
```

The peak should be in the first 2 s. Cut that stretch out and add it to
the dataset as a negative: speech in `negative-real/`, a cough, knock or
other sound in `sounds/`. Then retrain (see
[Adding data and training again](#adding-data-and-training-again)).

### Recordings from other devices

Fennec targets the Atom Echo today, but recordings can come from any
device: another ESP32 board, a phone, a laptop. Whether they need cleaning
before training depends on what the device adds around your voice. The
Atom Echo adds two short button clicks, which training already leaves out.
Another device might add nothing, a longer start beep, or a click right
before you speak. Check every new kind of device once, before training on
its recordings.

**What training does by itself.** Only recordings in `positive-real/` and
`positive-test/` are cut, to the wake word as described above:

- the noise floor is the level of the quietest 10% of the recording, in
  10 ms frames, and sound counts when it is at least 15 dB above it
- bursts shorter than 80 ms are dropped
- what remains is joined across gaps shorter than 0.4 s, and the stretch
  with the most energy is kept, with 30 ms of margin on each side

Synthetic clips in `positive/` only lose their leading and trailing
silence, and so do clips in `sounds/`. `negative/`, `negative-real/` and
`background/` are used as they are.

**When nothing needs to be done:**

- the recordings start and end with a bit of quiet room, with no sound from
  the device itself (a recording started by voice, or by a silent button)
- the device's sounds are shorter than 80 ms (clicks)
- clicks or beeps in `negative-real/` or `background/`: they are never part
  of the wake word, so they only teach the model to ignore them

**When the recordings need cleaning:**

- a beep, chime or other device sound of 80 ms or more, at the start or
  the end
- a click less than 0.4 s before or after the wake word, which gets joined
  to it
- a device sound that is louder and longer than the wake word, which gets
  picked instead of it
- a recording that is cut tight around the wake word with no quiet around
  it, so that the noise floor is the quietest part of the speech and part
  of the wake word can be dropped

**How to check.** Import the recordings and read how much speech
`fennec enroll` found in each. A wake word of two or three syllables at a
normal pace is 0.4 to 1 s, and one person's takes are similar: `fennec
enroll` warns about a take shorter than 0.6 or longer than 1.7 times the
median of the others. A length far outside that, or that warning, means
the detection caught something else. To see what is there, print the level of
each 10 ms frame (the development shell has ffmpeg):

```bash
ffmpeg -v error -i take.ogg -f s16le -ac 1 -ar 16000 - | python3 -c '
import math, sys, array
s = array.array("h", sys.stdin.buffer.read())
for i in range(0, len(s) - 160, 160):
    rms = math.sqrt(sum(x * x for x in s[i:i + 160]) / 160) / 32768
    db = 20 * math.log10(max(rms, 1e-9))
    print(f"{i // 16:5d} ms {db:6.1f} dB " + "#" * max(0, int(db + 70)))
'
```

In an Atom Echo button recording, this shows a frame at about -15 dB at
0 ms that fades within 50 ms (the press), a few frames up to about -30 dB
between 100 and 200 ms (the release), quiet at about -62 dB, and then the
wake word at about -35 dB.

**How to clean.** Cut the device's sounds off with sox or ffmpeg and keep
at least 0.3 s of quiet room before and after the wake word, so the noise
floor can still be measured. For example, to drop the first 0.25 s of
every recording before importing it:

```bash
mkdir -p clean
for f in raw/*.ogg; do
  sox "$f" -r 16000 -c 1 "clean/$(basename "${f%.*}").wav" trim 0.25
done
```

Keep the originals elsewhere, and check the cleaned files the same way.
If a device always adds the same sound, it is better to fix it where the
recording is made, so that every recording from it is clean.

## 1. Download voices

```bash
fennec voices --dir work/voices
```

This downloads six English voices (about 420 MB): LibriTTS-R (904 speakers),
VCTK (109 British speakers), L2-ARCTIC (24 non-native speakers), ARCTIC, Lessac
and Alba. Together they cover more than a thousand speakers of different
accents, ages and genders.

Add voices for other accents with their Piper names, listed at
[rhasspy/piper-voices](https://huggingface.co/rhasspy/piper-voices). For
example, a Spanish voice reading the English phrase gives Spanish-accented
samples:

```bash
fennec voices --dir work/voices es_ES-sharvard-medium es_MX-claude-high
```

## 2. Synthesize the dataset

```bash
fennec synth --phrase "hey fennec" --out work/hey_fennec --voices work/voices \
    --confusable "hey fenwick" --confusable "hey phoenix" --confusable "fennel"
```

This writes into `work/hey_fennec/`:

| Directory   | Contents                                                       |
|-------------|----------------------------------------------------------------|
| `positive/` | 4000 clips of the wake word, with random speakers, speeds and intonations |
| `negative/` | 6000 clips of other speech: fragments of the phrase ("hey", "fennec", "hey fenn"), the phrase's first word followed by random words, everyday sentences, other assistants' wake words, random word sequences, and your `--confusable` phrases |
| `meta.json` | The phrase                                                     |

`--confusable` phrases sound like the wake word but aren't it. They are the
best way to stop the model from reacting to them. Add the names of people and
pets in your home if they sound similar.

The run takes a few minutes, using half of your CPU cores by default
(`--workers`). Interrupt it and start it again and it resumes where it
stopped. Run it again with bigger `--positives` and `--negatives` to extend a
dataset.

## 3. Add real recordings (optional, recommended)

Synthetic voices get a model a long way, but real recordings help with the
last part, especially with the Atom Echo's microphone and your room.

**Your voice saying the wake word.** About 20 to 50 clips, at different
distances, speeds and moods:

```bash
fennec record --data work/hey_fennec --kind wake-word --count 30
```

A few more to test with, which training never sees:

```bash
fennec record --data work/hey_fennec --kind test --count 3
```

**The sound of your home.** This matters most for false wake-ups: the TV,
music, a podcast, the kitchen, children. Record a few minutes in one go, or
copy in any WAV files you have:

```bash
fennec record --data work/hey_fennec --kind background --count 5 --seconds 120
```

**Other things you say often**, especially near the device:

```bash
fennec record --data work/hey_fennec --kind negative --count 20 --seconds 3
```

**Sudden sounds** near the device that woke it up: coughs, sneezes,
laughter, claps, a door, keys on the table. Make one or a few per clip, and
don't speak:

```bash
fennec record --data work/hey_fennec --kind sound --count 20 --seconds 3
```

The clips end up in `positive-real/`, `positive-test/`, `background/`,
`negative-real/` and `sounds/`.
You can also put WAV files there yourself; any sample rate works. To record
through a particular microphone, pass `--device` with an ALSA device name
from `arecord -L`.

To use the Atom Echo itself as the microphone, flash it with
`wake_word.enabled = false`, press the button and talk. Every recording shows
up in your group chat; save them into `positive-real/` (for the wake word)
or `background/`. They work as they are: `fennec` reads both the voice notes
(`.ogg`, decoded with ffmpeg from the development shell) and the IMA ADPCM
WAVs of firmware built without the `opus` feature.

Large collections work too. Pass any directory of WAV files of continuous
noise with `--background`, e.g. the [MUSAN](https://www.openslr.org/17/)
corpus (music, speech and noise, 11 GB), and of short sound events with
`--sounds`, e.g. [ESC-50](https://github.com/karolpiczak/ESC-50) (coughs,
sneezes, doors, dogs, sirens...).
[Augmenting the dataset](#augmenting-the-dataset-with-nixomatic) shows how
to download and convert them, and where to find more.

## 4. Train

```bash
fennec train --data work/hey_fennec --out models/hey_fennec.fkw
```

Each training step generates a fresh batch of 1.5 s windows:

- **positives** end with the wake word, as the device hears it just after you
  said it. The speech gets a random speed (85% to 115%), voice (up to 3
  semitones higher or lower), distance, microphone and room, sometimes a
  little cut off at either end, a random level, and noise at a random
  signal-to-noise ratio (see
  [What happens to three takes](#what-happens-to-three-takes)).
- **negatives** hold other speech, the beginning or the end of the wake word
  on its own (so the model waits for the whole phrase), one to three sudden
  sounds, or just noise. Speech and sounds get the same changes as the wake
  word. Recordings in `positive-real/` also make negatives of their own,
  e.g. played backwards or with their words swapped.
- **sounds** are short, loud and close to the microphone, at the level of
  speech: coughs, sneezes, laughter, claps, doors, dishes. They come from
  `sounds/` and `--sounds` directories (60% of the time when there are any)
  or are synthesized: coughs and throat clearing, knocks and claps, clatter,
  beeps and whistles, breaths. Without them the model only hears such sounds
  as quiet background noise, and a cough right next to the device can wake
  it up. One in ten positives also gets a sound, quieter than the voice, so
  that a sound doesn't hide the wake word either.
- the noise is synthetic (white, pink, brown, mains hum), distant chatter mixed
  from the negative clips, or your background recordings.
- from step 1000 on, every 500 steps, `fennec` scores all negative clips
  (and recorded sounds) and replays the 10% the model likes most more often
  (hard-negative mining).
  Near-misses like "hey phoenix" or "hey fan" are what teach it where the
  wake word ends.

Features are computed with `fennec-core`, the same code the firmware runs.

After training, `fennec` exports the model, checks that the device's inference
code gives the same results as the training framework, and then runs the
real streaming detector over held-out audio it never saw during training.
This is the report for the bundled `models/hey_fennec.fkw`:

```
threshold    false acc./h        recall
     0.50           17.00         97.5%
     0.60           12.00         96.2%
     0.70            9.00         94.8%
     0.80            3.00         93.0%
     0.90            1.00         88.2%
     0.92            0.00         88.0%   <- chosen
     0.95            0.00         84.8%
     0.99            0.00         69.0%
(false accepts measured on 1.00 h of held-out synthetic speech and noise)
```

- **false acc./h**: how often the detector fired, per hour, on held-out
  speech and noise that doesn't contain the wake word. The held-out stream is
  much denser than real life (a new phrase or sound every 0.5 to 3 s, many of
  them near-misses), so these rates are pessimistic. One in four is a sound:
  a held-out clip of `sounds/` and `--sounds` or a synthetic one.
- **recall**: share of held-out wake word clips it detected, played over
  background noise at 10 to 20 dB SNR.
- **chosen**: the lowest threshold with at most `--max-false-accepts` (default
  0.5) false accepts per hour. It is stored in the model and the firmware uses
  it unless `wake_word.threshold` overrides it.

With recordings in `positive-real/`, the report gets a separate recall column
for them. That is the most honest number in the table.

These numbers are measured on synthetic data, so expect more false wake-ups
in a real home, especially with a TV on. Background recordings from your home
are what improves that.

Useful options:

| Option                    | Default | What it does                                   |
|---------------------------|---------|------------------------------------------------|
| `--steps`                 | 6000    | Training steps. 3000 is often enough; more helps with large datasets |
| `--batch-size`            | 256     | Windows per step                               |
| `--width`                 | 1.0     | Network size. 0.7 halves the compute on the device |
| `--background DIR`        |         | Extra background WAV directories               |
| `--sounds DIR`            |         | Extra directories of sound events (coughs, doors...) |
| `--eval-minutes`          | 60      | Held-out audio used for the false accept rate  |
| `--max-false-accepts`     | 0.5     | Target false accepts per hour for the threshold |
| `--seed`                  | 1       | Change it to get a different model             |

Re-run the evaluation of any model on any dataset with `fennec eval`, and see
what a model contains with `fennec inspect`. `fennec eval --consecutive 1`
measures the device setting `wake_word.consecutive = 1` (one evaluation above
the threshold is enough) instead of the default 2. To see how a model scores
a single recording, for example one the device uploaded, run
`fennec listen --scores <model> <file.wav>`. `fennec eval --worst 20` also
lists the held-out negative clips that score highest (each has a `.txt` next
to it with what it says), and how often the beginning of the wake word on its
own reaches the threshold:

```
1 of 1000 held-out negative clips reach the threshold (0.92). Highest scores:
  0.975  work/hey_fennec/negative/en_GB-vctk-medium-01604.wav    "hey fenn"
  0.843  work/hey_fennec/negative/en_US-arctic-medium-01178.wav    "hey phoebe"
  0.835  work/hey_fennec/negative/en_US-libritts_r-medium-02031.wav    "hey fenwick"
  ...
beginning of the wake word alone (share reaching the threshold):
  first  50%:   1.5%
  first  70%:  42.5%
```

Phrases that show up there are good `--confusable` candidates for the next
`fennec synth` run.

## 5. Test it on your computer

Run the detector live on your microphone. `fennec listen` reads raw 16 kHz
16-bit mono audio from stdin:

```bash
arecord -q -f S16_LE -r 16000 -c 1 -t raw | fennec listen models/hey_fennec.fkw -
```

```
listening for "hey fennec" (threshold 0.92)
   12.40s  DETECTED (probability 0.962)
```

Add `--scores` to see every score as a bar, which shows how close
near-misses get. Try a different threshold with `--threshold`. `fennec listen`
also takes a WAV file instead of `-`, which is handy for recordings that
woke the device by mistake.

To hear what the model is trained on, write some augmented windows to disk:

```bash
fennec preview --data work/hey_fennec --out work/preview --count 10
```

## 6. Use it on the device

Point `fennec.toml` at the model:

```toml
[wake_word]
model = "../models/hey_fennec.fkw"
# threshold = 0.85   # optional override
```

and send it to the device, over Wi-Fi or USB. The firmware stays as it is:

```bash
fennec ota --host fennec-kitchen --config firmware/fennec.toml --model
fennec flash --config firmware/fennec.toml          # over USB, writes everything
```

Both check the model first and stop with an error if it is invalid; the
device checks it again before using it.

## Improving a model

| Symptom                                   | What to do                                        |
|-------------------------------------------|---------------------------------------------------|
| Wakes up by itself, often with the TV on  | Set `audio.wake_preroll_ms` to capture what woke it up (see [Capturing false wake-ups](#capturing-false-wake-ups)). Record background audio near the device (`--kind background`) and retrain. Raise `wake_word.threshold` a little in the meantime |
| Wakes up on coughs, claps, doors, dishes  | Train with [ESC-50](#environmental-sounds-esc-50) as `--sounds`, and record the sounds that woke it up with `--kind sound` |
| Wakes up on a particular word or name     | Add it with `--confusable`, re-run `fennec synth` with more `--negatives`, retrain |
| Doesn't react to you                      | Run `fennec enroll`, ideally with recordings from the Atom Echo; then record more takes over time (30 or more is best); lower the threshold; check `audio.mic_gain_db` |
| Reacts to the first word alone ("hey")    | It needs more negatives: re-run `fennec synth` with `--negatives 10000` |
| Fine on the computer, worse on the device | Use recordings made with the Atom Echo for `positive-real/` and `background/` |

## Retraining a model

`fennec train` always starts from scratch: there is no fine-tuning of an
existing `.fkw`. Retraining a model means keeping (or rebuilding) its
dataset, adding to it, and training again.

### Rebuilding the dataset of `hey fennec`

The dataset behind `models/hey_fennec.fkw` has 4000 wake word clips and
10000 negative clips, with these confusable phrases:

```bash
fennec voices --dir work/voices
fennec synth --phrase "hey fennec" --out work/hey_fennec --voices work/voices \
    --negatives 10000 \
    --confusable "hey fenwick" --confusable "hey phoenix" --confusable "fennel" \
    --confusable "hey phoebe" --confusable "hey venice" --confusable "hey finn"
```

If you already have `work/hey_fennec/`, keep it. `fennec synth` only writes
the clips that are missing, so existing clips stay as they are: new
`--confusable` phrases only show up in the clips it adds, so raise
`--negatives` along with them.

### Adding data and training again

1. Add what the current model gets wrong: recordings of yourself in
   `positive-real/`, your home's sound in `background/`, phrases that woke it
   up in `negative-real/` or as `--confusable`, sounds that woke it up in
   `sounds/` (see
   [Improving a model](#improving-a-model) and
   [Augmenting the dataset](#augmenting-the-dataset-with-nixomatic)).
   Recordings the device uploaded are the most useful, since they went
   through its microphone.
2. Train into a new file, so the current model stays as it is:

   ```bash
   fennec train --data work/hey_fennec --out work/hey_fennec_v2.fkw
   ```

3. Compare the two models on the same data. Do this after all new files are
   in place: the held-out clips are every 10th file of each directory (every
   4th when it has fewer than 50), so adding files changes which ones are
   held out.

   ```bash
   fennec eval models/hey_fennec.fkw --data work/hey_fennec
   fennec eval work/hey_fennec_v2.fkw --data work/hey_fennec
   ```

   The old model never saw your recordings, so the recall column for
   `positive-real/` shows how much they helped. Also run
   `fennec listen --scores` on the recordings that woke the device by
   mistake, or that it missed.

4. If the new model is better, copy it over `models/hey_fennec.fkw` (or point
   `wake_word.model` at it) and send it with
   `fennec ota --host fennec-kitchen --config firmware/fennec.toml --model`.

## Augmenting the dataset with nixomatic

`fennec train` already augments every window it trains on (speed, room
echo, level and noise; see [Train](#4-train)). What it cannot invent is
sound it has never heard: your TV, your music, your kitchen. External
recordings fill that gap, but they rarely come as 16 kHz WAV files in a
single directory, which is what `fennec` reads.

[nixomatic](https://nixomatic.com) gives you the tools to download, convert
and mix them without installing anything, inside or outside the flake's
shell. Every recipe below runs in this shell:

```bash
nix --extra-experimental-features 'nix-command flakes' \
    develop 'https://nixomatic.com/?p=ffmpeg,sox,unzip,curl,coreutils' \
    --accept-flake-config
```

or one command at a time with `--command -- <command>` at the end, e.g.
`... --accept-flake-config --command -- ffmpeg -version`. The recipes are
written for bash.

How `fennec` reads extra audio:

- It reads `.wav`, `.ogg` and `.oga` files **directly inside** each
  directory, not in subdirectories. Any sample rate and channel count works;
  it converts to 16 kHz mono itself.
- Background recordings are loaded into memory, about 230 MB per hour of
  audio. A few hours is plenty; don't pass a whole 100 hour corpus.
- Each training window picks a background **file** at random, whatever its
  length, so a single two hour file is used as often as a five second one.
  Cut long recordings into pieces of about a minute.
- Background audio and sounds must not contain the wake word, or the model
  learns to ignore it.
- `--background` is for continuous noise, mixed below the speech in every
  window. `--sounds` is for short events (a cough, a door, a bark), played
  at the level of speech in their own negative windows. Each sound clip
  loses its leading and trailing silence and is used whole, so keep them
  short: a few seconds at most.

### Where to find background noise

The closer the noise is to what plays near the device, the better. In
order of usefulness:

1. **Your own home**, recorded with `fennec record --kind background`, or
   with the Atom Echo itself (see
   [Add real recordings](#3-add-real-recordings-optional-recommended)).
   Record with the TV, the radio or the dishwasher on, not just in silence.
2. **What you play at home**: episodes of the shows you watch, podcasts,
   your playlists, in any format ffmpeg reads (see
   [below](#your-own-audio-in-any-format)).
3. **Public corpora** (check each license before redistributing a model
   trained on them):

| Corpus | What it has | Size |
|--------|-------------|------|
| [ESC-50](https://github.com/karolpiczak/ESC-50) | 2000 five-second clips of 50 everyday sounds: coughs, sneezes, laughter, dogs, doors, kettles, keyboards | 600 MB |
| [MUSAN](https://www.openslr.org/17/) | About 100 hours of music, speech and noise | 11 GB |
| [DEMAND](https://zenodo.org/records/1227121) | Multi-channel recordings of 18 environments: kitchen, living room, office, café, car, street | one zip per environment |
| [FSD50K](https://zenodo.org/records/4060432) | 51,000 Freesound clips labelled with 200 sound classes | tens of GB |
| [Freesound](https://freesound.org) | Search for single sounds ("dishwasher", "vacuum cleaner", "tv static") | per sound |

Of these, ESC-50 and a slice of MUSAN are the quickest wins. The recipes
for both follow.

### Environmental sounds: ESC-50

ESC-50's WAV files are all in one directory, so they work as they are. Its
clips are single events (a cough, a door slam, a bark), so pass them as
`--sounds`, which plays them close to the microphone, rather than as
background:

```bash
curl -L -o work/esc50.zip https://github.com/karolpiczak/ESC-50/archive/master.zip
unzip -q work/esc50.zip -d work
fennec train --data work/hey_fennec --out work/hey_fennec_v2.fkw \
    --sounds work/ESC-50-master/audio
```

Pass the same `--sounds` to `fennec eval`, so that the false accept rate
includes held-out ESC-50 clips, and `--worst` lists the ones that fool the
model. The file name ends with the class number; `meta/esc50.csv` names
it:

```bash
fennec eval work/hey_fennec_v2.fkw --data work/hey_fennec \
    --sounds work/ESC-50-master/audio --worst 20
grep 2-70936-A-42.wav work/ESC-50-master/meta/esc50.csv     # siren
```

### Music, speech and noise: a slice of MUSAN

MUSAN's files are in nested directories, and many of them last several
minutes. Pick a random slice of each kind and cut it into one minute pieces
in a single directory:

```bash
curl -L -o work/musan.tar.gz https://www.openslr.org/resources/17/musan.tar.gz
tar -xzf work/musan.tar.gz -C work
mkdir -p work/musan-slice
for pick in music:30 speech:15 noise:200; do
  find "work/musan/${pick%:*}" -name '*.wav' | shuf -n "${pick#*:}" |
    while read -r f; do
      ffmpeg -nostdin -v error -i "$f" -ac 1 -ar 16000 -f segment -segment_time 60 \
          "work/musan-slice/$(basename "${f%.wav}")-%03d.wav"
    done
done
fennec train --data work/hey_fennec --out work/hey_fennec_v2.fkw \
    --background work/musan-slice
```

That is roughly five hours of audio, or about 1.2 GB of memory during
training. Change the numbers after the colons to take more or less.

### Your own audio in any format

Convert anything ffmpeg reads (MP3, FLAC, M4A, MKV...) to 16 kHz mono WAV,
in one minute pieces, straight into the dataset's `background/` directory:

```bash
mkdir -p work/hey_fennec/background
for f in ~/Music/evening/*; do
  name=$(basename "${f%.*}")
  ffmpeg -nostdin -v error -i "$f" -vn -ac 1 -ar 16000 -f segment -segment_time 60 \
      "work/hey_fennec/background/${name}-%03d.wav"
done
```

Files in `background/` are used without passing `--background`.

### Mixing your wake word recordings with noise

You don't have to mix anything for training: `fennec train` lays every
wake word clip (synthetic and recorded) over background noise itself, at a
new random level each time. 85% of the windows get a signal-to-noise ratio
of 5 to 35 dB and the rest a hard -3 to 5 dB. The noise is one of your
background files in 40% of the windows when there are any; the rest is
synthetic noise, mains hum or distant chatter made from the negative clips.
To hear the result, run `fennec preview`, with the same `--background`
options as for training.

Mixing by hand is useful for something else: testing how a model copes
with a particular noise at a particular level, say your own voice over
your TV at 10 dB. This function mixes a wake word clip with a random piece
of a background file. It pads the clip with 1 s of silence before and
0.5 s after, since the detector needs a 1.5 s window, and sets the
background's level from the peaks of both, so the signal-to-noise ratio is
approximate:

```bash
# mix WAKE.wav BACKGROUND.wav SNR_DB OUT.wav
mix() {
  local len start
  len=$(awk -v l="$(soxi -D "$1")" 'BEGIN { print l + 1.5 }')
  start=$(awk -v b="$(soxi -D "$2")" -v l="$len" -v r=$RANDOM \
      'BEGIN { print (b > l ? (b - l) * r / 32767 : 0) }')
  sox -m "|sox \"$1\" -p remix - gain -3 rate 16k norm -20 pad 1 0.5" \
         "|sox \"$2\" -p remix - gain -3 rate 16k trim $start $len norm -$((20 + $3))" \
      "$4"
}
```

Mix every recording at a few levels and see which ones the model detects:

```bash
mkdir -p work/mixed
for snr in 0 10 20; do
  for f in work/hey_fennec/positive-real/*.wav; do
    bg=$(ls work/hey_fennec/background/*.wav | shuf -n 1)
    mix "$f" "$bg" "$snr" "work/mixed/$(basename "${f%.wav}")-snr$snr.wav"
  done
done
for f in work/mixed/*.wav; do
  echo "$f: $(fennec listen models/hey_fennec.fkw "$f" | grep -c DETECTED)"
done
```

Each line ends with the number of detections: `0` is a miss. Misses at
0 dB are expected; if they show up at 10 or 20 dB with your own noise, add
more of that noise to `background/` and retrain.

You can also copy mixed clips into `positive-real/` to train on them. The
training adds its own noise on top, so keep the mixed level at 10 dB or
more. Mixed copies and pitch-shifted copies (next section) have the same
catch, though: a copy can land in the held-out split while its original is
trained on, which makes the reported recall for `positive-real/` look better
than it is.

### More variety from your own voice

Thirty recordings of one person are thirty takes of one voice. Training
already moves every take up to three semitones up or down (see
[What happens to three takes](#what-happens-to-three-takes)), which covers
voices close to yours. For voices further away, like a child's when you are
an adult, pitch-shifted copies (here six semitones up) are a cheap stand-in
until they record their own:

```bash
for f in work/hey_fennec/positive-real/*.wav; do
  case "$f" in *-pitch*) continue ;; esac
  sox "$f" "${f%.wav}-pitch600.wav" gain -3 pitch 600
done
```

Judge models trained on copies with recordings they have never seen, e.g.
`fennec listen --scores` on new uploads from the device.
