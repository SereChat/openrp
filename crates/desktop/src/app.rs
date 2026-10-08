//! Application state, event routing and background work.
//!
//! Threading model: the winit thread owns all state and renders. Network
//! calls, file imports, dialogs and searches run on short-lived
//! worker threads that report back through [`WorkerEvent`]s posted to the
//! event loop, and every file the app writes goes through one [`Writer`]
//! thread, so the UI never blocks.

use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use serechat::{
    Character, Client, Config, Error, InputItem, Library, LoreEntry, Model, Persona, Portraits, ResponseRequest, Role, SearchHit, Session,
    SessionStore, SessionSummary, SignIn, StreamEvent, ToolCall, ToolChoice, ToolSpec, Usage, World,
};
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::Key;
use winit::window::{CursorIcon, Theme, UserAttentionType, Window};

use crate::chat::{self, Chat, GenerateJob, Reasoning, ReasoningView, ReviewJob, SendJob};
use crate::gpu::{Frame, GpuError, Instance, Renderer};
use crate::image::{self, ImageAtlas, ImageKey};
use crate::library::Kind;
use crate::log;
use crate::login::Login;
use crate::paint::{Painter, Rect};
use crate::platform;
use crate::settings::Provider;
use crate::text::{Fonts, GlyphAtlas};
use crate::theme::{Palette, Scheme};
use crate::ui::{Ui, copy};

/// Length of the cross-fade when the colour scheme changes.
const THEME_FADE_SECS: f32 = 0.25;

/// The keychain entry holding the SereChat sign-in's refresh token.
const SIGN_IN: &str = "serechat";
/// The config's `provider` when replies come from a custom provider.
const CUSTOM: &str = "custom";
/// Window size on first start, in logical pixels.
const WINDOW_SIZE: (u32, u32) = (1200, 800);
/// Smallest window size, in logical pixels.
const MIN_WINDOW_SIZE: (u32, u32) = (760, 520);
/// Interface zoom limits, in percent.
const ZOOM: (u16, u16) = (50, 200);
/// How far one press of Ctrl/Cmd + or - zooms, in percent.
const ZOOM_STEP: u16 = 10;
/// Waits before trying to load the model list again, one per failure.
const MODEL_RETRIES: [Duration; 5] =
    [Duration::from_secs(5), Duration::from_secs(15), Duration::from_secs(30), Duration::from_secs(60), Duration::from_secs(120)];
/// Wait between attempts to bring the GPU back after it was lost.
const GPU_RETRY: Duration = Duration::from_secs(2);

/// Results delivered from worker threads.
pub enum WorkerEvent {
    /// A sign-in is listening for the browser: its consent page can open.
    SignInOpened {
        /// The sign-in it is for (see `Login`).
        attempt: u64,
        /// The consent page.
        url: String,
    },
    /// A sign-in finished.
    SignedIn {
        /// The sign-in it is for.
        attempt: u64,
        /// The signed-in client, or why there is none.
        result: Result<Client, Error>,
    },
    /// The model list arrived.
    Models {
        /// The connection it was asked for (see `App::connection`).
        connection: u64,
        /// The models, or why they could not be listed.
        result: Result<Vec<Model>, Error>,
    },
    /// A session's messages were read from disk.
    SessionLoaded {
        /// Conversation that requested them.
        conversation: u64,
        /// The session (boxed: it dwarfs the other events), or why it
        /// could not be read.
        result: Result<Box<Session>, Error>,
    },
    /// A streamed update for a reply.
    Stream {
        /// Conversation id.
        conversation: u64,
        /// Stream id.
        stream: u64,
        /// The update.
        event: StreamEvent,
    },
    /// A reply stream ended.
    StreamEnded {
        /// Conversation id.
        conversation: u64,
        /// Stream id.
        stream: u64,
        /// `Ok(true)` if completed, `Ok(false)` if cut short.
        result: Result<bool, Error>,
    },
    /// The character generator answered.
    CharacterGenerated {
        /// Conversation it was for; `None` for the Characters page.
        conversation: Option<u64>,
        /// Request id.
        request: u64,
        /// The call it made, if it made one.
        result: Result<Option<ToolCall>, Error>,
    },
    /// The memory review answered.
    MemoriesReviewed {
        /// Conversation it was for.
        conversation: u64,
        /// Request id.
        request: u64,
        /// The call it made, if it made one, and what the request used.
        result: Result<(Option<ToolCall>, Usage), Error>,
    },
    /// The saved worlds, characters and personas were read.
    Library {
        /// Every world, in no particular order.
        worlds: Vec<World>,
        /// Every character, in no particular order.
        characters: Vec<Character>,
        /// Every persona, in no particular order.
        personas: Vec<Persona>,
        /// Where their portraits are.
        portraits: Portraits,
    },
    /// The portrait picker closed: the name of the copied-in file, `None`
    /// when cancelled, or why it failed.
    PortraitPicked(Result<Option<String>, String>),
    /// Character cards were read: the characters (portraits copied in)
    /// and, for each file that could not be, why.
    CardsImported {
        /// One per card read, without ids yet.
        characters: Vec<Character>,
        /// What went wrong with the others.
        errors: Vec<String>,
    },
    /// A lorebook was read for the open form: its entries (none when the
    /// picker was cancelled), or why it could not be.
    LoreImported(Result<Vec<LoreEntry>, String>),
    /// A card or story was saved where the user chose; `None` when they
    /// cancelled.
    Exported(Option<PathBuf>),
    /// Message-content search results.
    SearchResults {
        /// Request generation, to drop stale results.
        generation: u64,
        /// Matching sessions.
        hits: Vec<SearchHit>,
    },
    /// A thumbnail was decoded (or could not be).
    Thumbnail(ImageKey, Result<Vec<u8>, String>),
    /// Something went wrong out of sight (a file could not be written):
    /// what to tell the user, and whether the data folder can help.
    Problem {
        /// What happened, in a sentence.
        text: String,
        /// Offer to open the data folder.
        folder: bool,
    },
}

/// Something a screen wants done that needs app-level resources.
pub enum Action {
    /// Begin a browser sign-in to SereChat.
    StartLogin {
        /// Numbers it, for its results.
        attempt: u64,
        /// Raised to give up on it.
        cancel: Arc<AtomicBool>,
    },
    /// Open a sign-in's consent page again.
    OpenAuthPage(String),
    /// Stream a reply.
    Send(SendJob),
    /// Ask the character generator for someone.
    GenerateCharacter(GenerateJob),
    /// Ask the memory reviewer what a story must not forget.
    ReviewMemories(ReviewJob),
    /// Persist a model choice.
    SelectModel(String),
    /// Persist a reasoning effort.
    SetReasoning(Reasoning),
    /// Switch and persist the colour scheme.
    SetTheme(Scheme),
    /// Switch and persist how replies show reasoning.
    SetReasoningView(ReasoningView),
    /// Read a session's messages on a worker thread.
    LoadSession {
        /// Conversation waiting for them.
        conversation: u64,
        /// Session id.
        session: String,
    },
    /// Write a session to disk.
    SaveSession(Session),
    /// Delete a session file by id.
    DeleteSession(String),
    /// Search every session's messages.
    Search {
        /// Text to find.
        query: String,
        /// Request generation.
        generation: u64,
    },
    /// Write a world to disk.
    SaveWorld(World),
    /// Write a character to disk.
    SaveCharacter(Character),
    /// Write a persona to disk.
    SavePersona(Persona),
    /// Delete a world's, character's or persona's file by id.
    DeleteRecord(Kind, String),
    /// Ask for an image and copy it in as a portrait.
    PickPortrait,
    /// Ask for character cards (PNG or JSON) and read them in.
    ImportCards,
    /// Ask for a lorebook and read its entries, for the open form.
    ImportLore,
    /// Ask where to save a character card and write it: a PNG made from
    /// the portrait when it has one, JSON otherwise.
    ExportCard {
        /// Who.
        character: Character,
        /// Their portrait's file.
        portrait: Option<PathBuf>,
    },
    /// Ask where to save a story and write it.
    ExportStory {
        /// The story, with every message.
        session: Session,
        /// The name of its world.
        world: String,
        /// As a SillyTavern chat (JSONL) rather than a readable transcript.
        jsonl: bool,
    },
    /// Show a folder (one a file was exported to) in the file manager.
    OpenFolder(PathBuf),
    /// Load the model list again (it failed before).
    LoadModels,
    /// Persist the model for work beside the story; `None`: the story's.
    SetUtilityModel(Option<String>),
    /// Show `~/.openrp/sessions` in the file manager.
    OpenDataDir,
    /// Open a link from a reply in the browser.
    OpenLink(String),
    /// Put text on the clipboard.
    Copy(String),
    /// A reply finished or failed: flash the window if it is in the background.
    Attention,
    /// Use a custom OpenAI-compatible provider from now on.
    ConnectCustom {
        /// Its API root, e.g. `https://openrouter.ai/api/v1`.
        base_url: String,
        /// Its key; `None` keeps the saved one if it was for the same address.
        api_key: Option<String>,
    },
    /// Use SereChat from now on, signing in first if needed.
    UseSereChat,
    /// Forget the custom provider's key (and stop using it).
    ForgetKey,
    /// Forget the credentials of the provider in use and move on: to
    /// SereChat when still signed in to it, to sign-in otherwise.
    SignOut,
}

