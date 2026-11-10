//! Finds text that looks like a secret (blueprint §14.4).
//!
//! History never stores a line in which [`find`] sees something (§14.2). The answer is a list
//! of byte ranges and not a yes or no, so that the same rules can later mask the secret parts
//! of a text instead of dropping it.
//!
//! The rules are written by hand, on bytes: there are few of them and each is a fixed prefix
//! followed by a run of characters, which does not need a regular-expression crate.
//!
//! A value that starts with `$` is not a secret: `API_KEY=$OTHER` and `--token=$(cat file)`
//! name where the secret is, they do not hold it.

use std::ops::Range;

/// The parts of `text` that look like secrets, in the order they are found. The ranges are byte
/// offsets and always fall on character boundaries. Two rules may report the same part.
#[must_use]
pub fn find(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    assignments(bytes, &mut found);
    bearer(bytes, &mut found);
    key_formats(bytes, &mut found);
    private_keys(bytes, &mut found);
    url_credentials(bytes, &mut found);
    found.sort_by_key(|range| (range.start, range.end));
    found
}

/// True if `text` holds something that looks like a secret.
#[must_use]
pub fn looks_secret(text: &str) -> bool {
    !find(text).is_empty()
}

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn ends_with_ignoring_case(name: &[u8], suffix: &[u8]) -> bool {
    name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// Where a value that starts at `start` ends: at the closing quote if it opens with one,
/// otherwise at the next blank. `None` for a value that is empty or starts with `$`.
fn value_at(bytes: &[u8], start: usize) -> Option<Range<usize>> {
    let first = *bytes.get(start)?;
    if first == b'\'' || first == b'"' {
        let inner = start + 1;
        let len = bytes[inner..]
            .iter()
            .position(|&b| b == first)
            .unwrap_or(bytes.len() - inner);
        if len == 0 || bytes[inner] == b'$' {
            return None;
        }
        return Some(inner..inner + len);
    }
    if first == b'$' || first.is_ascii_whitespace() {
        return None;
    }
    let len = bytes[start..]
        .iter()
        .position(u8::is_ascii_whitespace)
        .unwrap_or(bytes.len() - start);
    Some(start..start + len)
}

/// `*_KEY=`, `*_TOKEN=`, `*_SECRET=` and `password=`, in any case: the value is the secret.
fn assignments(bytes: &[u8], found: &mut Vec<Range<usize>>) {
    for (at, _) in bytes.iter().enumerate().filter(|(_, b)| **b == b'=') {
        let name_start = bytes[..at]
            .iter()
            .rposition(|&b| !is_name_byte(b))
            .map_or(0, |before| before + 1);
        let name = &bytes[name_start..at];
        let secret_name = [&b"_key"[..], b"_token", b"_secret", b"password"]
            .iter()
            .any(|suffix| ends_with_ignoring_case(name, suffix));
        if secret_name && let Some(value) = value_at(bytes, at + 1) {
            found.push(value);
        }
    }
}

/// `Authorization: Bearer <token>`, in any case.
fn bearer(bytes: &[u8], found: &mut Vec<Range<usize>>) {
    const HEAD: &[u8] = b"authorization:";
    const WORD: &[u8] = b"bearer";
    let mut at = 0;
    while at + HEAD.len() <= bytes.len() {
        if !bytes[at..at + HEAD.len()].eq_ignore_ascii_case(HEAD) {
            at += 1;
            continue;
        }
        let mut next = at + HEAD.len();
        while bytes.get(next).is_some_and(|b| *b == b' ' || *b == b'\t') {
            next += 1;
        }
        if bytes.len() >= next + WORD.len()
            && bytes[next..next + WORD.len()].eq_ignore_ascii_case(WORD)
        {
            let mut token = next + WORD.len();
            while bytes.get(token).is_some_and(|b| *b == b' ' || *b == b'\t') {
                token += 1;
            }
            let len = bytes[token..]
                .iter()
                .position(|b| b.is_ascii_whitespace() || *b == b'\'' || *b == b'"')
                .unwrap_or(bytes.len() - token);
            if len > 0 && bytes[token] != b'$' {
                found.push(token..token + len);
            }
        }
        at = next;
    }
}

/// One key format: a fixed start, then at least `min` (and, with `exact`, exactly `min`)
/// characters of the token alphabet.
struct Format {
    prefix: &'static [u8],
    min: usize,
    exact: bool,
    /// Only capital letters and digits after the prefix (AWS).
    upper: bool,
}

/// AWS access key ids, GitHub tokens, Google API keys, and Anthropic and OpenAI keys.
const FORMATS: &[Format] = &[
    Format {
        prefix: b"AKIA",
        min: 16,
        exact: true,
        upper: true,
    },
    Format {
        prefix: b"ASIA",
        min: 16,
        exact: true,
        upper: true,
    },
    Format {
        prefix: b"ghp_",
        min: 36,
        exact: false,
        upper: false,
    },
    Format {
        prefix: b"gho_",
        min: 36,
        exact: false,
        upper: false,
    },
    Format {
        prefix: b"ghu_",
        min: 36,
        exact: false,
        upper: false,
    },
    Format {
        prefix: b"ghs_",
        min: 36,
        exact: false,
        upper: false,
    },
    Format {
        prefix: b"ghr_",
        min: 36,
        exact: false,
        upper: false,
    },
    Format {
        prefix: b"github_pat_",
        min: 22,
        exact: false,
        upper: false,
    },
    Format {
        prefix: b"AIza",
        min: 35,
        exact: true,
        upper: false,
    },
    // Covers `sk-ant-...` (Anthropic) and `sk-proj-...` (OpenAI) too.
    Format {
        prefix: b"sk-",
        min: 20,
        exact: false,
        upper: false,
    },
];

fn key_formats(bytes: &[u8], found: &mut Vec<Range<usize>>) {
    for at in 0..bytes.len() {
        // A key starts a word: `task-...` is not `sk-...`.
        if at > 0 && is_token_byte(bytes[at - 1]) {
            continue;
        }
        for format in FORMATS {
            if !bytes[at..].starts_with(format.prefix) {
                continue;
            }
            let body = at + format.prefix.len();
            let fits = |b: &u8| {
                if format.upper {
                    b.is_ascii_uppercase() || b.is_ascii_digit()
                } else {
                    is_token_byte(*b)
                }
            };
            let len = bytes[body..].iter().take_while(|b| fits(b)).count();
            let enough = if format.exact {
                len == format.min
            } else {
                len >= format.min
            };
            if enough {
                found.push(at..body + len);
            }
        }
    }
}

/// `-----BEGIN ... PRIVATE KEY-----`, up to the matching `END` line if there is one.
fn private_keys(bytes: &[u8], found: &mut Vec<Range<usize>>) {
    const BEGIN: &[u8] = b"-----BEGIN ";
    const MARK: &[u8] = b"PRIVATE KEY-----";
    const END: &[u8] = b"-----END ";
    let mut at = 0;
    while let Some(start) = position(&bytes[at..], BEGIN).map(|found| at + found) {
        let line_end = bytes[start..]
            .iter()
            .position(|b| *b == b'\n')
            .map_or(bytes.len(), |len| start + len);
        let Some(mark) = position(&bytes[start..line_end], MARK) else {
            at = start + BEGIN.len();
            continue;
        };
        let after_mark = start + mark + MARK.len();
        let end = position(&bytes[after_mark..], END).map_or(bytes.len(), |found| {
            let end_line = after_mark + found;
            position(&bytes[end_line + END.len()..], b"-----")
                .map_or(bytes.len(), |dashes| end_line + END.len() + dashes + 5)
        });
        found.push(start..end);
        at = end.max(after_mark);
    }
}

/// `scheme://user:password@host`: the password is the secret.
fn url_credentials(bytes: &[u8], found: &mut Vec<Range<usize>>) {
    let mut at = 0;
    while let Some(scheme) = position(&bytes[at..], b"://").map(|found| at + found) {
        let start = scheme + 3;
        at = start;
        // The user information ends at the first `@` before any `/`, blank or quote.
        let Some(len) = bytes[start..].iter().position(|b| {
            *b == b'@' || *b == b'/' || b.is_ascii_whitespace() || *b == b'\'' || *b == b'"'
        }) else {
            continue;
        };
        if bytes[start + len] != b'@' {
            continue;
        }
        let user_info = &bytes[start..start + len];
        if let Some(colon) = user_info.iter().position(|b| *b == b':') {
            let password = start + colon + 1..start + len;
            if !password.is_empty() && bytes[password.start] != b'$' {
                found.push(password);
            }
        }
    }
}

/// Where `needle` first is in `haystack`.
fn position(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The secret parts of `text`, as text.
    fn parts(text: &str) -> Vec<&str> {
        find(text).into_iter().map(|range| &text[range]).collect()
    }

    #[test]
    fn assignments_to_secret_names() {
        assert_eq!(parts("export API_KEY=abc123"), ["abc123"]);
        assert_eq!(parts("GITHUB_TOKEN=xyz gh pr list"), ["xyz"]);
        assert_eq!(parts("CLIENT_SECRET='a b' run"), ["a b"]);
        assert_eq!(parts("mysql --password=hunter2 db"), ["hunter2"]);
        assert_eq!(parts("PGPASSWORD=pw psql"), ["pw"]);
        assert_eq!(parts("curl -d 'api_key=k1&x=1'"), ["k1&x=1'"]);
        assert_eq!(parts("my_token=\"t t\""), ["t t"]);
    }

    #[test]
    fn assignments_that_hold_no_secret() {
        for text in [
            "KEY=value",
            "MONKEY=1",
            "TOKENS=3",
            "export API_KEY=",
            "API_KEY= cmd",
            "API_KEY=$OTHER_VALUE cmd",
            "cmd --token=$(cat file)",
            "API_KEY=\"$FROM_ENV\"",
            "echo password",
            "x=1 y=2",
            "test a = b",
        ] {
            assert_eq!(parts(text), Vec::<&str>::new(), "{text}");
        }
    }

    #[test]
    fn bearer_tokens() {
        assert_eq!(
            parts("curl -H 'Authorization: Bearer abc.def' url"),
            ["abc.def"]
        );
        assert_eq!(parts("curl -H \"authorization:bearer  T0k\""), ["T0k"]);
    }

    #[test]
    fn bearer_without_a_literal_token() {
        for text in [
            "curl -H \"Authorization: Bearer $TOKEN\" url",
            "curl -H 'Authorization: Basic abc'",
            "echo Authorization: Bearer",
            "echo bearer of news",
        ] {
            assert_eq!(parts(text), Vec::<&str>::new(), "{text}");
        }
    }

    #[test]
    fn key_formats_of_the_known_services() {
        let aws = "AKIAIOSFODNN7EXAMPLE";
        let github = format!("ghp_{}", "a1B2".repeat(9));
        let pat = format!("github_pat_{}", "x_1".repeat(8));
        let google = format!("AIza{}", "Sy-_9".repeat(7));
        let anthropic = format!("sk-ant-api03-{}", "q".repeat(24));
        let openai = format!("sk-proj-{}", "Z9".repeat(12));
        let old_openai = format!("sk-{}", "a".repeat(20));
        for key in [
            aws,
            &github,
            &pat,
            &google,
            &anthropic,
            &openai,
            &old_openai,
        ] {
            let text = format!("echo {key} | cmd");
            assert_eq!(parts(&text), [key], "{text}");
        }
        for prefix in ["gho_", "ghu_", "ghs_", "ghr_"] {
            let key = format!("{prefix}{}", "0".repeat(36));
            assert_eq!(parts(&key), [key.as_str()]);
        }
        let temporary = "ASIAABCDEFGHIJKLMNOP";
        assert_eq!(parts(temporary), [temporary]);
    }

    #[test]
    fn text_that_only_resembles_a_key() {
        let short_github = format!("ghp_{}", "a".repeat(35));
        let long_aws = "AKIAIOSFODNN7EXAMPLEX";
        let lower_aws = "AKIAiosfodnn7example";
        let short_google = format!("AIza{}", "a".repeat(34));
        let inside_word = format!("task-{}", "a".repeat(30));
        for text in [
            short_github.as_str(),
            long_aws,
            lower_aws,
            short_google.as_str(),
            inside_word.as_str(),
            "sk-short",
            "git push origin sk-feature-branch",
            "ls github_pat_",
        ] {
            assert_eq!(parts(text), Vec::<&str>::new(), "{text}");
        }
    }

    #[test]
    fn private_key_blocks() {
        let one_line = "echo '-----BEGIN OPENSSH PRIVATE KEY-----' > k";
        assert_eq!(
            parts(one_line),
            ["-----BEGIN OPENSSH PRIVATE KEY-----' > k"]
        );
        let block =
            "cat <<E\n-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\nE";
        assert_eq!(
            parts(block),
            ["-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----"]
        );
        assert_eq!(parts("-----BEGIN PRIVATE KEY-----").len(), 1);
    }

    #[test]
    fn blocks_that_are_not_private_keys() {
        for text in [
            "echo '-----BEGIN CERTIFICATE-----'",
            "echo '-----BEGIN PUBLIC KEY-----'",
            "echo -----BEGIN \nPRIVATE KEY-----",
            "grep 'PRIVATE KEY' file",
        ] {
            assert_eq!(parts(text), Vec::<&str>::new(), "{text}");
        }
    }

    #[test]
    fn urls_with_a_password() {
        assert_eq!(
            parts("git clone https://me:s3cret@example.com/r.git"),
            ["s3cret"]
        );
        assert_eq!(parts("psql postgres://u:p@h:5432/db"), ["p"]);
    }

    #[test]
    fn urls_without_a_password() {
        for text in [
            "curl https://example.com/a:b@c",
            "ssh://git@example.com/repo",
            "curl http://host:8080/path",
            "curl https://user:@example.com",
            "curl https://user:$PASS@example.com",
            "scp a b:c@d",
            "echo :// x:y@z",
        ] {
            assert_eq!(parts(text), Vec::<&str>::new(), "{text}");
        }
    }

    #[test]
    fn ranges_are_in_order_and_on_character_boundaries() {
        let text = "ক API_KEY=গোপন curl https://u:পাস@h";
        let found = find(text);
        assert_eq!(found.len(), 2);
        assert!(found[0].start < found[1].start);
        for range in found {
            assert!(text.is_char_boundary(range.start) && text.is_char_boundary(range.end));
        }
        assert_eq!(parts(text), ["গোপন", "পাস"]);
    }

    #[test]
    fn plain_commands_hold_no_secret() {
        for text in [
            "",
            "ls -la",
            "git commit -m 'fix: key handling'",
            "echo $API_KEY",
            "cargo test --workspace",
            "ssh user@host",
        ] {
            assert!(!looks_secret(text), "{text}");
        }
    }
}
