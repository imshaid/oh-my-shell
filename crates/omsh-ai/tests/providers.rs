//! The real genai client against a fake HTTP server on this machine, for each protocol.
//!
//! The server answers what the real ones answer (server-sent events for OpenAI, Anthropic and
//! Gemini, one JSON object per line for Ollama) and records what it was sent, so the tests can
//! check the address, the model, and where the key went.

#![allow(
    clippy::unwrap_used,
    reason = "a test file: the helpers set up servers, and a failed setup should fail the test"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use futures::StreamExt;
use omsh_ai::{
    ChatEvent, ChatRequest, ErrorKind, GenaiProvider, Policy, Provider, ProviderError, Secret,
    Settings, collect_text, runtime, verify,
};
use omsh_config::Provider as Kind;

const KEY: &str = "sk-test-5f3c9a1e77b24d0c";

// -- the fake server --------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Seen {
    /// "POST /v1/chat/completions HTTP/1.1"
    line: String,
    /// Header names in lower case.
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn path(&self) -> &str {
        self.line.split(' ').nth(1).unwrap_or_default()
    }

    /// Everything that was sent, as one text, to look for a key in.
    fn everything(&self) -> String {
        format!("{:?} {} {}", self.line, self.headers.len(), self.body)
            + &self
                .headers
                .iter()
                .map(|(n, v)| format!(" {n}: {v}"))
                .collect::<String>()
    }
}

#[derive(Clone)]
struct Canned {
    status: u16,
    content_type: &'static str,
    body: String,
}

fn ok(content_type: &'static str, body: &str) -> Canned {
    Canned {
        status: 200,
        content_type,
        body: body.to_owned(),
    }
}

fn status(status: u16, body: &str) -> Canned {
    Canned {
        status,
        content_type: "application/json",
        body: body.to_owned(),
    }
}

struct Server {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Server {
    /// Serves each connection the next canned answer (the last one repeats).
    fn start(answers: Vec<Canned>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        thread::spawn(move || {
            for (index, stream) in listener.incoming().enumerate() {
                let Ok(stream) = stream else { return };
                let answer = answers[index.min(answers.len() - 1)].clone();
                let record = Arc::clone(&record);
                thread::spawn(move || handle(stream, &answer, &record));
            }
        });
        Self { addr, seen }
    }

    /// `http://127.0.0.1:PORT` plus `path`.
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn requests(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn only_request(&self) -> Seen {
        let all = self.requests();
        assert_eq!(all.len(), 1, "{all:?}");
        all[0].clone()
    }
}

fn handle(stream: TcpStream, answer: &Canned, record: &Mutex<Vec<Seen>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            return;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    record
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(Seen {
            line: line.trim_end().to_owned(),
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        });
    let mut out = stream;
    let _ = write!(
        out,
        "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        answer.status,
        answer.content_type,
        answer.body.len(),
        answer.body
    );
    let _ = out.flush();
}

/// A server that accepts a connection and never answers.
fn silent_server() -> (SocketAddr, Arc<AtomicBool>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let connected = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&connected);
    thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            flag.store(true, Ordering::Relaxed);
            held.push(stream);
        }
    });
    (addr, connected)
}

/// An address nothing listens on.
fn closed_port() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap()
}

// -- canned protocol answers ------------------------------------------------------------

const SSE: &str = "text/event-stream";

fn openai_stream() -> Canned {
    let chunk = |delta: &str, finish: &str| {
        format!(
            "data: {{\"id\":\"c1\",\"object\":\"chat.completion.chunk\",\"created\":1,\
             \"model\":\"m\",\"choices\":[{{\"index\":0,\"delta\":{delta},\
             \"finish_reason\":{finish}}}]}}\n\n"
        )
    };
    ok(
        SSE,
        &(chunk("{\"role\":\"assistant\",\"content\":\"O\"}", "null")
            + &chunk("{\"content\":\"K\"}", "null")
            + &chunk("{}", "\"stop\"")
            + "data: [DONE]\n\n"),
    )
}

