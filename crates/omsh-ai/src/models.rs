//! Facts about a model that genai does not give us (U4): how large its context window is, and
//! which of a provider's models are chat models.
//!
//! Two rules keep this out of the way:
//!
//! - **It runs only on AI paths.** A lookup starts when a request the person typed is sent (in
//!   parallel with it, see [`alongside`]) or while `omsh ai setup` runs; never at start-up.
//! - **It is quiet.** A key goes in a header, never in an address; a failure of any kind is a
//!   plain `None` (shown as `–`), and neither an address nor an error text is ever kept or
//!   printed. Every answer, a failure too, is remembered for the life of the process, so one
//!   model is asked about once.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::task::Poll;
use std::time::Duration;

use omsh_config::Provider as Kind;
use serde_json::Value;

use crate::runtime::prepare_tls;

/// How long one request of a lookup may take.
pub const LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);
/// The largest answer that is read.
const MAX_BODY: usize = 4 * 1024 * 1024;
/// The most pages of a model list that are followed.
const MAX_PAGES: usize = 10;

/// Where a context limit came from, to say so under the details table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitSource {
    /// Gemini's `inputTokenLimit` from `models.get`.
    Gemini,
    /// Anthropic's `max_input_tokens` from the models API.
    Anthropic,
    /// Ollama's `context_length` of the model that is loaded (what a request really gets).
    OllamaLoaded,
    /// `[ai] context_limit` in `config.toml`.
    Config,
}

impl LimitSource {
    /// One short line for the note under the table.
    #[must_use]
    pub const fn note(self) -> &'static str {
        match self {
            Self::Gemini => "context limit: inputTokenLimit from the Gemini models API",
            Self::Anthropic => "context limit: max_input_tokens from the Anthropic models API",
            Self::OllamaLoaded => {
                "context limit: context_length of the loaded model (Ollama /api/ps)"
            }
            Self::Config => "context limit: [ai] context_limit in config.toml",
        }
    }
}

/// The size of a model's context window in tokens, and where that number came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    pub tokens: u32,
    pub source: LimitSource,
}

impl Limit {
    /// A limit the person wrote in `config.toml`.
    #[must_use]
    pub const fn from_config(tokens: u32) -> Self {
        Self {
            tokens,
            source: LimitSource::Config,
        }
    }
}

type Key = (&'static str, String, String);

fn cache() -> &'static Mutex<HashMap<Key, Option<Limit>>> {
    static CACHE: OnceLock<Mutex<HashMap<Key, Option<Limit>>>> = OnceLock::new();
    CACHE.get_or_init(Mutex::default)
}

/// What an earlier lookup found, failures included: `Some(None)` means "asked, and no answer".
#[must_use]
pub fn remembered(kind: Kind, endpoint: &str, model: &str) -> Option<Option<Limit>> {
    cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&(kind.as_str(), endpoint.to_owned(), model.to_owned()))
        .copied()
}

fn remember(kind: Kind, endpoint: &str, model: &str, found: Option<Limit>) {
    cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            (kind.as_str(), endpoint.to_owned(), model.to_owned()),
            found,
        );
}

