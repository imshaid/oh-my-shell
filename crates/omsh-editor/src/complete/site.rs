//! Where the cursor is in the line, for completion: which word is being typed, what kind of
//! place it is (a command name, an argument, the target of a redirection), which command the
//! argument belongs to, and how the word is quoted. Also the rules for writing a name back into
//! the line (backslashes, quotes).
//!
//! Everything here is a pure function of the text before the cursor, so each rule can be tested
//! without a terminal.

use omsh_syntax::{HighlightKind, Highlighted, Trigger, lex_for_highlight, trigger};

/// What kind of place the word is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// A command name.
    Command,
    /// An argument of a command.
    Argument,
    /// The file after `<`, `>`, `>>` and the like.
    Target,
}

/// How the word is quoted at the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Quote {
    None,
    Single,
    Double,
}

/// The word under the cursor and what surrounds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Site {
    pub slot: Slot,
    /// Byte offset in the line where the word starts.
    pub start: usize,
    /// Byte offset of the cursor: the word is `line[start..end]`.
    pub end: usize,
    /// The word as typed, with its quotes and backslashes. Empty after a blank.
    pub raw: String,
    /// The command an argument belongs to, as typed without quotes. `None` for a command name.
    pub command: Option<String>,
    /// The arguments of that command in front of this one, without quotes.
    pub before: Vec<String>,
}

impl Site {
    /// How the end of the word is quoted.
    pub(crate) fn quote(&self) -> Quote {
        quote_state(&self.raw)
    }

    /// What the word says with its quotes and backslashes taken away. `None` if it holds an
    /// expansion (`$`, a backtick) that cannot be known from the text.
    pub(crate) fn decoded(&self) -> Option<String> {
        crate::highlight::literal_value(&self.raw)
    }

    /// The name of the variable being typed: where in the line the name starts, whether it is
    /// written `${name`, and the letters typed so far. `None` if the word does not end in one.
    pub(crate) fn variable(&self) -> Option<Variable> {
        let at = self.raw.rfind('$')?;
        let backslashes = self.raw[..at]
            .bytes()
            .rev()
            .take_while(|&b| b == b'\\')
            .count();
        if backslashes % 2 == 1 || quote_state(&self.raw[..at]) == Quote::Single {
            return None;
        }
        let tail = &self.raw[at + 1..];
        let (braced, name) = match tail.strip_prefix('{') {
            Some(name) => (true, name),
            None => (false, tail),
        };
        let valid = name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        valid.then(|| Variable {
            start: self.start + at + 1 + usize::from(braced),
            braced,
            typed: name.to_owned(),
        })
    }
}

/// A variable name being typed (see [`Site::variable`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Variable {
    pub start: usize,
    pub braced: bool,
    pub typed: String,
}

/// Reserved words after which the next word is not a command name.
const WORDS_BEFORE_ARGUMENTS: &[&str] = &["in", "for", "case", "select", "function"];

/// Finds the word to complete in `line`, with the cursor at byte `pos`. `None` where nothing is
/// completed: in an AI request, in a comment, in a here-document, in an assignment.
pub(crate) fn locate(line: &str, pos: usize) -> Option<Site> {
    if pos > line.len() || !line.is_char_boundary(pos) {
        return None;
    }
    match trigger(line) {
        Some(Trigger::Pipe { at, .. }) if pos <= at => {}
        Some(_) => return None,
        None => {}
    }
    let prefix = &line[..pos];
    let lexed = lex_for_highlight(prefix);
    if lexed.spans.iter().any(|piece| {
        piece.span.end >= pos
            && matches!(
                piece.kind,
                HighlightKind::Comment | HighlightKind::HereDocBody
            )
    }) {
        return None;
    }

    // The word that ends at the cursor; of several nested ones, the innermost (it starts last).
    let touching = lexed
        .words
        .iter()
        .filter(|word| word.span.end == pos && word.span.start < pos)
        .max_by_key(|word| word.span.start)
        .filter(|word| {
            let text = &prefix[word.span.start..];
            !ends_in_blank(text) && !opens_substitution(text)
        });
    let (start, kind) = match touching {
        Some(word) => (word.span.start, Some(word.kind)),
        None => (pos, None),
    };
    let raw = &prefix[start..];

    let slot = match kind {
        Some(
            HighlightKind::Assignment | HighlightKind::IoNumber | HighlightKind::HereDocDelimiter,
        ) => {
            return None;
        }
        Some(HighlightKind::Command | HighlightKind::Keyword) => Slot::Command,
        Some(_) | None => slot_after(&lexed, prefix, start, kind.is_none()),
    };
    let (command, before) = if slot == Slot::Argument {
        command_of(&lexed, prefix, start)
    } else {
        (None, Vec::new())
    };
    Some(Site {
        slot,
        start,
        end: pos,
        raw: raw.to_owned(),
        command,
        before,
    })
}