/// Failure to start the app.
#[derive(Debug)]
pub enum StartupError {
    /// The OS refused to create a window.
    Window(winit::error::OsError),
    /// GPU initialisation failed.
    Gpu(GpuError),
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Window(e) => write!(f, "cannot create the window: {e}"),
            Self::Gpu(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for StartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Window(e) => Some(e),
            Self::Gpu(e) => Some(e),
        }
    }
}

enum Screen {
    Login(Login),
    // Boxed: the chat screen is far larger than the login screen.
    Chat(Box<Chat>),
}

/// The running application.
pub struct App {
    window: Arc<Window>,
    /// For creating the renderer again after the GPU was lost.
    display: winit::event_loop::OwnedDisplayHandle,
    renderer: Renderer,
    /// When to try again to bring back a lost GPU.
    gpu_retry: Option<Instant>,
    /// When to load the model list again after it failed.
    models_retry: Option<Instant>,
    /// Loads of the model list that failed in a row.
    models_failures: usize,
    /// Counts provider changes, so a model list asked of an earlier
    /// provider is dropped.
    connection: u64,
    fonts: Fonts,
    atlas: GlyphAtlas,
    images: ImageAtlas,
    instances: Vec<Instance>,
    proxy: EventLoopProxy<WorkerEvent>,
    clipboard: Option<Clipboard>,
    ui: Ui,
    start: Instant,
    last_frame: Instant,
    config: Config,
    /// The client for the provider in use.
    client: Client,
    /// Signed in to SereChat: its client, whose clones share the tokens.
    serechat: Option<Client>,
    /// Where sessions are saved; `None` when there is no home directory.
    sessions_dir: Option<PathBuf>,
    /// Where worlds, characters and personas are saved; `None` without a home directory.
    library: Option<Store>,
    /// Performs every file write, in order, off the UI thread.
    writer: Writer,
    screen: Screen,
    scheme: Scheme,
    /// Colours drawn this frame; differs from `scheme` during a cross-fade.
    palette: Palette,
    /// Palette a cross-fade started from, and its progress in `0..=1`.
    fade: Option<(Palette, f32)>,
    /// Whether the window has been made visible (after the first frame).
    shown: bool,
    /// Cursor currently set on the window.
    cursor: CursorIcon,
    /// Input-method candidate area last sent to the window.
    ime_area: Option<Rect>,
    /// Interface zoom in percent, on top of the display's scale.
    zoom: u16,
}

impl App {
    /// Creates the window and GPU state and picks the first screen.
    ///
    /// # Errors
    /// See [`StartupError`].
    pub fn new(event_loop: &ActiveEventLoop, proxy: EventLoopProxy<WorkerEvent>) -> Result<Self, StartupError> {
        let config = Config::load().unwrap_or_else(|e| {
            log::error(format!("ignoring unreadable config: {e}"));
            Config::default()
        });
        let scheme = Scheme::from_key(config.theme.as_deref());
        let zoom = config.zoom.as_deref().and_then(|z| z.parse().ok()).map_or(100, |z: u16| z.clamp(ZOOM.0, ZOOM.1));
        let ((width, height), maximized) = window_state(config.window.as_deref());
        let attributes = Window::default_attributes()
            .with_title("OpenRP")
            .with_inner_size(LogicalSize::new(width, height))
            .with_min_inner_size(LogicalSize::new(MIN_WINDOW_SIZE.0, MIN_WINDOW_SIZE.1))
            // Windows shows a window as it maximizes it, so there it waits
            // for the first frame (see `frame`).
            .with_maximized(maximized && !cfg!(windows))
            .with_theme(Some(window_theme(scheme)))
            // Shown after the first frame so the user never sees a blank window.
            .with_visible(false);
        // Let the sidebar run under a transparent title bar on macOS.
        #[cfg(target_os = "macos")]
        let attributes = {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes.with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true)
        };
        // Wayland's app id and X11's WM_CLASS: desktops match it to the
        // installed `openrp.desktop` for the name and icon.
        #[cfg(target_os = "linux")]
        let attributes = winit::platform::wayland::WindowAttributesExtWayland::with_name(attributes, "openrp", "openrp");
        let window = Arc::new(event_loop.create_window(attributes).map_err(StartupError::Window)?);
        set_icon(&window);
        // Chinese, Japanese and Korean input methods deliver text through IME events.
        window.set_ime_allowed(true);
        let display = event_loop.owned_display_handle();
        let renderer = block_on(Renderer::new(Arc::clone(&window), display.clone())).map_err(StartupError::Gpu)?;

