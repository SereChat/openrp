//! Client library for [SereChat](https://serechat.com), the API behind OpenRP.
//!
//! * [`Client`]: model listing and streaming responses with function tools,
//!   from SereChat or any OpenAI-compatible provider.
//! * [`SignIn`]: SereChat's browser sign-in (OAuth 2.1 with PKCE), giving a
//!   [`Client`] that refreshes its tokens itself.
//! * [`Config`]: the user's settings in `~/.openrp/config.toml`.
//! * [`SessionStore`]: saved conversations in `~/.openrp/sessions/`.
//! * [`Library`]: [`World`]s, [`Character`]s and [`Persona`]s in
//!   `~/.openrp/worlds/`, `~/.openrp/characters/` and `~/.openrp/personas/`, and character cards and lorebooks to share
//!   them with other roleplay apps.
//!
//! All network calls are blocking; run them off the UI thread.

mod card;
mod client;
mod completions;
mod config;
mod error;
mod library;
mod oauth;
mod responses;
mod session;
mod sse;

pub use card::{card_json, embed_card, macros, read_card, read_lorebook};
pub use client::{BASE_URL, Client, Model};
pub use config::{Config, lock_instance};
pub use error::{Error, Result};
pub use library::{Character, Library, LoreEntry, Persona, Portraits, World};
pub use oauth::SignIn;
pub use responses::{Completion, InputItem, ResponseRequest, Role, StreamEvent, ToolCall, ToolChoice, ToolSpec, Usage};
pub use session::{CastMember, Player, SearchHit, Session, SessionStore, SessionSummary, StoredMessage, StoryChange, ToolResult, new_id, rename, unix_now};
