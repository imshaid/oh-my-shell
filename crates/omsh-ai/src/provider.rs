//! The interface the rest of the shell uses to talk to a model (blueprint §13.3).
//!
//! Everything above this module (the plan flow of S21, the explain features of S24) sees a
//! [`Provider`] and nothing about HTTP, genai or which company is behind it.

use std::pin::Pin;

use futures::future::BoxFuture;
use futures::{Stream, StreamExt};
use omsh_config::Provider as Kind;

use crate::error::ProviderError;
use crate::retry::{Policy, retry};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    pub text: String,
}

impl Message {
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
        }
    }

    #[must_use]
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
        }
    }
}

/// One request to a model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatRequest {
    /// Instructions for the model (the system prompt).
    pub system: Option<String>,
    /// The conversation so far; the last message is the user's.
    pub messages: Vec<Message>,
    /// The most the model may write. Every provider needs a limit; Anthropic requires one.
    pub max_tokens: u32,
    /// Ask for a JSON object (the plan schema of §13.4). The prompt must say so too.
    pub json: bool,
}

impl ChatRequest {
    /// A request with one user message.
    #[must_use]
    pub fn ask(text: impl Into<String>, max_tokens: u32) -> Self {
        Self {
            system: None,
            messages: vec![Message::user(text)],
            max_tokens,
            json: false,
        }
    }
}

/// What a model sends back, piece by piece.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatEvent {
    /// More of the answer.
    Text(String),
    /// The answer is complete.
    Done,
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<ChatEvent, ProviderError>> + Send>>;

/// A model reachable over some protocol.
pub trait Provider: Send + Sync {
    /// Which provider this is.
    fn kind(&self) -> Kind;

    /// Sends the request. The stream yields [`ChatEvent::Text`] as the answer arrives and
    /// ends with [`ChatEvent::Done`]; an error ends it. Nothing is sent until the stream is
    /// polled.
    fn chat(&self, request: ChatRequest) -> EventStream;

    /// The model names the server offers, to help choose one in the setup wizard.
    fn models(&self) -> BoxFuture<'_, Result<Vec<String>, ProviderError>>;
}

/// Sends the request and returns the whole answer. A transient failure is tried again
/// according to `policy`; each try starts a new request, and nothing of a failed try is kept.
///
/// # Errors
///
/// The error of the last try.
pub async fn collect_text(
    provider: &dyn Provider,
    request: &ChatRequest,
    policy: &Policy,
) -> Result<String, ProviderError> {
    retry(policy, || async {
        let mut stream = provider.chat(request.clone());
        let mut answer = String::new();
        while let Some(event) = stream.next().await {
            match event? {
                ChatEvent::Text(piece) => answer.push_str(&piece),
                ChatEvent::Done => break,
            }
        }
        Ok(answer)
    })
    .await
}

/// The smallest request that proves a key and a model work: one short question, a few tokens
/// of answer. Used by `omsh ai setup` and `omsh ai test`. Success means the server accepted
/// the key and knows the model; what the model said does not matter.
///
/// # Errors
///
/// The error of the last try.
pub async fn verify(provider: &dyn Provider, policy: &Policy) -> Result<String, ProviderError> {
    let request = ChatRequest::ask("Reply with the single word OK.", 32);
    collect_text(provider, &request, policy).await
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use omsh_config::Provider as Kind;

    use super::*;
    use crate::error::ErrorKind;
    use crate::fake::FakeProvider;
    use crate::runtime::block_on;

    fn fast_retries() -> Policy {
        Policy {
            attempts: 3,
            pauses: vec![Duration::ZERO],
        }
    }

    fn run<T>(future: impl std::future::Future<Output = T>) -> T {
        block_on(future, &|| false).unwrap()
    }

    #[test]
    fn the_pieces_of_an_answer_are_joined() {
        let fake = FakeProvider::new(Kind::Ollama).says(&["Hel", "lo ", "there"]);
        let text = run(collect_text(
            &fake,
            &ChatRequest::ask("hi", 8),
            &Policy::once(),
        ))
        .unwrap();
        assert_eq!(text, "Hello there");
    }

    #[test]
    fn the_request_reaches_the_provider_unchanged() {
        let fake = FakeProvider::new(Kind::Ollama).says(&["x"]);
        let mut request = ChatRequest::ask("question", 99);
        request.system = Some("be brief".to_owned());
        request.json = true;
        run(collect_text(&fake, &request, &Policy::once())).unwrap();
        assert_eq!(fake.requests(), [request]);
    }

    #[test]
    fn a_failure_is_returned_with_its_kind() {
        let fake =
            FakeProvider::new(Kind::OpenAi).fails(ProviderError::from_status(401, "Unauthorized"));
        let error = run(collect_text(
            &fake,
            &ChatRequest::ask("hi", 8),
            &fast_retries(),
        ))
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Auth);
        assert_eq!(fake.requests().len(), 1, "a wrong key is not tried again");
    }

    #[test]
    fn a_transient_failure_is_tried_again_with_a_new_request() {
        let fake = FakeProvider::new(Kind::Anthropic)
            .fails(ProviderError::from_status(503, "Service Unavailable"))
            .fails(ProviderError::from_status(429, "Too Many Requests"))
            .says(&["fine"]);
        let text = run(collect_text(
            &fake,
            &ChatRequest::ask("hi", 8),
            &fast_retries(),
        ))
        .unwrap();
        assert_eq!(text, "fine");
        assert_eq!(fake.requests().len(), 3);
    }

    #[test]
    fn a_failure_in_the_middle_of_an_answer_keeps_nothing_of_it() {
        // The first try gives half an answer and then breaks; the second is complete. The
        // result must not start with the half.
        let fake = FakeProvider::new(Kind::Gemini)
            .partial_then_fail(&["half an ans"], ProviderError::new(ErrorKind::Server, "x"))
            .says(&["whole answer"]);
        let text = run(collect_text(
            &fake,
            &ChatRequest::ask("hi", 8),
            &fast_retries(),
        ))
        .unwrap();
        assert_eq!(text, "whole answer");
    }

    #[test]
    fn verify_succeeds_on_any_answer_and_asks_for_little() {
        let fake = FakeProvider::new(Kind::Ollama).says(&[""]);
        run(verify(&fake, &Policy::once())).unwrap();
        let request = &fake.requests()[0];
        assert!(request.max_tokens <= 64, "{}", request.max_tokens);
        assert_eq!(request.messages.len(), 1);
        assert!(!request.json);
    }

    #[test]
    fn a_fake_with_nothing_left_to_say_says_so_and_does_not_hang() {
        let fake = FakeProvider::new(Kind::Ollama);
        let error = run(collect_text(
            &fake,
            &ChatRequest::ask("hi", 8),
            &Policy::once(),
        ))
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal);
    }

    #[test]
    fn the_fake_lists_the_models_it_was_given() {
        let fake = FakeProvider::new(Kind::Ollama).offering(&["a", "b"]);
        assert_eq!(run(fake.models()).unwrap(), ["a", "b"]);
    }
}