fn anthropic_stream() -> Canned {
    ok(
        SSE,
        "event: message_start\n\
         data: {\"type\":\"message_start\",\"message\":{\"id\":\"m1\",\"type\":\"message\",\
         \"role\":\"assistant\",\"content\":[],\"model\":\"m\",\"stop_reason\":null,\
         \"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n\
         event: content_block_start\n\
         data: {\"type\":\"content_block_start\",\"index\":0,\
         \"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n\
         event: content_block_delta\n\
         data: {\"type\":\"content_block_delta\",\"index\":0,\
         \"delta\":{\"type\":\"text_delta\",\"text\":\"O\"}}\n\n\
         event: content_block_delta\n\
         data: {\"type\":\"content_block_delta\",\"index\":0,\
         \"delta\":{\"type\":\"text_delta\",\"text\":\"K\"}}\n\n\
         event: content_block_stop\n\
         data: {\"type\":\"content_block_stop\",\"index\":0}\n\n\
         event: message_delta\n\
         data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\
         \"usage\":{\"output_tokens\":2}}\n\n\
         event: message_stop\n\
         data: {\"type\":\"message_stop\"}\n\n",
    )
}

fn gemini_stream() -> Canned {
    let part = |text: &str, finish: &str| {
        format!(
            "data: {{\"candidates\":[{{\"content\":{{\"role\":\"model\",\"parts\":[{{\"text\":\"{text}\"}}]}}{finish}}}],\
             \"usageMetadata\":{{\"promptTokenCount\":1,\"candidatesTokenCount\":1,\"totalTokenCount\":2}}}}\r\n\r\n"
        )
    };
    ok(
        SSE,
        &(part("O", "") + &part("K", ",\"finishReason\":\"STOP\"")),
    )
}

fn ollama_stream() -> Canned {
    let line = |content: &str, done: &str| {
        format!(
            "{{\"model\":\"m\",\"created_at\":\"2026-11-10T00:00:00Z\",\
             \"message\":{{\"role\":\"assistant\",\"content\":\"{content}\"}},\"done\":{done}}}\n"
        )
    };
    ok(
        "application/x-ndjson",
        &(line("O", "false")
            + &line("K", "false")
            + "{\"model\":\"m\",\"created_at\":\"2026-11-10T00:00:00Z\",\
               \"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\
               \"done_reason\":\"stop\",\"total_duration\":1,\"eval_count\":2}\n"),
    )
}

// -- helpers ----------------------------------------------------------------------------

fn secret() -> Secret {
    Secret::new(KEY.to_owned()).unwrap()
}

fn provider(kind: Kind, base_url: String, with_key: bool) -> GenaiProvider {
    let settings = Settings {
        kind,
        model: "test-model".to_owned(),
        base_url: Some(base_url),
    };
    let key = secret();
    GenaiProvider::new(&settings, with_key.then_some(&key)).unwrap()
}

fn no_cancel() -> bool {
    false
}

fn events(provider: &dyn Provider) -> Result<Vec<ChatEvent>, ProviderError> {
    runtime::block_on(
        async {
            let mut stream = provider.chat(ChatRequest::ask("hi", 16));
            let mut out = Vec::new();
            while let Some(event) = stream.next().await {
                out.push(event?);
            }
            Ok(out)
        },
        &no_cancel,
    )
    .unwrap()
}

fn text_of(events: &[ChatEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            ChatEvent::Text(text) => Some(text.as_str()),
            ChatEvent::Done => None,
        })
        .collect()
}

fn instant_retries() -> Policy {
    Policy {
        attempts: 3,
        pauses: vec![Duration::ZERO],
    }
}

/// The text of the error in every form it can be shown.
fn every_form(error: &ProviderError) -> String {
    let mut text = format!("{error} {error:?} {}", error.detail);
    for kind in Kind::ALL {
        text.push(' ');
        text.push_str(&error.one_line(kind));
    }
    text
}

// -- one test per protocol --------------------------------------------------------------

