//! Stand-ins for tests: a provider that answers from a script and a key store in memory.
//! Also used by the tests of the crates above this one.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt, stream};
use omsh_config::Provider as Kind;

use crate::error::ProviderError;
use crate::keys::{KeyError, KeyStore};
use crate::models::Limit;
use crate::provider::{ChatEvent, ChatRequest, EventStream, Provider, Usage};
use crate::secret::Secret;

/// What the fake does for one `chat` call.
enum Script {
    /// These events, in order; an `Err` ends the stream. The usage is what the server "reports"
    /// for this answer, if anything.
    Events(Vec<Result<ChatEvent, ProviderError>>, Option<Usage>),
}

/// A provider that answers each `chat` call with the next scripted answer, and remembers the
/// requests it got.
pub struct FakeProvider {
    kind: Kind,
    script: Mutex<VecDeque<Script>>,
    seen: Mutex<Vec<ChatRequest>>,
    models: Vec<String>,
    /// What `take_usage` gives, set when a request starts.
    usage: Mutex<Option<Usage>>,
    /// What `context_limit` finds, if the fake "knows" the window.
    limit: Option<Limit>,
    /// What `chat_models` gives (the text models), if the fake has such a list.
    chat_models: Option<Vec<String>>,
}

impl FakeProvider {
    #[must_use]
    pub fn new(kind: Kind) -> Self {
        Self {
            kind,
            script: Mutex::default(),
            seen: Mutex::default(),
            models: Vec::new(),
            usage: Mutex::default(),
            limit: None,
            chat_models: None,
        }
    }

    /// Adds an answer: the text arrives in the given pieces, then `Done`.
    #[must_use]
    pub fn says(self, pieces: &[&str]) -> Self {
        let mut events: Vec<ChatEvent> = pieces
            .iter()
            .map(|piece| ChatEvent::Text((*piece).to_owned()))
            .collect();
        events.push(ChatEvent::Done);
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(Script::Events(events.into_iter().map(Ok).collect(), None));
        self
    }

    /// The tokens the server "reports" for the answer added last. Without a call an answer
    /// reports none, like a server that sends no counts.
    #[must_use]
    pub fn reporting(self, usage: Usage) -> Self {
        let mut script = self.script.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(Script::Events(_, slot)) = script.back_mut() {
            *slot = Some(usage);
        }
        drop(script);
        self
    }

    /// Adds a failure.
    #[must_use]
    pub fn fails(self, error: ProviderError) -> Self {
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(Script::Events(vec![Err(error)], None));
        self
    }

    /// Adds an answer that sends some text and then fails, as a connection that breaks in the
    /// middle of a reply does.
    #[must_use]
    pub fn partial_then_fail(self, pieces: &[&str], error: ProviderError) -> Self {
        let mut events: Vec<Result<ChatEvent, ProviderError>> = pieces
            .iter()
            .map(|piece| Ok(ChatEvent::Text((*piece).to_owned())))
            .collect();
        events.push(Err(error));
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(Script::Events(events, None));
        self
    }

    /// The model names `models()` returns.
    #[must_use]
    pub fn offering(mut self, models: &[&str]) -> Self {
        self.models = models.iter().map(|m| (*m).to_owned()).collect();
        self
    }

    /// The context window `context_limit` finds.
    #[must_use]
    pub const fn with_limit(mut self, limit: Limit) -> Self {
        self.limit = Some(limit);
        self
    }

    /// The list `chat_models` gives: the models that chat in text (Gemini's filter).
    #[must_use]
    pub fn with_chat_models(mut self, models: &[&str]) -> Self {
        self.chat_models = Some(models.iter().map(|m| (*m).to_owned()).collect());
        self
    }

    /// The requests received so far.
    #[must_use]
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Provider for FakeProvider {
    fn kind(&self) -> Kind {
        self.kind
    }

    fn chat(&self, request: ChatRequest) -> EventStream {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        let next = self
            .script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        match next {
            Some(Script::Events(events, usage)) => {
                // A new request forgets the number of the one before.
                *self.usage.lock().unwrap_or_else(PoisonError::into_inner) = usage;
                stream::iter(events).boxed()
            }
            None => {
                *self.usage.lock().unwrap_or_else(PoisonError::into_inner) = None;
                stream::once(async {
                    Err(ProviderError::new(
                        crate::error::ErrorKind::Internal,
                        "the fake has no more answers",
                    ))
                })
                .boxed()
            }
        }
    }

    fn take_usage(&self) -> Option<Usage> {
        self.usage
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn models(&self) -> BoxFuture<'_, Result<Vec<String>, ProviderError>> {
        let models = self.models.clone();
        async move { Ok(models) }.boxed()
    }

    fn context_limit(&self) -> BoxFuture<'_, Option<Limit>> {
        let limit = self.limit;
        async move { limit }.boxed()
    }

    fn chat_models(&self) -> BoxFuture<'_, Option<Vec<String>>> {
        let models = self.chat_models.clone();
        async move { models }.boxed()
    }
}

/// A key store that keeps keys in memory. Clones share the same keys.
#[derive(Clone, Default)]
pub struct MemoryStore {
    name: String,
    keys: Arc<Mutex<HashMap<&'static str, String>>>,
    failure: Arc<Mutex<Option<String>>>,
}

impl MemoryStore {
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Self::default()
        }
    }

    /// Makes every later call fail with this text.
    pub fn fail_with(&self, text: &str) {
        *self.failure.lock().unwrap_or_else(PoisonError::into_inner) = Some(text.to_owned());
    }

    fn check(&self) -> Result<(), KeyError> {
        match &*self.failure.lock().unwrap_or_else(PoisonError::into_inner) {
            Some(text) => Err(KeyError::Failed(text.clone())),
            None => Ok(()),
        }
    }
}

impl KeyStore for MemoryStore {
    fn place(&self) -> String {
        self.name.clone()
    }

    fn get(&self, provider: Kind) -> Result<Option<Secret>, KeyError> {
        self.check()?;
        Ok(self
            .keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(provider.as_str())
            .and_then(|text| Secret::new(text.clone())))
    }

    fn set(&self, provider: Kind, key: &Secret) -> Result<(), KeyError> {
        self.check()?;
        self.keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(provider.as_str(), key.expose().to_owned());
        Ok(())
    }

    fn delete(&self, provider: Kind) -> Result<bool, KeyError> {
        self.check()?;
        Ok(self
            .keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(provider.as_str())
            .is_some())
    }
}