/// The slot of a word that starts at `start`, from the piece of the line in front of it.
fn slot_after(lexed: &Highlighted, prefix: &str, start: usize, empty: bool) -> Slot {
    // The blanks inside a substitution (`$(ls )`) are pieces of their own; they say nothing.
    let previous = lexed.spans.iter().rfind(|piece| {
        piece.span.end <= start
            && piece.span.start < start
            && !(piece.kind == HighlightKind::CommandSub
                && prefix[piece.span.start..piece.span.end].trim().is_empty())
    });
    let Some(previous) = previous else {
        // Nothing in front: the start of the line. A word that is not a command was handled by
        // the caller.
        return if empty { Slot::Command } else { Slot::Argument };
    };
    let text = &prefix[previous.span.start..previous.span.end];
    // `FOO=1` is two pieces (`FOO=` and `1`); as a whole it is an assignment, and a command
    // name comes after it.
    let word_before = lexed
        .words
        .iter()
        .filter(|word| word.span.end <= start)
        .max_by_key(|word| word.span.end);
    if word_before.is_some_and(|word| {
        word.kind == HighlightKind::Assignment && word.span.end == previous.span.end
    }) {
        return Slot::Command;
    }
    match previous.kind {
        HighlightKind::Redirect => Slot::Target,
        HighlightKind::Operator
        | HighlightKind::Separator
        | HighlightKind::Assignment
        | HighlightKind::HereDocDelimiter
        | HighlightKind::HereDocBody => Slot::Command,
        // `$(` and a backtick open a command; the `)` that closes one does not.
        HighlightKind::CommandSub if text.trim_end().ends_with(['(', '`']) => Slot::Command,
        HighlightKind::Keyword if !WORDS_BEFORE_ARGUMENTS.contains(&text) => Slot::Command,
        _ => Slot::Argument,
    }
}

/// The command word that the argument at `start` belongs to, and the arguments between them.
fn command_of(lexed: &Highlighted, prefix: &str, start: usize) -> (Option<String>, Vec<String>) {
    // A `;`, `|`, `&&` or similar ends the command; only the words after the last one count.
    let boundary = lexed
        .spans
        .iter()
        .filter(|piece| {
            piece.span.end <= start
                && matches!(
                    piece.kind,
                    HighlightKind::Operator | HighlightKind::Separator
                )
        })
        .map(|piece| piece.span.end)
        .max()
        .unwrap_or(0);
    let mut words: Vec<_> = lexed
        .words
        .iter()
        .filter(|word| word.span.start >= boundary && word.span.end <= start)
        .collect();
    words.sort_by_key(|word| (word.span.start, word.span.end));
    // Only a command name counts: after `for x in` there is no command to ask about.
    let Some(at) = words
        .iter()
        .rposition(|word| word.kind == HighlightKind::Command)
    else {
        return (None, Vec::new());
    };
    let text = |word: &omsh_syntax::HighlightWord| {
        crate::highlight::literal_value(&prefix[word.span.start..word.span.end])
    };
    let before = words[at + 1..]
        .iter()
        .filter(|word| word.kind == HighlightKind::Argument)
        .filter_map(|word| text(word))
        .collect();
    (text(words[at]), before)
}

