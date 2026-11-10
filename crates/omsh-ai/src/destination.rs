//! Where a key may be sent.
//!
//! A key travels in a request header (or, for Gemini, in the address). Sent over plain HTTP it
//! can be read by anyone on the path, and a typo in `base_url` would hand it to a stranger. So
//! a key is sent only over `https://`, with one exception: a server on this very machine
//! (`localhost`, `127.0.0.1`, `::1`), which never leaves it. Every request that carries a key
//! goes through [`key_may_go_to`] first.

/// Whether a key may be sent to `url`.
///
/// True for `https://host...` and for `http://` when the host is exactly `localhost`,
/// `127.0.0.1` or `::1` (any case, with or without a port). Everything else is false, also an
/// address with a user name in it (`http://localhost@evil.example/`) and anything that is not
/// a plain `http(s)://` address.
#[must_use]
pub fn key_may_go_to(url: &str) -> bool {
    let (secure, rest) = if let Some(rest) = strip_prefix_ignoring_case(url, "https://") {
        (true, rest)
    } else if let Some(rest) = strip_prefix_ignoring_case(url, "http://") {
        (false, rest)
    } else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') || authority.contains('\\') {
        return false;
    }
    let Some(host) = host_of(authority) else {
        return false;
    };
    secure || is_this_machine(&host)
}

/// The part of `url` that names the server (`host` or `host:port`), for a message. Anything
/// before an `@` (a user name, a password) is left out, and so is the path.
#[must_use]
pub fn server_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    authority
        .rsplit_once('@')
        .map_or(authority, |(_, server)| server)
        .to_owned()
}

/// The host part of `host`, `host:port`, `[v6]` or `[v6]:port`, lowercase; `None` if it is
/// empty or the port is not a number.
fn host_of(authority: &str) -> Option<String> {
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (host, after) = inner.split_once(']')?;
        // Only a port may follow the bracket: `[::1]x` and `[::1].evil.example` are not hosts.
        let port = if after.is_empty() {
            None
        } else {
            Some(after.strip_prefix(':')?)
        };
        (host, port)
    } else {
        match authority.split_once(':') {
            // A second colon outside brackets is not a host and a port.
            Some((_, port)) if port.contains(':') => return None,
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    if let Some(port) = port
        && (port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

fn is_this_machine(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn strip_prefix_ignoring_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_is_always_fine() {
        for url in [
            "https://api.openai.com/v1/",
            "https://api.anthropic.com",
            "HTTPS://Example.org:8443/v1",
            "https://10.0.0.1/v1",
            "https://[2001:db8::1]:443/",
        ] {
            assert!(key_may_go_to(url), "{url}");
        }
    }

    #[test]
    fn plain_http_is_fine_only_for_this_machine() {
        for url in [
            "http://localhost",
            "http://localhost:11434",
            "http://localhost:8080/v1/",
            "http://LOCALHOST:8080",
            "http://127.0.0.1",
            "http://127.0.0.1:1234/v1",
            "http://[::1]",
            "http://[::1]:8080/v1",
        ] {
            assert!(key_may_go_to(url), "{url}");
        }
    }

    #[test]
    fn plain_http_to_anywhere_else_is_refused() {
        for url in [
            "http://example.com",
            "http://example.com:443/",
            "http://127.0.0.2",
            "http://127.1",
            "http://localhost.evil.com",
            "http://127.0.0.1.evil.com",
            "http://10.0.0.1:8080",
            "http://192.168.1.5",
            "http://[::2]",
            "http://[2001:db8::1]",
            "http://0.0.0.0",
            "http://localhost.",
        ] {
            assert!(!key_may_go_to(url), "{url}");
        }
    }

    #[test]
    fn tricks_with_the_address_are_refused() {
        for url in [
            // The real host is after the `@`.
            "http://localhost@evil.example/",
            "http://localhost:80@evil.example/",
            "https://user:pass@example.org/",
            "http://127.0.0.1@evil.example",
            // A backslash is read as a slash by some clients.
            "http://localhost\\@evil.example/",
            "http://evil.example\\.localhost/",
            // The host is only up to the first slash, question mark or hash.
            "http://evil.example#@localhost",
            "http://evil.example?.localhost",
        ] {
            assert!(!key_may_go_to(url), "{url}");
        }
    }

    #[test]
    fn a_message_names_the_server_and_nothing_else() {
        assert_eq!(
            server_of("http://example.com:8080/v1/x?y=1"),
            "example.com:8080"
        );
        assert_eq!(
            server_of("http://user:secret@example.com/v1"),
            "example.com"
        );
        assert_eq!(server_of("example.com/path"), "example.com");
    }

    #[test]
    fn what_is_not_a_plain_web_address_is_refused() {
        for url in [
            "",
            "localhost",
            "ftp://localhost",
            "file:///etc/passwd",
            "http://",
            "https://",
            "https:///path",
            "http://:8080",
            "http://localhost:",
            "http://localhost:80x",
            "http://::1",
            "http://[::1",
            "http://[::1]x",
            "http://[::1].evil.example",
            "//localhost",
            "javascript:alert(1)",
        ] {
            assert!(!key_may_go_to(url), "{url}");
        }
    }
}
