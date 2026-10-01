//! Client library for [SereChat](https://serechat.com), the API behind OpenRP.
//!
//! * [`Client`]: device-code sign-in, model listing and streaming responses
//!   with function tools.
//! * [`Config`]: the user's settings in `~/.openrp/config.toml`.
//! * [`SessionStore`]: saved conversations in `~/.openrp/sessions/`.
//! * [`Library`]: [`World`]s and [`Character`]s in `~/.openrp/worlds/` and
//!   `~/.openrp/characters/`.
//!
//! All network calls are blocking; run them off the UI thread.

mod client;
mod config;
mod error;
mod library;
mod responses;
mod session;
mod sse;

pub use client::{AccessToken, BASE_URL, Client, Model};
pub use config::Config;
pub use error::{Error, Result};
pub use library::{Character, Library, Portraits, World};
pub use responses::{Completion, InputItem, ResponseRequest, Role, StreamEvent, ToolCall, ToolChoice, ToolSpec, Usage};
pub use session::{CastMember, Player, SearchHit, Session, SessionStore, SessionSummary, StoredMessage, ToolResult, new_id, unix_now};
