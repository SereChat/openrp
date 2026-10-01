//! Main screen: sidebar of saved sessions, messages, composer, menus,
//! Spotlight, the settings page and the worlds and characters pages.
//!
//! A session is a story played in a world: Play on a world starts one, and
//! its cast (characters present or absent) lives in `cast.rs`.
//!
//! The screen never touches the disk or network itself: it emits [`Action`]s
//! (send, save, …) that the app carries out on worker threads, and receives
//! their results through the `*_loaded` / `stream_*` methods. Streaming,
//! retries, compaction and the system prompt live in `stream.rs`.

mod cast;
mod composer;
mod dialog;
mod menu;
mod messages;
mod sidebar;
mod stream;
mod tools;

use std::sync::atomic::Ordering;

use arboard::Clipboard;
use serechat::{CastMember, Error, Model, Player, Role, Session, SessionSummary, StoredMessage, Usage, new_id, unix_now};
use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use crate::app::Action;
use crate::doc::Doc;
use crate::editor::Editor;
use crate::library::{Event as LibraryEvent, Kind, LibraryView};
use crate::paint::{Painter, Rect};
use crate::settings::{SettingsView, Totals};
use crate::spotlight::{Outcome, Pick, Spotlight};
use crate::text::TextLayout;
use crate::theme::{self, Scheme};
use crate::ui::{Ui, copy, edit_key, move_line};

use composer::Command;
pub use dialog::GenerateJob;
use stream::{ActiveStream, Retry};
pub use stream::{SendJob, input_items};
pub use tools::{generator_tool, tool_definitions};

/// Model used until the user picks one.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5.5";
/// Name of the platform's primary shortcut modifier.
const PRIMARY_KEY: &str = if cfg!(target_os = "macos") { "Cmd" } else { "Ctrl" };

/// How much the model should think before answering. Models accept
/// different efforts (see [`Model::reasoning_levels`]); one the selected
/// model does not accept falls back to [`Reasoning::Auto`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reasoning {
    /// Leave it to the model.
    #[default]
    Auto,
    /// No reasoning.
    Off,
    /// The least reasoning.
    Minimal,
    /// Brief reasoning.
    Low,
    /// Balanced reasoning.
    Medium,
    /// Thorough reasoning.
    High,
    /// More than high.
    ExtraHigh,
    /// As much as the model can.
    Max,
}

impl Reasoning {
    const ALL: [Self; 8] = [Self::Auto, Self::Off, Self::Minimal, Self::Low, Self::Medium, Self::High, Self::ExtraHigh, Self::Max];

    /// Value stored in the config file; also the API's effort name.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::ExtraHigh => "xhigh",
            Self::Max => "max",
        }
    }

    /// Parses a config value; unknown values mean [`Reasoning::Auto`].
    #[must_use]
    pub fn from_key(key: Option<&str>) -> Self {
        Self::ALL.into_iter().find(|r| Some(r.key()) == key).unwrap_or_default()
    }

    /// The choices for `model`: Auto plus the efforts it accepts, or every
    /// effort while the model list is unknown.
    fn choices(model: Option<&Model>) -> Vec<Self> {
        Self::ALL.into_iter().filter(|r| *r == Self::Auto || model.is_none_or(|m| m.reasoning_levels.iter().any(|l| l == r.key()))).collect()
    }

    fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Off => "Off",
            Self::Minimal => "Minimal",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
            Self::ExtraHigh => "Extra high",
            Self::Max => "Max",
        }
    }

    fn detail(self) -> &'static str {
        match self {
            Self::Auto => "Model default",
            Self::Off => "Answer right away",
            Self::Minimal => "Barely think",
            Self::Low => "Think briefly",
            Self::Medium => "Balanced",
            Self::High => "Think it through",
            Self::ExtraHigh => "Think longer",
            Self::Max => "Think as long as needed",
        }
    }
}

/// How replies show the model's reasoning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReasoningView {
    /// Never shown.
    Hidden,
    /// A "Thought for …" row that expands on click.
    #[default]
    Collapsed,
    /// Shown in full, streaming live when the server sends it.
    Expanded,
}

impl ReasoningView {
    /// Every choice, in settings order.
    pub const ALL: [Self; 3] = [Self::Hidden, Self::Collapsed, Self::Expanded];

    /// Value stored in the config file.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Hidden => "hidden",
            Self::Collapsed => "collapsed",
            Self::Expanded => "expanded",
        }
    }

    /// Parses a config value; unknown values mean [`ReasoningView::Collapsed`].
    #[must_use]
    pub fn from_key(key: Option<&str>) -> Self {
        Self::ALL.into_iter().find(|v| Some(v.key()) == key).unwrap_or_default()
    }

    /// Name shown in settings.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Hidden => "Hidden",
            Self::Collapsed => "Collapsed",
            Self::Expanded => "Expanded",
        }
    }
}

/// What the main area shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Chat,
    Settings,
    /// The worlds or characters page.
    Library(Kind),
}

/// Drop-down menus.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Model,
    Reasoning,
    /// Slash commands completing the composer text; open while any match.
    Commands,
    /// Ways to add to the story's cast.
    Cast,
    /// Library characters to add to the cast.
    CastLibrary,
    /// What to do with the cast member at this index.
    Member(usize),
}

/// One message in a conversation, with its cached layouts.
struct Entry {
    id: u64,
    message: StoredMessage,
    /// The user opened (`true`) or closed the reasoning block; `None`
    /// follows the [`ReasoningView`] setting.
    reasoning_open: Option<bool>,
    /// What the message shows, as Markdown: its text, then what its tool
    /// calls show (speech, arrivals).
    display: String,
    /// The (text bytes, call argument bytes, live) `display` was built for.
    display_key: (usize, usize, bool),
    doc: Option<Doc>,
    /// Display length and boxed-ness the doc was built for.
    doc_key: (usize, bool),
    reasoning_doc: Option<Doc>,
    reasoning_len: usize,
    /// Tool calls the model is writing right now (never saved).
    streaming_calls: Vec<StreamingCall>,
}

/// A tool call still being written, shown as it streams in.
struct StreamingCall {
    /// Its position in the response, as the stream identifies it.
    index: u64,
    name: String,
    /// The JSON arguments so far.
    arguments: String,
}

impl Entry {
    fn new(id: u64, message: StoredMessage) -> Self {
        Self {
            id,
            message,
            reasoning_open: None,
            display: String::new(),
            display_key: (usize::MAX, 0, false),
            doc: None,
            doc_key: (0, false),
            reasoning_doc: None,
            reasoning_len: 0,
            streaming_calls: Vec::new(),
        }
    }

    /// Rebuilds [`Entry::display`] if the message or its calls changed;
    /// `live` while the reply streams (its calls are still previews).
    fn refresh_display(&mut self, live: bool) {
        let calls: usize = if live {
            self.streaming_calls.iter().map(|c| c.arguments.len() + 1).sum()
        } else {
            self.message.tool_calls.iter().map(|r| r.call.arguments.len() + 1).sum()
        };
        let key = (self.message.content.len(), calls, live);
        if key == self.display_key {
            return;
        }
        self.display_key = key;
        let display = if live {
            tools::display(&self.message.content, self.streaming_calls.iter().map(|c| (c.name.as_str(), c.arguments.as_str())), true)
        } else {
            tools::display(&self.message.content, self.message.tool_calls.iter().map(|r| (r.call.name.as_str(), r.call.arguments.as_str())), false)
        };
        if display != self.display {
            // Not always longer: lay the document out again.
            self.display = display;
            self.doc_key = (usize::MAX, false);
        }
    }

    /// Drawn inside a bordered box (prompts and errors) rather than bare.
    fn boxed(&self) -> bool {
        self.message.role == Role::User || self.message.failed
    }

    /// Whether the reasoning text is shown under `view`.
    fn reasoning_shown(&self, view: ReasoningView) -> bool {
        view != ReasoningView::Hidden && !self.message.reasoning.is_empty() && self.reasoning_open.unwrap_or(view == ReasoningView::Expanded)
    }
}