/// One GET that returns a JSON value, or `None` for any problem. Redirects are not followed (a
/// key in a header would go with them).
async fn get_json(url: &str, headers: &[(&str, &str)]) -> Option<Value> {
    prepare_tls();
    let client = reqwest::Client::builder()
        .user_agent(concat!("oh-my-shell/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(LOOKUP_TIMEOUT)
        .timeout(LOOKUP_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let mut request = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut response = request.send().await.ok()?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|length| length > MAX_BODY as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(piece) = response.chunk().await.ok()? {
        if body.len() + piece.len() > MAX_BODY {
            return None;
        }
        body.extend_from_slice(&piece);
    }
    serde_json::from_slice(&body).ok()
}

/// `models/gemini-x` and `gemini-x` are the same model.
fn bare(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
}

/// A token count from a JSON number: a whole number above 0 that fits.
fn count(value: Option<&Value>) -> Option<u32> {
    value
        .and_then(Value::as_u64)
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
}

/// Asks the provider how large the context of `model` is. `endpoint` ends with a slash, as
/// `Settings::endpoint` gives it. Remembered, a failure too. OpenAI and OpenAI-compatible
/// servers do not say, so they are not asked.
///
/// If the future is dropped before it ends (the answer came first), nothing is remembered.
pub async fn context_limit(
    kind: Kind,
    endpoint: &str,
    model: &str,
    key: Option<&str>,
) -> Option<Limit> {
    if let Some(known) = remembered(kind, endpoint, model) {
        return known;
    }
    let found = match kind {
        Kind::Gemini => gemini_limit(endpoint, model, key).await,
        Kind::Anthropic => anthropic_limit(endpoint, model, key).await,
        Kind::Ollama => ollama_limit(endpoint, model).await,
        Kind::OpenAi | Kind::OpenAiCompatible => None,
    };
    remember(kind, endpoint, model, found);
    found
}

async fn gemini_limit(endpoint: &str, model: &str, key: Option<&str>) -> Option<Limit> {
    let url = format!("{endpoint}models/{}", bare(model));
    let headers: Vec<(&str, &str)> = key.map(|key| ("x-goog-api-key", key)).into_iter().collect();
    let value = get_json(&url, &headers).await?;
    Some(Limit {
        tokens: count(value.get("inputTokenLimit"))?,
        source: LimitSource::Gemini,
    })
}

async fn anthropic_limit(endpoint: &str, model: &str, key: Option<&str>) -> Option<Limit> {
    let url = format!("{endpoint}models/{model}");
    let mut headers = vec![("anthropic-version", "2023-06-01")];
    if let Some(key) = key {
        headers.push(("x-api-key", key));
    }
    let value = get_json(&url, &headers).await?;
    // `max_input_tokens` can be null: then the window is not known.
    Some(Limit {
        tokens: count(value.get("max_input_tokens"))?,
        source: LimitSource::Anthropic,
    })
}

async fn ollama_limit(endpoint: &str, model: &str) -> Option<Limit> {
    let value = get_json(&format!("{endpoint}api/ps"), &[]).await?;
    let loaded = value.get("models")?.as_array()?;
    let entry = loaded.iter().find(|entry| {
        ["name", "model"]
            .iter()
            .any(|field| entry.get(*field).and_then(Value::as_str) == Some(model))
    })?;
    Some(Limit {
        tokens: count(entry.get("context_length"))?,
        source: LimitSource::OllamaLoaded,
    })
}

// -- Gemini's model list ---------------------------------------------------------------------

/// One entry of Gemini's model list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeminiModel {
    /// The name without the `models/` prefix.
    pub name: String,
    /// Whether `supportedGenerationMethods` contains `generateContent`.
    pub generates_content: bool,
}

/// Gemini's whole model list with the methods each model supports, following the pages. `None`
/// when the first page cannot be read: then the caller shows the list it already has.
pub async fn gemini_models(endpoint: &str, key: Option<&str>) -> Option<Vec<GeminiModel>> {
    let headers: Vec<(&str, &str)> = key.map(|key| ("x-goog-api-key", key)).into_iter().collect();
    let mut out = Vec::new();
    let mut token: Option<String> = None;
    for page in 0..MAX_PAGES {
        let mut url = format!("{endpoint}models?pageSize=1000");
        if let Some(token) = &token {
            url.push_str("&pageToken=");
            url.push_str(&encode(token));
        }
        let Some(value) = get_json(&url, &headers).await else {
            // A later page that fails leaves the models read so far.
            return (page > 0).then_some(out);
        };
        for model in value
            .get("models")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(name) = model.get("name").and_then(Value::as_str) else {
                continue;
            };
            let generates_content = model
                .get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .is_some_and(|methods| {
                    methods
                        .iter()
                        .any(|m| m.as_str() == Some("generateContent"))
                });
            out.push(GeminiModel {
                name: bare(name).to_owned(),
                generates_content,
            });
        }
        token = value
            .get("nextPageToken")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(str::to_owned);
        if token.is_none() {
            break;
        }
    }
    Some(out)
}

/// Percent-encodes a page token for an address.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Words in a model id that mark a model that does not chat in text: images, speech, video,
/// embeddings, retrieval, live audio, research agents.
const NOT_TEXT: [&str; 11] = [
    "image",
    "imagen",
    "tts",
    "transcribe",
    "deep-research",
    "embedding",
    "aqa",
    "veo",
    "live",
    "audio",
    "robotics",
];

/// Whether an id looks like a text chat model (the name rule of the setup filter).
#[must_use]
pub fn looks_like_text_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    !id.split(|c: char| !c.is_ascii_alphanumeric())
        .any(|word| NOT_TEXT.contains(&word))
        && !id.contains("deep-research")
}

/// The Gemini models worth offering for a chat: they support `generateContent`, and their id
/// does not say image, speech, video, embedding or research. Sorted, without duplicates.
#[must_use]
pub fn text_models(list: &[GeminiModel]) -> Vec<String> {
    let mut names: Vec<String> = list
        .iter()
        .filter(|model| model.generates_content && looks_like_text_model(&model.name))
        .map(|model| model.name.clone())
        .collect();
    names.sort();
    names.dedup();
    names
}

// -- running a lookup next to a request ------------------------------------------------------

