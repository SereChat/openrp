# OpenRP

A native, GPU-rendered roleplay AI engine by [SereChat](https://serechat.com), built on its API.
It runs on Windows, macOS and Linux, and it is pure Rust: no web view and no UI framework.

```sh
cargo run --release -p openrp     # binary name: openrp
cargo test --workspace
cargo clippy --workspace --all-targets
```

## What it does

- **Chat** with any SereChat model, with prices shown in the model picker and per-reply token
  and cost captions. Reasoning effort is selectable. While a thinking model works, the reply
  shows "Thinking for 4s"; afterwards "Thought for 12s" expands to its reasoning. Settings
  choose whether reasoning is hidden, collapsed or expanded.
- **Markdown replies**: headings, lists and task lists, quotes, tables, links, inline code, and
  code blocks with syntax highlighting and a copy button. Code never wraps: wide blocks
  scroll sideways (trackpad, Shift+wheel, or the scrollbar). Text can be selected across
  messages (drag, double-click for a word, triple-click for a paragraph) and copied.
- **Worlds and characters**: the sidebar's Worlds and Characters pages list everything saved,
  starred ones first, then newest. A world is a setting to play in (Panem, a galaxy far away);
  a character is someone the AI plays, kept in one library to cast in any story. Each has a
  name, tags, a description and an optional portrait (PNG or JPEG), created and edited in a
  form (Ctrl/Cmd+S saves, Esc goes back), and a **comment** only you see (which version of a
  card it is, say): shown on its card and in pickers, never sent to the AI nor exported.
  Typing on a page filters it (names, tags, comments and descriptions; Esc clears it), the chips under the filter narrow it to tags or to the
  **★ Favorites**, and the star on a card sets one. A form with unsaved changes is kept as a
  draft when you look elsewhere, and reopens with its page; going back or Esc discards it only
  on a second press. Its **⋯** duplicates, exports (characters) or deletes. A character's form
  has **Generate**: the AI writes a name and a short description from whatever name or idea you
  typed (or anyone, if blank), for you to review and save; and **Play**, which asks which world
  to meet them in.
- **More than a description, out of sight until wanted**: under the description, sections open
  on click. A character's **Greetings**: their first message, which opens a story they are in
  before anyone writes, and alternates to swipe to with **‹ ›** (`{{user}}` and `{{char}}`
  stand for your character and theirs). Their **Example dialogue**: sample lines in their
  voice, which the AI learns from but never treats as events. And the **Lore** of worlds and
  characters: entries with keys (Lantern, the inn) that the AI reads only while the latest two
  turns mention one (as a whole word, plurals included), so a big setting costs context only
  where it matters, or always, when marked **Always**. A SillyTavern lorebook (world info) or a
  card's book can be imported into it.
- **Personas**: the Personas page keeps who you play (name, description, portrait, tags), like
  the other two pages. When a story asks who you are, your personas are chips to pick with a
  click, the starred one already filled in; what you type there can be kept with **Save as
  persona** (or **Update persona**, for one of that name). Your portrait shows in the **You**
  chip.
- **Character cards**: **Import** on the Characters page reads the cards SillyTavern, Character
  Hub and most roleplay apps share (PNG with the card inside, or JSON; V1, V2 and V3), several
  at once. The image becomes the portrait; personality and scenario join the description; the
  first message and alternate greetings, example dialogue, tags and the embedded lorebook carry
  over. One card opens to review; several are listed first. **Export card** (in a character's ⋯)
  writes one back: a PNG made from the portrait, holding both the V2 and V3 card, or JSON when
  there is no portrait.
- **Stories**: Clicking a world (**Edit** opens its form) (Ctrl/Cmd+N opens the worlds) starts a session in it, which
  first asks who you play: your character's name, description and portrait, or one of your
  personas (the **You** chip edits them later). **Begin** saves the story and lists it in the sidebar. The strip under the header
  shows the current scene (click it to change it yourself) and holds the story's cast: **+ Add** brings in characters from the
  library (the story keeps its own copy, so editing or deleting them in the library changes no
  story), creates one, or has the AI generate one for you to review. Click a member to move
  them in or out of the scene; its ⋯ menu edits or deletes them. Deleting a world deletes its
  stories: its Delete asks again and says how many.
- **No narrator**: the story is told only through its characters. Every reply must call a tool
  (the request sets `tool_choice: required`), and text written outside tool calls is dropped:
  there is no narration. `speak` holds the whole turn in one call, as one message per
  responding character: what they do (action) and what they say (text, with short actions in
  *asterisks*), so several characters can answer and react to each other. Each character gets
  one bubble per reply, with their portrait and name, their actions muted and their words in
  full colour; if a model gives someone two messages, they are merged and the model is told.
  Bubbles appear as they are written; a scene change, arrival or departure shows as a small
  note between them. Newcomers come in only when your message
  refers to someone not cast yet, or when nobody is in the scene (then the one character the
  moment needs); they are introduced in the same `speak` call, each with a description. No
  one is cast without a description: if a model lets someone act without introducing them,
  the app makes it describe them next (`tool_choice` forced to `create_character`, at most 3
  rounds). `create_character` also adds someone who matters but is not acting yet. When the
  place or ambiance changes (you walk into a house, night falls), `speak` sets the new scene,
  and members who stay behind or walk off go in its `leave`.
- **Names that hold**: models are loose with names, so a name finds its character ignoring
  case and punctuation, by a name they had before, or by part of their name when only one
  character fits ("Rex" for "Captain Rex"); the model is then told the name to use. Renaming a
  character (yours, or a cast member, in its dialog) keeps the old name as an alias, so earlier
  turns still mean them and nobody is cast twice. The model renames or redescribes a member
  itself with `update_character` (they reveal their real name, their role changes), shown as a
  note in the reply and undone with it.
- **Prompt caching**: the system prompt holds what rarely changes (the rules, the world, your
  character and the whole cast, described, with their example dialogue, and the lore marked
  Always), so providers cache it with the history after it. What changes every turn (the
  scene, who is in it and who is elsewhere, the lore the latest turns mention, the memories and
  the author's note) is sent after your latest message as the story state, and never saved.
- **Edit, Regenerate, swipes, Delete**: hovering a turn (your message and everything that
  answered it) shows them left of Copy. **Edit** makes all of its replies editable at once, a
  box per character bubble under their portrait and name; saving rewrites their speech in the
  reply's `speak` call, so it stays bubbles and the model sees your edit (an emptied box drops
  that character from the reply). Replies edited by earlier versions, saved as text, are read
  back into bubbles. **Regenerate** sends the last prompt again and keeps the reply it had:
  **‹ 2/3 ›** on the last turn switches between every reply its prompt got. **Delete** removes
  the prompt and its replies, on a second click. Each reply records what it changed in the
  story (who joined, moved or was renamed, the scene, memories), so regenerating, swiping or
  deleting the last turn rewinds that too (and swiping to a reply makes its changes again),
  except what you changed yourself since. Deleting an earlier turn only removes its messages.
  What swiped-away and removed replies cost stays counted.
- **Reliable replies**: dropped connections, rate limits and server errors are retried
  automatically (as long as the server's `Retry-After` asks, if it does); TLS and certificate
  failures are shown at once. When a conversation outgrows the model's context, the model
  summarises it, for a story as a story-so-far (events, where each character stands, how they
  speak, open threads), and carries on from the summary with the latest turns still word for
  word, so the characters keep their voices (the full history stays visible). A model that
  answers in plain text instead of through the characters is asked once more, then told to
  you plainly; some models (and some with reasoning on) don't follow the story's tools. A reply
  that stops early (an error, Esc or closing the app) shows a **Continue** button. Replies keep
  streaming in chats you switch away from.
- **Searchable pickers**: the lists that can grow long (characters to add to a story, the world
  a character's Play starts in, your personas, the models) have a search at the top: type to
  narrow them by name, then comment and tags; the arrows and Enter pick. People and worlds
  show their portrait, and their comment (or tags) on the right, to tell look-alikes apart.
  The player form shows six personas as chips; the rest are one search away.
- **Spotlight** (Ctrl/Cmd+K): one search over commands, stories, worlds, characters, personas, models,
  themes and the full text of every saved message. A story also matches the name of its world,
  so typing a world lists the stories played in it; a world opens its page or starts a story
  (**Play**), and a character or persona opens its form. Library records match by their comment
  too (shown beside their name), and by description.
- **Memories**: the AI keeps what the story must not forget (promises, secrets, injuries,
  changed relationships) with a `remember` tool, shown as a small note in the reply. They are
  sent with every request, so they survive summarising; **Memory** beside the scene lists
  them in a dialog where each can be edited or removed (×), and new ones added (Enter or
  **+ Add memory**). Regenerating or deleting the reply that remembered something forgets it
  again. Every 8 prompts a separate request reads the turns since the last time and adds what
  the story must not forget (Memory shows "…" while it works; it is billed with the next
  reply). One that fails or takes over three minutes leaves its turns for the next review.
  `/memorize` runs it at once.
- **Author's note and OOC**: **Note** beside the scene holds your guidance for the whole story
  (tone, pacing, limits), sent last in every request. For one reply only, end a message with a
  line starting `/ooc` (or send just `/ooc …` to nudge the story): the model is told it is your
  instruction, not your character speaking, and it shows muted under your message. Your own
  `*actions*` are muted too, like the characters'.
- **Write for me**: in the composer's toolbar (or `/impersonate`), the AI writes your
  character's next message, streaming it into the composer to edit and send, or not. What you
  typed first is its starting point. Clicking again or Esc stops it. It reads the story as a
  reply would (so the provider's cache serves it) and is billed with the next reply.
- **Duplicate, export and delete**: a session's ⋯ in the sidebar deletes it (on a second click),
  or (like `/duplicate` and `/frame`) copies it exactly, with every message, memory and the
  scene (replies are not billed twice), or as a **frame**: the world, your character, the cast
  and the author's note, ready for a new story. **Export as text** saves a readable transcript;
  **Export as JSONL** a SillyTavern chat, one line per character's message, which SillyTavern
  imports. A notice then offers the folder it went to.
- **Slash commands** in the composer, completed as you type (Tab completes, Enter runs, Esc
  dismisses): `/ooc`, `/impersonate`, `/note`, `/memory`, `/memorize`, `/duplicate`, `/frame`, and
  `/clear`, which deletes the open story and starts it over in the same world, cast and note.
- **Emoji and CJK**: colour emoji (Twemoji), and Chinese, Japanese and Korean text through the
  operating system's fonts, with input-method (IME) support for typing them.
- **Settings**, in tabs: Appearance (Dark, One Dark and Light schemes, reasoning display),
  Models (a **background model** for memory reviews, summaries and generated characters: a
  cheaper one saves money, and summaries stay on the story's model when it holds less
  context), Usage totals, and Provider (switch between SereChat and a custom provider, edit or
  forget its key, sign out; the data folder).
- **Custom providers**: under the sign-in button, **Use a custom provider** takes any
  OpenAI-compatible API (OpenRouter, Inworld, OpenAI, Ollama, ...): its base URL and key. It is
  spoken to with Chat Completions; the model must support tool calls. Its models list no
  prices, so costs are hidden while it is in use.
- **The window remembers** its size and whether it was maximized.
- **Problems are shown, not lost**: a story that could not be saved or read, or a model list
  that failed to load (retried by itself), shows as a notice under the header, with the data
  folder a click away. Everything is also logged to `~/.openrp/desktop.log`. A crash writes
  `~/.openrp/desktop-crash.log` and says so; if the window cannot open, a dialog says why. A
  lost GPU (a driver update or reset) is set up again. Only one copy of the app runs at a time.

## Layout

| Crate             | Purpose                                                                          |
|-------------------|----------------------------------------------------------------------------------|
| `crates/serechat` | API client: sign-in, models, streaming Responses API with tools (Chat Completions for custom providers), config, sessions, worlds and characters, character cards and lorebooks (`card.rs`) |
| `crates/desktop`  | The app (`openrp`): winit window, wgpu renderer, text engine, immediate-mode UI  |

`crates/desktop/src`:

- `gpu.rs` + `shader.wgsl`: a single instanced pipeline. Rounded rectangles, borders,
  soft shadows, glyphs and images are all quads, drawn with one draw call per frame.
- `atlas.rs`: shelf packing for the glyph and image atlases, evicting the least recently
  drawn shelf when full.
- `image.rs`: thumbnails: PNG/JPEG decoding (with EXIF orientation) on worker threads,
  cover-cropping, and the image atlas, for portraits.
- `font.rs` + `raster.rs`: font faces (via `ttf-parser`), fallback to system fonts for other
  scripts, GPOS kerning, GSUB emoji ligatures, COLR colour layers, and an exact-area
  anti-aliasing rasteriser.
- `text.rs`: rich text layout (runs of fonts, sizes, colours, decorations and links), line
  breaking including CJK, hit-testing, and the glyph atlas.
- `markdown.rs` → `doc.rs`: a streaming-friendly Markdown parser, and laid-out documents with
  selection, links and incremental rebuilds while a reply streams. `highlight.rs` colours code.
- `paint.rs`, `ui.rs`, `editor.rs`: drawing API, widgets and input state, text editing.
- `chat/`: the main screen (sidebar, composer, messages, menus); `chat/stream.rs` streams
  replies, retries failed requests and compacts long conversations.
- `spotlight.rs`, `settings.rs`, `login.rs`: the other surfaces.
- `library.rs`: the Worlds, Characters and Personas pages (filter, tags, favourites, card import), their
  form with its sections (greetings, example dialogue, lore) and portraits.
- `chat/cast.rs`: Play, the cast strip and the story's system prompt and state (built in
  `chat/stream.rs`); `chat/dialog.rs`: the form asking who you play, and the dialogs for cast
  members, the scene, the author's note and memories; `chat/tools.rs`: the `speak`,
  `create_character`, `update_character` and `remember` tools, how they show, and parsing
  their arguments while they stream; `chat/names.rs`: telling characters apart by name;
  `chat/turns.rs`: editing, regenerating, swiping (replies and greetings) and deleting turns;
  `chat/impersonate.rs`: Write for me; `chat/export.rs`: stories as text and SillyTavern chats;
  `chat/notice.rs`: notices.
- `form.rs`: text fields for forms (focus, selection, keys).
- `platform.rs`: OS integration (browser, file manager, the native open and save dialogs).
- `theme.rs`: the colour schemes, sizes and text styles.
- `app.rs`: event routing, worker threads and the frame loop.
- `log.rs`: the log file, the crash report and the panic hook.

The app redraws only when something changes: input, network events, animations, or the
caret blink timer. Network calls, reading saved files, searches and image decoding run on
worker threads and post results back to the event loop. Every file write goes through one
writer thread, in order, which finishes its queue before the app exits; the exceptions are
portraits copied in (picked, or from an imported card) under fresh names, and exports, which
the worker that asked where writes there.

CI (`.github/workflows/ci.yml`) runs clippy and the tests on Windows, macOS and Linux. Pushing
a `v*` tag attaches release builds to a GitHub release: `openrp.exe` (icon embedded), `OpenRP.app`
for macOS, and for Linux the binary with `openrp.desktop`, its icon and `install.sh` (installs
all three for the current user). The packaging files are in `packaging/`.

## Sign-in and storage

The first time the app starts, it signs in to SereChat with OAuth 2.1 (authorization code
with PKCE, as RFC 8252 has native apps do it): the app listens on a free port at
`http://127.0.0.1:{port}/callback`, the system browser opens SereChat's consent page, and once
the user allows OpenRP (scope `chat`) the browser comes back to the app by itself. Or it
connects to a custom provider instead: a base URL and an API key.

The access token lasts an hour and is refreshed before it runs out (or when the API answers
`401`), in one place, so two refreshes never race; refresh tokens rotate, and each new one goes
straight to the OS keychain: Credential Manager on Windows, the login keychain on macOS (through
`security`), and the Secret Service on Linux (through `secret-tool`, from `libsecret-tools` on
Debian and Ubuntu; without it, the sign-in lasts until the app closes). Signing out revokes
the grant. Everything else lives in `~/.openrp/`:

- `config.toml`: the provider in use and the custom provider's base URL and key, model, background model, reasoning effort and display, colour scheme, and the window's
  size. Lines a newer version added are kept as they are.
- `sessions/<id>.json`: one file per story, with its world, your character, its cast (copies),
  its scene, memories and author's note, each reply's tool calls, and each reply's tokens and cost at the
  prices of the time. `.index.json` next to them holds titles and totals, so startup reads
  only the index; a session's messages load when it is opened.
- `worlds/<id>.json`, `characters/<id>.json` and `personas/<id>.json`: one file per world,
  character or persona, with its tags and star (and the lore of worlds and characters, and a
  character's greetings and example dialogue).
- `portraits/`: the app's copies of portrait images, shared by the library and the stories that
  copied a character; a sweep at startup deletes those nothing uses (after a day), and skips
  itself when a story could not be read.
- `desktop.log`, `desktop-crash.log`: problems and crashes; `desktop.lock`: held while the app
  runs, so a second copy refuses to start.

Stories, worlds and characters record the format version that wrote them: older ones are
upgraded as they are read, and one from a newer version is shown but never saved over (a
story from a newer version is listed but does not open).

On Unix these files have mode `0600`, and they are written atomically, through a temporary file
unique to each write. Signing out (or SereChat refusing the sign-in: a `401` after a refresh, a refresh token
unused for 60 days, or a missing scope) removes the sign-in, or the custom provider's key,
but keeps sessions; signed in to the other provider, the app carries on with it.
A saved key is only ever sent to the address it was saved for.

## Known limits (deliberate, for now)

- No complex-script shaping (Arabic joining, Indic reordering) and no bidi.
- The world and character forms show input-method text only once it is committed.
- The API key field shows the key as it is typed (a saved key is never shown again).
- The window opens where the system places it: its position is not remembered.
- The file dialogs run the platform's helper (PowerShell, `osascript`, `zenity`/`kdialog`), so
  they take a moment to appear.
- Lore keys are plain words: SillyTavern's regex keys, secondary keys and recursion are not
  read, and the lore a story state carries is capped in characters, not tokens. Compressed PNG
  text chunks and CHARX (zip) cards are not read.
- Release builds are unsigned (the macOS `.app` is ad-hoc signed only) and there are no installers.

## License

Copyright (c) 2026 Luvarly and the OpenRP contributors. OpenRP is licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in OpenRP, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.

The bundled fonts keep their own licenses, below.

## Fonts

- Inter, © The Inter Project Authors, SIL Open Font License (`assets/Inter-LICENSE.txt`).
- JetBrains Mono, © The JetBrains Mono Project Authors, SIL Open Font License
  (`assets/JetBrainsMono-LICENSE.txt`).
- Twemoji Mozilla: code Apache 2.0, graphics © Twitter, Inc. and other contributors, CC-BY 4.0
  (`assets/Twemoji-LICENSE.txt`).