/// Whether a conversation's messages are in memory.
#[derive(Clone, Debug, PartialEq)]
enum Load {
    /// Known from the index only.
    Summary,
    /// Being read on a worker thread.
    Loading,
    /// `entries` holds every message.
    Loaded,
    /// The file could not be read; shown instead of the messages.
    Failed(String),
}

/// One conversation; saved as a [`Session`] once it has messages.
struct Conversation {
    id: u64,
    session_id: String,
    title: String,
    created: u64,
    updated: u64,
    /// Id of the world the story is played in; `None` until Play (or for a
    /// plain chat from before worlds).
    world: Option<String>,
    /// The characters taking part, present in the scene or not.
    cast: Vec<CastMember>,
    /// Who the user plays; `None` until they say.
    player: Option<Player>,
    /// Replies in a row that continued on their own after only calling
    /// tools; reset by every prompt.
    auto_rounds: u32,
    /// The next request makes the model describe characters who acted
    /// without being cast with a description.
    repairing: bool,
    load: Load,
    /// Totals from the index, used until the messages are loaded.
    indexed_cost: f64,
    indexed_tokens: u64,
    entries: Vec<Entry>,
    stream: Option<ActiveStream>,
    /// A failed request waiting to be sent again.
    retry: Option<Retry>,
    /// USD billed for failed requests, not yet added to a message.
    carried_cost: f64,
}

impl Conversation {
    fn new(id: u64) -> Self {
        let now = unix_now();
        Self {
            id,
            session_id: new_id(),
            title: String::new(),
            created: now,
            updated: now,
            world: None,
            cast: Vec::new(),
            player: None,
            auto_rounds: 0,
            repairing: false,
            load: Load::Loaded,
            indexed_cost: 0.0,
            indexed_tokens: 0,
            entries: Vec::new(),
            stream: None,
            retry: None,
            carried_cost: 0.0,
        }
    }

    /// A saved session whose messages are read when it is opened.
    fn from_summary(id: u64, summary: SessionSummary) -> Self {
        Self {
            session_id: summary.id,
            title: summary.title,
            created: summary.created,
            updated: summary.updated,
            world: summary.world,
            load: Load::Summary,
            indexed_cost: summary.cost,
            indexed_tokens: summary.tokens,
            ..Self::new(id)
        }
    }

    /// A new conversation nobody has written in yet (never saved or listed).
    fn is_fresh(&self) -> bool {
        self.load == Load::Loaded && self.entries.is_empty()
    }

    /// Snapshot for saving; the empty placeholder of a pending reply is skipped.
    fn to_session(&self) -> Session {
        debug_assert_eq!(self.load, Load::Loaded, "saving would drop unloaded messages");
        Session {
            id: self.session_id.clone(),
            title: self.title.clone(),
            created: self.created,
            updated: self.updated,
            world: self.world.clone(),
            cast: self.cast.clone(),
            player: self.player.clone(),
            messages: self
                .entries
                .iter()
                .filter(|e| !e.message.content.is_empty() || !e.message.tool_calls.is_empty())
                .map(|e| e.message.clone())
                .collect(),
        }
    }

    fn cost(&self) -> f64 {
        if self.load == Load::Loaded { self.entries.iter().map(|e| e.message.cost).sum() } else { self.indexed_cost }
    }

    fn tokens(&self) -> u64 {
        if self.load == Load::Loaded {
            self.entries.iter().map(|e| e.message.usage.input_tokens + e.message.usage.output_tokens).sum()
        } else {
            self.indexed_tokens
        }
    }

    /// Streaming or waiting to retry.
    fn busy(&self) -> bool {
        self.stream.is_some() || self.retry.is_some()
    }

    /// Whether the user can write in it: a story whose player is known, or
    /// one (or an older chat) that already has messages.
    fn playable(&self) -> bool {
        !self.entries.is_empty() || (self.world.is_some() && self.player.is_some())
    }

    /// A new story that still needs to know who the user plays.
    fn needs_player(&self) -> bool {
        self.world.is_some() && self.player.is_none() && self.is_fresh()
    }
}

/// A position in the open conversation: entry, document (0 reasoning,
/// 1 content), text piece and byte.
type SelPos = (usize, u8, usize, usize);

/// One row of a drop-down menu.
struct MenuItem {
    label: String,
    detail: String,
    selected: bool,
}

/// State of the chat screen.
pub struct Chat {
    conversations: Vec<Conversation>,
    current: u64,
    next_id: u64,
    page: Page,
    settings: SettingsView,
    composer: Editor,
    /// Composer layout and text origin from the last frame, for keyboard
    /// navigation and mouse hit-testing.
    composer_layout: Option<(TextLayout, (f32, f32))>,
    composer_scroll: f32,
    /// Dragging a selection in the composer.
    selecting: bool,
    /// Text being composed by an input method, and its caret.
    preedit: Option<(String, Option<(usize, usize)>)>,
    /// Composer caret rectangle, for placing the input method's window.
    caret_rect: Option<Rect>,
    models: Vec<Model>,
    model: String,
    reasoning: Reasoning,
    reasoning_view: ReasoningView,
    menu: Option<Menu>,
    /// Where the open menu was drawn last frame; blocks hover beneath it.
    menu_rect: Option<Rect>,
    menu_scroll: f32,
    /// The slash command Enter runs, among those matching.
    command_pick: usize,
    /// Composer text the command menu was closed for; it stays closed
    /// until the text changes.
    command_dismissed: Option<String>,
    scroll: f32,
    scroll_target: f32,
    stick_to_bottom: bool,
    /// Scroll offset when the scrollbar was grabbed, while it is dragged.
    bar_drag: Option<f32>,
    /// Message selection: anchor and focus.
    selection: Option<(SelPos, SelPos)>,
    /// Dragging a message selection.
    dragging: bool,
    sidebar_scroll: f32,
    /// Conversation whose delete button was clicked once, awaiting confirmation.
    confirm_delete: Option<u64>,
    /// What was just copied (entry, code block or whole message) and when.
    copied: Option<(u64, Option<usize>, f32)>,
    spotlight: Option<Spotlight>,
    /// The character dialog (who the user plays, or a cast member to
    /// edit, create or generate), for one conversation.
    character_form: Option<dialog::CharacterForm>,
    /// The worlds and characters pages.
    library: LibraryView,
}

impl Chat {
    /// The chat screen with the saved `sessions` (newest first) in the
    /// sidebar and a fresh conversation open.
    #[must_use]
    pub fn new(model: Option<String>, reasoning: Reasoning, sessions: Vec<SessionSummary>) -> Self {
        let mut chat = Self {
            conversations: Vec::new(),
            current: 0,
            next_id: 1,
            page: Page::Chat,
            settings: SettingsView::default(),
            composer: Editor::default(),
            composer_layout: None,
            composer_scroll: 0.0,
            selecting: false,
            preedit: None,
            caret_rect: None,
            models: Vec::new(),
            model: model.unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            reasoning,
            reasoning_view: ReasoningView::default(),
            menu: None,
            menu_rect: None,
            menu_scroll: 0.0,
            command_pick: 0,
            command_dismissed: None,
            scroll: 0.0,
            scroll_target: 0.0,
            stick_to_bottom: true,
            bar_drag: None,
            selection: None,
            dragging: false,
            sidebar_scroll: 0.0,
            confirm_delete: None,
            copied: None,
            library: LibraryView::default(),
            character_form: None,
            spotlight: None,
        };
        for summary in sessions {
            let id = chat.next_id();
            chat.conversations.push(Conversation::from_summary(id, summary));
        }
        chat.new_conversation();
        chat
    }

    /// The selected model, once the model list has loaded.
    fn selected_model(&self) -> Option<&Model> {
        self.models.iter().find(|m| m.id == self.model)
    }