/// Drives `work` and `probe` together. When `work` is done the answer is given at once, with
/// whatever `probe` has found by then (`None` when it has not finished: it is dropped, not
/// waited for, and nothing of it is remembered). The request is never delayed by the lookup.
pub async fn alongside<W, P>(work: W, probe: P) -> (W::Output, Option<Limit>)
where
    W: Future,
    P: Future<Output = Option<Limit>>,
{
    let mut work = std::pin::pin!(work);
    let mut probe = std::pin::pin!(probe);
    let mut found = None;
    let mut probing = true;
    let output = std::future::poll_fn(|cx| {
        // The request first: when it is done, nothing else is looked at.
        if let Poll::Ready(output) = work.as_mut().poll(cx) {
            return Poll::Ready(output);
        }
        if probing && let Poll::Ready(limit) = probe.as_mut().poll(cx) {
            found = limit;
            probing = false;
        }
        Poll::Pending
    })
    .await;
    (output, found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str, content: bool) -> GeminiModel {
        GeminiModel {
            name: name.to_owned(),
            generates_content: content,
        }
    }

    #[test]
    fn the_filter_keeps_chat_models_and_drops_the_rest() {
        let list = [
            model("gemini-2.5-flash", true),
            model("gemini-2.5-pro", true),
            model("gemini-2.5-flash-image", true),
            model("imagen-4.0-generate-001", false),
            model("gemini-2.5-flash-preview-tts", true),
            model("gemini-live-2.5-flash", true),
            model("text-embedding-004", false),
            model("gemini-embedding-001", true),
            model("aqa", false),
            model("veo-3.0-generate-001", false),
            model("deep-research-pro-preview", true),
            model("gemini-2.5-pro-transcribe", true),
            model("gemini-2.0-flash", false),
            model("gemini-2.5-flash", true),
        ];
        assert_eq!(
            text_models(&list),
            ["gemini-2.5-flash", "gemini-2.5-pro"],
            "generateContent is needed, the id words drop the rest, duplicates go"
        );
    }

    #[test]
    fn a_word_inside_another_word_does_not_drop_a_model() {
        // `live` is a word; `deliver` and `alive` are not.
        assert!(looks_like_text_model("gemini-deliver-1"));
        assert!(looks_like_text_model("alive-model"));
        assert!(!looks_like_text_model("gemini-2.5-flash-live"));
        assert!(!looks_like_text_model("Gemini-Image-1"));
    }

    #[test]
    fn page_tokens_are_encoded_for_an_address() {
        assert_eq!(encode("abc-DEF_1.2~"), "abc-DEF_1.2~");
        assert_eq!(encode("a b&c=d"), "a%20b%26c%3Dd");
    }

    #[test]
    fn a_count_is_a_positive_whole_number_that_fits() {
        use serde_json::json;
        assert_eq!(count(Some(&json!(32768))), Some(32768));
        assert_eq!(count(Some(&json!(0))), None);
        assert_eq!(count(Some(&json!(null))), None);
        assert_eq!(count(Some(&json!("5"))), None);
        assert_eq!(count(Some(&json!(-3))), None);
        assert_eq!(count(Some(&json!(1u64 << 40))), None);
        assert_eq!(count(None), None);
    }

    use std::pin::Pin;
    use std::task::Context;

    /// Pending once, then ready: gives the other future a turn.
    struct Yield(bool);

    impl Future for Yield {
        type Output = u32;
        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<u32> {
            if self.0 {
                Poll::Ready(7)
            } else {
                self.0 = true;
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    fn run<F: Future>(future: F) -> F::Output {
        crate::runtime::block_on(future, &|| false).unwrap()
    }

    #[test]
    fn the_answer_is_not_held_up_by_a_lookup_that_never_ends() {
        let started = std::time::Instant::now();
        let out = run(alongside(
            async { 5 },
            std::future::pending::<Option<Limit>>(),
        ));
        assert_eq!(out, (5, None));
        assert!(started.elapsed() < Duration::from_millis(200));
    }

    #[test]
    fn a_lookup_that_finishes_before_the_answer_is_kept() {
        let limit = Limit::from_config(1000);
        let out = run(alongside(Yield(false), async { Some(limit) }));
        assert_eq!(out, (7, Some(limit)));
    }

    #[test]
    fn a_lookup_that_has_no_answer_gives_none_and_does_not_end_the_request() {
        let out = run(alongside(Yield(false), async { None }));
        assert_eq!(out, (7, None));
    }

    #[test]
    fn a_lookup_that_is_dropped_before_it_ends_remembers_nothing() {
        // A server that accepts and never answers, so the lookup cannot finish.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/", listener.local_addr().unwrap());
        let probe = context_limit(Kind::Gemini, &endpoint, "dropped-model", Some("k"));
        let out = run(alongside(Yield(false), probe));
        assert_eq!(out, (7, None));
        assert_eq!(
            remembered(Kind::Gemini, &endpoint, "dropped-model"),
            None,
            "the answer came first: the model is asked again next time, not marked as failed"
        );
        drop(listener);
    }
}