        let serechat = match platform::secret(SIGN_IN) {
            Ok(token) => token.map(|token| Client::signed_in(token, keychain(proxy.clone()))),
            Err(e) => {
                log::error(format!("cannot read the sign-in from the keychain: {e}"));
                None
            }
        };
        let client = client_for(&config, serechat.as_ref());
        let signed_in = client.is_some();
        let sessions_dir =
            SessionStore::open().inspect_err(|e| log::error(format!("sessions will not be saved: {e}"))).ok().map(|store| store.dir().to_owned());
        let library = Library::worlds()
            .and_then(|worlds| Ok(Store { worlds, characters: Library::characters()?, personas: Library::personas()?, portraits: Portraits::open()? }))
            .inspect_err(|e| log::error(format!("worlds, characters and personas will not be saved: {e}")))
            .ok();
        let reporter = proxy.clone();
        let report = move |text: String| {
            let _ = reporter.send_event(WorkerEvent::Problem { text, folder: true });
        };
        let now = Instant::now();
        let mut app = Self {
            window,
            display,
            renderer,
            gpu_retry: None,
            models_retry: None,
            models_failures: 0,
            connection: 0,
            fonts: Fonts::load(),
            atlas: GlyphAtlas::default(),
            images: ImageAtlas::default(),
            instances: Vec::new(),
            proxy,
            clipboard: None,
            ui: Ui::default(),
            start: now,
            last_frame: now,
            screen: Screen::Login(Login::new(None)),
            config,
            client: client.unwrap_or_default(),
            serechat,
            writer: Writer::start(sessions_dir.clone(), report),
            sessions_dir,
            library,
            scheme,
            palette: *scheme.palette(),
            fade: None,
            shown: false,
            cursor: CursorIcon::Default,
            ime_area: None,
            zoom,
        };
        app.ui.focused = true;
        if signed_in {
            app.enter_chat();
        }
        // Draw the first frame directly: hidden windows never receive
        // `RedrawRequested` on Windows, so waiting for one would keep the
        // window invisible forever. `frame` shows it once drawn.
        app.frame();
        Ok(app)
    }

    fn enter_chat(&mut self) {
        // Only the index is read here; messages load when a session opens.
        let (sessions, unreadable) = self.writer.list_sessions();
        let session_ids = sessions.iter().map(|s| s.id.clone()).collect();
        let reasoning = Reasoning::from_key(self.config.reasoning.as_deref());
        let mut chat = Chat::new(self.config.model.clone(), reasoning, sessions);
        chat.set_reasoning_view(ReasoningView::from_key(self.config.reasoning_view.as_deref()));
        chat.set_utility_model(self.config.utility_model.clone());
        chat.set_provider(provider(&self.config, self.serechat.is_some()));
        if unreadable > 0 {
            let stories = if unreadable == 1 { "1 saved story" } else { "Some saved stories" };
            chat.notify(format!("{stories} could not be read and are not listed. Their files are left untouched."), true);
        }
        self.screen = Screen::Chat(Box::new(chat));
        self.load_models();
        if let Some(Store { worlds, characters, personas, portraits }) = self.library.clone() {
            let proxy = self.proxy.clone();
            self.spawn(move |_, _| {
                let (worlds, world_errors) = report(worlds.list(), "worlds");
                let (characters, character_errors) = report(characters.list(), "characters");
                let (personas, persona_errors) = report(personas.list(), "personas");
                if world_errors + character_errors + persona_errors > 0 {
                    let text = "Some saved worlds, characters or personas could not be read and are not shown. Their files are left untouched.";
                    let _ = proxy.send_event(WorkerEvent::Problem { text: text.to_owned(), folder: true });
                }
                WorkerEvent::Library { worlds, characters, personas, portraits }
            });
        }
        // A story that could not be read might use any portrait.
        if unreadable == 0 {
            self.sweep_portraits(session_ids);
        }
    }

    /// Fetches the model list on a worker thread.
    fn load_models(&mut self) {
        self.models_retry = None;
        let connection = self.connection;
        self.spawn(move |client, _| WorkerEvent::Models { connection, result: client.models() });
    }

    /// Deletes, on a background thread, portraits no world, character or
    /// story (of `session_ids`) uses any more and that were not picked
    /// recently. It only reads: deleting portraits is all it writes.
    fn sweep_portraits(&self, session_ids: Vec<String>) {
        let (Some(store), Some(sessions)) = (self.library.clone(), self.reader()) else {
            return;
        };
        let sweep = move || {
            let mut keep = std::collections::HashSet::new();
            let (worlds, world_errors) = store.worlds.list::<World>().ok()?;
            let (characters, character_errors) = store.characters.list::<Character>().ok()?;
            let (personas, persona_errors) = store.personas.list::<Persona>().ok()?;
            // An unreadable file might use any portrait: keep them all.
            if !world_errors.is_empty() || !character_errors.is_empty() || !persona_errors.is_empty() {
                return None;
            }
            keep.extend(worlds.into_iter().map(|w| w.portrait));
            keep.extend(characters.into_iter().map(|c| c.portrait));
            keep.extend(personas.into_iter().map(|p| p.portrait));
            for id in session_ids {
                let session = sessions.load(&id).ok()?;
                keep.extend(session.cast.into_iter().map(|m| m.portrait).chain(session.player.map(|p| p.portrait)));
            }
            store.portraits.collect_garbage(&keep, Duration::from_secs(24 * 60 * 60)).ok()
        };
        let spawned = std::thread::Builder::new().name("openrp-sweep".into()).spawn(move || {
            if let Some(deleted @ 1..) = sweep() {
                eprintln!("openrp: deleted {deleted} unused portraits");
            }
        });
        if let Err(e) = spawned {
            log::error(format!("cannot sweep portraits: {e}"));
        }
    }

    /// Starts a cross-fade to `scheme` and persists it.
    fn set_scheme(&mut self, scheme: Scheme) {
        self.fade = Some((self.palette, 0.0));
        self.scheme = scheme;
        self.window.set_theme(Some(window_theme(scheme)));
        self.config.theme = Some(scheme.key().to_owned());
        self.save_config();
    }

    /// Forgets the credentials of the provider in use (a custom one keeps
    /// its address) and moves on; see [`Action::SignOut`]. `reason` says
    /// why, when it was not the user's choice.
    fn sign_out(&mut self, reason: Option<String>) {
        let custom = provider(&self.config, self.serechat.is_some()).custom;
        if custom {
            self.config.provider = None;
            self.config.api_key = None;
        } else if let Some(client) = self.serechat.take() {
            // Its keychain entry goes first, then the grant is revoked.
            let spawned = std::thread::Builder::new().name("openrp-sign-out".into()).spawn(move || {
                if let Err(e) = client.sign_out() {
                    log::error(format!("could not revoke the sign-in: {e}"));
                }
            });
            if let Err(e) = spawned {
                log::error(format!("cannot spawn the sign-out thread: {e}"));
            }
        }
        self.connect(reason.clone());
        // Sign-in offers the custom provider's address again.
        if custom && let (Screen::Login(_), Some(url)) = (&self.screen, &self.config.base_url) {
            self.screen = Screen::Login(Login::custom(reason, url));
        }
    }

    /// Saves the config and talks to the provider it names from now on: the
    /// chat lists that provider's models, or sign-in comes up when there is
    /// nothing to sign in with. `reason` is told to the user.
    fn connect(&mut self, reason: Option<String>) {
        self.save_config();
        self.connection += 1;
        self.models_failures = 0;
        let Some(client) = client_for(&self.config, self.serechat.as_ref()) else {
            if let Screen::Chat(chat) = &mut self.screen {
                chat.cancel_all();
            }
            self.client = Client::new();
            self.models_retry = None;
            self.screen = Screen::Login(Login::new(reason));
            return;
        };
        self.client = client;
        let Screen::Chat(chat) = &mut self.screen else {
            self.enter_chat();
            if let (Some(reason), Screen::Chat(chat)) = (reason, &mut self.screen) {
                chat.notify(reason, false);
            }
            return;
        };
        // The old provider's models mean nothing to the new one.
        chat.set_models(Vec::new());
        chat.set_provider(provider(&self.config, self.serechat.is_some()));
        if let Some(reason) = reason {
            chat.notify(reason, false);
        }
        self.load_models();
    }

    /// Whether a reply is streaming, which the provider must not change
    /// under; the user is told so.
    fn refuse_switch(&mut self) -> bool {
        let Screen::Chat(chat) = &mut self.screen else {
            return false;
        };
        let busy = chat.is_busy();
        if busy {
            chat.notify("Wait for the reply to finish, or stop it, before changing the provider.".into(), false);
        }
        busy
    }

    /// Why the provider stopped taking requests, for the user.
    fn rejected(&self) -> String {
        let text = if provider(&self.config, self.serechat.is_some()).custom {
            "The provider refused the API key. Enter it again to keep using it."
        } else {
            "Your session has expired. Please sign in again."
        };
        text.to_owned()
    }

    /// Physical pixels per logical pixel: the display's scale, zoomed.
    fn scale(&self) -> f32 {
        self.window.scale_factor() as f32 * f32::from(self.zoom) / 100.0
    }

    fn save_config(&mut self) {
        self.writer.send(Job::SaveConfig(self.config.clone()));
    }

    /// A store for reading sessions on a worker thread. Reads never see a
    /// half-written file: the writer replaces files by atomic rename.
    fn reader(&self) -> Option<SessionStore> {
        self.sessions_dir.clone().map(SessionStore::at)
    }

    /// Runs `job` on a worker thread and posts its result to the event loop.
    fn spawn(&self, job: impl FnOnce(&Client, &EventLoopProxy<WorkerEvent>) -> WorkerEvent + Send + 'static) {
        let client = self.client.clone();
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new().name("openrp-worker".into()).spawn(move || {
            let event = job(&client, &proxy);
            // Fails only if the event loop is gone, i.e. the app is exiting.
            let _ = proxy.send_event(event);
        });
        if let Err(e) = spawned {
            log::error(format!("cannot spawn worker thread: {e}"));
        }
    }

    /// Routes a window event.
    pub fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let scale = self.scale();
        let mut actions = Vec::new();
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                self.frame();
                return;
            }
            WindowEvent::Resized(size) => self.renderer.resize(size.width, size.height),
            WindowEvent::Focused(focused) => self.ui.focused = focused,
            WindowEvent::ModifiersChanged(mods) => self.ui.mods = mods.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.ui.mouse = (position.x as f32 / scale, position.y as f32 / scale);
            }
            WindowEvent::CursorLeft { .. } => self.ui.mouse = (f32::MIN, f32::MIN),
            WindowEvent::MouseInput { state, button: MouseButton::Left, .. } => {
                if state == ElementState::Pressed {
                    self.ui.press(self.start.elapsed().as_secs_f32());
                } else {
                    self.ui.down = false;
                    self.ui.released = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 100.0, y * 100.0),
                    MouseScrollDelta::PixelDelta(p) => (p.x as f32 / scale, p.y as f32 / scale),
                };
                // Shift turns a plain wheel sideways, as everywhere else.
                let (dx, dy) = if self.ui.mods.shift_key() && dx == 0.0 { (dy, 0.0) } else { (dx, dy) };
                self.ui.scroll -= dy;
                self.ui.scroll_x -= dx;
            }
            WindowEvent::KeyboardInput { event, is_synthetic: false, .. } if event.state == ElementState::Pressed => {
                let primary = if cfg!(target_os = "macos") { self.ui.mods.super_key() } else { self.ui.mods.control_key() };
                if primary && let Some(zoom) = zoomed(self.zoom, &event.logical_key) {
                    if zoom != self.zoom {
                        self.zoom = zoom;
                        // Resent next frame at the new zoom.
                        self.ime_area = None;
                        self.config.zoom = (zoom != 100).then(|| zoom.to_string());
                        self.save_config();
                    }
                    self.window.request_redraw();
                    return;
                }
                match &mut self.screen {
                    Screen::Login(login) => login.key(&event, self.ui.mods, &mut self.clipboard, &mut actions),
                    Screen::Chat(chat) => chat.key(&event, self.ui.mods, &mut self.clipboard, &mut actions),
                }
                self.ui.last_edit = self.ui.time;
            }
            WindowEvent::Ime(ime) => {
                match (ime, &mut self.screen) {
                    (Ime::Commit(text), Screen::Chat(chat)) => chat.ime_commit(&text),
                    (Ime::Commit(text), Screen::Login(login)) => login.commit(&text),
                    (Ime::Preedit(text, cursor), Screen::Chat(chat)) => chat.ime_preedit(text, cursor),
                    (Ime::Disabled, Screen::Chat(chat)) => chat.ime_preedit(String::new(), None),
                    _ => return,
                }
                self.ui.last_edit = self.ui.time;
            }
            _ => return,
        }
        self.apply(actions);
        self.window.request_redraw();
    }

    /// Applies a result from a worker thread.
    pub fn worker_event(&mut self, event: WorkerEvent) {
        let mut actions = Vec::new();
        match (event, &mut self.screen) {
            (WorkerEvent::Thumbnail(key, pixels), _) => self.images.insert(key, pixels),
            (WorkerEvent::SignInOpened { attempt, url }, Screen::Login(login)) => {
                if login.opened(attempt, &url) {
                    self.open_auth_page(&url);
                }
            }
            (WorkerEvent::SignedIn { attempt, result }, Screen::Login(login)) => {
                if let Some(client) = login.signed_in(attempt, result) {
                    let problem = login.problem.take();
                    self.serechat = Some(client);
                    self.config.provider = None;
                    self.connect(problem);
                }
            }
            // Asked of a provider no longer in use.
            (WorkerEvent::Models { connection, .. }, _) if connection != self.connection => return,
            (WorkerEvent::Models { result: Ok(models), .. }, Screen::Chat(chat)) => {
                (self.models_retry, self.models_failures) = (None, 0);
                chat.set_models(models);
            }
            (WorkerEvent::Models { result: Err(e), .. }, Screen::Chat(_)) if e.is_unauthorized() => self.sign_out(Some(self.rejected())),
            (WorkerEvent::Models { result: Err(e), .. }, Screen::Chat(chat)) => {
                log::error(format!("could not load models: {e}"));
                chat.models_failed(format!("Could not load the models: {e}"));
                let wait = MODEL_RETRIES[self.models_failures.min(MODEL_RETRIES.len() - 1)];
                self.models_failures += 1;
                self.models_retry = Some(Instant::now() + wait);
            }
            (WorkerEvent::Problem { text, folder }, Screen::Chat(chat)) => chat.notify(text, folder),
            (WorkerEvent::Problem { text, .. }, Screen::Login(login)) => login.problem = Some(text),
            (WorkerEvent::SessionLoaded { conversation, result }, Screen::Chat(chat)) => chat.session_loaded(conversation, result.map(|s| *s), &mut actions),
            (WorkerEvent::Stream { conversation, stream, event }, Screen::Chat(chat)) => {
                chat.stream_event(conversation, stream, event);
            }
            (WorkerEvent::StreamEnded { conversation, stream, result }, Screen::Chat(chat)) => {
                if chat.stream_end(conversation, stream, result, &mut actions) {
                    self.apply(actions);
                    self.sign_out(Some(self.rejected()));
                    self.window.request_redraw();
                    return;
                }
            }
            (WorkerEvent::Library { worlds, characters, personas, portraits }, Screen::Chat(chat)) => {
                chat.library_loaded(worlds, characters, personas, portraits);
            }
            (WorkerEvent::PortraitPicked(result), Screen::Chat(chat)) => chat.portrait_picked(result),
            (WorkerEvent::CardsImported { characters, errors }, Screen::Chat(chat)) => chat.cards_imported(characters, errors, &mut actions),
            (WorkerEvent::LoreImported(result), Screen::Chat(chat)) => chat.lore_imported(result),
            (WorkerEvent::Exported(path), Screen::Chat(chat)) => chat.exported(path),
            (WorkerEvent::CharacterGenerated { conversation, request, result }, Screen::Chat(chat)) => {
                chat.character_generated(conversation, request, result);
            }
            (WorkerEvent::MemoriesReviewed { conversation, request, result }, Screen::Chat(chat)) => {
                chat.memories_reviewed(conversation, request, result, &mut actions);
            }
            (WorkerEvent::SearchResults { generation, hits }, Screen::Chat(chat)) => chat.search_results(generation, hits),
            // Results for a screen that is no longer shown.
            _ => return,
        }
        self.apply(actions);
        self.window.request_redraw();
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for action in actions {
            self.run(action);
        }
    }

    fn run(&mut self, action: Action) {
        match action {
            Action::StartLogin { attempt, cancel } => {
                let save = keychain(self.proxy.clone());
                self.spawn(move |_, proxy| {
                    let result = SignIn::start().and_then(|sign_in| {
                        let _ = proxy.send_event(WorkerEvent::SignInOpened { attempt, url: sign_in.url().to_owned() });
                        sign_in.finish(&cancel, save)
                    });
                    WorkerEvent::SignedIn { attempt, result }
                });
            }
            Action::OpenAuthPage(url) => self.open_auth_page(&url),
            Action::Send(job) => self.spawn(move |client, proxy| {
                let (conversation, stream) = (job.conversation, job.stream);
                let input = job.input();
                // Stories offer the story's tools; plain chats none.
                let definitions = if job.tools { chat::tool_definitions() } else { Vec::new() };
                let specs: Vec<ToolSpec<'_>> =
                    definitions.iter().map(|(name, description, parameters)| ToolSpec { name, description, parameters }).collect();
                let request = ResponseRequest {
                    model: &job.model,
                    instructions: Some(&job.instructions),
                    reasoning: job.reasoning,
                    input: &input,
                    tools: &specs,
                    tool_choice: job.tool_choice,
                };
                let result = client.stream_response(&request, &job.cancel, |event| {
                    let _ = proxy.send_event(WorkerEvent::Stream { conversation, stream, event });
                });
                WorkerEvent::StreamEnded { conversation, stream, result }
            }),
            // ponytail: not streamed, cancellable or billed to the story; the
            // reply is one short call. Stream it if generations grow long.
            Action::GenerateCharacter(job) => self.spawn(move |client, _| {
                let (name, description, parameters) = chat::generator_tool();
                let request = ResponseRequest {
                    model: &job.model,
                    instructions: Some(&job.instructions),
                    reasoning: job.reasoning,
                    input: &[InputItem::text(Role::User, job.idea)],
                    tools: &[ToolSpec { name, description, parameters: &parameters }],
                    tool_choice: Some(ToolChoice::Function(name)),
                };
                let mut made = None;
                let result = client.stream_response(&request, &AtomicBool::new(false), |event| {
                    // A call cut off may be incomplete: not used.
                    if let StreamEvent::Completed(completion) = event
                        && completion.incomplete.is_none()
                    {
                        made = completion.tool_calls.into_iter().next();
                    }
                });
                WorkerEvent::CharacterGenerated { conversation: job.conversation, request: job.request, result: result.map(|_| made) }
            }),
            // ponytail: like the generator, not streamed; the chat gives up
            // on one that takes too long, and reads its turns again next time.
            Action::ReviewMemories(job) => self.spawn(move |client, _| {
                let (name, description, parameters) = chat::remember_tool();
                let request = ResponseRequest {
                    model: &job.model,
                    instructions: Some(&job.instructions),
                    reasoning: job.reasoning,
                    input: &[InputItem::text(Role::User, job.transcript)],
                    tools: &[ToolSpec { name, description, parameters: &parameters }],
                    tool_choice: Some(ToolChoice::Function(name)),
                };
                let (mut made, mut used) = (None, Usage::default());
                let result = client.stream_response(&request, &job.cancel, |event| {
                    // A call cut off may be incomplete: not used.
                    if let StreamEvent::Completed(completion) = event {
                        used = completion.usage;
                        if completion.incomplete.is_none() {
                            made = completion.tool_calls.into_iter().next();
                        }
                    }
                });
                WorkerEvent::MemoriesReviewed { conversation: job.conversation, request: job.request, result: result.map(|_| (made, used)) }
            }),
            Action::SelectModel(model) => {
                self.config.model = Some(model);
                self.save_config();
            }
            Action::SetReasoning(reasoning) => {
                self.config.reasoning = Some(reasoning.key().to_owned());
                self.save_config();
            }
            Action::SetTheme(scheme) => self.set_scheme(scheme),
            Action::SetReasoningView(view) => {
                if let Screen::Chat(chat) = &mut self.screen {
                    chat.set_reasoning_view(view);
                }
                self.config.reasoning_view = Some(view.key().to_owned());
                self.save_config();
            }
            Action::LoadSession { conversation, session } => {
                if let Some(store) = self.reader() {
                    self.spawn(move |_, _| WorkerEvent::SessionLoaded { conversation, result: store.load(&session).map(Box::new) });
                }
            }
            Action::SaveSession(session) => self.writer.send(Job::SaveSession(session)),
            Action::DeleteSession(id) => self.writer.send(Job::DeleteSession(id)),
            Action::Search { query, generation } => {
                if let Some(store) = self.reader() {
                    self.spawn(move |_, _| WorkerEvent::SearchResults { generation, hits: store.search(&query, 30).unwrap_or_default() });
                }
            }
            Action::SaveWorld(world) => {
                if let Some(Store { worlds, .. }) = &self.library {
                    self.writer.send(Job::SaveWorld(worlds.clone(), world));
                }
            }
            Action::SaveCharacter(character) => {
                if let Some(Store { characters, .. }) = &self.library {
                    self.writer.send(Job::SaveCharacter(characters.clone(), character));
                }
            }
            Action::SavePersona(persona) => {
                if let Some(Store { personas, .. }) = &self.library {
                    self.writer.send(Job::SavePersona(personas.clone(), persona));
                }
            }
            Action::DeleteRecord(kind, id) => {
                if let Some(Store { worlds, characters, personas, .. }) = &self.library {
                    let library = match kind {
                        Kind::World => worlds,
                        Kind::Character => characters,
                        Kind::Persona => personas,
                    };
                    self.writer.send(Job::DeleteRecord(library.clone(), id));
                }
            }
            Action::PickPortrait => {
                let Some(Store { portraits, .. }) = self.library.clone() else {
                    return;
                };
                self.spawn(move |_, _| {
                    let picked = platform::pick_image().map_err(|e| format!("Could not open the file picker: {e}"));
                    WorkerEvent::PortraitPicked(picked.and_then(|path| match path {
                        Some(path) => portraits.import(&path).map(Some).map_err(|e| e.to_string()),
                        None => Ok(None),
                    }))
                });
            }
            Action::ImportCards => {
                let Some(Store { portraits, .. }) = self.library.clone() else {
                    return;
                };
                self.spawn(move |_, _| import_cards(&portraits));
            }
            Action::ImportLore => self.spawn(|_, _| {
                let picked = platform::pick_files("Import a lorebook", ("Lorebooks and cards", &["json", "png"]), false);
                let read = |path: &std::path::Path| read_file(path).and_then(|bytes| serechat::read_lorebook(&bytes).map_err(|e| e.to_string()));
                WorkerEvent::LoreImported(match picked {
                    Ok(paths) => paths.first().map_or(Ok(Vec::new()), |path| read(path)),
                    Err(e) => Err(format!("Could not open the file picker: {e}")),
                })
            }),
            // The file goes where the user chose, written by the worker that
            // asked: nothing else writes there.
            Action::ExportCard { character, portrait } => self.spawn(move |_, _| {
                let kind = if portrait.is_some() { "png" } else { "json" };
                let card = || match &portrait {
                    Some(path) => image::png_bytes(path).and_then(|png| serechat::embed_card(&png, &character).map_err(|e| e.to_string())),
                    None => Ok(serechat::card_json(&character).into_bytes()),
                };
                export("Export character card", &character.name, ("Character card", &[kind]), card)
            }),
            Action::ExportStory { session, world, jsonl } => self.spawn(move |_, _| {
                let name = if session.title.is_empty() { world.as_str() } else { session.title.as_str() };
                let (filter, render): (platform::Filter<'_>, fn(&Session, &str) -> String) =
                    if jsonl { (("SillyTavern chat", &["jsonl"]), chat::export::jsonl) } else { (("Text", &["txt"]), chat::export::text) };
                export("Export story", name, filter, || Ok(render(&session, &world).into_bytes()))
            }),
            Action::OpenFolder(path) => {
                if let Err(e) = platform::open_folder(&path) {
                    log::error(format!("cannot open the folder: {e}"));
                }
            }
            Action::LoadModels => self.load_models(),
            Action::SetUtilityModel(model) => {
                if let Screen::Chat(chat) = &mut self.screen {
                    chat.set_utility_model(model.clone());
                }
                self.config.utility_model = model;
                self.save_config();
            }
            Action::OpenDataDir => {
                if let Some(dir) = &self.sessions_dir {
                    // The folder may not exist before the first save.
                    let _ = std::fs::create_dir_all(dir);
                    if let Err(e) = platform::open_folder(dir) {
                        log::error(format!("cannot open the data folder: {e}"));
                    }
                }
            }
            Action::OpenLink(url) => {
                // Only web and mail links: a reply must never launch local programs.
                let safe = ["https://", "http://", "mailto:"].iter().any(|scheme| url.starts_with(scheme));
                if safe && let Err(e) = platform::open_url(&url) {
                    log::error(format!("cannot open link: {e}"));
                }
            }
            Action::Copy(text) => copy(&mut self.clipboard, &text),
            Action::Attention if !self.ui.focused => self.window.request_user_attention(Some(UserAttentionType::Informational)),
            Action::Attention => {}
            Action::ConnectCustom { base_url, api_key } => {
                if self.refuse_switch() {
                    return;
                }
                // A saved key only ever goes to the address it was saved for.
                let saved = self.config.api_key.take().filter(|_| self.config.base_url.as_deref() == Some(base_url.as_str()));
                self.config.api_key = api_key.or(saved);
                self.config.provider = Some(CUSTOM.to_owned());
                self.config.base_url = Some(base_url);
                self.connect(None);
            }
            Action::UseSereChat => {
                if !self.refuse_switch() {
                    self.config.provider = None;
                    self.connect(None);
                }
            }
            Action::ForgetKey if provider(&self.config, self.serechat.is_some()).custom => {
                if !self.refuse_switch() {
                    self.sign_out(None);
                }
            }
            Action::ForgetKey => {
                self.config.api_key = None;
                self.save_config();
                if let Screen::Chat(chat) = &mut self.screen {
                    chat.set_provider(provider(&self.config, self.serechat.is_some()));
                }
            }
            Action::SignOut => self.sign_out(None),
        }
    }

    /// Opens a sign-in's consent page in the browser, or copies its link.
    fn open_auth_page(&mut self, url: &str) {
        if let Err(e) = platform::open_url(url) {
            log::error(format!("cannot open browser: {e}"));
            copy(&mut self.clipboard, url);
            if let Screen::Login(login) = &mut self.screen {
                login.browser_failed();
            }
        }
    }

    /// Creates the renderer again after the GPU was lost (a driver reset or
    /// update, a GPU switch), with fresh atlases: theirs lived on the old
    /// device. Tries again every [`GPU_RETRY`] while it fails.
    fn recover_gpu(&mut self) {
        if self.gpu_retry.is_some_and(|at| at > Instant::now()) {
            return;
        }
        match block_on(Renderer::new(Arc::clone(&self.window), self.display.clone())) {
            Ok(renderer) => {
                self.renderer = renderer;
                self.atlas.clear();
                self.images = ImageAtlas::default();
                self.gpu_retry = None;
                let size = self.window.inner_size();
                self.renderer.resize(size.width, size.height);
            }
            Err(e) => {
                log::error(format!("cannot restore the GPU: {e}"));
                self.gpu_retry = Some(Instant::now() + GPU_RETRY);
            }
        }
    }

    /// Builds and presents one frame.
    fn frame(&mut self) {
        if self.renderer.is_lost() {
            self.recover_gpu();
        }
        self.tick();
        let now = Instant::now();
        let dt = (now - self.last_frame).as_secs_f32().min(0.05);
        self.last_frame = now;
        self.ui.begin(dt, (now - self.start).as_secs_f32());

        // Cross-fade between colour schemes with a smoothstep ease.
        let target = self.scheme.palette();
        self.palette = match &mut self.fade {
            Some((from, progress)) if *progress < 1.0 => {
                *progress = (*progress + dt / THEME_FADE_SECS).min(1.0);
                let eased = *progress * *progress * (3.0 - 2.0 * *progress);
                self.ui.animating = true;
                from.mix(target, eased)
            }
            _ => {
                self.fade = None;
                *target
            }
        };

        let scale = self.scale();
        let size = self.window.inner_size();
        let view = Rect::new(0.0, 0.0, size.width as f32 / scale, size.height as f32 / scale);
        let mut actions = Vec::new();
        // A second pass runs only if the glyph atlas overflowed mid-frame.
        for _ in 0..2 {
            let mut painter = Painter::new(&self.fonts, (&mut self.atlas, &mut self.images), &mut self.instances, scale, view, &self.palette);
            match &mut self.screen {
                Screen::Login(login) => login.draw(&mut painter, &mut self.ui, view, &mut actions),
                Screen::Chat(chat) => chat.draw(&mut painter, &mut self.ui, view, self.scheme, &mut actions),
            }
            if !painter.atlas_full {
                break;
            }
            // The first pass already acted on this frame's input; the redraw
            // must not act on it twice.
            self.atlas.clear();
            self.ui.end();
        }

        let drawn = self.renderer.render(&self.instances, &mut self.atlas.uploads, &mut self.images.uploads, scale, self.palette.bg);
        self.ui.end();
        if drawn == Frame::Retry {
            // The surface was set up again: draw this frame onto it.
            self.window.request_redraw();
        }
        if !self.shown {
            self.shown = true;
            // Windows shows a window as it maximizes it: only now, drawn.
            if cfg!(windows) && window_state(self.config.window.as_deref()).1 {
                self.window.set_maximized(true);
            }
            self.window.set_visible(true);
            // Some platforms refuse to present to a hidden surface; draw
            // again now that the window is visible.
            self.window.request_redraw();
        }
        if self.cursor != self.ui.cursor {
            self.cursor = self.ui.cursor;
            self.window.set_cursor(self.cursor);
        }
        // Keep the input method's candidate window next to the caret.
        let area = match &self.screen {
            Screen::Chat(chat) => chat.ime_area(),
            Screen::Login(login) => login.ime_area(),
        };
        if area != self.ime_area {
            self.ime_area = area;
            if let Some(r) = area {
                let z = f32::from(self.zoom) / 100.0;
                self.window.set_ime_cursor_area(LogicalPosition::new(r.x * z, r.y * z), LogicalSize::new(r.w * z, r.h * z));
            }
        }
        let busy = matches!(&self.screen, Screen::Chat(chat) if chat.is_busy());
        self.apply(actions);
        // One worker per frame's misses, decoding them in turn, so a page
        // of images never decodes dozens at once.
        let mut wanted = self.images.take_wanted();
        if let Some(last) = wanted.pop() {
            let proxy = self.proxy.clone();
            self.spawn(move |_, _| {
                for key in wanted {
                    let pixels = image::thumbnail(&key);
                    let _ = proxy.send_event(WorkerEvent::Thumbnail(key, pixels));
                }
                let pixels = image::thumbnail(&last);
                WorkerEvent::Thumbnail(last, pixels)
            });
        }
        if self.ui.animating || busy {
            self.window.request_redraw();
        }
    }

    /// How long the event loop may sleep: until the caret blinks next or the
    /// chat has something to do (a retry, a stream to check on).
    pub fn control_flow(&self) -> ControlFlow {
        let blink = self.ui.focused.then(|| self.last_frame + Duration::from_secs_f32(self.ui.next_blink().max(0.01)));
        let chat = match &self.screen {
            Screen::Chat(chat) => chat.next_deadline(),
            Screen::Login(_) => None,
        };
        match blink.into_iter().chain(chat).chain(self.models_retry).chain(self.gpu_retry).min() {
            Some(at) => ControlFlow::WaitUntil(at),
            None => ControlFlow::Wait,
        }
    }

    /// A timer fired: runs the chat's due work, even while the window is
    /// hidden and gets no frames, and redraws for the caret.
    pub fn wake(&mut self) {
        self.tick();
        self.window.request_redraw();
    }

    /// Sends due retries and drops silent streams.
    fn tick(&mut self) {
        if self.models_retry.is_some_and(|at| at <= Instant::now()) {
            self.load_models();
        }
        let mut actions = Vec::new();
        if let Screen::Chat(chat) = &mut self.screen {
            chat.tick(Instant::now(), &mut actions);
        }
        self.apply(actions);
    }
}

