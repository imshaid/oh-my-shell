//! What can go wrong with a request, in the words a person needs.
//!
//! The HTTP client's own error text is never shown: some of it holds the request or the
//! response body, and a reqwest error holds the address, which for Gemini carries the key.
//! [`ProviderError`] has a kind and a short detail made here (an HTTP status, never a body).

use std::fmt;

use omsh_config::Provider;

/// The kinds of failure that are handled differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// The server cannot be reached (refused, no route, DNS).
    Offline,
    /// The server did not answer in time.
    Timeout,
    /// The key is missing, wrong or not allowed (HTTP 401, 403).
    Auth,
    /// Too many requests (HTTP 429).
    RateLimited,
    /// The server failed (HTTP 5xx).
    Server,
    /// The model or the address does not exist (HTTP 404).
    NotFound,
    /// The server refused the request itself (other HTTP 4xx).
    BadRequest,
    /// The answer was not what the protocol says.
    BadResponse,
    /// The key would have gone to a plain-HTTP address that is not this machine.
    KeyNotAllowed,
    /// No key is stored for this provider.
    NoKey,
    /// The user pressed Ctrl+C.
    Cancelled,
    /// Something inside the shell (the runtime could not start, ...).
    Internal,
}

/// A failed request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderError {
    pub kind: ErrorKind,
    /// A few words about the cause: "HTTP 401 Unauthorized", "connection refused". Never a
    /// response body, never an address, never a key.
    pub detail: String,
}

impl ProviderError {
    #[must_use]
    pub fn new(kind: ErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }

    /// Whether trying again soon may work. A refused connection is not retried: it answers at
    /// once, and the useful thing is to say so. A wrong key, a missing model and a Ctrl+C
    /// never succeed on a second try.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(
            self.kind,
            ErrorKind::Timeout | ErrorKind::RateLimited | ErrorKind::Server
        )
    }

    /// The one line shown to the user (blueprint §13.1: a clear line, no crash, and a pointer
    /// to a local model when the network is the problem).
    #[must_use]
    pub fn one_line(&self, provider: Provider) -> String {
        let name = provider.label();
        let detail = &self.detail;
        match self.kind {
            ErrorKind::Offline if provider == Provider::Ollama => format!(
                "cannot reach Ollama ({detail}); is it running? start it with `ollama serve`"
            ),
            ErrorKind::Offline => format!(
                "cannot reach {name} ({detail}); check the network, or use a local model: \
                 run `omsh ai setup` and choose Ollama"
            ),
            ErrorKind::Timeout => format!("{name} did not answer in time ({detail})"),
            ErrorKind::Auth => {
                format!("{name} did not accept the key ({detail}); run `omsh ai setup` again")
            }
            ErrorKind::RateLimited => {
                format!("{name} says there are too many requests ({detail}); wait a moment")
            }
            ErrorKind::Server => format!("{name} had an error on its side ({detail})"),
            ErrorKind::NotFound => format!(
                "{name} does not know this model or address ({detail}); check `omsh ai status`"
            ),
            ErrorKind::BadRequest => format!("{name} refused the request ({detail})"),
            ErrorKind::BadResponse => {
                format!("the answer from {name} could not be read ({detail})")
            }
            ErrorKind::KeyNotAllowed => {
                format!("the key is not sent to {detail}: it is not https and not on this machine")
            }
            ErrorKind::NoKey => format!("no key is stored for {name}; run `omsh ai setup`"),
            ErrorKind::Cancelled => "cancelled".to_owned(),
            ErrorKind::Internal => format!("internal error ({detail})"),
        }
    }

    /// From an HTTP status.
    #[must_use]
    pub fn from_status(status: u16, reason: &str) -> Self {
        let kind = match status {
            401 | 403 => ErrorKind::Auth,
            404 => ErrorKind::NotFound,
            408 | 504 => ErrorKind::Timeout,
            429 => ErrorKind::RateLimited,
            500..=599 => ErrorKind::Server,
            _ => ErrorKind::BadRequest,
        };
        Self::new(
            kind,
            format!("HTTP {status} {reason}").trim_end().to_owned(),
        )
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.detail)
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_become_kinds() {
        let kind = |status| ProviderError::from_status(status, "x").kind;
        assert_eq!(kind(401), ErrorKind::Auth);
        assert_eq!(kind(403), ErrorKind::Auth);
        assert_eq!(kind(404), ErrorKind::NotFound);
        assert_eq!(kind(408), ErrorKind::Timeout);
        assert_eq!(kind(504), ErrorKind::Timeout);
        assert_eq!(kind(429), ErrorKind::RateLimited);
        assert_eq!(kind(500), ErrorKind::Server);
        assert_eq!(kind(503), ErrorKind::Server);
        assert_eq!(kind(400), ErrorKind::BadRequest);
        assert_eq!(kind(422), ErrorKind::BadRequest);
    }

    #[test]
    fn only_timeouts_rate_limits_and_server_errors_are_retried() {
        let transient = |kind| ProviderError::new(kind, "").is_transient();
        assert!(transient(ErrorKind::Timeout));
        assert!(transient(ErrorKind::RateLimited));
        assert!(transient(ErrorKind::Server));
        for kind in [
            ErrorKind::Offline,
            ErrorKind::Auth,
            ErrorKind::NotFound,
            ErrorKind::BadRequest,
            ErrorKind::BadResponse,
            ErrorKind::KeyNotAllowed,
            ErrorKind::NoKey,
            ErrorKind::Cancelled,
            ErrorKind::Internal,
        ] {
            assert!(!transient(kind), "{kind:?}");
        }
    }

    #[test]
    fn offline_points_to_a_local_model_but_not_for_ollama_itself() {
        let error = ProviderError::new(ErrorKind::Offline, "connection refused");
        assert!(error.one_line(Provider::Anthropic).contains("Ollama"));
        let ollama = error.one_line(Provider::Ollama);
        assert!(ollama.contains("ollama serve"), "{ollama}");
        assert!(!ollama.contains("omsh ai setup"), "{ollama}");
    }

    #[test]
    fn every_kind_has_a_single_line() {
        for kind in [
            ErrorKind::Offline,
            ErrorKind::Timeout,
            ErrorKind::Auth,
            ErrorKind::RateLimited,
            ErrorKind::Server,
            ErrorKind::NotFound,
            ErrorKind::BadRequest,
            ErrorKind::BadResponse,
            ErrorKind::KeyNotAllowed,
            ErrorKind::NoKey,
            ErrorKind::Cancelled,
            ErrorKind::Internal,
        ] {
            for provider in Provider::ALL {
                let line = ProviderError::new(kind, "detail").one_line(provider);
                assert!(!line.is_empty() && !line.contains('\n'), "{kind:?}: {line}");
            }
        }
    }
}
