//! The five providers of blueprint §13.3, through the `genai` crate.
//!
//! genai speaks each company's protocol. This module decides what it is given: the address,
//! the key (only from the credential store, never from an environment variable such as
//! `OPENAI_API_KEY`, so there is one place a key can come from), the timeouts; and it turns
//! genai's errors into [`ProviderError`]s with short fixed wording.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use futures::stream;
use futures::{FutureExt, StreamExt};
use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest as GenaiRequest, ChatResponseFormat, ChatStreamEvent,
};
use genai::resolver::{AuthData, Endpoint, ProviderConfig, ServiceTargetResolver};
use genai::{Client, ModelIden, ServiceTarget, WebConfig};
use omsh_config::Provider as Kind;
use zeroize::Zeroizing;

use crate::destination::{key_may_go_to, server_of};
use crate::error::{ErrorKind, ProviderError};
use crate::provider::{ChatEvent, ChatRequest, EventStream, Provider, Role};
use crate::runtime::prepare_tls;
use crate::secret::Secret;

/// How long to wait for a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a whole request may take, answer included. A model on a slow computer can be
/// slow, so this is generous.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// What to talk to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub kind: Kind,
    pub model: String,
    /// `[ai] base_url`. Needed for an OpenAI-compatible server; optional for the others.
    pub base_url: Option<String>,
}

impl Settings {
    /// The address requests go to, with a slash at the end (genai appends the path). `None`
    /// for an OpenAI-compatible server that has no address yet.
    #[must_use]
    pub fn endpoint(&self) -> Option<String> {
        let own = self
            .base_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty());
        let url = match (own, self.kind) {
            (Some(url), _) => url,
            (None, Kind::Ollama) => "http://localhost:11434",
            (None, Kind::Anthropic) => "https://api.anthropic.com/v1",
            (None, Kind::OpenAi) => "https://api.openai.com/v1",
            (None, Kind::Gemini) => "https://generativelanguage.googleapis.com/v1beta",
            (None, Kind::OpenAiCompatible) => return None,
        };
        Some(format!("{}/", url.trim_end_matches('/')))
    }
}

/// A provider made with genai.
pub struct GenaiProvider {
    client: Client,
    settings: Settings,
    adapter: AdapterKind,
    endpoint: Arc<str>,
    key: Option<Arc<Zeroizing<String>>>,
}

const fn adapter_for(kind: Kind) -> AdapterKind {
    match kind {
        Kind::Ollama => AdapterKind::Ollama,
        Kind::Anthropic => AdapterKind::Anthropic,
        Kind::Gemini => AdapterKind::Gemini,
        Kind::OpenAi | Kind::OpenAiCompatible => AdapterKind::OpenAI,
    }
}

/// What a server that wants no key is sent in the key's place. Ollama and many local servers
/// ignore the header; some client code insists on one.
const NO_KEY: &str = "none";

impl GenaiProvider {
    /// Builds the client. Nothing goes over the network yet.
    ///
    /// `key` is used only when the provider takes one (everything but Ollama).
    ///
    /// # Errors
    ///
    /// [`ErrorKind::KeyNotAllowed`] when a key would go to a plain-HTTP address that is not
    /// this machine; [`ErrorKind::BadRequest`] when an OpenAI-compatible server has no
    /// address; [`ErrorKind::Internal`] when the client cannot be built.
    pub fn new(settings: &Settings, key: Option<&Secret>) -> Result<Self, ProviderError> {
        let Some(endpoint) = settings.endpoint() else {
            return Err(ProviderError::new(
                ErrorKind::BadRequest,
                "an OpenAI-compatible server needs [ai] base_url",
            ));
        };
        let key = key.filter(|_| settings.kind.takes_a_key());
        if key.is_some() && !key_may_go_to(&endpoint) {
            return Err(ProviderError::new(
                ErrorKind::KeyNotAllowed,
                server_of(&endpoint),
            ));
        }
        prepare_tls();

        let adapter = adapter_for(settings.kind);
        let endpoint: Arc<str> = Arc::from(endpoint);
        let key = key.map(|key| Arc::new(Zeroizing::new(key.expose().to_owned())));

        let resolver = {
            let endpoint = Arc::clone(&endpoint);
            let key = key.clone();
            ServiceTargetResolver::from_resolver_fn(
                move |target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                    let auth = AuthData::from_single(
                        key.as_ref().map_or(NO_KEY, |key| key.as_str()).to_owned(),
                    );
                    Ok(ServiceTarget {
                        endpoint: Endpoint::from_owned(Arc::clone(&endpoint)),
                        auth,
                        model: ModelIden::new(adapter, target.model.model_name),
                    })
                },
            )
        };
        let web = WebConfig::default()
            .with_connect_timeout(CONNECT_TIMEOUT)
            .with_timeout(REQUEST_TIMEOUT);
        let client = Client::builder()
            .with_web_config(web)
            .with_service_target_resolver(resolver)
            .build()
            .map_err(|error| ProviderError::new(ErrorKind::Internal, short(&error)))?;

        Ok(Self {
            client,
            settings: settings.clone(),
            adapter,
            endpoint,
            key,
        })
    }
}

impl Provider for GenaiProvider {
    fn kind(&self) -> Kind {
        self.settings.kind
    }

