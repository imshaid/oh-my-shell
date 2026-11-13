//! Plain downloads for `omsh update` and the daily release check (S29b).
//!
//! This is not AI code, but it lives in this crate on purpose: this crate owns the tokio
//! runtime, the TLS setup and the HTTP client, and Gate C (§21) says no other crate may open a
//! socket. Nothing here runs before a person types `omsh update` or turns `[update] check` on.
//!
//! Two ways to run a download:
//!
//! - [`get`] on the shared runtime, for a command a person is waiting for (Ctrl+C aware);
//! - [`get_detached`] on a runtime of its own that is dropped at the end, for the background
//!   check, so that it never waits for (or holds up) a `?` request.
//!
//! Nothing downloaded is trusted here: callers check the checksum.

use std::time::Duration;

use tokio::runtime::Builder;

use crate::destination::this_machine_origin;
use crate::error::{ErrorKind, ProviderError};
use crate::runtime::{block_on, prepare_tls};

/// GitHub's web address. The latest release is found by asking `<web>/<repo>/releases/latest`
/// for its redirect (not the API, which allows an address only 60 requests an hour), and the
/// release files are downloaded from the same place.
pub const GITHUB_WEB: &str = "https://github.com";
/// The test hook: a server on this machine that stands in for GitHub. It is not documented for
/// users, and any value that is not a loopback address is refused.
pub const SERVER_VARIABLE: &str = "OMSH_RELEASE_SERVER";

/// The `Accept` header for a file.
pub const ACCEPT_ANY: &str = "*/*";

/// Where releases are looked up and downloaded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    /// Where `/<owner>/<name>/releases/latest` is asked (it redirects to the newest tag) and
    /// `/<owner>/<name>/releases/download/<tag>/<file>` is downloaded.
    pub web: String,
}

impl Origin {
    /// GitHub.
    #[must_use]
    pub fn github() -> Self {
        Self {
            web: GITHUB_WEB.to_owned(),
        }
    }
}

/// The server to use. `value` is what [`SERVER_VARIABLE`] holds, if it is set. Unset gives
/// GitHub; a loopback address (`localhost`, `127.0.0.1`, `::1`, http or https) replaces it;
/// anything else is an error, so the variable can never point updates at a remote
/// server.
///
/// # Errors
///
/// A sentence saying the value is not accepted.
pub fn release_origin(value: Option<&str>) -> Result<Origin, String> {
    let Some(value) = value else {
        return Ok(Origin::github());
    };
    match this_machine_origin(value) {
        Some(origin) => Ok(Origin { web: origin }),
        None => Err(format!(
            "{SERVER_VARIABLE} is set, but it is not a server on this computer \
             (localhost, 127.0.0.1 or ::1); the real release server is not replaced"
        )),
    }
}

/// The tag in the address that `releases/latest` redirects to: `.../releases/tag/v0.1.1` gives
/// `v0.1.1`. `None` for any other address (a repository with no release redirects to
/// `.../releases`) and for a tag that is not a short plain word, so a tag can never change the
/// path of a download address.
#[must_use]
pub fn tag_from_location(location: &str) -> Option<String> {
    let path = location.split(['?', '#']).next().unwrap_or_default();
    let tag = path.rsplit_once("/releases/tag/")?.1;
    let plain = !tag.is_empty()
        && tag.len() <= 64
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'));
    plain.then(|| tag.to_owned())
}

/// How the redirects of a download are followed: to `https` always, to `http` only between
/// servers on this machine (so a loopback test server can redirect, and nothing can be sent
/// from https to plain http), at most five times.
fn redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
            return attempt.error("too many redirects");
        }
        let next = attempt.url().as_str();
        let secure = attempt.url().scheme() == "https";
        let local = this_machine_origin(&origin_of(next)).is_some()
            && attempt
                .previous()
                .iter()
                .all(|url| this_machine_origin(&origin_of(url.as_str())).is_some());
        if secure || local {
            attempt.follow()
        } else {
            attempt.error("redirect to plain http")
        }
    })
}

/// `scheme://authority` of `url`.
fn origin_of(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return String::new();
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    format!("{scheme}://{}", &rest[..end])
}