    /// The effort requests use: the chosen one if the model accepts it.
    fn reasoning_in_use(&self) -> Reasoning {
        if Reasoning::choices(self.selected_model()).contains(&self.reasoning) { self.reasoning } else { Reasoning::Auto }
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn current(&mut self) -> &mut Conversation {
        let current = self.current;
        self.conversations.iter_mut().find(|c| c.id == current).expect("the current conversation always exists")
    }

    fn find(&mut self, id: u64) -> Option<&mut Conversation> {
        self.conversations.iter_mut().find(|c| c.id == id)
    }

    fn select(&mut self, id: u64) {
        self.current = id;
        self.page = Page::Chat;
        self.menu = None;
        self.selection = None;
        self.scroll = 0.0;
        self.scroll_target = 0.0;
        self.stick_to_bottom = true;
    }

    /// Selects a saved conversation, requesting its messages if needed.
    fn open(&mut self, id: u64, actions: &mut Vec<Action>) {
        self.select(id);
        let conversation = self.current();
        if conversation.load == Load::Summary {
            conversation.load = Load::Loading;
            actions.push(Action::LoadSession { conversation: id, session: conversation.session_id.clone() });
        }
    }

    /// Opens an empty conversation in no world, reusing an empty one.
    fn new_conversation(&mut self) {
        if let Some(fresh) = self.conversations.iter_mut().find(|c| c.is_fresh()) {
            fresh.world = None;
            fresh.cast.clear();
            fresh.player = None;
            let id = fresh.id;
            self.select(id);
            return;
        }
        let id = self.next_id();
        self.conversations.insert(0, Conversation::new(id));
        self.select(id);
    }

    /// Removes a conversation and its saved session.
    fn delete_conversation(&mut self, id: u64, actions: &mut Vec<Action>) {
        let Some(index) = self.conversations.iter().position(|c| c.id == id) else {
            return;
        };
        let conversation = self.conversations.remove(index);
        if let Some(stream) = conversation.stream {
            stream.cancel.store(true, Ordering::Relaxed);
        }
        actions.push(Action::DeleteSession(conversation.session_id));
        if self.current == id {
            self.new_conversation();
        }
    }

    /// Deletes every story played in `world` (which was just deleted).
    fn delete_world_stories(&mut self, world: &str, actions: &mut Vec<Action>) {
        let doomed: Vec<u64> = self.conversations.iter().filter(|c| c.world.as_deref() == Some(world)).map(|c| c.id).collect();
        for id in doomed {
            self.delete_conversation(id, actions);
        }
    }

    /// Slash commands the composer text completes to, unless the user
    /// closed their menu for this text.
    fn commands(&self) -> Vec<Command> {
        let text = self.composer.text();
        if self.page != Page::Chat || self.command_dismissed.as_deref() == Some(text) {
            return Vec::new();
        }
        Command::matching(text)
    }

    /// Opens the command menu while commands match and closes it otherwise.
    fn sync_command_menu(&mut self) {
        let open = !self.commands().is_empty();
        match self.menu {
            None if open => {
                self.menu = Some(Menu::Commands);
                self.command_pick = 0;
                self.menu_scroll = 0.0;
            }
            Some(Menu::Commands) if !open => self.menu = None,
            _ => {}
        }
    }

    /// Closes the open menu; the command menu stays closed for this text.
    fn close_menu(&mut self) {
        if self.menu == Some(Menu::Commands) {
            self.command_dismissed = Some(self.composer.text().to_owned());
        }
        self.menu = None;
    }

    /// Runs a slash command, taking it out of the composer.
    fn run_command(&mut self, command: Command, actions: &mut Vec<Action>) {
        self.composer.take();
        self.command_dismissed = None;
        self.menu = None;
        match command {
            Command::Clear => self.clear(actions),
        }
    }

    /// Deletes the open story (stopping its reply) and starts it over in
    /// the same world, with the same cast and player.
    fn clear(&mut self, actions: &mut Vec<Action>) {
        let conversation = self.current();
        if conversation.is_fresh() {
            return;
        }
        let (id, world, cast, player) = (conversation.id, conversation.world.clone(), conversation.cast.clone(), conversation.player.clone());
        self.delete_conversation(id, actions);
        let fresh = self.current();
        fresh.world = world;
        fresh.cast = cast;
        fresh.player = player;
    }

    /// Stores the messages of a session read on a worker thread.
    pub fn session_loaded(&mut self, conversation: u64, result: Result<Session, Error>) {
        let Some(index) = self.conversations.iter().position(|c| c.id == conversation && c.load == Load::Loading) else {
            return;
        };
        let session = match result {
            Ok(session) => session,
            Err(e) => {
                self.conversations[index].load = Load::Failed(format!("This session could not be opened: {e}"));
                return;
            }
        };
        let first = self.next_id;
        self.next_id += session.messages.len() as u64;
        let entries = session.messages.into_iter().zip(first + 1..).map(|(m, id)| Entry::new(id, m)).collect();
        let mut cast = session.cast;
        // Casts saved before members were copies hold only ids: copy the
        // library's character in while it still exists.
        for member in cast.iter_mut().filter(|m| m.name.is_empty()) {
            if let Some(character) = self.library.get(Kind::Character, &member.id) {
                member.name.clone_from(&character.name);
                member.description.clone_from(&character.description);
                member.portrait.clone_from(&character.portrait);
            }
        }
        cast.retain(|m| !m.name.is_empty());
        let conversation = &mut self.conversations[index];
        conversation.entries = entries;
        conversation.world = session.world;
        conversation.cast = cast;
        conversation.player = session.player;
        conversation.load = Load::Loaded;
    }

    /// Sets how replies show reasoning. Blocks the user opened or closed
    /// by hand start following the setting again.
    pub fn set_reasoning_view(&mut self, view: ReasoningView) {
        self.reasoning_view = view;
        for entry in self.conversations.iter_mut().flat_map(|c| c.entries.iter_mut()) {
            entry.reasoning_open = None;
        }
        self.selection = None;
    }

    /// Stores the model list, keeping the selection valid.
    pub fn set_models(&mut self, models: Vec<Model>) {
        if !models.iter().any(|m| m.id == self.model)
            && let Some(fallback) = models.iter().find(|m| m.id == DEFAULT_MODEL).or(models.first())
        {
            self.model.clone_from(&fallback.id);
        }
        self.models = models;
    }

    /// Cancels every running stream (used on sign-out).
    pub fn cancel_all(&mut self) {
        for conversation in &mut self.conversations {
            if let Some(stream) = conversation.stream.take() {
                stream.cancel.store(true, Ordering::Relaxed);
            }
            conversation.retry = None;
        }
    }

    /// Usage summed over every conversation with messages.
    fn totals(&self) -> Totals {
        let saved = self.conversations.iter().filter(|c| !c.is_fresh());
        saved.fold(Totals::default(), |t, c| Totals { cost: t.cost + c.cost(), tokens: t.tokens + c.tokens(), sessions: t.sessions + 1 })
    }

    /// Takes the composer text and starts a reply, if sending is possible.
    fn send(&mut self, actions: &mut Vec<Action>) {
        let has_content = !self.composer.text().trim().is_empty();
        let conversation = self.current();
        // Sending into an unloaded session would save it without its history;
        // a story starts with Play, in a world.
        if conversation.load != Load::Loaded || conversation.busy() || !has_content || !conversation.playable() {
            return;
        }
        let text = self.composer.take().trim().to_owned();
        let user_id = self.next_id();
        let conversation = self.current();
        conversation.auto_rounds = 0;
        conversation.repairing = false;
        if conversation.title.is_empty() {
            conversation.title = text.lines().next().unwrap_or_default().chars().take(80).collect();
        }
        conversation.entries.push(Entry::new(user_id, StoredMessage::new(Role::User, text)));
        let id = conversation.id;

        // Most recently used conversation moves to the top of the sidebar.
        if let Some(index) = self.conversations.iter().position(|c| c.id == id) {
            let moved = self.conversations.remove(index);
            self.conversations.insert(0, moved);
        }
        self.composer_scroll = 0.0;
        self.selection = None;
        self.request_reply(id, actions);
    }

    fn toggle_settings(&mut self) {
        self.show(if self.page == Page::Settings { Page::Chat } else { Page::Settings });
    }

    /// Switches the main area to `page`.
    fn show(&mut self, page: Page) {
        self.menu = None;
        if let Page::Library(_) = page {
            self.library.show_index();
        }
        self.page = page;
    }

    /// Stores the worlds and characters read from disk.
    pub fn library_loaded(&mut self, worlds: Vec<serechat::World>, characters: Vec<serechat::Character>, portraits: serechat::Portraits) {
        self.library.loaded(worlds, characters, portraits);
    }

    /// The portrait picker closed (see [`LibraryView::portrait_picked`]).
    pub fn portrait_picked(&mut self, result: Result<Option<String>, String>) {
        self.library.portrait_picked(result);
    }

    /// Text of the message selection, if any.
    fn selected_text(&self) -> Option<String> {
        let (a, b) = self.selection?;
        let (from, to) = if a <= b { (a, b) } else { (b, a) };
        if from == to {
            return None;
        }
        let conversation = self.conversations.iter().find(|c| c.id == self.current)?;
        let mut out = String::new();
        for (index, entry) in conversation.entries.iter().enumerate().take(to.0 + 1).skip(from.0) {
            let reasoning = entry.reasoning_doc.as_ref().filter(|_| entry.reasoning_shown(self.reasoning_view));
            for (doc_id, doc) in [(0u8, reasoning), (1, entry.doc.as_ref())] {
                let Some(doc) = doc else { continue };
                if (index, doc_id) < (from.0, from.1) || (index, doc_id) > (to.0, to.1) {
                    continue;
                }
                let start = if (index, doc_id) == (from.0, from.1) { (from.2, from.3) } else { (0, 0) };
                let end = if (index, doc_id) == (to.0, to.1) { (to.2, to.3) } else { doc.end() };
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&doc.text_between(start, end));
            }
        }
        (!out.is_empty()).then_some(out)
    }

