# Telegram setup

Telegram is currently the messaging service Fennec supports. Fennec posts
recordings to a Telegram group using the Bot API. Your own,
always-running bot sits in the same group and reacts to them. This guide
explains how the two bots talk to each other, then walks you through the
setup, about five minutes in total.

## How bot-to-bot communication works

There are three pieces:

- **A device bot** (e.g. `@kitchen_echo_bot`), one per Atom Echo. The device
  posts as this bot. It only ever *sends*: the firmware calls `sendVoice`
  (or `sendDocument` for WAV files, see below) and never reads messages, so
  nothing anyone writes in the group reaches the device.
- **Your main bot** (e.g. `@your_main_bot`): the bot you already have, or will
  write, that does the actual work. It only ever *receives* recordings, and
  replies to them if you want it to.
- **A Telegram group** that both bots are members of. This is where they
  meet. You can join it too, which is handy for watching what happens.

```
+-----------------+                       +-----------------------+
| Atom Echo       |  sendVoice (Ogg Opus) |  Telegram group       |
| posts as        | --------------------> |                       |
| @kitchen_echo   |                       |  @kitchen_echo        |
+-----------------+                       |  @your_main_bot  <----+-- reads the
                                          |  you                  |   recordings
                                          +-----------------------+
```

### The life of one recording

1. You say "hey fennec, turn off the lights".
2. The Atom Echo posts the recording to the group with the **device bot's**
   token: a voice note with a caption such as
   `#fennec device=kitchen trigger=wake_word score=0.97 duration=3.4s`.
3. Telegram delivers that message to every member of the group that is
   allowed to see it. Your **main bot** gets it as a normal update
   (`getUpdates` or webhook), just like a message you typed yourself, except
   that `from.is_bot` is `true`.
4. Your main bot checks that the sender is one of your device bots, downloads
   the voice note, transcribes it and acts on it.