/// One GET. The body is read in pieces and the download is refused as soon as it passes
/// `max_bytes`.
async fn fetch(
    url: &str,
    accept: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Vec<u8>, ProviderError> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("oh-my-shell/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .redirect(redirects())
        .build()
        .map_err(|_| ProviderError::new(ErrorKind::Internal, "the HTTP client did not start"))?;
    let mut response = client
        .get(url)
        .header(reqwest::header::ACCEPT, accept)
        .send()
        .await
        .map_err(|error| crate::genai_provider::map_reqwest_public(&error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(ProviderError::from_status(
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(piece) = response
        .chunk()
        .await
        .map_err(|error| crate::genai_provider::map_reqwest_public(&error))?
    {
        if body.len() + piece.len() > max_bytes {
            return Err(too_big());
        }
        body.extend_from_slice(&piece);
    }
    Ok(body)
}

fn too_big() -> ProviderError {
    ProviderError::new(
        ErrorKind::BadResponse,
        "the download is larger than allowed",
    )
}

/// The longest `Location` header that is read.
const MAX_LOCATION: usize = 2048;

/// One GET that is **not** followed: the address a redirect points to. A status other than a
/// redirect is an error (`NotFound` for a plain 200, because that is not what a repository
/// with a release does).
async fn locate(url: &str, timeout: Duration) -> Result<String, ProviderError> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("oh-my-shell/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ProviderError::new(ErrorKind::Internal, "the HTTP client did not start"))?;
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, ACCEPT_ANY)
        .send()
        .await
        .map_err(|error| crate::genai_provider::map_reqwest_public(&error))?;
    let status = response.status();
    if !status.is_redirection() {
        if status.is_success() {
            return Err(ProviderError::new(
                ErrorKind::NotFound,
                "the address does not redirect",
            ));
        }
        return Err(ProviderError::from_status(
            status.as_u16(),
            status.canonical_reason().unwrap_or(""),
        ));
    }
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= MAX_LOCATION)
        .ok_or_else(|| ProviderError::new(ErrorKind::BadResponse, "the redirect has no address"))?;
    // A relative address is resolved against the one that was asked.
    let target = response
        .url()
        .join(location)
        .map_err(|_| ProviderError::new(ErrorKind::BadResponse, "the redirect address is bad"))?;
    Ok(target.to_string())
}

/// Where `url` redirects to, asked on the shared runtime (Ctrl+C aware). The redirect is not
/// followed.
///
/// # Errors
///
/// [`ProviderError`] for a network failure, an answer that is not a redirect, or a Ctrl+C.
pub fn location(
    url: &str,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
) -> Result<String, ProviderError> {
    block_on(locate(url, timeout), interrupted)?
}

/// As [`location`], on a runtime made for this call and dropped at its end (the background
/// check).
///
/// # Errors
///
/// As [`location`], without the Ctrl+C.
pub fn location_detached(url: &str, timeout: Duration) -> Result<String, ProviderError> {
    prepare_tls();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ProviderError::new(ErrorKind::Internal, format!("no runtime: {error}")))?;
    let result = runtime.block_on(locate(url, timeout));
    drop(runtime);
    result
}

/// Downloads `url` on the shared runtime. Returns early with [`ErrorKind::Cancelled`] when
/// `interrupted` says so (it is looked at every 50 ms).
///
/// # Errors
///
/// [`ProviderError`] for a network failure, a status other than 2xx, a body over `max_bytes`,
/// or a Ctrl+C.
pub fn get(
    url: &str,
    accept: &str,
    max_bytes: usize,
    timeout: Duration,
    interrupted: &dyn Fn() -> bool,
) -> Result<Vec<u8>, ProviderError> {
    block_on(fetch(url, accept, max_bytes, timeout), interrupted)?
}

/// Downloads `url` on a runtime made for this call and dropped at its end. For the background
/// check, which runs on a helper thread.
///
/// # Errors
///
/// As [`get`], without the Ctrl+C.
pub fn get_detached(
    url: &str,
    accept: &str,
    max_bytes: usize,
    timeout: Duration,
) -> Result<Vec<u8>, ProviderError> {
    prepare_tls();
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ProviderError::new(ErrorKind::Internal, format!("no runtime: {error}")))?;
    let result = runtime.block_on(fetch(url, accept, max_bytes, timeout));
    // Dropping the runtime closes what is left of the connection.
    drop(runtime);
    result
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    /// A server on this machine that answers each connection with `answer`, `count` times.
    fn serve(answer: Vec<u8>, count: usize) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(&answer);
            }
        });
        (format!("http://{address}/x"), handle)
    }

    fn ok(body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    const FIVE: Duration = Duration::from_secs(5);

    #[test]
    fn unset_means_github() {
        let origin = release_origin(None).unwrap();
        assert_eq!(origin.web, "https://github.com");
    }

    #[test]
    fn a_loopback_server_replaces_github() {
        let origin = release_origin(Some("http://127.0.0.1:8123/")).unwrap();
        assert_eq!(origin.web, "http://127.0.0.1:8123");
    }

    #[test]
    fn a_remote_or_odd_server_is_refused() {
        for value in [
            "",
            "http://example.com",
            "https://github.com.evil.example",
            "http://127.0.0.2",
            "http://localhost.evil.example",
            "http://localhost@evil.example",
            "localhost",
        ] {
            let error = release_origin(Some(value)).unwrap_err();
            assert!(error.contains("not replaced"), "{value}: {error}");
        }
    }

    #[test]
    fn the_tag_is_read_from_the_redirect_address() {
        assert_eq!(
            tag_from_location("https://github.com/imshaid/oh-my-shell/releases/tag/v0.2.0")
                .as_deref(),
            Some("v0.2.0")
        );
        assert_eq!(
            tag_from_location("/imshaid/oh-my-shell/releases/tag/v1.0.0-rc.1").as_deref(),
            Some("v1.0.0-rc.1")
        );
        // A query or a fragment is not part of the tag.
        assert_eq!(
            tag_from_location("https://github.com/o/r/releases/tag/v1.2.3?x=1#y").as_deref(),
            Some("v1.2.3")
        );
    }

    #[test]
    fn an_address_that_is_not_a_tag_or_could_change_a_path_is_not_used() {
        for location in [
            "https://github.com/imshaid/oh-my-shell/releases",
            "https://github.com/imshaid/oh-my-shell/releases/tag/",
            "https://github.com/o/r/releases/tag/../../x",
            "https://github.com/o/r/releases/tag/v1/2",
            "https://github.com/o/r/releases/tag/v1%202",
            "https://github.com/o/r/releases/tag/v 1",
            "https://github.com/o/r/releases/latest",
            "",
        ] {
            assert_eq!(tag_from_location(location), None, "{location}");
        }
        let long = format!("https://github.com/o/r/releases/tag/v{}", "1".repeat(80));
        assert_eq!(tag_from_location(&long), None);
    }

    fn redirect(location: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .into_bytes()
    }

    #[test]
    fn a_redirect_is_reported_and_not_followed() {
        // One connection only: following the redirect would need a second one.
        let (url, server) = serve(redirect("http://127.0.0.1:1/releases/tag/v7.7.7"), 1);
        let target = location(&url, FIVE, &|| false).unwrap();
        assert_eq!(target, "http://127.0.0.1:1/releases/tag/v7.7.7");
        server.join().unwrap();
    }

    #[test]
    fn a_relative_redirect_is_resolved_against_the_asked_address() {
        let (url, server) = serve(redirect("/o/r/releases/tag/v3.0.0"), 1);
        let origin = url.trim_end_matches("/x").to_owned();
        let target = location_detached(&url, FIVE).unwrap();
        assert_eq!(target, format!("{origin}/o/r/releases/tag/v3.0.0"));
        server.join().unwrap();
    }

    #[test]
    fn an_answer_that_is_not_a_redirect_is_an_error() {
        let (url, server) = serve(ok("hello"), 1);
        let error = location(&url, FIVE, &|| false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);
        server.join().unwrap();
        let (url, server) = serve(
            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            1,
        );
        let error = location(&url, FIVE, &|| false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);
        server.join().unwrap();
        let (url, server) = serve(
            b"HTTP/1.1 302 Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
            1,
        );
        let error = location(&url, FIVE, &|| false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadResponse);
        server.join().unwrap();
    }

    #[test]
    fn a_body_is_read() {
        let (url, server) = serve(ok("hello"), 1);
        let body = get(&url, ACCEPT_ANY, 100, FIVE, &|| false).unwrap();
        assert_eq!(body, b"hello");
        server.join().unwrap();
    }

    #[test]
    fn a_body_is_read_on_its_own_runtime_too() {
        let (url, server) = serve(ok("hello"), 1);
        let body = get_detached(&url, ACCEPT_ANY, 100, FIVE).unwrap();
        assert_eq!(body, b"hello");
        server.join().unwrap();
    }

    #[test]
    fn a_body_over_the_limit_is_refused() {
        let (url, server) = serve(ok("0123456789"), 1);
        let error = get(&url, ACCEPT_ANY, 5, FIVE, &|| false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadResponse);
        server.join().unwrap();
    }

    #[test]
    fn a_body_without_a_length_is_cut_at_the_limit() {
        let answer = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n0123456789".to_vec();
        let (url, server) = serve(answer, 1);
        let error = get(&url, ACCEPT_ANY, 5, FIVE, &|| false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::BadResponse);
        server.join().unwrap();
    }

    #[test]
    fn a_status_other_than_success_is_an_error() {
        let answer = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (url, server) = serve(answer.to_vec(), 1);
        let error = get(&url, ACCEPT_ANY, 100, FIVE, &|| false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound);
        server.join().unwrap();
    }

    #[test]
    fn a_redirect_between_servers_on_this_machine_is_followed() {
        let (target, target_server) = serve(ok("there"), 1);
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let (url, server) = serve(redirect.into_bytes(), 1);
        let body = get(&url, ACCEPT_ANY, 100, FIVE, &|| false).unwrap();
        assert_eq!(body, b"there");
        server.join().unwrap();
        target_server.join().unwrap();
    }

    #[test]
    fn a_redirect_to_plain_http_elsewhere_is_refused() {
        let redirect = b"HTTP/1.1 302 Found\r\nLocation: http://example.com/x\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let (url, server) = serve(redirect.to_vec(), 1);
        let error = get(&url, ACCEPT_ANY, 100, FIVE, &|| false).unwrap_err();
        assert_ne!(error.kind, ErrorKind::Cancelled);
        server.join().unwrap();
    }

    #[test]
    fn a_refused_connection_is_offline() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/x", listener.local_addr().unwrap());
        drop(listener);
        let error = get(&url, ACCEPT_ANY, 100, FIVE, &|| false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Offline);
    }
}
