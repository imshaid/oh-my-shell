//! A fake AI server on this machine, for the tests of `omsh ai`.
//!
//! It speaks just enough of the OpenAI protocol (`GET /v1/models`, `POST /v1/chat/completions`
//! as a server-sent-event stream) and records every request it gets, so a test can see which
//! key was sent where. `omsh-ai` has a similar one for its own tests (`tests/providers.rs`);
//! a test crate cannot be shared with it, so the code is repeated.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

/// A request the server received.
#[derive(Clone, Debug)]
pub struct Seen {
    /// "POST /v1/chat/completions HTTP/1.1"
    pub line: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn path(&self) -> &str {
        self.line.split(' ').nth(1).unwrap_or_default()
    }
}

pub struct Server {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

const CHAT: &str = concat!(
    "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",",
    "\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"OK\"},",
    "\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",",
    "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n"
);

const MODELS: &str = concat!(
    "{\"object\":\"list\",\"data\":[{\"id\":\"model-one\",\"object\":\"model\"},",
    "{\"id\":\"model-two\",\"object\":\"model\"}]}"
);

impl Server {
    /// Starts the server on a free port of 127.0.0.1. It waits `delay` before it answers
    /// a chat request, so that a test can look at the shell while a request is in flight.
    /// Each connection serves one request and is closed.
    ///
    /// # Errors
    ///
    /// When no local port can be opened.
    pub fn start(delay: Duration) -> io::Result<Self> {
        Self::start_with(delay, false)
    }

    /// Like [`Server::start`], but with `keep_alive` the connections stay open for more
    /// requests, as the servers of the real providers keep them. The client then holds a
    /// pooled connection after the answer, which must still be closed when it is dropped.
    ///
    /// # Errors
    ///
    /// When no local port can be opened.
    pub fn start_with(delay: Duration, keep_alive: bool) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let record = Arc::clone(&record);
                thread::spawn(move || handle(stream, delay, keep_alive, &record));
            }
        });
        Ok(Self { addr, seen })
    }

    /// `http://127.0.0.1:PORT` plus `path`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn handle(stream: TcpStream, delay: Duration, keep_alive: bool, record: &Mutex<Vec<Seen>>) {
    let Ok(clone) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(clone);
    let mut out = stream;
    loop {
        if !serve_one(&mut reader, &mut out, delay, keep_alive, record) || !keep_alive {
            return;
        }
    }
}

/// Reads one request and answers it. False when the connection is over.
fn serve_one(
    reader: &mut BufReader<TcpStream>,
    out: &mut TcpStream,
    delay: Duration,
    keep_alive: bool,
    record: &Mutex<Vec<Seen>>,
) -> bool {
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return false;
    }
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            return false;
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
        return false;
    }
    let seen = Seen {
        line: line.trim_end().to_owned(),
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    };
    let is_chat = seen.path().ends_with("/chat/completions");
    record
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(seen);
    if is_chat {
        thread::sleep(delay);
    }
    let (content_type, text) = if is_chat {
        ("text/event-stream", CHAT)
    } else {
        ("application/json", MODELS)
    };
    let connection = if keep_alive { "keep-alive" } else { "close" };
    let sent = write!(
        out,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: {connection}\r\n\r\n{text}",
        text.len()
    );
    sent.is_ok() && out.flush().is_ok()
}