/// True if the word ends where a substitution opens (`$(` or a backtick): the cursor is at the
/// start of a new command, not in the word.
fn opens_substitution(text: &str) -> bool {
    (text.ends_with("$(") && quote_state(text) != Quote::Single && !ends_in_escape(text))
        || text.ends_with('`')
}

/// True if the text ends in a blank that is not quoted or escaped: the cursor is after a space,
/// not inside a word.
fn ends_in_blank(text: &str) -> bool {
    text.ends_with([' ', '\t']) && quote_state(text) == Quote::None && !ends_in_escape(text)
}

/// True if the last character is escaped by a backslash.
fn ends_in_escape(text: &str) -> bool {
    let without_last = &text[..text.len() - text.chars().next_back().map_or(0, char::len_utf8)];
    without_last
        .bytes()
        .rev()
        .take_while(|&b| b == b'\\')
        .count()
        % 2
        == 1
}

/// The quote that is open at the end of `text`.
pub(crate) fn quote_state(text: &str) -> Quote {
    let mut state = Quote::None;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        state = match (state, c) {
            (Quote::None | Quote::Double, '\\') => {
                chars.next();
                state
            }
            (Quote::None, '\'') => Quote::Single,
            (Quote::None, '"') => Quote::Double,
            (Quote::Single, '\'') | (Quote::Double, '"') => Quote::None,
            _ => state,
        };
    }
    state
}

/// True for a character that may stand in a word without a backslash.
fn is_plain(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | ',' | '+' | '-' | '/' | ':' | '@' | '%' | '=')
}

