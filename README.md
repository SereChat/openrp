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
  newest first. A world is a setting to play in (Panem, a galaxy far away); a character is
  someone the AI plays, kept in one library to cast in any story. Each has a name, a
  description and an optional portrait (PNG or JPEG), created and edited in a form
  (Ctrl/Cmd+S saves, Esc goes back). A form with unsaved changes is kept as a draft when you
  look elsewhere, and reopens with its page; going back or Esc discards it only on a second
  press. A character's form has **Generate**: the AI writes a name and a short description
  from whatever name or idea you typed (or anyone, if blank), for you to review and save.
- **Stories**: Clicking a world (**Edit** opens its form) (Ctrl/Cmd+N opens the worlds) starts a session in it, which
  first asks who you play: your character's name and description (the **You** chip edits them
  later). **Begin** saves the story and lists it in the sidebar. The strip under the header
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
  character and the whole cast, described), so providers cache it with the history after it.
  What changes every turn (the scene, who is in it and who is elsewhere, the memories and the
  author's note) is sent after your latest message as the story state, and never saved.
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
- **Spotlight** (Ctrl/Cmd+K): one search over commands, stories, worlds, characters, models,
  themes and the full text of every saved message. A story also matches the name of its world,
  so typing a world lists the stories played in it; a world opens its page or starts a story
  (**Play**), and a character opens its form. Worlds and characters match by description too.
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
- **Duplicate and delete**: a session's ⋯ in the sidebar deletes it (on a second click), or (like
  `/duplicate` and `/frame`) copies it exactly, with every message, memory and the scene
  (replies are not billed twice), or as a **frame**: the world, your character, the cast and
  the author's note, ready for a new story.
- **Slash commands** in the composer, completed as you type (Tab completes, Enter runs, Esc
  dismisses): `/ooc`, `/note`, `/memory`, `/memorize`, `/duplicate`, `/frame`, and `/clear`, which deletes
  the open story and starts it over in the same world, cast and note.
- **Emoji and CJK**: colour emoji (Twemoji), and Chinese, Japanese and Korean text through the
  operating system's fonts, with input-method (IME) support for typing them.
- **Settings**, in tabs: Appearance (Dark, One Dark and Light schemes, reasoning display),
  Models (a **background model** for memory reviews, summaries and generated characters: a
  cheaper one saves money, and summaries stay on the story's model when it holds less
  context), Usage totals, and Account (data folder, sign out).
- **Problems are shown, not lost**: a story that could not be saved or read, or a model list
  that failed to load (retried by itself), shows as a notice under the header, with the data
  folder a click away. Everything is also logged to `~/.openrp/desktop.log`. A crash writes
  `~/.openrp/desktop-crash.log` and says so; if the window cannot open, a dialog says why. A
  lost GPU (a driver update or reset) is set up again. Only one copy of the app runs at a time.

## Layout

| Crate             | Purpose                                                                          |
|-------------------|----------------------------------------------------------------------------------|
| `crates/serechat` | API client: sign-in, models, streaming Responses API with tools, config, sessions, worlds and characters |
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
- `library.rs`: the Worlds and Characters pages, their form and portraits.
- `chat/cast.rs`: Play, the cast strip and the story's system prompt and state (built in
  `chat/stream.rs`); `chat/dialog.rs`: the form asking who you play, and the dialogs for cast
  members, the scene, the author's note and memories; `chat/tools.rs`: the `speak`,
  `create_character`, `update_character` and `remember` tools, how they show, and parsing
  their arguments while they stream; `chat/names.rs`: telling characters apart by name;
  `chat/turns.rs`: editing, regenerating, swiping and deleting turns; `chat/notice.rs`: notices.
- `form.rs`: text fields for forms (focus, selection, keys).
- `platform.rs`: OS integration (browser, file manager, the native image picker).
- `theme.rs`: the colour schemes, sizes and text styles.
- `app.rs`: event routing, worker threads and the frame loop.
- `log.rs`: the log file, the crash report and the panic hook.

The app redraws only when something changes: input, network events, animations, or the
caret blink timer. Network calls, reading saved files, searches and image decoding run on
worker threads and post results back to the event loop. Every file write goes through one
writer thread, in order, which finishes its queue before the app exits.

CI (`.github/workflows/ci.yml`) runs clippy and the tests on Windows, macOS and Linux. Pushing
a `v*` tag attaches release builds to a GitHub release: `openrp.exe` (icon embedded), `OpenRP.app`
for macOS, and for Linux the binary with `openrp.desktop`, its icon and `install.sh` (installs
all three for the current user). The packaging files are in `packaging/`.

## Sign-in and storage

The first time the app starts, it runs SereChat's device-code flow. The browser opens the
approval page, and the user types the 6-digit code into the app. Everything lives in
`~/.openrp/`:

- `config.toml`: token, model, background model, reasoning effort and display, and colour
  scheme. Lines a newer version added are kept as they are.
- `sessions/<id>.json`: one file per story, with its world, your character, its cast (copies),
  its scene, memories and author's note, each reply's tool calls, and each reply's tokens and cost at the
  prices of the time. `.index.json` next to them holds titles and totals, so startup reads
  only the index; a session's messages load when it is opened.
- `worlds/<id>.json` and `characters/<id>.json`: one file per world or character.
- `portraits/`: the app's copies of portrait images, shared by the library and the stories that
  copied a character; a sweep at startup deletes those nothing uses (after a day), and skips
  itself when a story could not be read.
- `desktop.log`, `desktop-crash.log`: problems and crashes; `desktop.lock`: held while the app
  runs, so a second copy refuses to start.

Stories, worlds and characters record the format version that wrote them: older ones are
upgraded as they are read, and one from a newer version is shown but never saved over (a
story from a newer version is listed but does not open).

On Unix these files have mode `0600`, and they are written atomically, through a temporary file
unique to each write. Signing out (or a `401` from the API) removes the token but keeps sessions.

## Known limits (deliberate, for now)

- No complex-script shaping (Arabic joining, Indic reordering) and no bidi.
- The world and character forms show input-method text only once it is committed.
- The portrait picker runs the platform's helper (PowerShell, `osascript`, `zenity`/`kdialog`), so
  it takes a moment to appear.
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