impl Drop for App {
    fn drop(&mut self) {
        // The next start opens the window as it was closed. A minimized
        // window tells nothing, and a maximized one keeps its normal size.
        if self.window.is_minimized() == Some(true) {
            return;
        }
        // macOS's green button makes it fullscreen: as good as maximized.
        let maximized = self.window.is_maximized() || self.window.fullscreen().is_some();
        let (mut size, _) = window_state(self.config.window.as_deref());
        if !maximized {
            let logical = self.window.inner_size().to_logical::<f64>(self.window.scale_factor());
            size = (logical.width.round() as u32, logical.height.round() as u32);
        }
        let state = format!("{}x{}{}", size.0, size.1, if maximized { " maximized" } else { "" });
        if self.config.window.as_deref() != Some(state.as_str()) {
            self.config.window = Some(state);
            // The writer, dropped after this, finishes the write.
            self.save_config();
        }
    }
}

/// The zoom after the primary modifier plus `key`, or `None` if it is not
/// a zoom key: `+` (or `=`, unshifted on many layouts) in, `-` out, `0` back
/// to 100%.
fn zoomed(zoom: u16, key: &Key) -> Option<u16> {
    let Key::Character(c) = key else { return None };
    let zoom = match c.as_str() {
        "+" | "=" => zoom.saturating_add(ZOOM_STEP),
        "-" => zoom.saturating_sub(ZOOM_STEP),
        "0" => 100,
        _ => return None,
    };
    Some(zoom.clamp(ZOOM.0, ZOOM.1))
}