    fn chat(&self, request: ChatRequest) -> EventStream {
        let client = self.client.clone();
        let model = self.settings.model.clone();
        let (request, options) = to_genai(request);
        let events = stream::once(async move {
            match client
                .exec_chat_stream(model.as_str(), request, Some(&options))
                .await
            {
                Ok(response) => response.stream.boxed(),
                Err(error) => stream::once(async move { Err(error) }).boxed(),
            }
        })
        .flatten();
        let mut finished = false;
        events
            .filter_map(move |item| {
                let out = match item {
                    Ok(ChatStreamEvent::Chunk(chunk)) => Some(Ok(ChatEvent::Text(chunk.content))),
                    Ok(ChatStreamEvent::End(_)) if !finished => {
                        finished = true;
                        Some(Ok(ChatEvent::Done))
                    }
                    Ok(_) => None,
                    Err(error) => Some(Err(map_error(&error))),
                };
                std::future::ready(out)
            })
            .boxed()
    }

    fn models(&self) -> BoxFuture<'_, Result<Vec<String>, ProviderError>> {
        let client = self.client.clone();
        let config = {
            let auth = AuthData::from_single(
                self.key
                    .as_ref()
                    .map_or(NO_KEY, |key| key.as_str())
                    .to_owned(),
            );
            ProviderConfig::from_endpoint(Endpoint::from_owned(Arc::clone(&self.endpoint)))
                .with_auth(auth)
        };
        let adapter = self.adapter;
        async move {
            client
                .all_model_names(adapter, config)
                .await
                .map_err(|error| map_error(&error))
        }
        .boxed()
    }
}

type Pieces = (GenaiRequest, ChatOptions);

fn to_genai(request: ChatRequest) -> Pieces {
    let mut out = GenaiRequest::default();
    if let Some(system) = request.system {
        out = out.with_system(system);
    }
    for message in request.messages {
        out = out.append_message(match message.role {
            Role::User => ChatMessage::user(message.text),
            Role::Assistant => ChatMessage::assistant(message.text),
        });
    }
    let mut options = ChatOptions::default().with_max_tokens(request.max_tokens);
    if request.json {
        options = options.with_response_format(ChatResponseFormat::JsonMode);
    }
    (out, options)
}

/// Short wording for a genai error. genai's own text can hold the request, the response body
/// and (through reqwest) the address with the key in it, so it is never used.
fn short(_error: &genai::Error) -> &'static str {
    "the client could not be built"
}

/// Turns any genai error into ours. Only fixed words and an HTTP status come out.
#[must_use]
pub fn map_error(error: &genai::Error) -> ProviderError {
    use genai::Error as E;
    if let Some(status) = error.status() {
        return ProviderError::from_status(
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
        );
    }
    match error {
        E::RequiresApiKey { .. } | E::NoAuthData { .. } | E::NoAuthResolver { .. } => {
            ProviderError::new(ErrorKind::Auth, "no key was given")
        }
        E::WebModelCall { webc_error, .. } | E::WebAdapterCall { webc_error, .. } => {
            map_web(webc_error)
        }
        // On the streaming path an HTTP failure (401, 429, 500...) is a genai error boxed
        // inside this one, and a broken connection is a reqwest error.
        E::WebStream { error, .. } => {
            if let Some(inner) = error.downcast_ref::<genai::Error>() {
                map_error(inner)
            } else if let Some(inner) = error.downcast_ref::<reqwest::Error>() {
                map_reqwest(inner)
            } else {
                ProviderError::new(ErrorKind::Offline, "the connection broke")
            }
        }
        // An error event in the middle of a stream: the server's doing.
        E::ChatResponse { .. } => {
            ProviderError::new(ErrorKind::Server, "error event from the server")
        }
        E::StreamParse { .. }
        | E::ChatResponseGeneration { .. }
        | E::NoChatResponse { .. }
        | E::InvalidJsonResponseElement { .. }
        | E::JsonValueExt(_)
        | E::SerdeJson(_) => ProviderError::new(ErrorKind::BadResponse, "unexpected answer"),
        _ => ProviderError::new(ErrorKind::Internal, "the request could not be made"),
    }
}

fn map_web(error: &genai::webc::Error) -> ProviderError {
    use genai::webc::Error as W;
    match error {
        W::Reqwest(error) => map_reqwest(error),
        W::ResponseFailedNotJson { .. }
        | W::ResponseFailedInvalidJson { .. }
        | W::JsonValueExt(_) => {
            ProviderError::new(ErrorKind::BadResponse, "the answer is not JSON")
        }
        W::ResponseFailedStatus { status, .. } => {
            ProviderError::from_status(status.as_u16(), status.canonical_reason().unwrap_or(""))
        }
    }
}

fn map_reqwest(error: &reqwest::Error) -> ProviderError {
    if let Some(status) = error.status() {
        return ProviderError::from_status(
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
        );
    }
    if error.is_timeout() {
        return ProviderError::new(ErrorKind::Timeout, "timed out");
    }
    if error.is_connect() {
        return ProviderError::new(ErrorKind::Offline, connect_detail(error));
    }
    if error.is_decode() || error.is_body() {
        return ProviderError::new(ErrorKind::BadResponse, "the answer could not be read");
    }
    ProviderError::new(ErrorKind::Offline, "the request failed")
}

/// Why a connection failed, in fixed words. The error's own text has the address in it and is
/// only looked at here, never shown.
fn connect_detail(error: &reqwest::Error) -> &'static str {
    let mut text = String::new();
    let mut source: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(current) = source {
        if let Some(io) = current.downcast_ref::<std::io::Error>()
            && io.kind() == std::io::ErrorKind::ConnectionRefused
        {
            return "connection refused";
        }
        text.push_str(&current.to_string().to_ascii_lowercase());
        text.push(' ');
        source = current.source();
    }
    if text.contains("certificate") || text.contains("tls") || text.contains("handshake") {
        "the TLS certificate or handshake was not accepted"
    } else if text.contains("dns") || text.contains("resolve") || text.contains("lookup") {
        "the name could not be resolved"
    } else {
        "could not connect"
    }
}
