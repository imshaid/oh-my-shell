//! Here-documents: their delimiters, and finding and scanning their bodies.

use super::events::{Close, HereDocInfo, Open, Sink};
use super::position::ListState;
use super::{Ctx, Scanner};
use crate::span::Span;
use crate::token::LexErrorKind;

/// A here-document whose body starts at the next line ending.
pub(super) struct PendingHereDoc {
    pub(super) delimiter: String,
    pub(super) quoted: bool,
    pub(super) strip_tabs: bool,
    /// The delimiter word, used to point at the problem if the body never ends.
    pub(super) opener: Span,
}

/// Removes quoting from a here-document delimiter. Returns the text and whether any part
/// of it was quoted.
pub(super) fn cook_delimiter(raw: &str) -> (String, bool) {
    let mut out = String::with_capacity(raw.len());
    let mut quoted = false;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(next) => {
                    quoted = true;
                    out.push(next);
                }
            },
            '\'' => {
                quoted = true;
                out.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '"' => {
                quoted = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next() {
                            Some('\n') | None => {}
                            Some(next @ ('$' | '`' | '"' | '\\')) => out.push(next),
                            Some(next) => {
                                out.push('\\');
                                out.push(next);
                            }
                        },
                        _ => out.push(c),
                    }
                }
            }
            _ => out.push(c),
        }
    }
    (out, quoted)
}

/// True if a line ends in an odd number of backslashes, so that it continues on the next.
fn ends_in_continuation(line: &str) -> bool {
    line.bytes().rev().take_while(|&b| b == b'\\').count() % 2 == 1
}

impl<S: Sink> Scanner<'_, S> {
    /// Emits an empty body for here-documents that never reached a line ending, so that
    /// each `<<` still has its body. `/bin/sh` also gives them an empty body when `)`
    /// closes the substitution they are in.
    pub(super) fn abandon_heredocs(&mut self, state: &mut ListState) {
        for doc in std::mem::take(&mut state.pending) {
            self.error(
                LexErrorKind::UnterminatedHereDoc,
                doc.opener.start,
                doc.opener.end,
            );
            self.sink.open(Open::HereDoc, self.pos);
            self.sink.close(
                Close::HereDoc(HereDocInfo {
                    body: Span::new(self.pos, self.pos),
                    end_marker: None,
                    quoted: doc.quoted,
                    strip_tabs: doc.strip_tabs,
                }),
                self.pos,
            );
        }
    }

    pub(super) fn scan_heredoc(&mut self, doc: &PendingHereDoc) {
        let start = self.pos;
        self.sink.open(Open::HereDoc, start);
        let (body_end, end_marker) = self.find_heredoc_end(doc);

        let saved_limit = self.limit;
        self.limit = body_end;
        self.scan_parts(Ctx::HereDoc {
            strip_tabs: doc.strip_tabs,
            expand: !doc.quoted,
        });
        self.limit = saved_limit;

        if !self.aborted {
            self.pos = match end_marker {
                Some(marker) if self.at(marker.end) == Some(b'\n') => marker.end + 1,
                Some(marker) => marker.end,
                None => body_end,
            };
        }
        if end_marker.is_none() {
            self.error(
                LexErrorKind::UnterminatedHereDoc,
                doc.opener.start,
                doc.opener.end,
            );
        }
        self.sink.close(
            Close::HereDoc(HereDocInfo {
                body: Span::new(start, body_end),
                end_marker,
                quoted: doc.quoted,
                strip_tabs: doc.strip_tabs,
            }),
            self.pos,
        );
    }

    /// Looks for the delimiter line. Returns where the body ends and that line's span.
    ///
    /// A line continuation in an unquoted here-document joins two lines, so the line
    /// after one can never be the delimiter.
    fn find_heredoc_end(&self, doc: &PendingHereDoc) -> (usize, Option<Span>) {
        let mut line_start = self.pos;
        let mut continued = false;
        while line_start < self.limit {
            let line_end = self
                .bytes
                .get(line_start..self.limit)
                .and_then(|rest| rest.iter().position(|&b| b == b'\n'))
                .map_or(self.limit, |offset| line_start + offset);
            let line = Span::new(line_start, line_end).text(self.src);
            let candidate = if doc.strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line
            };
            if !continued && candidate == doc.delimiter {
                return (line_start, Some(Span::new(line_start, line_end)));
            }
            continued = !doc.quoted && ends_in_continuation(line);
            line_start = line_end + 1;
        }
        (self.limit, None)
    }
}