/// The client for the provider `config` names, or `None` when there is
/// nothing to sign in with: no custom provider in use and not signed in to
/// SereChat (`serechat`).
fn client_for(config: &Config, serechat: Option<&Client>) -> Option<Client> {
    match (config.provider.as_deref(), &config.base_url) {
        (Some(CUSTOM), Some(url)) => Some(Client::custom(url, config.api_key.clone())),
        _ => serechat.cloned(),
    }
}

/// Keeps SereChat's refresh token in the OS keychain (`None`: removes it),
/// for [`Client::signed_in`]. Runs on whichever worker refreshed; a failure
/// is logged and told once.
fn keychain(proxy: EventLoopProxy<WorkerEvent>) -> impl Fn(Option<&str>) + Send + Sync + 'static {
    let (proxy, told) = (Mutex::new(proxy), AtomicBool::new(false));
    move |token| {
        let Err(e) = platform::keep_secret(SIGN_IN, token) else { return };
        log::error(format!("cannot keep the sign-in in the keychain: {e}"));
        if !told.swap(true, Ordering::Relaxed) {
            let text = if token.is_some() {
                format!("Could not save your sign-in in the system keychain, so you will have to sign in again next time: {e}")
            } else {
                format!("Could not remove your sign-in from the system keychain: {e}")
            };
            let _ = proxy.lock().unwrap_or_else(PoisonError::into_inner).send_event(WorkerEvent::Problem { text, folder: false });
        }
    }
}

