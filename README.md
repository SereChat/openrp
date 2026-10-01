# OpenRP

A native, GPU-rendered roleplay AI engine, built on the [SereChat](https://serechat.com) API.
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
  (Ctrl/Cmd+S saves, Esc goes back).
- **Stories**: **Play** on a world (Ctrl/Cmd+N opens the worlds) starts a session in it, which
  first asks who you play: your character's name and description (the **You** chip edits them
  later). **Begin** saves the story and lists it in the sidebar. The strip under the header
  shows the current scene (click it to change it yourself) and holds the story's cast: **+ Add** brings in characters from the
  library (the story keeps its own copy, so editing or deleting them in the library changes no
  story), creates one, or has the AI generate one for you to review. Click a member to move
  them in or out of the scene; its ⋯ menu edits or deletes them. Deleting a world deletes its
  stories.
- **No narrator**: the story is told only through its characters. Every reply must call a tool
  (the request sets `tool_choice: required`); `speak` holds every line of the turn in one call,
  in order, each a character, what they do (action) and what they say (text, empty when they
  only act), so several characters can answer and react to each other. Lines appear as they
  are written, as **Name** · *action* above a quote. Newcomers come in only when your message
  refers to someone not cast yet, or when nobody is in the scene (then the one character the
  moment needs); they are introduced in the same `speak` call, each with a description. No
  one is cast without a description: if a model lets someone act without introducing them,
  the app makes it describe them next (`tool_choice` forced to `create_character`, at most 3
  rounds). `create_character` also adds someone who matters but is not acting yet. When the
  place or ambiance changes (you walk into a house, night falls), `speak` sets the new scene,
  and members who stay behind or walk off go in its `leave`. The world,
  your character, the characters present and those elsewhere, and the scene make up its system prompt,
  rebuilt for every reply.
- **Edit, Regen, Delete**: hovering a turn (your message and everything that answered it)
  shows them left of Copy. **Edit** makes all of its replies editable at once (the model then
  sees your text instead of the tool calls); **Regen** sends the last prompt again;
  **Delete** (click twice) removes the prompt and its replies. Each reply records what it
  changed in the story (who joined or moved, the scene), so regenerating or deleting the last
  turn rewinds that too, except what you changed yourself since. Deleting an earlier turn only
  removes its messages. What removed replies cost stays counted.
- **Reliable replies**: dropped connections, rate limits and server errors are retried
  automatically; when a conversation outgrows the model's context, the model summarises it
  and carries on from the summary (the full history stays visible). A reply that stops early
  (an error, Esc or closing the app) shows a **Continue** button. Replies keep streaming in
  chats you switch away from.
- **Spotlight** (Ctrl/Cmd+K): one search over commands, sessions, models, themes and the full
  text of every saved message.
- **Slash commands** in the composer, completed as you type (Tab completes, Enter runs, Esc
  dismisses): `/clear` deletes the open story and starts it over in the same world and cast.
- **Emoji and CJK**: colour emoji (Twemoji), and Chinese, Japanese and Korean text through the
  operating system's fonts, with input-method (IME) support for typing them.
- **Settings**, in tabs: Appearance (Dark, One Dark and Light schemes, reasoning display),
  Usage totals, and Account (data folder, sign out).

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
- `chat/cast.rs`: Play, the cast strip and the story's system prompt (built in `chat/stream.rs`);
  `chat/player.rs`: the form asking who you play; `chat/tools.rs`: the `speak` and
  `create_character` tools, how they show, and parsing their arguments while they stream.
- `form.rs`: text fields for forms (focus, selection, keys).
- `platform.rs`: OS integration (browser, file manager, the native image picker).
- `theme.rs`: the colour schemes, sizes and text styles.
- `app.rs`: event routing, worker threads and the frame loop.

The app redraws only when something changes: input, network events, animations, or the
caret blink timer. Network calls, reading saved files, searches and image decoding run on
worker threads and post results back to the event loop. Every file write goes through one
writer thread, in order, which finishes its queue before the app exits.

CI (`.github/workflows/ci.yml`) runs clippy and the tests on Windows, macOS and Linux. Pushing
a `v*` tag attaches release binaries to a GitHub release.

## Sign-in and storage

The first time the app starts, it runs SereChat's device-code flow. The browser opens the
approval page, and the user types the 6-digit code into the app. Everything lives in
`~/.openrp/`:

- `config.toml`: token, model, reasoning effort and display, and colour scheme.
- `sessions/<id>.json`: one file per story, with its world, your character, its cast (copies),
  each reply's tool calls, and each reply's tokens and cost at the
  prices of the time. `.index.json` next to them holds titles and totals, so startup reads
  only the index; a session's messages load when it is opened.
- `worlds/<id>.json` and `characters/<id>.json`: one file per world or character.
- `portraits/`: the app's copies of portrait images, shared by the library and the stories that
  copied a character; a sweep at startup deletes those nothing uses (after a day).

On Unix these files have mode `0600`, and they are written atomically. Signing out (or a `401`
from the API) removes the token but keeps sessions.

## Known limits (deliberate, for now)

- No complex-script shaping (Arabic joining, Indic reordering) and no bidi.
- The world and character forms show input-method text only once it is committed.
- The portrait picker runs the platform's helper (PowerShell, `osascript`, `zenity`/`kdialog`), so
  it takes a moment to appear.
- Release binaries are unsigned and not packaged as installers or a macOS `.app` yet.

## Fonts

- Inter, © The Inter Project Authors, SIL Open Font License (`assets/Inter-LICENSE.txt`).
- JetBrains Mono, © The JetBrains Mono Project Authors, SIL Open Font License
  (`assets/JetBrainsMono-LICENSE.txt`).
- Twemoji Mozilla: code Apache 2.0, graphics © Twitter, Inc. and other contributors, CC-BY 4.0
  (`assets/Twemoji-LICENSE.txt`).