    /// Text committed by an input method (IME).
    pub fn ime_commit(&mut self, text: &str) {
        self.preedit = None;
        if let Some(spotlight) = &mut self.spotlight {
            spotlight.insert(text);
            return;
        }
        match self.page {
            Page::Chat => match self.character_form() {
                Some(form) => form.fields.insert(text),
                None => self.composer.insert(text),
            },
            Page::Library(_) => self.library.insert(text),
            Page::Settings => {}
        }
    }

    /// Text being composed by an input method; empty clears it.
    pub fn ime_preedit(&mut self, text: String, cursor: Option<(usize, usize)>) {
        self.preedit = (!text.is_empty()).then_some((text, cursor));
    }

    /// Where the input method's candidate window should appear.
    #[must_use]
    pub fn ime_area(&self) -> Option<Rect> {
        match self.page {
            Page::Library(_) => self.library.caret(),
            _ => match &self.character_form {
                Some(form) if form.conversation == self.current => form.fields.caret(),
                _ => self.caret_rect,
            },
        }
    }

    /// Opens Spotlight.
    fn open_spotlight(&mut self) {
        self.menu = None;
        self.spotlight = Some(Spotlight::default());
    }

    /// Content-search results for Spotlight.
    pub fn search_results(&mut self, generation: u64, hits: Vec<serechat::SearchHit>) {
        if let Some(spotlight) = &mut self.spotlight {
            spotlight.set_hits(generation, hits);
        }
    }

    /// Applies what the user picked in Spotlight.
    fn apply_pick(&mut self, pick: Pick, actions: &mut Vec<Action>) {
        self.spotlight = None;
        match pick {
            Pick::Settings => self.show(Page::Settings),
            Pick::Worlds => self.show(Page::Library(Kind::World)),
            Pick::Characters => self.show(Page::Library(Kind::Character)),
            Pick::Theme(scheme) => actions.push(Action::SetTheme(scheme)),
            Pick::Session(session) => {
                if let Some(id) = self.conversations.iter().find(|c| c.session_id == session).map(|c| c.id) {
                    self.open(id, actions);
                }
            }
            Pick::Model(model) => {
                self.model.clone_from(&model);
                actions.push(Action::SelectModel(model));
            }
        }
    }