/// What the settings page shows of the providers in `config`, and whether
/// SereChat is `signed_in`.
fn provider(config: &Config, signed_in: bool) -> Provider {
    Provider {
        custom: config.provider.as_deref() == Some(CUSTOM) && config.base_url.is_some(),
        signed_in,
        base_url: config.base_url.clone().unwrap_or_default(),
        has_key: config.api_key.is_some(),
    }
}

/// Logical size and maximized state of the window from the config's
/// `window` (`1200x800`, `1200x800 maximized`); the first-start size when
/// absent or unreadable.
fn window_state(saved: Option<&str>) -> ((u32, u32), bool) {
    let saved = saved.unwrap_or_default();
    let (size, state) = saved.split_once(' ').unwrap_or((saved, ""));
    let size = size.split_once('x').and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)));
    let (w, h) = size.unwrap_or(WINDOW_SIZE);
    // Never smaller than allowed, nor absurdly large from an edited file.
    ((w.clamp(MIN_WINDOW_SIZE.0, 16_384), h.clamp(MIN_WINDOW_SIZE.1, 16_384)), state == "maximized")
}

/// Where worlds, characters, personas and portraits are saved.
#[derive(Clone)]
struct Store {
    worlds: Library,
    characters: Library,
    personas: Library,
    portraits: Portraits,
}