5. Optionally, your main bot replies in the group ("🎙 kitchen: turn off the
   lights" → "Done"). You see the reply. The device bot does not, because
   the device never reads messages.

### Why Telegram needs a setting for this

By default, Telegram never delivers a bot's messages to other bots. This
stops bots from accidentally talking to each other forever. Since Bot API
10.0 (May 2026), a bot can opt in with **Bot-to-Bot Communication Mode**.
With the mode enabled, a bot receives *every* message other bots post in a
group, as long as it can see every message in that group, which means it is:

- an **admin** of the group, or
- has **group privacy turned off**.

So everything depends on your main bot:

| Setting                       | Main bot           | Device bots     |
|-------------------------------|--------------------|-----------------|
| Bot-to-Bot Communication Mode | **on** (required)  | not needed      |
| Sees every group message      | **yes**: admin *or* privacy off | not needed |
| Member of the group           | yes                | yes             |

The device bots need no special settings. They just post, which any bot that
is a member of a group can do. Telegram's
[bot features](https://core.telegram.org/bots/features) page has the full
rules.

## 1. Create a bot for each device

Talk to [@BotFather](https://t.me/BotFather):

1. Send `/newbot` and follow the prompts. A name like "Kitchen Echo" and a
   username like `kitchen_echo_bot` work well.
2. BotFather replies with a **token** such as
   `123456789:AAH4b...`. This is `telegram.bot_token` in `fennec.toml`. Keep it
   secret: anyone with it can post as the bot.

Use one bot per device. That way the sender tells your main bot which room a
recording came from, and you can revoke a lost device's token without
touching the others.

You also need your main bot. If you do not have one yet, create it the same
way.

## 2. Enable bot-to-bot communication on your main bot

In @BotFather, send `/mybots`, pick **your main bot**, open *Bot Settings*,
and enable **Bot-to-Bot Communication Mode**.

## 3. Create the group

1. Create a Telegram group, e.g. "Home voice".
2. Add your main bot and every device bot.
3. Let your main bot see every message. Do **one** of these:
   - make it an **admin** of the group (it needs no particular rights), or
   - turn off its group privacy: `/mybots` → your main bot → *Bot Settings* →
     *Group Privacy* → *Turn off*. If the bot was already in the group,
     remove it and add it again, because Telegram applies this setting when a
     bot joins.

Forum groups (groups with topics) work too. Set `telegram.message_thread_id`
to post into a specific topic.

## 4. Find the chat id

The device needs the group's numeric id. Device bots keep group privacy on,
so they only see commands addressed to them. Send this in the group, using
your device bot's username:

```
/start@kitchen_echo_bot
```

Then ask the Bot API what the device bot has seen:

```bash
curl -s "https://api.telegram.org/bot<DEVICE_BOT_TOKEN>/getUpdates" \
  | jq '.result[] | (.message // .my_chat_member).chat'
```

```json
{
  "id": -1001234567890,
  "title": "Home voice",
  "type": "supergroup"
}
```

The `id` is `telegram.chat_id`. Group ids are negative. If you see two
different ids, use the most recent one: Telegram gives a group a new id when
it becomes a supergroup, which happens for example when you make someone an
admin.

## 5. Check it works without the device

Test the whole bot-to-bot chain from your computer before you flash anything.
This posts a voice note as the device bot, exactly as the firmware does
(ffmpeg, in the development shell, turns any audio file into one):

```bash
ffmpeg -i some.wav -ac 1 -ar 16000 -c:a libopus -b:a 24k test.ogg
curl -s -F chat_id=-1001234567890 \
     -F caption='#fennec device=test trigger=button duration=1.0s' \
     -F voice=@test.ogg \
     "https://api.telegram.org/bot<DEVICE_BOT_TOKEN>/sendVoice" | jq .ok
```

1. If it prints `true`, the device bot can post: the message is in the group.
2. If your main bot reacts to it, your main bot can see it, and the whole
   chain works. All that's left is setting up the device.

If your main bot is not written yet, check step 2 by hand. Stop the bot if it
is running, because only one client can poll for updates at a time. Then
post the test file again and run:

```bash
curl -s "https://api.telegram.org/bot<MAIN_BOT_TOKEN>/getUpdates" \
  | jq '.result[].message | {from: .from.username, caption}'
```

You should see the device bot as the sender and the `#fennec` caption. If the
main bot uses a webhook, `getUpdates` fails. Look at your webhook's logs
instead.

## Handling recordings in your bot

A recording arrives as a regular message:

- `message.from.is_bot` is `true`, and `message.from.id` is the device bot.
  **Only accept recordings from the device bots you created.** Anyone who can
  add a bot to the group could otherwise feed your bot commands. To find a
  device bot's id, look at `from.id` in the `getUpdates` output above, or
  take the number before the `:` in its token.
- `message.voice` holds the recording: Ogg Opus, 16 kHz mono, with
  `mime_type` `audio/ogg` and its `duration` in seconds. ffmpeg reads it, and
  so do faster-whisper and OpenAI's Whisper API directly.
- Firmware built without the `opus` feature sends WAV files instead:
  `message.document`, `mime_type` `audio/wav`, a file name like
  `kitchen-1234.wav`, IMA ADPCM 16 kHz mono. The handler below accepts both.
- `message.caption` looks like
  `#fennec device=kitchen trigger=wake_word score=0.97 duration=3.4s`.
  `trigger` is `wake_word` or `button`. `score` is only there for the wake
  word. With `audio.button_preroll_ms` set, button recordings also carry
  `preroll=3.0s`: the recording starts that long before the press, so skip
  it before transcribing if you only want what was said after the press.
  `audio.wake_preroll_ms` does the same for wake word recordings, which
  then start with the wake word itself.

A typical handler downloads the file (`getFile`, then
`https://api.telegram.org/file/bot<TOKEN>/<file_path>`), converts it to PCM,
transcribes it and passes the text to the same code that handles typed
messages. For example, with Python and
[python-telegram-bot](https://python-telegram-bot.org/):

```python
import subprocess
from telegram import Update
from telegram.ext import ContextTypes, MessageHandler, filters

DEVICE_BOTS = {7123456789: "kitchen", 7123456790: "office"}  # bot ids

async def on_recording(update: Update, context: ContextTypes.DEFAULT_TYPE):
    message = update.effective_message
    room = DEVICE_BOTS.get(message.from_user.id)
    if room is None or not (message.caption or "").startswith("#fennec"):
        return
    # A voice note, or a WAV document from firmware without the `opus` feature.
    file = await (message.voice or message.document).get_file()
    await file.download_to_drive("/tmp/fennec-recording")
    # whisper.cpp wants 16-bit PCM.
    subprocess.run(["ffmpeg", "-y", "-loglevel", "error", "-i", "/tmp/fennec-recording",
                    "-c:a", "pcm_s16le", "/tmp/fennec-pcm.wav"], check=True)
    text = subprocess.run(["whisper-cli", "-m", "ggml-base.bin", "-nt", "-np",
                           "-f", "/tmp/fennec-pcm.wav"],
                          capture_output=True, text=True, check=True).stdout.strip()
    await message.reply_text(f"🎙 {room}: {text}")
    await handle_user_request(text, room=room)  # your existing logic

app.add_handler(MessageHandler(filters.VOICE | filters.Document.MimeType("audio/wav"),
                               on_recording))
```

Replying with the transcript, as above, is worth it: you see what the bot
understood, and you can continue the conversation by typing in the same
thread.

### Avoiding bot loops

With bot-to-bot communication enabled, your main bot also receives messages
from *any other* bot in the group. Telegram asks bots that use this mode to
protect themselves against endless back-and-forth between bots. The device
bots can't start a loop, because they never read anything. A single check
covers the rest: **ignore messages from bots that are not in your
`DEVICE_BOTS` list**, as the example above does. If you add other bots to the
group, also make sure your main bot never answers a bot's reply to its own
message.

## Troubleshooting

**`sendVoice` (or `sendDocument`) fails with `chat not found`.** The chat id
is wrong, or the device bot is not a member of the group. Check that the id
is the newest one (see step 4) and is negative.

**`sendVoice` (or `sendDocument`) fails with `Unauthorized`.** The token is
wrong or was revoked. Copy it again from @BotFather (`/mybots` → the device
bot → *API Token*).

**The recording shows up in the group, but your main bot ignores it.** Your
main bot never received it. Check, in this order:

1. Bot-to-Bot Communication Mode is enabled for the **main** bot, not only
   the device bot (step 2).
2. The main bot is an admin of the group, or its group privacy is off. If
   you turned privacy off after adding it, remove the bot and add it again
   (step 3).
3. The `getUpdates` check in step 5 shows the message. If it does, the
   problem is in your bot's code: for example, a filter that drops messages
   from bots, or `allowed_updates` not including `message`.

**Your main bot receives the message, but the sender check rejects it.**
Compare `message.from.id` with the ids in your list. The id is the number
before the `:` in the device bot's token.
