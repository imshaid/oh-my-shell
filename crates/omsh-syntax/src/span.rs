//! Byte ranges into the source text.

use std::borrow::Cow;

/// A half-open byte range `start..end` into the source text.
///
/// Both ends always fall on UTF-8 character boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    /// Offset of the first byte.
    pub start: usize,
    /// Offset one past the last byte.
    pub end: usize,
}

impl Span {
    /// Creates a span from its two ends.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// Returns the text this span covers, or `""` if `src` is not the text it came from.
    #[must_use]
    pub fn text(self, src: &str) -> &str {
        src.get(self.start..self.end).unwrap_or("")
    }
}

/// Removes backslash-newline pairs, which the shell treats as if they were not there.
///
/// `i\<newline>f` means `if`, so code that compares a piece of source text with a known
/// string should compare the result of this function.
#[must_use]
pub fn strip_line_continuations(text: &str) -> Cow<'_, str> {
    if text.contains("\\\n") {
        Cow::Owned(text.replace("\\\n", ""))
    } else {
        Cow::Borrowed(text)
    }
}