/// A file write for the [`Writer`] thread.
enum Job {
    SaveSession(Session),
    DeleteSession(String),
    SaveConfig(Config),
    SaveWorld(Library, World),
    SaveCharacter(Library, Character),
    SavePersona(Library, Persona),
    /// Delete a world's, character's or persona's file from its library.
    DeleteRecord(Library, String),
    /// List the saved sessions and send them back, with how many files
    /// could not be read.
    ListSessions(mpsc::Sender<(Vec<SessionSummary>, usize)>),
}

/// Tells the user about a write that failed; called on the writer thread.
type Report = Arc<dyn Fn(String) + Send + Sync>;

impl Job {
    /// Performs the job against `store` (`None`: sessions are not saved).
    /// A failure is logged and handed to `report`, for the user.
    fn run(self, store: Option<&mut SessionStore>, report: &dyn Fn(String)) {
        let result = match (self, store) {
            (Self::SaveSession(session), Some(store)) => store.save(&session).map_err(|e| ("save the story", e)),
            (Self::DeleteSession(id), Some(store)) => store.delete(&id).map_err(|e| ("delete the story", e)),
            (Self::SaveSession(_) | Self::DeleteSession(_), None) => Ok(()),
            (Self::SaveConfig(config), _) => config.save().map_err(|e| ("save the settings", e)),
            (Self::SaveWorld(library, world), _) => library.save(&world.id, &world).map_err(|e| ("save the world", e)),
            (Self::SaveCharacter(library, character), _) => library.save(&character.id, &character).map_err(|e| ("save the character", e)),
            (Self::SavePersona(library, persona), _) => library.save(&persona.id, &persona).map_err(|e| ("save the persona", e)),
            (Self::DeleteRecord(library, id), _) => library.delete(&id).map_err(|e| ("delete the file", e)),
            (Self::ListSessions(reply), store) => {
                let listed = match store.map(SessionStore::list) {
                    Some(Ok((sessions, errors))) => {
                        for e in &errors {
                            log::error(format!("skipping unreadable story: {e}"));
                        }
                        (sessions, errors.len())
                    }
                    Some(Err(e)) => {
                        log::error(format!("cannot list stories: {e}"));
                        (Vec::new(), 1)
                    }
                    None => (Vec::new(), 0),
                };
                // The receiver only disappears if the app is exiting.
                let _ = reply.send(listed);
                Ok(())
            }
        };
        if let Err((what, e)) = result {
            log::error(format!("could not {what}: {e}"));
            report(format!("Could not {what}: {e}"));
        }
    }
}

/// Owns the session store on a background thread and performs every file
/// write in the order it was queued, so the UI never waits on the disk.
/// Dropping it finishes the queue first, so nothing is lost on exit.
struct Writer {
    queue: Option<mpsc::Sender<Job>>,
    thread: Option<JoinHandle<()>>,
    /// Used on the calling thread if the writer thread could not start.
    inline: Option<SessionStore>,
    /// Told about every write that fails.
    report: Report,
}

impl Writer {
    /// Starts the thread for sessions in `dir` (`None`: not saved), telling
    /// `report` about every write that fails.
    fn start(dir: Option<PathBuf>, report: impl Fn(String) + Send + Sync + 'static) -> Self {
        let (queue, jobs) = mpsc::channel::<Job>();
        let report: Report = Arc::new(report);
        let (fallback, theirs) = (dir.clone(), Arc::clone(&report));
        let spawned = std::thread::Builder::new().name("openrp-writer".into()).spawn(move || {
            let mut store = dir.map(SessionStore::at);
            for job in jobs {
                job.run(store.as_mut(), &*theirs);
            }
        });
        match spawned {
            Ok(thread) => Self { queue: Some(queue), thread: Some(thread), inline: None, report },
            Err(e) => {
                log::error(format!("cannot start the writer thread, saving on the UI thread: {e}"));
                Self { queue: None, thread: None, inline: fallback.map(SessionStore::at), report }
            }
        }
    }