    /// Keyboard input.
    pub fn key(&mut self, event: &KeyEvent, mods: ModifiersState, cb: &mut Option<Clipboard>, actions: &mut Vec<Action>) {
        let primary = if cfg!(target_os = "macos") { mods.super_key() } else { mods.control_key() };
        if let Some(spotlight) = &mut self.spotlight {
            match spotlight.key(event, mods, cb) {
                Outcome::Pick(pick) => self.apply_pick(pick, actions),
                Outcome::Close => self.spotlight = None,
                Outcome::Stay => {}
            }
            return;
        }
        let is = |c: &str, ch: &str| c.eq_ignore_ascii_case(ch);
        let commands = self.commands();
        match &event.logical_key {
            Key::Named(NamedKey::Escape) if self.menu.is_some() => self.close_menu(),
            Key::Named(NamedKey::Escape) if self.page == Page::Settings => self.page = Page::Chat,
            Key::Character(c) if primary && is(c, "k") => self.open_spotlight(),
            Key::Character(c) if primary && is(c, ",") => self.toggle_settings(),
            // A story starts from a world.
            Key::Character(c) if primary && is(c, "n") => self.show(Page::Library(Kind::World)),
            _ if self.page == Page::Settings => {}
            _ if matches!(self.page, Page::Library(_)) => {
                if !self.library.key(event, mods, cb, actions) && event.logical_key == Key::Named(NamedKey::Escape) {
                    self.page = Page::Chat;
                }
            }
            // The player form, while open, takes the keys.
            _ if self.character_form.as_ref().is_some_and(|f| f.conversation == self.current) => {
                self.dialog_key(event, mods, cb, actions);
            }
            // Copy a message selection; otherwise the composer handles it.
            Key::Character(c) if primary && is(c, "c") && self.composer.selection().is_empty() && self.selection.is_some() => {
                if let Some(text) = self.selected_text() {
                    copy(cb, &text);
                }
            }
            // Enter runs the picked slash command, Tab completes it.
            Key::Named(key @ (NamedKey::Enter | NamedKey::Tab)) if !commands.is_empty() && !mods.shift_key() => {
                let command = commands[self.command_pick.min(commands.len() - 1)];
                if *key == NamedKey::Enter {
                    self.run_command(command, actions);
                } else {
                    self.composer.take();
                    self.composer.insert(&format!("/{}", command.name()));
                }
            }
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) if !commands.is_empty() => {
                let step = if *key == NamedKey::ArrowUp { commands.len() - 1 } else { 1 };
                self.command_pick = (self.command_pick.min(commands.len() - 1) + step) % commands.len();
            }
            Key::Named(NamedKey::Enter) if mods.shift_key() => self.composer.insert("\n"),
            Key::Named(NamedKey::Enter) => self.send(actions),
            Key::Named(NamedKey::Escape) if self.selection.is_some() => self.selection = None,
            Key::Named(NamedKey::Escape) => self.stop(actions),
            Key::Named(key @ (NamedKey::ArrowUp | NamedKey::ArrowDown)) => {
                if let Some((layout, _)) = &self.composer_layout {
                    move_line(&mut self.composer, layout, *key == NamedKey::ArrowUp, mods.shift_key());
                }
            }
            _ => {
                if edit_key(&mut self.composer, event, mods, cb) {
                    self.selection = None;
                }
            }
        }
    }

    /// Draws the screen.
    pub fn draw(&mut self, p: &mut Painter, ui: &mut Ui, view: Rect, scheme: Scheme, actions: &mut Vec<Action>) {
        let sidebar = Rect::new(0.0, 0.0, theme::SIDEBAR_WIDTH, view.h);
        let main = Rect::new(sidebar.w, 0.0, view.w - sidebar.w, view.h);

        // Overlays are drawn last, on top. Spotlight blocks everything
        // beneath it; an open menu blocks its own area (rect from last frame).
        self.sync_command_menu();
        let modal = self.spotlight.is_some();
        ui.blocker = if modal {
            Some(view)
        } else if self.menu.is_some() {
            self.menu_rect
        } else {
            None
        };
        self.draw_sidebar(p, ui, sidebar, actions);
        let toolbar = match self.page {
            Page::Settings => {
                let totals = self.totals();
                self.settings.draw(p, ui, main, scheme, self.reasoning_view, totals, actions);
                [Rect::default(); 4]
            }
            Page::Library(kind) => {
                match self.library.draw(p, ui, main, kind, actions) {
                    Some(LibraryEvent::Play(world)) => self.play(world),
                    Some(LibraryEvent::WorldDeleted(world)) => self.delete_world_stories(&world, actions),
                    None => {}
                }
                [Rect::default(); 4]
            }
            Page::Chat => {
                self.sync_character_form();
                // A dialog editing the player covers the story: nothing
                // beneath it reacts.
                let editing = self.character_form.is_some() && !self.current().needs_player();
                let covered = ui.blocker;
                if editing && !modal {
                    ui.blocker = Some(main);
                }
                self.draw_header(p, main);
                let (strip_bottom, add_cast) = self.draw_cast(p, ui, main, actions);
                let body = Rect::new(main.x, strip_bottom, main.w, main.bottom() - strip_bottom);
                let toolbar = if self.current().needs_player() {
                    self.draw_character_form(p, ui, body, actions);
                    [Rect::default(); 3]
                } else {
                    let (composer_top, toolbar) =
                        if self.current().playable() { self.draw_composer(p, ui, main, actions) } else { (main.bottom(), [Rect::default(); 3]) };
                    let messages = Rect::new(main.x, strip_bottom, main.w, composer_top - strip_bottom - 12.0);
                    self.draw_messages(p, ui, messages, actions);
                    toolbar
                };
                if editing {
                    ui.blocker = covered;
                    self.draw_character_form(p, ui, main, actions);
                }
                let [model_button, reasoning_button, card] = toolbar;
                [model_button, reasoning_button, card, add_cast]
            }
        };
        if !modal {
            ui.blocker = None;
        }
        self.draw_open_menu(p, ui, toolbar, actions);
        if let Some(spotlight) = &mut self.spotlight {
            ui.blocker = None;
            let context = crate::spotlight::Context {
                sessions: self
                    .conversations
                    .iter()
                    .filter(|c| !c.is_fresh())
                    .map(|c| crate::spotlight::SessionRef { id: &c.session_id, title: &c.title, updated: c.updated })
                    .collect(),
                models: &self.models,
                model: &self.model,
                scheme,
            };
            match spotlight.draw(p, ui, view, &context, actions) {
                Outcome::Pick(pick) => self.apply_pick(pick, actions),
                Outcome::Close => self.spotlight = None,
                Outcome::Stay => {}
            }
        }
    }

    /// Title bar of the chat area: session title and cost.
    fn draw_header(&mut self, p: &mut Painter, main: Rect) {
        let t = p.theme;
        let bar = Rect::new(main.x, 0.0, main.w, theme::HEADER_HEIGHT);
        p.rect(Rect::new(bar.x, bar.bottom() - 1.0, bar.w, 1.0), t.border, 0.0);
        let conversation = self.current();
        let (tokens, cost) = (conversation.tokens(), conversation.cost());
        let (world, title) = (conversation.world.clone(), conversation.title.clone());
        // The world first, then what this story is about.
        let world = world.map(|id| self.library.get(Kind::World, &id).map_or("A deleted world".to_owned(), |w| w.name.clone()));
        let title = match (world, title.is_empty()) {
            (Some(world), true) => world,
            (Some(world), false) => format!("{world}  ·  {title}"),
            (None, true) => "No story yet".to_owned(),
            (None, false) => title,
        };

        let mut right = bar.right() - 16.0;
        if tokens > 0 {
            let spent = p.layout(&format!("{} tokens  ·  {}", group_digits(tokens), format_cost(cost)), theme::SMALL, None);
            right -= spent.width();
            p.text(&spent, right, bar.y + (bar.h - spent.height()) * 0.5, t.text_faint);
            right -= 20.0;
        }
        let mut layout = p.layout(&title, theme::LABEL, None);
        layout.truncate(p.fonts, (right - bar.x - 16.0).max(40.0));
        p.text(&layout, bar.x + 16.0, bar.y + (bar.h - layout.height()) * 0.5, t.text);
    }

    /// Column holding messages and the composer inside `main`.
    fn column(main: Rect) -> (f32, f32) {
        let width = theme::COLUMN_WIDTH.min(main.w - 64.0);
        (main.x + ((main.w - width) * 0.5).round(), width)
    }
}

/// Display name of a model id.
fn model_name<'a>(models: &'a [Model], id: &'a str) -> &'a str {
    models.iter().find(|m| m.id == id && !m.name.is_empty()).map_or(id, |m| m.name.as_str())
}

/// A model's prices, e.g. `$2 / $10`; empty when the server sent none.
fn price(model: &Model) -> String {
    if model.input_cost_per_million > 0.0 || model.output_cost_per_million > 0.0 {
        format!("${} / ${}", model.input_cost_per_million, model.output_cost_per_million)
    } else {
        String::new()
    }
}

/// Caption for a finished reply, e.g. `Claude Sonnet 5.5 · 1,204 tokens · $0.0031`.
fn usage_caption(model: &str, usage: Usage, cost: f64) -> String {
    let tokens = usage.input_tokens + usage.output_tokens;
    match (tokens, cost > 0.0) {
        (0, _) => model.to_owned(),
        (_, true) => format!("{model}  ·  {} tokens  ·  {}", group_digits(tokens), format_cost(cost)),
        (_, false) => format!("{model}  ·  {} tokens", group_digits(tokens)),
    }
}

/// Compact age of a timestamp: `now`, `5m`, `3h`, `2d`, `6w`, `1y`.
pub(crate) fn ago(now: u64, then: u64) -> String {
    let secs = now.saturating_sub(then);
    match secs {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m", secs / 60),
        3_600..86_400 => format!("{}h", secs / 3_600),
        86_400..604_800 => format!("{}d", secs / 86_400),
        604_800..31_536_000 => format!("{}w", secs / 604_800),
        _ => format!("{}y", secs / 31_536_000),
    }
}

/// `1234567` -> `1,234,567`.
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A USD amount with enough precision to be meaningful for tiny requests.
pub fn format_cost(usd: f64) -> String {
    if usd <= 0.0 {
        "$0.00".to_owned()
    } else if usd < 0.0001 {
        "<$0.0001".to_owned()
    } else if usd < 0.01 {
        format!("${usd:.4}")
    } else {
        format!("${usd:.2}")
    }
}

#[cfg(test)]
mod tests {
    use serechat::{Completion, InputItem, StreamEvent, ToolCall};

    use super::*;

    #[test]
    fn captions() {
        assert_eq!(group_digits(7), "7");
        assert_eq!(group_digits(1_234), "1,234");
        assert_eq!(group_digits(1_234_567), "1,234,567");
        assert_eq!(format_cost(0.0), "$0.00");
        assert_eq!(format_cost(0.000_01), "<$0.0001");
        assert_eq!(format_cost(0.003_14), "$0.0031");
        assert_eq!(format_cost(1.5), "$1.50");
        let usage = Usage::new(1_000, 204);
        assert_eq!(usage_caption("M", usage, 0.0031), "M  ·  1,204 tokens  ·  $0.0031");
        assert_eq!(usage_caption("M", usage, 0.0), "M  ·  1,204 tokens");
        assert_eq!(usage_caption("M", Usage::default(), 1.0), "M");
    }

