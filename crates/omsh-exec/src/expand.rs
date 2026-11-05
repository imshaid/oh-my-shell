//! S05's stand-in for word expansion.
//!
//! Real expansion (parameters, `$(...)`, globbing, tilde) arrives in S06. Until then a word
//! is run only if its meaning is plain: it is the text it shows, with the quotes taken off.
//! Everything else is refused with a [`NotYet`], and nothing in the line runs. Running such a
//! word with a different meaning (for example `rm *.tmp` with a literal `*.tmp`) would be
//! worse than refusing it.

use omsh_syntax::Span;
use omsh_syntax::ast::{PartKind, Word, WordPart};

/// Something the shell cannot run yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotYet {
    /// What it is, as a noun phrase: "pipelines" in "pipelines are not implemented yet".
    pub what: &'static str,
    /// Where it is in the line.
    pub span: Span,
}

impl NotYet {
    pub(crate) const fn new(what: &'static str, span: Span) -> Self {
        Self { what, span }
    }
}

/// Where a word is used, because tildes mean different things in the two places.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Position {
    /// A command name or argument.
    Argument,
    /// The value of `NAME=value`, where a tilde also counts after each `:`.
    AssignmentValue,
}

/// Turns a word into the text it stands for, or says why it cannot yet.
pub(crate) fn expand_word(word: &Word, position: Position) -> Result<String, NotYet> {
    let mut text = String::new();
    let mut glob = GlobScan::default();
    for (index, part) in word.parts.iter().enumerate() {
        match &part.kind {
            PartKind::Literal(s) => {
                check_unquoted(s, index == 0, position, part.span, &mut glob)?;
                text.push_str(s);
            }
            PartKind::Escaped(c) => {
                text.push(*c);
            }
            PartKind::SingleQuoted(s) => {
                text.push_str(s);
            }
            PartKind::DoubleQuoted(parts) => {
                push_quoted(parts, &mut text)?;
            }
            PartKind::AnsiCQuoted(_) => {
                return Err(NotYet::new("`$'...'` quoting", part.span));
            }
            PartKind::Parameter(_) => {
                return Err(NotYet::new("parameter expansion", part.span));
            }
            PartKind::CommandSub(_) => {
                return Err(NotYet::new("command substitution", part.span));
            }
            PartKind::Arithmetic(_) => {
                return Err(NotYet::new("arithmetic expansion", part.span));
            }
        }
    }
    Ok(text)
}

/// The text of the pieces inside double quotes. Characters there are never special to the
/// shell's pattern matching, but `$` forms still need S06.
fn push_quoted(parts: &[WordPart], text: &mut String) -> Result<(), NotYet> {
    for part in parts {
        match &part.kind {
            PartKind::Literal(s) | PartKind::SingleQuoted(s) => text.push_str(s),
            PartKind::Escaped(c) => text.push(*c),
            PartKind::Parameter(_) => {
                return Err(NotYet::new("parameter expansion", part.span));
            }
            PartKind::CommandSub(_) => {
                return Err(NotYet::new("command substitution", part.span));
            }
            PartKind::Arithmetic(_) => {
                return Err(NotYet::new("arithmetic expansion", part.span));
            }
            PartKind::AnsiCQuoted(_) | PartKind::DoubleQuoted(_) => {
                return Err(NotYet::new("this quoting", part.span));
            }
        }
    }
    Ok(())
}

/// Tracks whether an unquoted `[` is waiting for a `]`.
#[derive(Default)]
struct GlobScan {
    open_bracket: bool,
}

