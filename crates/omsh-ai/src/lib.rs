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
//! This crate depends on `omsh-config` only.

mod destination;
mod error;
mod genai_provider;
mod keys;
mod provider;
mod retry;
mod secret;

pub mod fake;
pub mod runtime;

pub use destination::{key_may_go_to, server_of};
pub use error::{ErrorKind, ProviderError};
pub use genai_provider::{GenaiProvider, Settings, map_error};
pub use keys::{
    CredentialsFile, KeyError, KeyStore, Keys, SecretService, Stored, parse_credentials,
};
pub use provider::{
    ChatEvent, ChatRequest, EventStream, Message, Provider, Role, collect_text, verify,
};
pub use retry::{Policy, retry};
pub use secret::Secret;