#[test]
fn openai_compatible_server() {
    let server = Server::start(vec![openai_stream()]);
    let p = provider(Kind::OpenAiCompatible, server.url("/v1"), true);
    let got = events(&p).unwrap();
    assert_eq!(text_of(&got), "OK");
    assert_eq!(got.last(), Some(&ChatEvent::Done));
    assert_eq!(got.iter().filter(|e| **e == ChatEvent::Done).count(), 1);

    let request = server.only_request();
    assert_eq!(request.line, "POST /v1/chat/completions HTTP/1.1");
    assert_eq!(
        request.header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
    assert!(
        request.body.contains("\"model\":\"test-model\""),
        "{}",
        request.body
    );
    assert!(request.body.contains("\"stream\":true"), "{}", request.body);
    assert!(request.body.contains("\"hi\""), "{}", request.body);
}

#[test]
fn openai() {
    let server = Server::start(vec![openai_stream()]);
    let p = provider(Kind::OpenAi, server.url("/v1"), true);
    assert_eq!(text_of(&events(&p).unwrap()), "OK");
    let request = server.only_request();
    assert_eq!(request.path(), "/v1/chat/completions");
    assert_eq!(
        request.header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
}

#[test]
fn anthropic() {
    let server = Server::start(vec![anthropic_stream()]);
    let p = provider(Kind::Anthropic, server.url("/v1"), true);
    let got = events(&p).unwrap();
    assert_eq!(text_of(&got), "OK");
    assert_eq!(got.last(), Some(&ChatEvent::Done));
    let request = server.only_request();
    assert_eq!(request.path(), "/v1/messages");
    assert_eq!(request.header("x-api-key"), Some(KEY));
    assert!(request.header("anthropic-version").is_some());
    assert!(
        request.body.contains("\"max_tokens\":16"),
        "{}",
        request.body
    );
    assert!(
        request.body.contains("\"model\":\"test-model\""),
        "{}",
        request.body
    );
}

#[test]
fn gemini() {
    let server = Server::start(vec![gemini_stream()]);
    let p = provider(Kind::Gemini, server.url("/v1beta"), true);
    let got = events(&p).unwrap();
    assert_eq!(text_of(&got), "OK");
    let request = server.only_request();
    assert!(
        request
            .path()
            .starts_with("/v1beta/models/test-model:streamGenerateContent"),
        "{}",
        request.path()
    );
    // In a header, not in the address: addresses end up in proxy and server logs.
    assert_eq!(request.header("x-goog-api-key"), Some(KEY));
    assert!(!request.line.contains(KEY), "{}", request.line);
}

#[test]
fn ollama_gets_no_key() {
    let server = Server::start(vec![ollama_stream()]);
    // A key is offered, and must be dropped: Ollama has none.
    let p = provider(Kind::Ollama, server.url(""), true);
    let got = events(&p).unwrap();
    assert_eq!(text_of(&got), "OK");
    assert_eq!(got.last(), Some(&ChatEvent::Done));
    let request = server.only_request();
    assert_eq!(request.line, "POST /api/chat HTTP/1.1");
    assert!(
        !request.everything().contains(KEY),
        "{}",
        request.everything()
    );
    assert!(
        request.body.contains("\"model\":\"test-model\""),
        "{}",
        request.body
    );
}

#[test]
fn a_server_without_a_key_is_sent_a_placeholder() {
    let server = Server::start(vec![openai_stream()]);
    let settings = Settings {
        kind: Kind::OpenAiCompatible,
        model: "m".to_owned(),
        base_url: Some(server.url("/v1")),
    };
    let p = GenaiProvider::new(&settings, None).unwrap();
    assert_eq!(text_of(&events(&p).unwrap()), "OK");
    assert_eq!(
        server.only_request().header("authorization"),
        Some("Bearer none")
    );
}

#[test]
fn a_system_prompt_and_json_mode_reach_the_server() {
    let server = Server::start(vec![openai_stream()]);
    let p = provider(Kind::OpenAiCompatible, server.url("/v1"), true);
    let mut request = ChatRequest::ask("give json", 64);
    request.system = Some("You answer in JSON.".to_owned());
    request.json = true;
    runtime::block_on(collect_text(&p, &request, &Policy::once()), &no_cancel)
        .unwrap()
        .unwrap();
    let body = server.only_request().body;
    assert!(body.contains("You answer in JSON."), "{body}");
    assert!(body.contains("json_object"), "{body}");
}

// -- failures ---------------------------------------------------------------------------

#[test]
fn http_errors_become_kinds_and_the_body_is_never_shown() {
    let echo = format!("{{\"error\":{{\"message\":\"Incorrect API key provided: {KEY}\"}}}}");
    for (code, kind) in [
        (401, ErrorKind::Auth),
        (403, ErrorKind::Auth),
        (404, ErrorKind::NotFound),
        (429, ErrorKind::RateLimited),
        (500, ErrorKind::Server),
        (400, ErrorKind::BadRequest),
    ] {
        let server = Server::start(vec![status(code, &echo)]);
        let p = provider(Kind::OpenAiCompatible, server.url("/v1"), true);
        let error = events(&p).unwrap_err();
        assert_eq!(error.kind, kind, "HTTP {code}: {error}");
        assert!(error.detail.contains(&code.to_string()), "{}", error.detail);
        assert!(
            !every_form(&error).contains(KEY),
            "the response body must not be shown: {}",
            every_form(&error)
        );
    }
}

#[test]
fn nothing_listening_is_offline() {
    let p = provider(
        Kind::OpenAiCompatible,
        format!("http://{}/v1", closed_port()),
        true,
    );
    let error = events(&p).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Offline, "{error}");
    assert_eq!(error.detail, "connection refused");
    assert!(!every_form(&error).contains(KEY));
    assert!(
        !every_form(&error).contains("127.0.0.1"),
        "no address either"
    );
}

#[test]
fn a_transient_error_is_retried_and_then_works() {
    let server = Server::start(vec![status(503, "{}"), status(500, "{}"), openai_stream()]);
    let p = provider(Kind::OpenAiCompatible, server.url("/v1"), true);
    let text = runtime::block_on(verify(&p, &instant_retries()), &no_cancel)
        .unwrap()
        .unwrap();
    assert_eq!(text, "OK");
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn a_wrong_key_is_asked_once_only() {
    let server = Server::start(vec![status(401, "{}")]);
    let p = provider(Kind::OpenAiCompatible, server.url("/v1"), true);
    let error = runtime::block_on(verify(&p, &instant_retries()), &no_cancel)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Auth);
    assert_eq!(server.requests().len(), 1, "a wrong key is not retried");
}

#[test]
fn a_server_that_keeps_failing_gives_up_after_three_tries() {
    let server = Server::start(vec![status(500, "{}")]);
    let p = provider(Kind::OpenAiCompatible, server.url("/v1"), true);
    let error = runtime::block_on(verify(&p, &instant_retries()), &no_cancel)
        .unwrap()
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Server);
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn ctrl_c_ends_a_request_the_server_never_answers() {
    let (addr, connected) = silent_server();
    let p = provider(Kind::OpenAiCompatible, format!("http://{addr}/v1"), true);
    let started = Instant::now();
    let flag = AtomicBool::new(false);
    let result = thread::scope(|scope| {
        scope.spawn(|| {
            while !connected.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(5));
            }
            flag.store(true, Ordering::Relaxed);
        });
        runtime::block_on(
            collect_text(&p, &ChatRequest::ask("hi", 8), &Policy::once()),
            &|| flag.load(Ordering::Relaxed),
        )
    });
    assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