/// `text` written so that the shell reads it back as the same name: a backslash in front of
/// every character that is not plain. A control character is not handled here (see [`render`],
/// which refuses it first).
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 4);
    for c in text.chars() {
        if !is_plain(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The text to put in the line for a name (`text`: a directory part and a file name).
///
/// - `quote`: how the word is quoted now. The new text keeps that quote: inside `'…'` or
///   `"…"` the name is written with that quote's rules.
/// - `tilde`: the word starts with `~`, which is kept as it is.
/// - `closing`: close an open quote after the name (a file, not a folder).
///
/// `None` if the name cannot be written (a control character).
pub(crate) fn render(quote: Quote, tilde: bool, text: &str, closing: bool) -> Option<String> {
    if text.chars().any(char::is_control) {
        return None;
    }
    Some(match quote {
        Quote::None => match text.strip_prefix('~').filter(|_| tilde) {
            Some(rest) => format!("~{}", escape(rest)),
            None => escape(text),
        },
        Quote::Single => {
            let body = text.replace('\'', "'\\''");
            format!("'{body}{}", if closing { "'" } else { "" })
        }
        Quote::Double => {
            let mut body = String::with_capacity(text.len() + 2);
            for c in text.chars() {
                if matches!(c, '"' | '\\' | '$' | '`') {
                    body.push('\\');
                }
                body.push(c);
            }
            format!("\"{body}{}", if closing { "\"" } else { "" })
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locates the word at the end of `line` (the cursor is at the end).
    fn at_end(line: &str) -> Option<Site> {
        locate(line, line.len())
    }

    fn slot(line: &str) -> Option<Slot> {
        at_end(line).map(|site| site.slot)
    }

    #[test]
    fn the_first_word_is_a_command_name() {
        let site = at_end("gi").unwrap();
        assert_eq!(site.slot, Slot::Command);
        assert_eq!((site.start, site.end, site.raw.as_str()), (0, 2, "gi"));
        assert_eq!(site.command, None);
    }

    #[test]
    fn an_empty_line_is_an_empty_command() {
        let site = at_end("").unwrap();
        assert_eq!(site.slot, Slot::Command);
        assert_eq!(site.raw, "");
    }

    #[test]
    fn after_a_blank_it_is_an_argument_of_the_command() {
        let site = at_end("git checkout ").unwrap();
        assert_eq!(site.slot, Slot::Argument);
        assert_eq!(site.raw, "");
        assert_eq!(site.start, 13);
        assert_eq!(site.command.as_deref(), Some("git"));
        assert_eq!(site.before, vec!["checkout".to_owned()]);
    }

    #[test]
    fn a_word_in_the_middle_of_arguments_knows_its_command_and_what_came_before() {
        let site = at_end("git -C dir ch").unwrap();
        assert_eq!(site.slot, Slot::Argument);
        assert_eq!(site.raw, "ch");
        assert_eq!(site.command.as_deref(), Some("git"));
        assert_eq!(site.before, vec!["-C".to_owned(), "dir".to_owned()]);
    }

    #[test]
    fn after_an_operator_or_a_separator_it_is_a_command_again() {
        for line in [
            "ls | ", "ls && ", "ls || ", "ls; ", "ls &", "ls|", "(", "echo $(", "if ",
        ] {
            assert_eq!(slot(line), Some(Slot::Command), "{line:?}");
        }
        for line in ["ls | gr", "true && ca", "ls;ca"] {
            assert_eq!(slot(line), Some(Slot::Command), "{line:?}");
        }
    }

    #[test]
    fn the_command_is_the_one_after_the_last_separator() {
        let site = at_end("cd /tmp; git ").unwrap();
        assert_eq!(site.command.as_deref(), Some("git"));
        assert!(site.before.is_empty());
        let site = at_end("echo a | git b ").unwrap();
        assert_eq!(site.command.as_deref(), Some("git"));
        assert_eq!(site.before, vec!["b".to_owned()]);
    }

    #[test]
    fn an_argument_with_no_command_of_its_own_does_not_borrow_the_one_before_the_separator() {
        // `for x in ...` has no command word; `ls` ended at the `;`.
        let site = at_end("ls -l; for x in a").unwrap();
        assert_eq!(site.slot, Slot::Argument);
        assert_eq!(site.command, None);
        assert!(site.before.is_empty());
    }

    #[test]
    fn a_command_inside_a_substitution_is_found() {
        let site = at_end("echo $(git ch").unwrap();
        assert_eq!(site.slot, Slot::Argument);
        assert_eq!(site.raw, "ch");
        assert_eq!(site.command.as_deref(), Some("git"));
        let site = at_end("echo \"$(git ch").unwrap();
        assert_eq!(site.raw, "ch");
        assert_eq!(site.command.as_deref(), Some("git"));
    }

    #[test]
    fn a_blank_inside_an_open_substitution_starts_an_empty_argument() {
        let site = at_end("echo $(ls ").unwrap();
        assert_eq!(site.slot, Slot::Argument);
        assert_eq!(site.raw, "");
        assert_eq!(site.command.as_deref(), Some("ls"));
    }

    #[test]
    fn after_a_redirection_it_is_a_file() {
        for line in ["ls > ", "ls >out", "ls 2> ", "cat < in", "ls >> ou"] {
            assert_eq!(slot(line), Some(Slot::Target), "{line:?}");
        }
        assert_eq!(slot("ls out"), Some(Slot::Argument));
    }

    #[test]
    fn assignments_and_numbers_and_delimiters_are_not_completed() {
        assert!(at_end("FOO=ba").is_none());
        assert!(at_end("cat <<EO").is_none());
        // The assignment in front of a command leaves the command name to be completed.
        assert_eq!(slot("FOO=1 ls"), Some(Slot::Command));
        assert_eq!(slot("FOO=1 "), Some(Slot::Command));
    }

    #[test]
    fn keywords_open_a_command_except_in_and_for() {
        for line in ["if ", "while ", "then ", "do ", "else ", "! ", "{ "] {
            assert_eq!(slot(line), Some(Slot::Command), "{line:?}");
        }
        assert_eq!(slot("for x in "), Some(Slot::Argument));
        assert_eq!(slot("case "), Some(Slot::Argument));
    }

    #[test]
    fn nothing_is_completed_in_a_comment_a_here_document_or_an_ai_request() {
        assert!(at_end("ls # com").is_none());
        assert!(at_end("ls #").is_none());
        assert!(at_end("cat <<EOF\nline of text").is_none());
        assert!(at_end("? what is he").is_none());
        assert!(at_end("?? ls -").is_none());
        assert!(at_end("? ").is_none());
    }

    #[test]
    fn a_pipe_to_the_ai_completes_the_command_but_not_the_question() {
        let line = "ls fo | ? what";
        let site = locate(line, 5).unwrap();
        assert_eq!((site.slot, site.raw.as_str()), (Slot::Argument, "fo"));
        assert!(locate(line, line.len()).is_none());
        assert!(locate(line, 9).is_none());
    }

    #[test]
    fn a_hash_in_a_word_or_quotes_is_not_a_comment() {
        assert_eq!(slot("echo a#b"), Some(Slot::Argument));
        assert_eq!(slot("echo '# x"), Some(Slot::Argument));
        // After a comment on an earlier line, a new line is a new command.
        assert_eq!(slot("ls # note\nec"), Some(Slot::Command));
    }

    #[test]
    fn the_cursor_may_be_in_the_middle_of_the_line() {
        let site = locate("git checkout ma && ls", 15).unwrap();
        assert_eq!(site.raw, "ma");
        assert_eq!((site.start, site.end), (13, 15));
        assert!(locate("é", 1).is_none(), "not a character boundary");
        assert!(locate("ls", 9).is_none(), "past the end");
    }

    #[test]
    fn quotes_are_tracked() {
        assert_eq!(quote_state("abc"), Quote::None);
        assert_eq!(quote_state("'ab"), Quote::Single);
        assert_eq!(quote_state("'ab'"), Quote::None);
        assert_eq!(quote_state("\"ab"), Quote::Double);
        assert_eq!(quote_state("\"a\\\"b"), Quote::Double, "escaped quote");
        assert_eq!(quote_state("a\\'b"), Quote::None, "escaped quote");
        assert_eq!(
            quote_state("'a\\'"),
            Quote::None,
            "no escapes in single quotes"
        );
        assert_eq!(quote_state("\"it's"), Quote::Double);
        assert_eq!(quote_state("'say \"hi"), Quote::Single);
    }

    #[test]
    fn a_blank_in_quotes_or_after_a_backslash_does_not_end_the_word() {
        let site = at_end("ls 'my ").unwrap();
        assert_eq!(site.raw, "'my ");
        assert_eq!(site.quote(), Quote::Single);
        let site = at_end("ls my\\ ").unwrap();
        assert_eq!(site.raw, "my\\ ");
        assert_eq!(site.decoded().as_deref(), Some("my "));
        let site = at_end("ls my ").unwrap();
        assert_eq!(site.raw, "");
    }

    #[test]
    fn the_typed_word_is_decoded() {
        assert_eq!(
            at_end("ls 'a b'/c").unwrap().decoded().as_deref(),
            Some("a b/c")
        );
        assert_eq!(
            at_end("ls \"a b\"").unwrap().decoded().as_deref(),
            Some("a b")
        );
        assert_eq!(at_end("ls a$b").unwrap().decoded(), None);
        assert_eq!(at_end("ls 'a$b").unwrap().decoded().as_deref(), Some("a$b"));
    }

    fn variable(line: &str) -> Option<Variable> {
        at_end(line).and_then(|site| site.variable())
    }

    #[test]
    fn a_variable_name_is_found_at_the_end_of_the_word() {
        let found = variable("echo $HO").unwrap();
        assert_eq!(
            (found.start, found.braced, found.typed.as_str()),
            (6, false, "HO")
        );
        let found = variable("echo ${HO").unwrap();
        assert_eq!(
            (found.start, found.braced, found.typed.as_str()),
            (7, true, "HO")
        );
        let found = variable("echo x/$").unwrap();
        assert_eq!((found.start, found.typed.as_str()), (8, ""));
        let found = variable("echo \"a $US").unwrap();
        assert_eq!(found.typed, "US");
        assert!(variable("echo $HOME/b").is_none(), "the name is over");
        assert!(variable("echo 'a $US").is_none(), "single quotes");
        assert!(variable("echo \\$US").is_none(), "escaped");
        assert!(variable("echo \\\\$US").is_some(), "an escaped backslash");
        assert!(variable("echo $(ls").is_none());
        assert!(
            variable("echo $1x2").is_some(),
            "digits are name characters here"
        );
        assert!(variable("echo plain").is_none());
    }

    #[test]
    fn names_are_escaped_for_the_shell() {
        let w = |text: &str| render(Quote::None, false, text, true);
        assert_eq!(w("plain-name_1.txt").as_deref(), Some("plain-name_1.txt"));
        assert_eq!(w("my file").as_deref(), Some("my\\ file"));
        assert_eq!(w("a'b\"c").as_deref(), Some("a\\'b\\\"c"));
        assert_eq!(
            w("$HOME&;|<>()*?[]{}!#~^`\\")
                .as_deref()
                .map(|s| s.matches('\\').count()),
            Some(21)
        );
        assert_eq!(w("dir/sub").as_deref(), Some("dir/sub"));
        assert_eq!(w("naïve-বাংলা").as_deref(), Some("naïve-বাংলা"));
        assert_eq!(w("a\nb"), None);
        assert_eq!(w("a\tb"), None);
        assert_eq!(w("a\x1b[31mb"), None);
    }

    #[test]
    fn a_leading_tilde_is_kept_only_when_it_means_the_home_directory() {
        assert_eq!(
            render(Quote::None, true, "~/my dir", false).as_deref(),
            Some("~/my\\ dir")
        );
        assert_eq!(
            render(Quote::None, false, "~/my", false).as_deref(),
            Some("\\~/my")
        );
        assert_eq!(
            render(Quote::None, false, "a~b", false).as_deref(),
            Some("a\\~b")
        );
    }

    #[test]
    fn names_are_written_in_the_quote_that_is_open() {
        assert_eq!(
            render(Quote::Single, false, "a b", true).as_deref(),
            Some("'a b'")
        );
        assert_eq!(
            render(Quote::Single, false, "a b/", false).as_deref(),
            Some("'a b/")
        );
        assert_eq!(
            render(Quote::Single, false, "it's", true).as_deref(),
            Some("'it'\\''s'")
        );
        assert_eq!(
            render(Quote::Double, false, "a b", true).as_deref(),
            Some("\"a b\"")
        );
        assert_eq!(
            render(Quote::Double, false, "a$b`c\"d\\e", true).as_deref(),
            Some("\"a\\$b\\`c\\\"d\\\\e\"")
        );
        assert_eq!(
            render(Quote::Double, false, "dir/", false).as_deref(),
            Some("\"dir/")
        );
        assert_eq!(render(Quote::Single, false, "a\nb", true), None);
    }

    #[test]
    fn what_render_writes_is_read_back_as_the_same_name() {
        // The decoder the highlighter uses is the inverse of the writer.
        for name in ["a b", "it's", "x\"y", "$HOME", "a*b?c", "tab\u{0}x"] {
            for quote in [Quote::None, Quote::Single, Quote::Double] {
                let Some(written) = render(quote, false, name, true) else {
                    assert!(name.chars().any(char::is_control));
                    continue;
                };
                assert_eq!(
                    crate::highlight::literal_value(&written).as_deref(),
                    Some(name),
                    "{quote:?} {name:?} -> {written:?}"
                );
            }
        }
    }
}