    /// Queues `job`.
    fn send(&mut self, job: Job) {
        let job = match &self.queue {
            Some(queue) => match queue.send(job) {
                Ok(()) => return,
                // The thread is gone; still don't lose the write.
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        job.run(self.inline.as_mut(), &*self.report);
    }

    /// The saved sessions, newest first, and how many files could not be
    /// read. Waits for the writes queued before it, which only happens when
    /// the chat screen opens.
    fn list_sessions(&mut self) -> (Vec<SessionSummary>, usize) {
        let (reply, sessions) = mpsc::channel();
        self.send(Job::ListSessions(reply));
        sessions.recv().unwrap_or_default()
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // Closing the queue ends the thread once everything is written.
        self.queue = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The records of a library listing and how many files could not be read;
/// those are logged and skipped.
fn report<T>(listing: Result<(Vec<T>, Vec<Error>), Error>, what: &str) -> (Vec<T>, usize) {
    match listing {
        Ok((records, errors)) => {
            for e in &errors {
                log::error(format!("skipping unreadable file in {what}: {e}"));
            }
            (records, errors.len())
        }
        Err(e) => {
            log::error(format!("cannot list {what}: {e}"));
            (Vec::new(), 1)
        }
    }
}

/// Largest file read for an import, as for portraits.
const MAX_IMPORT: u64 = 20 << 20;

/// Reads a file the user picked, refusing huge ones.
fn read_file(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let too_big = std::fs::metadata(path).map_err(|e| e.to_string())?.len() > MAX_IMPORT;
    if too_big { Err("the file is larger than 20 MB".to_owned()) } else { std::fs::read(path).map_err(|e| e.to_string()) }
}

/// Asks for character cards and reads each: a PNG card's image becomes
/// the character's portrait, copied in under a fresh name. Runs on a worker.
fn import_cards(portraits: &Portraits) -> WorkerEvent {
    let paths = match platform::pick_files("Import character cards", ("Character cards", &["png", "json"]), true) {
        Ok(paths) => paths,
        Err(e) => return WorkerEvent::CardsImported { characters: Vec::new(), errors: vec![format!("Could not open the file picker: {e}")] },
    };
    let (mut characters, mut errors) = (Vec::new(), Vec::new());
    for path in paths {
        let read = read_file(&path).and_then(|bytes| {
            let mut character = serechat::read_card(&bytes).map_err(|e| e.to_string())?;
            if bytes.starts_with(b"\x89PNG") {
                character.portrait = portraits.add(&bytes).map_err(|e| e.to_string())?;
            }
            Ok(character)
        });
        match read {
            Ok(character) => characters.push(character),
            Err(e) => {
                let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                log::error(format!("could not import {name}: {e}"));
                errors.push(format!("Could not import {name}: {e}"));
            }
        }
    }
    WorkerEvent::CardsImported { characters, errors }
}

/// Asks where to save a file named after `name`, then writes what `bytes`
/// makes there. Runs on a worker; a failure is told as a problem.
fn export(title: &str, name: &str, filter: platform::Filter<'_>, bytes: impl FnOnce() -> Result<Vec<u8>, String>) -> WorkerEvent {
    let written = platform::save_file(title, name, filter)
        .map_err(|e| format!("Could not open the file picker: {e}"))
        .and_then(|path| path.map(|path| bytes().and_then(|b| std::fs::write(&path, b).map_err(|e| e.to_string())).map(|()| path)).transpose());
    match written {
        Ok(path) => WorkerEvent::Exported(path),
        Err(e) => {
            log::error(format!("could not export: {e}"));
            WorkerEvent::Problem { text: format!("Could not export: {e}"), folder: false }
        }
    }
}

/// The OS window decoration style matching `scheme`.
fn window_theme(scheme: Scheme) -> Theme {
    if scheme.is_light() { Theme::Light } else { Theme::Dark }
}

/// Gives the window the app icon for its title bar and taskbar entry.
///
/// Windows loads the sizes `build.rs` embedded, picked for the display's
/// scale; X11 gets the 256px PNG. macOS ignores window icons and Wayland
/// takes them from a `.desktop` file, so both keep the default.
fn set_icon(window: &Window) {
    #[cfg(windows)]
    {
        use winit::platform::windows::{IconExtWindows, WindowExtWindows};
        let scale = window.scale_factor();
        let load = |side: f64| {
            let side = (side * scale).round() as u32;
            winit::window::Icon::from_resource(1, Some(winit::dpi::PhysicalSize::new(side, side))).ok()
        };
        window.set_window_icon(load(16.0));
        window.set_taskbar_icon(load(32.0));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Ok((rgba, w, h)) = image::decode_png(include_bytes!("../assets/icon/256.png"))
        && let Ok(icon) = winit::window::Icon::from_rgba(rgba, w, h)
    {
        window.set_window_icon(Some(icon));
    }
    #[cfg(target_os = "macos")]
    let _ = window;
}

/// Minimal executor for wgpu's initialisation futures.
fn block_on<F: Future>(future: F) -> F::Output {
    struct Unpark(std::thread::Thread);
    impl std::task::Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Arc::new(Unpark(std::thread::current())).into();
    let mut cx = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        std::thread::park();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keys_step_and_clamp() {
        let key = |c: &str| Key::Character(c.into());
        assert_eq!(zoomed(100, &key("=")), Some(110));
        assert_eq!(zoomed(100, &key("+")), Some(110));
        assert_eq!(zoomed(100, &key("-")), Some(90));
        assert_eq!(zoomed(150, &key("0")), Some(100));
        assert_eq!(zoomed(ZOOM.1, &key("+")), Some(ZOOM.1), "clamped");
        assert_eq!(zoomed(ZOOM.0, &key("-")), Some(ZOOM.0), "clamped");
        assert_eq!(zoomed(100, &key("k")), None);
        assert_eq!(zoomed(100, &Key::Named(winit::keyboard::NamedKey::Enter)), None);
    }

    #[test]
    fn window_state_survives_odd_input() {
        assert_eq!(window_state(None), (WINDOW_SIZE, false));
        assert_eq!(window_state(Some("1440x900")), ((1440, 900), false));
        assert_eq!(window_state(Some("1440x900 maximized")), ((1440, 900), true));
        assert_eq!(window_state(Some("10x99999")), ((MIN_WINDOW_SIZE.0, 16_384), false), "clamped");
        assert_eq!(window_state(Some("NaNxinf maximized")), (WINDOW_SIZE, true));
        assert_eq!(window_state(Some("")), (WINDOW_SIZE, false));
    }

    #[test]
    fn custom_provider_needs_an_address() {
        let config = Config { provider: Some(CUSTOM.into()), ..Config::default() };
        assert!(client_for(&config, None).is_none() && !provider(&config, false).custom, "nothing to talk to");
        let config = Config { base_url: Some("http://localhost:11434/v1".into()), ..config };
        assert!(client_for(&config, None).is_some() && provider(&config, false).custom, "a local server needs no key");
        let config = Config { provider: None, ..config };
        let serechat = Client::signed_in("refresh".into(), |_| {});
        assert!(client_for(&config, Some(&serechat)).is_some() && !provider(&config, true).custom);
        assert!(client_for(&config, None).is_none(), "signed out of SereChat");
    }

    #[test]
    fn writer_keeps_order_and_flushes_on_drop() {
        let dir = std::env::temp_dir().join(format!("openrp-writer-{}", std::process::id())).join("sessions");
        let session = |id: &str, updated| Session { id: id.into(), title: id.into(), updated, ..Session::default() };
        let failures = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&failures);
        let mut writer = Writer::start(Some(dir.clone()), move |text| seen.lock().unwrap().push(text));
        writer.send(Job::SaveSession(session("a", 1)));
        writer.send(Job::SaveSession(session("b", 2)));
        writer.send(Job::DeleteSession("a".into()));
        // Listing waits for the writes queued before it.
        let ids: Vec<String> = writer.list_sessions().0.into_iter().map(|s| s.id).collect();
        assert_eq!(ids, ["b"]);
        // A write that fails is reported.
        writer.send(Job::SaveSession(session("../bad", 4)));
        assert_eq!(writer.list_sessions().1, 0);
        assert!(failures.lock().unwrap()[0].starts_with("Could not save the story"));
        writer.send(Job::SaveSession(session("c", 3)));
        drop(writer);
        assert!(dir.join("c.json").exists(), "dropping the writer finishes its queue");
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