    #[test]
    fn ages() {
        assert_eq!(ago(100, 100), "now");
        assert_eq!(ago(100, 200), "now", "clock skew must not underflow");
        assert_eq!(ago(3_600 + 59, 0), "1h");
        assert_eq!(ago(86_400 * 3, 0), "3d");
        assert_eq!(ago(604_800 * 5, 0), "5w");
        assert_eq!(ago(31_536_000 * 2, 0), "2y");
    }

    #[test]
    fn reasoning_keys() {
        for r in Reasoning::ALL {
            assert_eq!(Reasoning::from_key(Some(r.key())), r);
        }
        assert_eq!(Reasoning::from_key(Some("bogus")), Reasoning::Auto);
    }

    #[test]
    fn reasoning_follows_the_model() {
        let model = |levels: &[&str]| Model {
            id: "m".into(),
            name: String::new(),
            input_cost_per_million: 0.0,
            output_cost_per_million: 0.0,
            cache_read_cost_per_million: None,
            cache_write_cost_per_million: None,
            input_types: Vec::new(),
            context_window: 0,
            reasoning_levels: levels.iter().map(|l| (*l).to_owned()).collect(),
        };
        let claude = model(&["low", "medium", "high", "xhigh", "max"]);
        assert_eq!(Reasoning::choices(Some(&claude))[..2], [Reasoning::Auto, Reasoning::Low]);
        assert_eq!(Reasoning::choices(Some(&model(&[]))), [Reasoning::Auto], "a model that cannot reason");
        assert_eq!(Reasoning::choices(None).len(), Reasoning::ALL.len());

        let mut chat = Chat::new(Some("m".into()), Reasoning::Off, Vec::new());
        assert_eq!(chat.reasoning_in_use(), Reasoning::Off, "unknown models keep the choice");
        chat.set_models(vec![claude]);
        assert_eq!(chat.reasoning_in_use(), Reasoning::Auto, "Off is not a level this model accepts");
        chat.reasoning = Reasoning::Max;
        assert_eq!(chat.reasoning_in_use(), Reasoning::Max);
    }

    #[test]
    fn reasoning_visibility_follows_the_setting() {
        for v in ReasoningView::ALL {
            assert_eq!(ReasoningView::from_key(Some(v.key())), v);
        }
        assert_eq!(ReasoningView::from_key(None), ReasoningView::Collapsed);

        let mut message = StoredMessage::new(Role::Assistant, "42".into());
        message.reasoning = "6 × 7".into();
        let mut entry = Entry::new(1, message);
        assert!(!entry.reasoning_shown(ReasoningView::Collapsed));
        assert!(entry.reasoning_shown(ReasoningView::Expanded));
        entry.reasoning_open = Some(true);
        assert!(entry.reasoning_shown(ReasoningView::Collapsed), "a click overrides the setting");
        assert!(!entry.reasoning_shown(ReasoningView::Hidden), "hidden always wins");
    }

    #[test]
    fn only_reasoning_replies_record_thinking_time() {
        let mut chat = new_chat();
        for reasoning in ["", "thought"] {
            chat.composer.insert("hi");
            let mut actions = Vec::new();
            chat.send(&mut actions);
            let Some(Action::Send(job)) = actions.pop() else { panic!("no send") };
            if let Some(stream) = &mut chat.current().stream {
                stream.started -= std::time::Duration::from_secs(3);
            }
            chat.stream_event(job.conversation, job.stream, StreamEvent::Text("answer".into()));
            let completion = Completion { reasoning: reasoning.into(), ..Completion::default() };
            chat.stream_event(job.conversation, job.stream, StreamEvent::Completed(completion));
            chat.stream_end(job.conversation, job.stream, Ok(true), &mut Vec::new());
            let ms = chat.current().entries.last().map(|e| e.message.reasoning_ms);
            assert_eq!(ms.is_some_and(|ms| ms >= 3000), !reasoning.is_empty(), "reasoning {reasoning:?}");
        }
    }

    #[test]
    fn sessions_round_trip_through_the_screen() {
        let mut reply = StoredMessage::new(Role::Assistant, "hello".into());
        reply.cost = 0.5;
        reply.usage = Usage::new(3, 4);
        let session = Session {
            id: "abc".into(),
            title: "Hi".into(),
            created: 1,
            updated: 2,
            world: None,
            cast: Vec::new(),
            player: None,
            messages: vec![StoredMessage::new(Role::User, "hi".into()), reply],
        };
        let mut chat = Chat::new(None, Reasoning::Auto, vec![session.summary()]);
        // The saved session plus a fresh, unsaved conversation.
        assert_eq!(chat.conversations.len(), 2);
        // Totals come from the index without loading any messages.
        let totals = chat.totals();
        assert_eq!((totals.sessions, totals.tokens), (1, 7));

        let id = chat.conversations.iter().find(|c| c.session_id == "abc").map(|c| c.id).unwrap();
        let mut actions = Vec::new();
        chat.open(id, &mut actions);
        assert!(matches!(&actions[..], [Action::LoadSession { session, .. }] if session == "abc"));

        // Sending must wait for the history, or saving would drop it.
        chat.composer.insert("more");
        let mut actions = Vec::new();
        chat.send(&mut actions);
        assert!(actions.is_empty());

        chat.session_loaded(id, Ok(session.clone()));
        assert_eq!(chat.current().to_session(), session);
        chat.send(&mut actions);
        assert!(matches!(&actions[..], [Action::SaveSession(saved), Action::Send(_)] if saved.messages.len() == 3));
    }

    fn new_chat() -> Chat {
        let mut chat = Chat::new(None, Reasoning::Auto, Vec::new());
        chat.play("w".into());
        chat.current().player = Some(Player { name: "Gale".into(), description: String::new() });
        chat
    }

    /// Sends `prompt` and returns the request.
    fn start(chat: &mut Chat, prompt: &str) -> SendJob {
        chat.composer.insert(prompt);
        let mut actions = Vec::new();
        chat.send(&mut actions);
        next_send(actions).expect("a request")
    }

    /// Completes `job` with `completion` and returns what the chat asked for.
    fn finish(chat: &mut Chat, job: &SendJob, completion: Completion) -> Vec<Action> {
        chat.stream_event(job.conversation, job.stream, StreamEvent::Completed(completion));
        let mut actions = Vec::new();
        chat.stream_end(job.conversation, job.stream, Ok(true), &mut actions);
        actions
    }

    fn next_send(actions: Vec<Action>) -> Option<SendJob> {
        actions.into_iter().find_map(|a| if let Action::Send(job) = a { Some(job) } else { None })
    }

    #[test]
    fn failed_requests_retry_then_give_up() {
        use std::time::Instant;
        let mut chat = new_chat();
        let mut job = start(&mut chat, "hi");
        for attempt in 1..=stream::MAX_RETRIES {
            chat.stream_event(job.conversation, job.stream, StreamEvent::Text("partial".into()));
            let error = Error::Response { code: Some("server_error".into()), message: "boom".into() };
            chat.stream_end(job.conversation, job.stream, Err(error), &mut Vec::new());
            let c = chat.current();
            assert_eq!(c.retry.as_ref().map(|r| r.attempt), Some(attempt));
            assert_eq!(c.entries.len(), 1, "the partial reply is dropped");
            assert!(c.busy() && !c.resumable());
            let mut actions = Vec::new();
            chat.tick(Instant::now(), &mut actions);
            assert!(actions.is_empty(), "not due yet");
            chat.tick(chat.next_deadline().unwrap(), &mut actions);
            job = next_send(actions).expect("retried");
        }
        let error = Error::Response { code: Some("server_error".into()), message: "boom".into() };
        chat.stream_end(job.conversation, job.stream, Err(error), &mut Vec::new());
        let c = chat.current();
        assert!(c.retry.is_none() && c.entries.last().is_some_and(|e| e.message.failed && e.message.content == "boom"));
        assert!(c.resumable(), "Continue tries again by hand");
        let id = c.id;
        let mut actions = Vec::new();
        chat.resume(id, &mut actions);
        let job = next_send(actions).expect("continued");

        // Final errors are shown at once.
        let error = Error::Response { code: Some("invalid_request_error".into()), message: "bad".into() };
        chat.stream_end(job.conversation, job.stream, Err(error), &mut Vec::new());
        assert!(chat.current().retry.is_none() && !chat.current().busy());
    }