// -- the key's destination --------------------------------------------------------------

#[test]
fn a_key_is_not_given_to_a_plain_http_server_elsewhere() {
    for url in [
        "http://example.com/v1",
        "http://10.0.0.5:8080/v1",
        "http://localhost.evil.example/v1",
        "http://localhost@evil.example/v1",
    ] {
        let settings = Settings {
            kind: Kind::OpenAiCompatible,
            model: "m".to_owned(),
            base_url: Some(url.to_owned()),
        };
        let error = GenaiProvider::new(&settings, Some(&secret()))
            .err()
            .unwrap();
        assert_eq!(error.kind, ErrorKind::KeyNotAllowed, "{url}");
        assert!(!every_form(&error).contains(KEY));
        assert!(
            !error.detail.contains("/v1"),
            "only the server is named: {}",
            error.detail
        );
    }
}

#[test]
fn https_and_this_machine_are_accepted_with_a_key() {
    for (kind, url) in [
        (Kind::OpenAiCompatible, "https://llm.example.org/v1"),
        (Kind::OpenAiCompatible, "http://localhost:8080/v1"),
        (Kind::OpenAiCompatible, "http://127.0.0.1:8080/v1"),
        (Kind::OpenAiCompatible, "http://[::1]:8080/v1"),
    ] {
        let settings = Settings {
            kind,
            model: "m".to_owned(),
            base_url: Some(url.to_owned()),
        };
        assert!(
            GenaiProvider::new(&settings, Some(&secret())).is_ok(),
            "{url}"
        );
    }
}