/// Refuses unquoted text that the shell would have to treat as a pattern or a tilde.
///
/// A lone `[` or `]` is fine, because it is a pattern only when the other one follows: this
/// keeps `[ -f x ]` working.
fn check_unquoted(
    text: &str,
    first_part: bool,
    position: Position,
    span: Span,
    glob: &mut GlobScan,
) -> Result<(), NotYet> {
    if first_part && text.starts_with('~') {
        return Err(NotYet::new("tilde expansion", span));
    }
    if position == Position::AssignmentValue && text.contains(":~") {
        return Err(NotYet::new("tilde expansion", span));
    }
    if position == Position::AssignmentValue {
        // The value of an assignment is never matched against file names.
        return Ok(());
    }
    for c in text.chars() {
        match c {
            '*' | '?' => return Err(NotYet::new("pathname expansion", span)),
            '[' => glob.open_bracket = true,
            ']' if glob.open_bracket => {
                return Err(NotYet::new("pathname expansion", span));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use omsh_syntax::ast::CommandKind;
    use omsh_syntax::parse;

    use super::*;

    /// Expands the first word of the first command of `src`.
    fn expand(src: &str) -> Result<String, NotYet> {
        let list = parse(src).unwrap();
        let first = &list.items[0].and_or.first.commands[0];
        let CommandKind::Simple(simple) = &first.kind else {
            panic!("not a simple command");
        };
        expand_word(&simple.words[0], Position::Argument)
    }

    fn refused(src: &str) -> &'static str {
        expand(src).unwrap_err().what
    }

    #[test]
    fn plain_text_stays_as_it_is() {
        assert_eq!(expand("hello").as_deref(), Ok("hello"));
        assert_eq!(expand("a.b-c_d/e").as_deref(), Ok("a.b-c_d/e"));
    }

    #[test]
    fn quotes_and_escapes_are_removed() {
        assert_eq!(expand(r#"'a b'"c d"e\ f"#).as_deref(), Ok("a bc de f"));
        assert_eq!(expand(r#""a\"b""#).as_deref(), Ok(r#"a"b"#));
        assert_eq!(expand(r#""a\\b""#).as_deref(), Ok("a\\b"));
    }

    #[test]
    fn a_quoted_glob_character_is_text() {
        assert_eq!(expand(r#""*.rs""#).as_deref(), Ok("*.rs"));
        assert_eq!(expand("'?'").as_deref(), Ok("?"));
        assert_eq!(expand(r"\*").as_deref(), Ok("*"));
        assert_eq!(expand(r#"a"["b]"#).as_deref(), Ok("a[b]"));
    }

    #[test]
    fn a_lone_bracket_is_text() {
        assert_eq!(expand("[").as_deref(), Ok("["));
        assert_eq!(expand("]").as_deref(), Ok("]"));
        assert_eq!(expand("]a[").as_deref(), Ok("]a["));
    }

    #[test]
    fn patterns_are_refused() {
        assert_eq!(refused("*.rs"), "pathname expansion");
        assert_eq!(refused("a?"), "pathname expansion");
        assert_eq!(refused("[abc]"), "pathname expansion");
        assert_eq!(refused("[!a]x"), "pathname expansion");
    }

    #[test]
    fn a_leading_tilde_is_refused_but_a_later_one_is_text() {
        assert_eq!(refused("~"), "tilde expansion");
        assert_eq!(refused("~/x"), "tilde expansion");
        assert_eq!(expand("a~").as_deref(), Ok("a~"));
        assert_eq!(expand(r#""~""#).as_deref(), Ok("~"));
        assert_eq!(expand(r"\~").as_deref(), Ok("~"));
    }

    #[test]
    fn dollar_forms_are_refused_with_their_place() {
        assert_eq!(refused("$x"), "parameter expansion");
        assert_eq!(refused("${x:-y}"), "parameter expansion");
        assert_eq!(refused("$(true)"), "command substitution");
        assert_eq!(refused("`true`"), "command substitution");
        assert_eq!(refused("$((1+2))"), "arithmetic expansion");
        assert_eq!(refused("$'a'"), "`$'...'` quoting");

        let error = expand(r#"ab"c$x""#).unwrap_err();
        assert_eq!(error.what, "parameter expansion");
        assert_eq!((error.span.start, error.span.end), (4, 6));
    }

    #[test]
    fn a_tilde_after_a_colon_counts_only_in_an_assignment_value() {
        let list = parse("A=x:~/y true").unwrap();
        let CommandKind::Simple(simple) = &list.items[0].and_or.first.commands[0].kind else {
            panic!("not a simple command");
        };
        let value = &simple.assignments[0].value;
        let error = expand_word(value, Position::AssignmentValue).unwrap_err();
        assert_eq!(error.what, "tilde expansion");
        assert_eq!(
            expand_word(value, Position::Argument).as_deref(),
            Ok("x:~/y")
        );
    }

    #[test]
    fn a_glob_character_in_an_assignment_value_is_text() {
        let list = parse("A=*.rs true").unwrap();
        let CommandKind::Simple(simple) = &list.items[0].and_or.first.commands[0].kind else {
            panic!("not a simple command");
        };
        let value = &simple.assignments[0].value;
        assert_eq!(
            expand_word(value, Position::AssignmentValue).as_deref(),
            Ok("*.rs")
        );
    }
}
