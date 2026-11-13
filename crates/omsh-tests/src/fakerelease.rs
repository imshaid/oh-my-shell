//! A fake release server on this machine, for the tests of `omsh update` and the daily check
//! (S29b). It serves what GitHub serves for a repository:
//!
//! - `GET /imshaid/oh-my-shell/releases/latest` as a `302` to `.../releases/tag/<tag>`, as
//!   github.com does (the address in `Location` is absolute and names this server);
//! - `GET /imshaid/oh-my-shell/releases/download/<tag>/<file>` for the archive of this CPU
//!   and `sha256.sum`.
//!
//! The archive is made with the system `tar` (the same program that makes the real one) and the
//! checksum with `sha256sum`, so the test reads a real `.tar.gz`. Every request is recorded.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

const REPO: &str = "imshaid/oh-my-shell";

/// How a published release is spoiled, to test what the program does about it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Spoil {
    /// Nothing: a correct release.
    #[default]
    None,
    /// `sha256.sum` lists a checksum that is not the archive's.
    WrongChecksum,
    /// `sha256.sum` does not list the archive at all.
    NoChecksum,
}

type Routes = Arc<Mutex<HashMap<String, Vec<u8>>>>;
/// Paths that answer with a redirect, and where to.
type Redirects = Arc<Mutex<HashMap<String, String>>>;

pub struct FakeRelease {
    addr: SocketAddr,
    routes: Routes,
    redirects: Redirects,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// The release archive's name for this CPU, as the program computes it.
#[must_use]
pub fn archive_name() -> String {
    let target = match std::env::consts::ARCH {
        "aarch64" => "aarch64-unknown-linux-gnu",
        _ => "x86_64-unknown-linux-gnu",
    };
    format!("oh-my-shell-{target}.tar.gz")
}

impl FakeRelease {
    /// Starts the server. Nothing is published yet, so every path answers 404.
    ///
    /// # Panics
    ///
    /// When no port on this machine can be opened.
    #[must_use]
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .unwrap_or_else(|error| fail("open a port on 127.0.0.1", &error));
        listener
            .set_nonblocking(true)
            .unwrap_or_else(|error| fail("make the port non-blocking", &error));
        let addr = listener
            .local_addr()
            .unwrap_or_else(|error| fail("the port's address", &error));
        let routes: Routes = Arc::default();
        let redirects: Redirects = Arc::default();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (routes, redirects, seen, stop) = (
                Arc::clone(&routes),
                Arc::clone(&redirects),
                Arc::clone(&seen),
                Arc::clone(&stop),
            );
            thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = answer(stream, &routes, &redirects, &seen);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            addr,
            routes,
            redirects,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    /// The address for `OMSH_RELEASE_SERVER`.
    #[must_use]
    pub fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Publishes release `tag` whose program file is `program`.
    ///
    /// # Panics
    ///
    /// When `tar` or `sha256sum` cannot run.
    pub fn publish(&self, tag: &str, program: &[u8], spoil: Spoil) {
        let work = std::env::temp_dir().join(format!(
            "omsh-fakerelease-{}-{}",
            std::process::id(),
            self.addr.port()
        ));
        let _ = fs::remove_dir_all(&work);
        let name = archive_name();
        let folder = name.trim_end_matches(".tar.gz");
        fs::create_dir_all(work.join(folder))
            .unwrap_or_else(|error| fail("make the archive folder", &error));
        fs::write(work.join(folder).join("oh-my-shell"), program)
            .unwrap_or_else(|error| fail("write the program", &error));
        fs::write(work.join(folder).join("LICENSE"), "MIT\n")
            .unwrap_or_else(|error| fail("write a second file", &error));
        let archive = work.join(&name);
        let status = Command::new("tar")
            .args(["--owner=0", "--group=0", "--numeric-owner", "-C"])
            .arg(&work)
            .arg("-czf")
            .arg(&archive)
            .arg(folder)
            .status()
            .unwrap_or_else(|error| fail("run tar", &error));
        assert!(status.success(), "tar failed");
        let bytes = fs::read(&archive).unwrap_or_else(|error| fail("read the archive", &error));
        let sum = sha256(&archive);
        let _ = fs::remove_dir_all(&work);

        let line = match spoil {
            Spoil::None => format!("{sum}  {name}\n"),
            Spoil::WrongChecksum => format!("{}  {name}\n", "0".repeat(64)),
            Spoil::NoChecksum => format!("{sum}  some-other-file.tar.gz\n"),
        };
        let base = format!("/{REPO}/releases/download/{tag}");
        let mut routes = self.routes.lock().unwrap_or_else(PoisonError::into_inner);
        // github.com: `releases/latest` redirects to the page of the newest release.
        let tag_page = format!("/{REPO}/releases/tag/{tag}");
        self.redirects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                format!("/{REPO}/releases/latest"),
                format!("http://{}{tag_page}", self.addr),
            );
        routes.insert(tag_page, b"<title>release</title>".to_vec());
        routes.insert(format!("{base}/{name}"), bytes);
        routes.insert(format!("{base}/sha256.sum"), line.into_bytes());
    }

    /// A repository with no release yet: `releases/latest` redirects to `.../releases`, as
    /// github.com does.
    pub fn publish_nothing(&self) {
        self.redirects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                format!("/{REPO}/releases/latest"),
                format!("http://{}/{REPO}/releases", self.addr),
            );
    }

    /// Every request so far, as `GET /path`.
    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for FakeRelease {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Ends the test with a message: the fake server could not be set up.
fn fail(what: &str, error: &dyn std::fmt::Display) -> ! {
    panic!("fake release server: could not {what}: {error}")
}

/// The SHA-256 of a file, from `sha256sum`.
fn sha256(path: &Path) -> String {
    let out = Command::new("sha256sum")
        .arg(path)
        .output()
        .unwrap_or_else(|error| fail("run sha256sum", &error));
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn answer(
    mut stream: TcpStream,
    routes: &Routes,
    redirects: &Redirects,
    seen: &Arc<Mutex<Vec<String>>>,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request = String::new();
    reader.read_line(&mut request)?;
    // The headers, up to the empty line.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut words = request.split_whitespace();
    let (method, path) = (
        words.next().unwrap_or_default(),
        words.next().unwrap_or_default(),
    );
    seen.lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(format!("{method} {path}"));
    let target = redirects
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(path)
        .cloned();
    if let (Some(target), "GET") = (target, method) {
        write!(
            stream,
            "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return stream.flush();
    }
    let body = routes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(path)
        .cloned();
    match body {
        Some(body) if method == "GET" => {
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )?;
            stream.write_all(&body)?;
        }
        _ => {
            stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )?;
        }
    }
    stream.flush()
}