    #[test]
    fn silent_streams_are_retried() {
        let mut chat = new_chat();
        let job = start(&mut chat, "hi");
        let mut actions = Vec::new();
        chat.tick(chat.next_deadline().unwrap() + std::time::Duration::from_secs(1), &mut actions);
        assert!(job.cancel.load(Ordering::Relaxed), "the dead stream is abandoned");
        assert_eq!(chat.current().retry.as_ref().map(|r| r.attempt), Some(1));
        // Its late end is ignored.
        chat.stream_end(job.conversation, job.stream, Ok(true), &mut Vec::new());
        assert!(chat.current().retry.is_some());
    }

    #[test]
    fn stopping_keeps_partial_output() {
        let mut chat = new_chat();
        let job = start(&mut chat, "go");
        chat.stream_event(job.conversation, job.stream, StreamEvent::Text("half".into()));
        chat.stop(&mut Vec::new());
        assert!(job.cancel.load(Ordering::Relaxed) && !chat.is_busy());
        assert_eq!(chat.current().entries.last().map(|e| e.message.content.as_str()), Some("half"));
    }

    #[test]
    fn replies_cut_off_say_so() {
        let mut chat = new_chat();
        let job = start(&mut chat, "write it");
        chat.stream_event(job.conversation, job.stream, StreamEvent::Text("Long".into()));
        let actions = finish(&mut chat, &job, Completion { incomplete: Some("max_output_tokens".into()), ..Completion::default() });
        assert!(next_send(actions).is_none());
        assert!(chat.current().entries.last().is_some_and(|e| e.message.failed && e.message.content.contains("cut off")));
    }

    #[test]
    fn long_conversations_are_summarised() {
        let mut chat = new_chat();
        let job = start(&mut chat, "first");
        chat.stream_event(job.conversation, job.stream, StreamEvent::Text("ok".into()));
        // The reply used most of the (default) window.
        finish(&mut chat, &job, Completion { usage: Usage::new(120_000, 1_000), ..Completion::default() });

        let summary = start(&mut chat, "second");
        assert!(summary.history.last().is_some_and(|m| m.role == Role::User && m.content.contains("summary")));
        chat.stream_event(summary.conversation, summary.stream, StreamEvent::Text("They said first.".into()));
        let actions = finish(&mut chat, &summary, Completion { usage: Usage::new(121_000, 50), ..Completion::default() });
        let reply = next_send(actions).expect("the reply");
        let items = input_items(&reply.history);
        assert_eq!(items.len(), 2, "the summary, then the new prompt word for word");
        assert!(matches!(&items[0], InputItem::Message { text, .. } if text.contains("They said first.")));
        assert!(matches!(&items[1], InputItem::Message { text, .. } if text == "second"));
        // The history itself is kept for the user.
        assert!(chat.current().entries.len() >= 4);
    }

