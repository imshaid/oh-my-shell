//! An API key in memory.

use std::fmt;

use zeroize::Zeroizing;

/// An API key. It is wiped from memory when dropped, and it never prints: `{:?}` shows
/// `Secret(<hidden>)`. Reading the text takes the explicit call [`Secret::expose`].
///
/// This cannot wipe every copy: the HTTP client copies the key into its own request headers,
/// which are not ours to clear. What it does is keep our own copy short-lived: the setup
/// wizard drops the key as soon as it is verified and stored, and later requests read it from
/// the store again.
pub struct Secret(Zeroizing<String>);

impl Secret {
    /// Wraps a key. Leading and trailing white space is removed (a paste often has a line
    /// ending). `None` for a key that is empty afterwards or has a control character, which
    /// no provider's key has and which would break a header or the credentials file.
    #[must_use]
    pub fn new(text: String) -> Option<Self> {
        let text = Zeroizing::new(text);
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.chars().any(char::is_control) {
            return None;
        }
        Some(Self(Zeroizing::new(trimmed.to_owned())))
    }

    /// Takes a key from a typed line. `bytes` is emptied and wiped, whatever the result, so
    /// the caller is left holding no copy. `None` for the cases [`Secret::new`] refuses and
    /// for bytes that are not UTF-8.
    #[must_use]
    pub fn take(bytes: &mut Vec<u8>) -> Option<Self> {
        let taken = Zeroizing::new(std::mem::take(bytes));
        let text = std::str::from_utf8(&taken).ok()?;
        Self::new(text.to_owned())
    }

    /// The key itself. Pass it straight to the request; do not log it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_never_prints() {
        let secret = Secret::new("sk-very-secret".to_owned()).unwrap();
        assert_eq!(format!("{secret:?}"), "Secret(<hidden>)");
        assert_eq!(format!("{secret:#?}"), "Secret(<hidden>)");
    }

    #[test]
    fn white_space_around_a_pasted_key_is_removed() {
        let secret = Secret::new("  sk-abc \r\n".to_owned()).unwrap();
        assert_eq!(secret.expose(), "sk-abc");
    }

    #[test]
    fn a_typed_line_is_taken_and_the_buffer_is_emptied() {
        let mut line = b"  sk-typed \n".to_vec();
        let secret = Secret::take(&mut line).unwrap();
        assert_eq!(secret.expose(), "sk-typed");
        assert!(line.is_empty());
        // Also when it is refused.
        let mut bad = vec![0xff, 0xfe];
        assert!(Secret::take(&mut bad).is_none());
        assert!(bad.is_empty());
        let mut blank = b"   ".to_vec();
        assert!(Secret::take(&mut blank).is_none());
        assert!(blank.is_empty());
    }

    #[test]
    fn empty_keys_and_keys_with_control_characters_are_refused() {
        assert!(Secret::new(String::new()).is_none());
        assert!(Secret::new(" \n".to_owned()).is_none());
        assert!(Secret::new("sk-a\nb".to_owned()).is_none());
        assert!(Secret::new("sk-a\0b".to_owned()).is_none());
        assert!(Secret::new("sk-a\x1b[31m".to_owned()).is_none());
    }
}
