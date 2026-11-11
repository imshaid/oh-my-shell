//! The AI layer: the provider interface, the providers, key storage, and the small runtime
//! that makes requests (blueprint §13).
//!
//! **Nothing here runs until the user asks.** The crate has no static initializers that start
//! a thread or open a socket: the tokio runtime, the TLS setup, the HTTP client and the
//! keyring connection are all created by the first call that needs them (`omsh ai ...`, and
//! from S21 a `?` request). `omsh-tests/tests/ai_lazy.rs` shows it from outside.
//!
//! - [`Provider`] is what the rest of the shell talks to; [`GenaiProvider`] implements it for
//!   Ollama, Anthropic, OpenAI, Gemini and any OpenAI-compatible server.
//! - [`Keys`] keeps API keys in the system keyring or, failing that, a 0600 file.
//! - [`key_may_go_to`] decides where a key may be sent: `https://`, or this machine.
//! - [`runtime::block_on`] runs a request from the shell's ordinary (synchronous) code and
//!   stops it on Ctrl+C.
//!
//! This crate depends on `omsh-config` and `omsh-secrets` only.
//!
//! - [`PlanContext`] is what may be sent besides the request: it masks every text with the
//!   rules of `omsh-secrets` and applies the `[ai.context]` switches.

mod context;
mod conversation;
mod destination;
mod error;
mod genai_provider;
mod keys;
mod plan;
mod prompt;
mod provider;
mod retry;
mod secret;

pub mod fake;
pub mod runtime;

pub use context::{Gathered, GitState, PlanContext, RECENT_COMMANDS, RecentCommand};
pub use conversation::{
    Conversation, MAX_FOLLOW_UP_CHARS, SOFT_TURN_LIMIT, follow_up_hides_secret,
};
pub use destination::{key_may_go_to, server_of};
pub use error::{ErrorKind, ProviderError};
pub use genai_provider::{GenaiProvider, Settings, map_error};
pub use keys::{
    CredentialsFile, KeyError, KeyStore, Keys, SecretService, Stored, parse_credentials,
};
pub use plan::{
    Lang, MAX_COMMAND_CHARS, MAX_STEPS, ModelRisk, Plan, PlanError, Step, placeholder_in,
};
pub use prompt::{PLAN_MAX_TOKENS, PlanRequest, hides_secret, plan_request, retry_request};
pub use provider::{
    ChatEvent, ChatRequest, EventStream, Message, Provider, Role, collect_text,
    collect_text_counting, verify,
};
pub use retry::{Policy, retry};
pub use secret::Secret;
