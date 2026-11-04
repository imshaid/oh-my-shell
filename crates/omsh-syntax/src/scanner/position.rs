//! Tracks just enough grammar to know what a word's position means.

use super::events::WordClass;
use super::heredoc::PendingHereDoc;
use super::{is_name_char, is_name_start};
use crate::token::{Op, ReservedWord};

/// What the next word's position means after `for` or `case`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Expect {
    Nothing,
    ForName,
    ForAfterName,
    CaseWord,
    CaseIn,
}

/// The state of one command list: the whole input or one `$(...)`.
///
/// It tracks just enough grammar to tell a command from an argument, because `)` in
/// `$(case x in a) ...; esac)` closes a pattern and not the substitution, and the word
/// `case` only opens a case statement where a command could start.
pub(super) struct ListState {
    /// The next word could name a command.
    command_position: bool,
    /// The next word is a `case` pattern.
    pattern_position: bool,
    /// The next word is the very first of a pattern, the only place where `esac` closes
    /// the case. `(esac)` and `a|esac` are ordinary patterns.
    pattern_start: bool,
    /// An assignment or redirection came before the next word. It can still name a
    /// command, but it is no longer the first word, so it cannot be a reserved word.
    /// `X=1 if` runs a command called `if`.
    command_prefix_seen: bool,
    /// The last thing read ended a compound command. A redirection after it belongs to
    /// that command, so it does not count as a prefix for the next word.
    after_compound: bool,
    /// The next word is the target of a redirection.
    after_redirect: bool,
    /// The next word is a here-document delimiter; the value is `strip_tabs`.
    pub(super) want_delimiter: Option<bool>,
    expect: Expect,
    /// Open `(` in this list.
    parens: u32,
    /// For each open `case`, the value of `parens` when it started.
    case_stack: Vec<u32>,
    pub(super) pending: Vec<PendingHereDoc>,
}

impl ListState {
    pub(super) const fn new() -> Self {
        Self {
            command_position: true,
            pattern_position: false,
            pattern_start: false,
            command_prefix_seen: false,
            after_compound: false,
            after_redirect: false,
            want_delimiter: None,
            expect: Expect::Nothing,
            parens: 0,
            case_stack: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Handles `)`. Returns true if no `(` or pattern in this list claims it.
    pub(super) fn close_paren(&mut self) -> bool {
        self.after_redirect = false;
        self.want_delimiter = None;
        self.command_prefix_seen = false;
        self.after_compound = true;
        self.command_position = true;
        if self.case_stack.last() == Some(&self.parens) {
            self.pattern_position = false;
            return false;
        }
        if self.parens > 0 {
            self.parens -= 1;
            return false;
        }
        true
    }

    pub(super) fn after_operator(&mut self, op: Op) {
        if op.is_redirect() {
            self.after_redirect = true;
            self.command_prefix_seen |= self.command_position && !self.after_compound;
            self.want_delimiter = match op {
                Op::HereDoc => Some(false),
                Op::HereDocStripTabs => Some(true),
                _ => None,
            };
            return;
        }
        self.after_redirect = false;
        self.want_delimiter = None;
        self.expect = Expect::Nothing;
        self.command_prefix_seen = false;
        self.after_compound = false;
        if self.pattern_position {
            // `|` and an opening `(` are part of the pattern. Only a closing `)` ends it.
            self.pattern_start = false;
            return;
        }
        match op {
            Op::LeftParen => self.parens += 1,
            Op::DoubleSemi if !self.case_stack.is_empty() => {
                self.enter_pattern();
                return;
            }
            _ => {}
        }
        self.command_position = true;
    }

    pub(super) fn after_newline(&mut self) {
        self.after_redirect = false;
        self.want_delimiter = None;
        self.command_prefix_seen = false;
        self.after_compound = false;
        if !self.pattern_position {
            self.command_position = true;
        }
    }

    fn enter_pattern(&mut self) {
        self.pattern_position = true;
        self.pattern_start = true;
        self.command_position = false;
    }

    /// Decides what a word is and updates the state for the word after it.
    pub(super) fn classify(&mut self, text: &str, before_redirect: bool) -> WordClass {
        if before_redirect && !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
            return WordClass::IoNumber;
        }
        if self.want_delimiter.is_some() {
            // The caller reads the value and records the pending here-document.
            self.after_redirect = false;
            return WordClass::HereDocDelimiter;
        }
        if self.after_redirect {
            self.after_redirect = false;
            return WordClass::Plain;
        }
        let reserved = ReservedWord::from_text(text);
        match self.expect {
            Expect::Nothing => {}
            Expect::ForName => {
                self.expect = Expect::ForAfterName;
                return WordClass::Plain;
            }
            Expect::ForAfterName => {
                self.expect = Expect::Nothing;
                return match reserved {
                    Some(ReservedWord::In) => WordClass::Keyword,
                    Some(ReservedWord::Do) => {
                        self.command_position = true;
                        WordClass::Keyword
                    }
                    _ => WordClass::Plain,
                };
            }
            Expect::CaseWord => {
                self.expect = Expect::CaseIn;
                return WordClass::Plain;
            }
            Expect::CaseIn => {
                self.expect = Expect::Nothing;
                if reserved == Some(ReservedWord::In) {
                    self.enter_pattern();
                    return WordClass::Keyword;
                }
                return WordClass::Plain;
            }
        }
        if self.pattern_position {
            if self.pattern_start && reserved == Some(ReservedWord::Esac) {
                self.close_case();
                return WordClass::Keyword;
            }
            self.pattern_start = false;
            return WordClass::Plain;
        }
        if !self.command_position {
            return WordClass::Plain;
        }
        self.after_compound = false;
        let reserved = reserved.filter(|_| !self.command_prefix_seen);
        match reserved {
            Some(ReservedWord::For) => {
                self.expect = Expect::ForName;
                self.command_position = false;
                WordClass::Keyword
            }
            Some(ReservedWord::Case) => {
                self.expect = Expect::CaseWord;
                self.command_position = false;
                self.case_stack.push(self.parens);
                WordClass::Keyword
            }
            Some(ReservedWord::Esac) => {
                self.close_case();
                WordClass::Keyword
            }
            Some(ReservedWord::Fi | ReservedWord::Done | ReservedWord::RightBrace) => {
                self.after_compound = true;
                WordClass::Keyword
            }
            Some(_) => WordClass::Keyword,
            None if assignment_name_len(text).is_some() => {
                self.command_prefix_seen = true;
                WordClass::Assignment
            }
            None => {
                self.command_position = false;
                WordClass::Command
            }
        }
    }

    fn close_case(&mut self) {
        self.case_stack.pop();
        self.pattern_position = false;
        self.command_position = true;
        self.after_compound = true;
    }
}

/// If `text` starts with `NAME=`, returns the length of `NAME`.
pub(super) fn assignment_name_len(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let first = *bytes.first()?;
    if !is_name_start(first) {
        return None;
    }
    let len = bytes.iter().take_while(|&&b| is_name_char(b)).count();
    (bytes.get(len) == Some(&b'=')).then_some(len)
}