    #[test]
    fn overflowing_requests_compact_and_keep_the_new_prompt() {
        let mut chat = new_chat();
        let job = start(&mut chat, "first");
        chat.stream_event(job.conversation, job.stream, StreamEvent::Text("ok".into()));
        finish(&mut chat, &job, Completion { usage: Usage::new(1_000, 10), ..Completion::default() });

        let job = start(&mut chat, "second");
        let mut actions = Vec::new();
        let overflow = Error::Response { code: Some("context_length_exceeded".into()), message: "too long".into() };
        chat.stream_end(job.conversation, job.stream, Err(overflow), &mut actions);
        let summary = next_send(actions).expect("a summary request");
        assert!(summary.history.iter().all(|m| m.content != "second"), "the new prompt is not summarised");
        chat.stream_event(summary.conversation, summary.stream, StreamEvent::Text("They said first.".into()));
        let reply = next_send(finish(&mut chat, &summary, Completion::default())).expect("the reply");
        let items = input_items(&reply.history);
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[1], InputItem::Message { text, .. } if text == "second"));
    }

    #[test]
    fn replies_keep_streaming_in_the_background() {
        let mut chat = new_chat();
        let first = start(&mut chat, "one");
        chat.play("w".into());
        chat.current().player = Some(Player { name: "Gale".into(), description: String::new() });
        let second = start(&mut chat, "two");
        assert_ne!(first.conversation, second.conversation);
        assert_eq!(chat.conversations.iter().filter(|c| c.busy()).count(), 2, "both run at once");

        // Events for the chat in the background land in it, not the open one.
        chat.stream_event(first.conversation, first.stream, StreamEvent::Text("a".into()));
        chat.stream_event(second.conversation, second.stream, StreamEvent::Text("b".into()));
        finish(&mut chat, &first, Completion::default());
        finish(&mut chat, &second, Completion::default());
        let reply = |id| chat.conversations.iter().find(|c| c.id == id).and_then(|c| c.entries.last()).map(|e| e.message.content.clone());
        assert_eq!(reply(first.conversation).as_deref(), Some("a"));
        assert_eq!(reply(second.conversation).as_deref(), Some("b"));
        assert!(!chat.is_busy());
    }

    #[test]
    fn clear_starts_over() {
        let mut chat = new_chat();
        let job = start(&mut chat, "hi");
        chat.composer.insert("/cl");
        let mut actions = Vec::new();
        let command = chat.commands()[chat.command_pick];
        chat.run_command(command, &mut actions);
        assert!(job.cancel.load(Ordering::Relaxed), "the running reply is stopped");
        assert!(matches!(&actions[..], [Action::DeleteSession(_)]));
        assert!(chat.current().is_fresh());
        assert!(chat.composer.text().is_empty());

        // Closing the menu keeps it closed until the text changes.
        chat.composer.insert("/");
        chat.sync_command_menu();
        assert!(chat.menu == Some(Menu::Commands));
        chat.close_menu();
        chat.sync_command_menu();
        assert!(chat.menu.is_none() && chat.commands().is_empty());
        chat.composer.insert("c");
        assert_eq!(chat.commands(), [Command::Clear]);
    }

    #[test]
    fn characters_speak_and_join_through_tools() {
        let mut chat = new_chat();
        chat.current().cast.push(CastMember {
            id: "k".into(),
            name: "Katniss".into(),
            description: "A hunter.".into(),
            present: true,
            ..CastMember::default()
        });
        let job = start(&mut chat, "I call out.");
        assert!(job.tools && job.tool_choice == Some(serechat::ToolChoice::Required) && job.instructions.contains("There is no narrator"));

        // Speech shows while its call is written.
        chat.stream_event(job.conversation, job.stream, StreamEvent::Text("Leaves rustle.".into()));
        chat.stream_event(job.conversation, job.stream, StreamEvent::ToolCallStarted { index: 1, name: tools::SPEAK.into() });
        let partial = r#"{"lines":[{"character":"Katniss","text":"Who's th"#;
        chat.stream_event(job.conversation, job.stream, StreamEvent::ToolCallDelta { index: 1, delta: partial.into() });
        let entry = chat.current().entries.last_mut().unwrap();
        entry.refresh_display(true);
        assert_eq!(entry.display, "Leaves rustle.\n\n**Katniss**\n> Who's th");
        let asked = ToolCall {
            call_id: "c0".into(),
            name: tools::SPEAK.into(),
            arguments: r#"{"lines":[{"character":"Katniss","text":"Who's there?"}]}"#.into(),
        };
        assert!(
            start_over(&mut chat, &job, Completion { tool_calls: vec![asked], ..Completion::default() }).is_none(),
            "narration and speech end the turn"
        );

        // A reply that only created someone carries on by itself.
        let job = start(&mut chat, "Me.");
        let created =
            ToolCall { call_id: "c1".into(), name: tools::CREATE_CHARACTER.into(), arguments: r#"{"name":"Cato","description":"A career."}"#.into() };
        let fresh = start_over(&mut chat, &job, Completion { tool_calls: vec![created], ..Completion::default() });
        let next = fresh.expect("the model continues after creating a character");
        assert!(chat.current().cast.iter().any(|m| m.name == "Cato" && m.present));
        let items = input_items(&next.history);
        assert!(items.iter().any(|i| matches!(i, InputItem::ToolOutput { output, .. } if output.contains("Cato joined"))));
        assert!(next.instructions.contains("## Cato"), "the new character is in the prompt");

        // Speech ends the turn; its result goes back with the next prompt.
        let spoken = ToolCall {
            call_id: "c2".into(),
            name: tools::SPEAK.into(),
            arguments: r#"{"lines":[{"character":"Cato","action":"grins","text":"Found you."},{"character":"Katniss","text":"Run!"}]}"#.into(),
        };
        assert!(start_over(&mut chat, &next, Completion { tool_calls: vec![spoken], ..Completion::default() }).is_none());
        let entry = chat.current().entries.last_mut().unwrap();
        entry.refresh_display(false);
        assert_eq!(entry.display, "**Cato** · *grins*\n> Found you.\n\n**Katniss**\n> Run!");
        assert_eq!(entry.message.tool_calls[0].output, "Spoken.");
        let follow = start(&mut chat, "I run.");
        assert!(matches!(input_items(&follow.history).last(), Some(InputItem::Message { text, .. }) if text == "I run."));

        // Speech still being written when the user stops stays as text.
        chat.stream_event(follow.conversation, follow.stream, StreamEvent::ToolCallStarted { index: 0, name: tools::SPEAK.into() });
        chat.stream_event(
            follow.conversation,
            follow.stream,
            StreamEvent::ToolCallDelta { index: 0, delta: r#"{"lines":[{"character":"Cato","text":"Wait"#.into() },
        );
        chat.stop(&mut Vec::new());
        assert_eq!(chat.current().entries.last().map(|e| e.message.content.as_str()), Some("**Cato**\n> Wait"));
    }

    /// Completes `job` and returns the request it led to, if any.
    fn start_over(chat: &mut Chat, job: &SendJob, completion: Completion) -> Option<SendJob> {
        next_send(finish(chat, job, completion))
    }

    #[test]
    fn tool_only_replies_carry_on_a_bounded_number_of_times() {
        let mut chat = new_chat();
        let mut job = start(&mut chat, "Go.");
        let mut rounds = 0;
        loop {
            let call = ToolCall {
                call_id: format!("c{rounds}"),
                name: tools::CREATE_CHARACTER.into(),
                arguments: format!(r#"{{"name":"N{rounds}","description":""}}"#),
            };
            match start_over(&mut chat, &job, Completion { tool_calls: vec![call], ..Completion::default() }) {
                Some(next) => {
                    rounds += 1;
                    job = next;
                }
                None => break,
            }
        }
        assert_eq!(rounds, 3);
        // A new prompt resets the count.
        assert_eq!(chat.current().auto_rounds, 3);
        start(&mut chat, "More.");
        assert_eq!(chat.current().auto_rounds, 0);
    }

    #[test]
    fn a_character_created_in_a_reply_speaks_in_it() {
        let mut chat = new_chat();
        let job = start(&mut chat, "I walk into the tavern and meet the barkeep, Mira.");
        assert!(job.instructions.contains("No one is in the scene yet"));
        // The model may list the speech before the character it needs.
        let speak = ToolCall {
            call_id: "s".into(),
            name: tools::SPEAK.into(),
            arguments: r#"{"lines":[{"character":"Mira","action":"wipes a glass","text":"What'll it be?"}]}"#.into(),
        };
        let create = ToolCall {
            call_id: "c".into(),
            name: tools::CREATE_CHARACTER.into(),
            arguments: r#"{"name":"Mira","description":"The barkeep."}"#.into(),
        };
        let next = start_over(&mut chat, &job, Completion { tool_calls: vec![speak, create], ..Completion::default() });
        assert!(next.is_none(), "she spoke: the turn is over");
        let calls = &chat.current().entries.last().unwrap().message.tool_calls;
        assert_eq!(calls[0].output, "Spoken.", "Mira was created before she spoke");
        let follow = start(&mut chat, "A beer, please.");
        assert!(follow.instructions.contains("## Mira") && !follow.instructions.contains("No one is in the scene yet"));
    }

    #[test]
    fn newcomers_are_introduced_or_described_before_the_story_moves_on() {
        let mut chat = new_chat();
        // Introduced in the speak call itself: cast at once, nothing to repair.
        let job = start(&mut chat, "I enter the cantina.");
        let introduced = r#"{"introduce":[{"name":"Rex","description":"A clone captain."}],"lines":[{"character":"Rex","text":"General."}]}"#;
        let speak = ToolCall { call_id: "s1".into(), name: tools::SPEAK.into(), arguments: introduced.into() };
        assert!(start_over(&mut chat, &job, Completion { tool_calls: vec![speak], ..Completion::default() }).is_none());
        assert!(chat.current().cast.iter().any(|m| m.name == "Rex" && m.description == "A clone captain." && m.present));
        let entry = chat.current().entries.last_mut().unwrap();
        entry.refresh_display(false);
        assert_eq!(entry.display, "*Rex joins the story.*\n\n**Rex**\n> General.");

        // Someone acting without being introduced is not cast half-made: the
        // model is made to describe them next.
        let job = start(&mut chat, "Who else is here?");
        let skipped = r#"{"introduce":[],"lines":[{"character":"Kicker","action":"checks his rifle","text":"Just us, sir."}]}"#;
        let speak = ToolCall { call_id: "s2".into(), name: tools::SPEAK.into(), arguments: skipped.into() };
        let repair = start_over(&mut chat, &job, Completion { tool_calls: vec![speak], ..Completion::default() }).expect("a repair");
        assert!(!chat.current().cast.iter().any(|m| m.name == "Kicker"));
        assert_eq!(repair.tool_choice, Some(serechat::ToolChoice::Function(tools::CREATE_CHARACTER)));
        let items = input_items(&repair.history);
        assert!(
            items.iter().any(|i| matches!(i, InputItem::ToolOutput { output, .. } if output.contains("Not in the cast with a description: Kicker")))
        );

        // The description arrives; the turn was already shown, so it ends there.
        let described = ToolCall {
            call_id: "c1".into(),
            name: tools::CREATE_CHARACTER.into(),
            arguments: r#"{"name":"Kicker","description":"A clone trooper."}"#.into(),
        };
        assert!(start_over(&mut chat, &repair, Completion { tool_calls: vec![described], ..Completion::default() }).is_none());
        assert!(chat.current().cast.iter().any(|m| m.name == "Kicker" && m.description == "A clone trooper."));
        let next = start(&mut chat, "Good.");
        assert_eq!(next.tool_choice, Some(serechat::ToolChoice::Required), "back to normal");

        // A model that never describes them gives up after the bounded rounds.
        let skipped = r#"{"introduce":[],"lines":[{"character":"Echo","text":"Hey."}]}"#;
        let mut job = next;
        let mut calls = vec![ToolCall { call_id: "s3".into(), name: tools::SPEAK.into(), arguments: skipped.into() }];
        let mut rounds = 0;
        while let Some(again) = start_over(&mut chat, &job, Completion { tool_calls: std::mem::take(&mut calls), ..Completion::default() }) {
            rounds += 1;
            job = again;
            calls = vec![ToolCall {
                call_id: format!("x{rounds}"),
                name: tools::CREATE_CHARACTER.into(),
                arguments: r#"{"name":"Echo","description":""}"#.into(),
            }];
        }
        assert_eq!(rounds, 3);
    }
}