#[test]
fn the_default_addresses_are_all_acceptable_for_a_key() {
    for kind in [Kind::Anthropic, Kind::OpenAi, Kind::Gemini] {
        let settings = Settings {
            kind,
            model: "m".to_owned(),
            base_url: None,
        };
        assert!(
            settings.endpoint().unwrap().starts_with("https://"),
            "{kind:?}"
        );
        assert!(GenaiProvider::new(&settings, Some(&secret())).is_ok());
    }
}

#[test]
fn plain_http_is_fine_when_no_key_is_sent() {
    let settings = Settings {
        kind: Kind::OpenAiCompatible,
        model: "m".to_owned(),
        base_url: Some("http://example.com/v1".to_owned()),
    };
    assert!(GenaiProvider::new(&settings, None).is_ok());
}

#[test]
fn an_openai_compatible_server_needs_an_address() {
    let settings = Settings {
        kind: Kind::OpenAiCompatible,
        model: "m".to_owned(),
        base_url: None,
    };
    let error = GenaiProvider::new(&settings, None).err().unwrap();
    assert_eq!(error.kind, ErrorKind::BadRequest);
}

#[test]
fn the_address_gets_one_slash_at_the_end() {
    let endpoint = |url: &str| {
        Settings {
            kind: Kind::OpenAiCompatible,
            model: "m".to_owned(),
            base_url: Some(url.to_owned()),
        }
        .endpoint()
        .unwrap()
    };
    assert_eq!(
        endpoint("http://localhost:8080/v1"),
        "http://localhost:8080/v1/"
    );
    assert_eq!(
        endpoint("http://localhost:8080/v1/"),
        "http://localhost:8080/v1/"
    );
    assert_eq!(
        endpoint("  http://localhost:8080  "),
        "http://localhost:8080/"
    );
}

// -- model lists ------------------------------------------------------------------------

#[test]
fn the_models_an_openai_server_offers() {
    let server = Server::start(vec![ok(
        "application/json",
        "{\"object\":\"list\",\"data\":[{\"id\":\"gpt-a\",\"object\":\"model\"},{\"id\":\"gpt-b\",\"object\":\"model\"}]}",
    )]);
    let p = provider(Kind::OpenAiCompatible, server.url("/v1"), true);
    let models = runtime::block_on(p.models(), &no_cancel).unwrap().unwrap();
    assert_eq!(models, ["gpt-a", "gpt-b"]);
    let request = server.only_request();
    assert_eq!(request.path(), "/v1/models");
    assert_eq!(
        request.header("authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
}

#[test]
fn the_models_ollama_has() {
    let server = Server::start(vec![ok(
        "application/json",
        "{\"models\":[{\"name\":\"llama3.2:latest\"},{\"name\":\"qwen\"}]}",
    )]);
    let p = provider(Kind::Ollama, server.url(""), false);
    let models = runtime::block_on(p.models(), &no_cancel).unwrap().unwrap();
    assert_eq!(models, ["llama3.2:latest", "qwen"]);
}
