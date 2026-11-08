//! Lexer tests through the public API: snapshots per §6.1 category, a check that every
//! kind is exercised, invariants on every prefix of every line, and comparisons with
//! `/bin/sh`.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::process::Command;
use std::time::{Duration, Instant};

mod common;

use proptest::prelude::*;

use omsh_syntax::{
    HighlightKind, LexErrorKind, Lexed, Op, ParamOp, PartKind, Span, Token, TokenKind, Word,
    WordPart, lex, lex_for_highlight,
};

const SIMPLE: &[&str] = &[
    "echo hello world",
    "  echo\t  spaced  ",
    "echo é বাংলা",
    "FOO=bar cmd",
    "FOO=bar",
    "A=1 B=\"x y\" cmd A=not-prefix",
    "FOO= cmd",
    "1A=b",
    "F\"OO\"=b",
    "FOO\\=b",
    "a\nb\n",
];

const QUOTING: &[&str] = &[
    "'single $x `y` \\'",
    "\"double\"",
    "\"a $x `y` \\$ \\` \\\" \\\\ \\q\"",
    "\"it's\" 'say \"hi\"'",
    "a'b'\"c\"d",
    "''",
    "\"\"",
    "a\\ b \\$x \\\\ \\'",
    "\\é",
    "$'tab\\tquote\\'end'",
    "\"$'not ansi'\"",
    "$\"x\"",
    "a$ $ \"$\"",
    "a\\\nb",
    "\"a\\\nb\"",
    "'a\\\nb'",
];

const OPERATORS: &[&str] = &[
    "a | b",
    "a;b",
    "a && b || c",
    "a &",
    "a&b",
    "! a",
    "!a",
    "(a; b)",
    "{ a; }",
    "{a; }",
    "a }",
    "case x in a) b;; esac",
    "a &\\\n& b",
];

const REDIRECTIONS: &[&str] = &[
    "a > f",
    "a >> f",
    "a < f",
    "a 2> f",
    "a 2>&1",
    "a >&2",
    "a &> f",
    "a &>> f",
    "a <<< word",
    "a 3>f",
    "a 3<f",
    "a 3>&4",
    "a 3>&-",
    "a <& 3",
    "a <> f",
    "a >| f",
    "2>f a",
    "a2>f",
    "\"2\">f",
    "a 2 >f",
    "a 2&>f",
    "a >\\\n> f",
    "a 2\\\n>f",
    "a <<",
];

const HERE_DOCS: &[&str] = &[
    "cat <<EOF\nhello $x $(y) `z` \\$ \\q \"q\" 'q'\nEOF\n",
    "cat <<'EOF'\nraw $x \\$\nEOF\n",
    "cat <<\"EOF\"\nraw $x\nEOF\n",
    "cat <<\\EOF\nraw $x\nEOF\n",
    "cat <<E\"O\"F\nraw $x\nEOF\n",
    "cat <<-EOF\n\tone\n\t\ttwo $x\n\tEOF\n",
    "cat <<-'EOF'\n\tone\n\tEOF\n",
    "cat <<A <<B | grep x # note\nfirst\nA\nsecond\nB\nnext\n",
    "cat <<EOF\nno newline after delimiter\nEOF",
    "cat <<EOF\nx\\\nEOF\nEOF\n",
    "cat <<EOF\nEOF \nEOF\n",
    "cat <<''\nbody\n\nnext",
    "cat << -EOF\n-EOF\n",
    "x=$(cat <<EOF\ninner )\nEOF\n)\n",
    "x=$(cat <<EOF)\nhello\nEOF\n",
];

const EXPANSIONS: &[&str] = &[
    "~ ~user ~/x a~ \"~\"",
    "$VAR ${VAR} $_a1 $é",
    "${VAR:-x} ${VAR:=x} ${VAR:?msg} ${VAR:+x}",
    "${VAR-x} ${VAR=x} ${VAR?msg} ${VAR+x}",
    "${VAR#p} ${VAR##p} ${VAR%p} ${VAR%%p}",
    "${#VAR} ${#} ${##} ${#?} ${#-} ${#1} ${#@}",
    "${#-x} ${#:-x} ${##x} ${#%x}",
    "$? $$ $! $0 $# $@ $* $- $1 $9 $10 ${10}",
    "${VAR:-a b} ${VAR:-$OTHER} ${VAR:-${INNER:-$(cmd)}}",
    "${VAR:-'a}b'} \"${VAR:-it's}\" \"${VAR#'a'}\" \"${VAR:-a\\}b}\"",
    "\"${VAR:-\"a b\"}\"",
    "$(cmd arg)x $(a $(b \"$(c)\")) \"$(d e)\"",
    "$(echo \")\") $(echo ')') $(echo \\))",
    "$(a # )\n)",
    "$(case x in a) b;; esac) $(case x in (a) b;; esac)",
    "$( (case x in a) b;; esac) ) $(echo case) $(case esac in a|b) c; esac)",
    "`cmd arg` \"`a`\" `a \\` b`",
    "$((1 + 2)) $(( (1 + 2) * $x )) $(($(a) + ${b:-1})) $(())",
    "$((a) ) $((a); (b)) $((echo ')'))",
    "* ?.txt [abc] [!abc] \"*\" \\*",
    "a$\\\nb $\\\n(c) ${\\\nd} $e\\\nf",
];

const COMPOUND: &[&str] = &[
    "if a; then b; elif c; then d; else e; fi",
    "while a; do b; done",
    "until a; do b; done",
    "for i in a b; do c; done",
    "for i\ndo a; done",
    "for i do a; done",
    "for do in in; do done; done",
    "case $x in\n  a|b) c;;\n  (d) e;;\n  *) f;\nesac",
    "case esac in esac) a;; esac",
    "name() { a; }",
    "if\\\n true; then a; fi\\\n\n",
    "i\\\nf a; then b; fi",
    "echo if then fi done",
    "\"if\" a; \\if b",
    "FOO=1 if",
    ">f if",
    "fi >f done",
    "case x in (esac) a;; esac",
    "case x in a|esac) b;; esac",
];

const COMMENTS: &[&str] = &[
    "# whole line",
    "a # trailing $x 'unclosed",
    "a#b #c",
    "#a\n#b\nc",
    "a \\\n#b",
    "a\\\n#b",
    "a #b\\\nc",
];

const ERRORS: &[&str] = &[
    "a 'open",
    "a \"open $x",
    "a $'open",
    "a `open",
    "a $(open",
    "a $(open \"quote",
    "a $((1 +",
    "a ${open",
    "a ${open:-x",
    "cat <<EOF\nbody",
    "cat <<EOF",
    "x=$(cat <<EOF\nhello\nEOF)\n",
    "a\\",
    "a\\\n",
    "a \\\n",
];

/// Bash-only forms from §6.2. S04 turns these into "not supported" messages, so the lexer
/// has to hand them over in one piece.
const UNSUPPORTED: &[&str] = &[
    "${x//a/b} ${x:1:2} ${x^^} ${!x} ${x[0]} ${x:x} ${#x:-y} ${} ${ x}",
    "a=(1 2)",
    "[[ a ]]",
    "diff <(a) >(b)",
    "{a,b}",
    "a |& b",
    "((1))",
];

const ALL_GROUPS: &[(&str, &[&str])] = &[
    ("simple", SIMPLE),
    ("quoting", QUOTING),
    ("operators", OPERATORS),
    ("redirections", REDIRECTIONS),
    ("here_docs", HERE_DOCS),
    ("expansions", EXPANSIONS),
    ("compound", COMPOUND),
    ("comments", COMMENTS),
    ("errors", ERRORS),
    ("unsupported", UNSUPPORTED),
];

fn write_parts(out: &mut String, src: &str, parts: &[WordPart], indent: usize) {
    for part in parts {
        let _ = write!(out, "{:indent$}{}..{} ", "", part.span.start, part.span.end);
        match &part.kind {
            PartKind::Literal => {
                let _ = writeln!(out, "Literal {:?}", part.span.text(src));
            }
            PartKind::Escaped => {
                let _ = writeln!(out, "Escaped {:?}", part.span.text(src));
            }
            PartKind::LineContinuation => {
                let _ = writeln!(out, "LineContinuation");
            }
            PartKind::SingleQuoted { content } => {
                let _ = writeln!(out, "SingleQuoted {:?}", content.text(src));
            }
            PartKind::AnsiCQuoted { content } => {
                let _ = writeln!(out, "AnsiCQuoted {:?}", content.text(src));
            }
            PartKind::DoubleQuoted { parts } => {
                let _ = writeln!(out, "DoubleQuoted");
                write_parts(out, src, parts, indent + 2);
            }
            PartKind::Parameter(param) => {
                let _ = write!(out, "Parameter {:?}", param.name.text(src));
                if param.braced {
                    let _ = write!(out, " braced");
                }
                if param.length {
                    let _ = write!(out, " length");
                }
                if let Some((op, span)) = param.op {
                    let _ = write!(out, " {op:?}({:?})", span.text(src));
                }
                let _ = writeln!(out);
                write_parts(out, src, &param.arg, indent + 2);
            }
            PartKind::CommandSub { body } => {
                let _ = writeln!(out, "CommandSub {:?}", body.text(src));
            }
            PartKind::Backtick { body } => {
                let _ = writeln!(out, "Backtick {:?}", body.text(src));
            }
            PartKind::Arithmetic { body, parts } => {
                let _ = writeln!(out, "Arithmetic {:?}", body.text(src));
                write_parts(out, src, parts, indent + 2);
            }
        }
    }
}

fn write_token(out: &mut String, src: &str, token: &Token) {
    let span = token.span;
    let _ = write!(out, "  {}..{} ", span.start, span.end);
    match &token.kind {
        TokenKind::Word(word) => {
            let _ = write!(out, "Word {:?}", span.text(src));
            if let Some(name) = word.assignment_name {
                let _ = write!(out, " assigns={:?}", name.text(src));
            }
            if let Some(reserved) = Word::reserved_word(span, src) {
                let _ = write!(out, " reserved={reserved:?}");
            }
            let _ = writeln!(out);
            let only_literal = matches!(
                word.parts.as_slice(),
                [WordPart {
                    kind: PartKind::Literal,
                    ..
                }]
            );
            if !only_literal {
                write_parts(out, src, &word.parts, 4);
            }
        }
        TokenKind::IoNumber => {
            let _ = writeln!(out, "IoNumber {:?}", span.text(src));
        }
        TokenKind::Op(op) => {
            let _ = writeln!(out, "Op {op:?}");
        }
        TokenKind::Newline => {
            let _ = writeln!(out, "Newline");
        }
        TokenKind::Comment => {
            let _ = writeln!(out, "Comment {:?}", span.text(src));
        }
        TokenKind::HereDocBody(doc) => {
            let _ = write!(out, "HereDocBody {:?}", doc.body.text(src));
            if let Some(marker) = doc.end_marker {
                let _ = write!(out, " end={:?}", marker.text(src));
            }
            if doc.quoted {
                let _ = write!(out, " quoted");
            }
            if doc.strip_tabs {
                let _ = write!(out, " strip_tabs");
            }
            let _ = writeln!(out);
            write_parts(out, src, &doc.parts, 4);
        }
    }
}

fn render_tokens(lines: &[&str]) -> String {
    let mut out = String::new();
    for src in lines {
        let _ = writeln!(out, "{src:?}");
        let lexed = lex(src);
        for token in &lexed.tokens {
            write_token(&mut out, src, token);
        }
        for error in &lexed.errors {
            let _ = writeln!(
                out,
                "  ERROR {:?} at {:?}",
                error.kind,
                error.span.text(src)
            );
        }
        let _ = writeln!(out);
    }
    out
}

fn render_highlight(lines: &[&str]) -> String {
    let mut out = String::new();
    for src in lines {
        let _ = writeln!(out, "{src:?}");
        for span in lex_for_highlight(src).spans {
            let _ = writeln!(out, "  {:?} {:?}", span.kind, span.span.text(src));
        }
        let _ = writeln!(out);
    }
    out
}

#[test]
fn tokens_snapshot() {
    for (name, lines) in ALL_GROUPS {
        insta::assert_snapshot!(format!("tokens_{name}"), render_tokens(lines));
    }
}

#[test]
fn highlight_snapshot() {
    for (name, lines) in ALL_GROUPS {
        insta::assert_snapshot!(format!("highlight_{name}"), render_highlight(lines));
    }
}

/// The text before the first parenthesis of a Debug-printed value: its variant's name.
fn variant<T: std::fmt::Debug>(value: &T) -> String {
    let debug = format!("{value:?}");
    debug
        .split(['(', ' ', '{'])
        .next()
        .unwrap_or_default()
        .to_owned()
}

fn collect_parts(parts: &[WordPart], seen: &mut BTreeSet<String>) {
    for part in parts {
        seen.insert(variant(&part.kind));
        match &part.kind {
            PartKind::DoubleQuoted { parts } | PartKind::Arithmetic { parts, .. } => {
                collect_parts(parts, seen);
            }
            PartKind::Parameter(param) => {
                if let Some((op, _)) = param.op {
                    seen.insert(variant::<ParamOp>(&op));
                }
                collect_parts(&param.arg, seen);
            }
            _ => {}
        }
    }
}

/// Fails if the test lines stop exercising any kind, so the snapshots above do cover
/// every §6.1 token kind.
#[test]
fn every_kind_is_covered() {
    let mut seen = BTreeSet::new();
    let mut highlight_seen = BTreeSet::new();
    let mut error_seen = BTreeSet::new();
    for (_, lines) in ALL_GROUPS {
        for src in *lines {
            let lexed = lex(src);
            for token in &lexed.tokens {
                match &token.kind {
                    TokenKind::Op(op) => {
                        seen.insert(variant::<Op>(op));
                    }
                    TokenKind::Word(word) => collect_parts(&word.parts, &mut seen),
                    TokenKind::HereDocBody(doc) => collect_parts(&doc.parts, &mut seen),
                    _ => {}
                }
                seen.insert(variant(&token.kind));
            }
            for error in &lexed.errors {
                error_seen.insert(variant::<LexErrorKind>(&error.kind));
            }
            for span in lex_for_highlight(src).spans {
                highlight_seen.insert(variant::<HighlightKind>(&span.kind));
            }
        }
    }
    let expected = [
        // Token kinds.
        "Word",
        "IoNumber",
        "Op",
        "Newline",
        "Comment",
        "HereDocBody",
        // Operators.
        "Pipe",
        "AndIf",
        "OrIf",
        "Semi",
        "DoubleSemi",
        "Amp",
        "LeftParen",
        "RightParen",
        "Less",
        "Great",
        "DoubleGreat",
        "LessAmp",
        "GreatAmp",
        "AmpGreat",
        "AmpDoubleGreat",
        "HereDoc",
        "HereDocStripTabs",
        "HereString",
        "LessGreat",
        "GreatPipe",
        // Word parts.
        "Literal",
        "Escaped",
        "LineContinuation",
        "SingleQuoted",
        "AnsiCQuoted",
        "DoubleQuoted",
        "Parameter",
        "CommandSub",
        "Backtick",
        "Arithmetic",
        // Parameter operators.
        "ColonDash",
        "ColonEquals",
        "ColonQuestion",
        "ColonPlus",
        "Dash",
        "Equals",
        "Question",
        "Plus",
        "Hash",
        "DoubleHash",
        "Percent",
        "DoublePercent",
        "Unsupported",
    ];
    for name in expected {
        assert!(seen.contains(name), "no test line produces {name}");
    }
    for name in [
        "Command",
        "Keyword",
        "Argument",
        "Assignment",
        "Operator",
        "Redirect",
        "IoNumber",
        "Separator",
        "Comment",
        "SingleQuoted",
        "DoubleQuoted",
        "Escape",
        "Variable",
        "CommandSub",
        "Arithmetic",
        "HereDocDelimiter",
        "HereDocBody",
    ] {
        assert!(
            highlight_seen.contains(name),
            "no line highlights as {name}"
        );
    }
    for name in [
        "UnterminatedSingleQuote",
        "UnterminatedAnsiCQuote",
        "UnterminatedDoubleQuote",
        "UnterminatedBacktick",
        "UnterminatedCommandSub",
        "UnterminatedArithmetic",
        "UnterminatedParameter",
        "UnterminatedHereDoc",
        "TrailingBackslash",
    ] {
        assert!(error_seen.contains(name), "no line produces {name}");
    }
}

fn assert_span_valid(src: &str, span: Span) {
    assert!(span.start <= span.end, "{span:?} backwards in {src:?}");
    assert!(span.end <= src.len(), "{span:?} past the end of {src:?}");
    assert!(
        src.is_char_boundary(span.start),
        "{span:?} splits a character in {src:?}"
    );
    assert!(
        src.is_char_boundary(span.end),
        "{span:?} splits a character in {src:?}"
    );
}

fn assert_parts_valid(src: &str, parts: &[WordPart], outer: Span) {
    let mut cursor = outer.start;
    for part in parts {
        assert_span_valid(src, part.span);
        assert!(
            part.span.start >= cursor,
            "{part:?} out of order in {src:?}"
        );
        assert!(
            part.span.end <= outer.end,
            "{part:?} leaves {outer:?} in {src:?}"
        );
        cursor = part.span.end;
        match &part.kind {
            PartKind::DoubleQuoted { parts } | PartKind::Arithmetic { parts, .. } => {
                assert_parts_valid(src, parts, part.span);
            }
            PartKind::Parameter(param) => {
                assert_span_valid(src, param.name);
                assert_parts_valid(src, &param.arg, part.span);
            }
            PartKind::SingleQuoted { content } | PartKind::AnsiCQuoted { content } => {
                assert_span_valid(src, *content);
            }
            PartKind::CommandSub { body } | PartKind::Backtick { body } => {
                assert_span_valid(src, *body);
            }
            _ => {}
        }
    }
}

fn is_only_blanks(gap: &str) -> bool {
    gap.replace("\\\n", "")
        .bytes()
        .all(|b| b == b' ' || b == b'\t')
}

/// Checks what must hold for any input at all.
fn assert_invariants(src: &str) {
    let lexed: Lexed = lex(src);
    let mut cursor = 0;
    for token in &lexed.tokens {
        assert_span_valid(src, token.span);
        assert!(
            token.span.start >= cursor,
            "{token:?} out of order in {src:?}"
        );
        let gap = Span::new(cursor, token.span.start).text(src);
        assert!(
            is_only_blanks(gap),
            "{gap:?} lost before {token:?} in {src:?}"
        );
        cursor = token.span.end;
        match &token.kind {
            TokenKind::Word(word) => {
                assert_parts_valid(src, &word.parts, token.span);
                // The pieces of a word cover it with nothing left over.
                let joined: usize = word.parts.iter().map(|p| p.span.end - p.span.start).sum();
                assert_eq!(
                    joined,
                    token.span.end - token.span.start,
                    "{token:?} in {src:?}"
                );
            }
            TokenKind::HereDocBody(doc) => {
                assert_span_valid(src, doc.body);
                assert_parts_valid(src, &doc.parts, doc.body);
            }
            _ => {}
        }
    }
    let gap = Span::new(cursor, src.len()).text(src);
    assert!(is_only_blanks(gap), "{gap:?} lost at the end of {src:?}");
    for error in &lexed.errors {
        assert_span_valid(src, error.span);
    }

    let highlighted = lex_for_highlight(src);
    assert_eq!(
        highlighted.errors, lexed.errors,
        "the two paths disagree on {src:?}"
    );
    let mut cursor = 0;
    for span in &highlighted.spans {
        assert_span_valid(src, span.span);
        assert!(span.span.start >= cursor, "{span:?} overlaps in {src:?}");
        assert!(
            span.span.start < span.span.end,
            "{span:?} is empty in {src:?}"
        );
        cursor = span.span.end;
    }
    for word in &highlighted.words {
        assert_span_valid(src, word.span);
        assert!(
            word.span.start < word.span.end,
            "{word:?} is empty in {src:?}"
        );
        assert!(
            matches!(
                word.kind,
                HighlightKind::Command
                    | HighlightKind::Keyword
                    | HighlightKind::Argument
                    | HighlightKind::Assignment
                    | HighlightKind::IoNumber
                    | HighlightKind::HereDocDelimiter
            ),
            "{word:?} has a kind that is not a word's in {src:?}"
        );
    }
    // Every token except a line ending is colored from its first byte to its last.
    for token in &lexed.tokens {
        if !matches!(token.kind, TokenKind::Newline) && token.span.start < token.span.end {
            let mut covered = token.span.start;
            for span in &highlighted.spans {
                if span.span.start <= covered && covered < span.span.end {
                    covered = span.span.end;
                }
            }
            assert!(
                covered >= token.span.end,
                "{token:?} not fully colored in {src:?}"
            );
        }
    }
}

/// Text being typed is a prefix of a finished line, so every prefix has to work.
#[test]
fn invariants_hold_for_every_prefix() {
    for (_, lines) in ALL_GROUPS {
        for src in *lines {
            for end in (0..=src.len()).filter(|&end| src.is_char_boundary(end)) {
                assert_invariants(src.get(..end).unwrap_or(""));
            }
        }
    }
}

/// Complete lines must not report problems. Only the `errors` group should.
#[test]
fn complete_lines_have_no_errors() {
    for (name, lines) in ALL_GROUPS {
        if *name == "errors" {
            continue;
        }
        for src in *lines {
            let errors = lex(src).errors;
            assert!(errors.is_empty(), "{src:?} gave {errors:?}");
        }
    }
}

#[test]
fn unfinished_input_is_incomplete_and_finished_input_is_not() {
    for src in ERRORS {
        assert!(
            lex(src).is_incomplete(),
            "{src:?} should ask for more input"
        );
    }
    assert!(!lex("echo hello").is_incomplete());
}

fn error_kinds(src: &str) -> Vec<LexErrorKind> {
    lex(src).errors.iter().map(|e| e.kind).collect()
}

/// Deep nesting must end in an error. The default test thread has a 2 MiB stack, less
/// than the shell's main thread gets, so this also shows the limit leaves enough room.
#[test]
fn deep_nesting_stops_with_an_error_and_keeps_the_stack() {
    let openers = ["$(", "\"$(", "${x:-", "\"${x:-", "$((", "$(($(", "`$("];
    for opener in openers {
        let src = opener.repeat(100_000);
        let kinds = error_kinds(&src);
        assert_eq!(kinds, [LexErrorKind::NestingTooDeep], "{opener:?}");
        assert!(!lex(&src).is_incomplete());
        assert_eq!(lex_for_highlight(&src).errors.len(), 1);
    }
}

#[test]
fn nesting_below_the_limit_is_fine() {
    let depth = 60;
    let src = format!("{}a{}", "$(".repeat(depth), ")".repeat(depth));
    assert_eq!(error_kinds(&src), []);
    assert_eq!(lex(&src).tokens.len(), 1);
}

/// Work must not explode on awkward input, because this runs on every keystroke. The
/// time limit is generous on purpose; it catches exponential behavior, not slow machines.
#[test]
fn awkward_input_stays_fast() {
    let cases = [
        "$((".repeat(63),
        "$((a) ".repeat(63),
        "$(( (".repeat(63),
        "$((a)".repeat(20_000),
        "$((1)) ".repeat(20_000),
        "(".repeat(200_000),
        "'".repeat(200_001),
        "<<A ".repeat(5_000),
        format!("{}\n", "cat <<A; ".repeat(2_000)),
        "\\\n".repeat(100_000),
        "a".repeat(1_000_000),
    ];
    for src in &cases {
        let started = Instant::now();
        let _ = lex(src);
        let _ = lex_for_highlight(src);
        let elapsed = started.elapsed();
        let head: String = src.chars().take(12).collect();
        assert!(
            elapsed < Duration::from_secs(5),
            "{head:?} took {elapsed:?}"
        );
    }
}

fn sh(script: &str) -> String {
    let output = Command::new("/bin/sh").arg("-c").arg(script).output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(error) => panic!("cannot run /bin/sh: {error}"),
    }
}

/// `/bin/sh` is the reference for word boundaries: it counts its arguments and we count
/// our tokens. Every line avoids anything that field splitting or globbing would change.
#[test]
fn word_count_matches_sh() {
    let lines = [
        "a b  c",
        "a\\ b",
        "'a b'c",
        "\"a  b\"",
        "a\"b c\"d",
        "'' \"\"",
        "a\\\nb",
        "a \\\n b",
        "\"$(echo \"a b\")\"",
        "$((1+1))x y",
        "$(( (1+1) * 2 ))",
        "\"${Z_UNSET:-\"a b\"}\"",
        "${Z_UNSET:-a}",
        "\"${Z_UNSET:-a\\}b}\"",
        "\"${Z_UNSET:-it's}\"",
        "$(echo a)",
        "$(echo ')')",
        "$(echo \\))",
        "$(case x in x) echo a;; esac)",
        "$( (case x in x) echo a;; esac) )",
        // Regression: only the first word of a pattern can be the closing `esac`.
        "$(case esac in (esac) echo a;; esac)",
        "$(case x in y|esac) echo b;; x) echo a;; esac)",
        // Regression: a word after an assignment or redirection is never reserved.
        "\"$(X=1 case)\"",
        "\"$(>/dev/null case)\"",
        // A redirection after a compound command belongs to it, so `esac` still counts.
        "$(case x in x) { echo a; } >/dev/null esac; echo a)",
        "$(echo a # )\n)",
        "`echo a` b",
        "`echo \\`echo a\\``",
        "a#b #c",
        "$ a$ \"$\"",
        "$'a b'",
        "#only a comment",
        "a #b\\\n",
        "~",
        "{a,b}",
        "!a }",
        "é \"বাংলা\" ই\\ ই",
    ];
    for line in lines {
        let expected = sh(&format!("set -- {line}\necho $#"));
        let tokens = lex(line).tokens;
        let words = tokens
            .iter()
            .filter(|t| matches!(t.kind, TokenKind::Word(_)))
            .count();
        assert_eq!(words.to_string(), expected.trim(), "{line:?}");
        assert!(lex(line).errors.is_empty(), "{line:?}");
    }
}

/// Puts a here-document's text back together from its pieces.
fn cook_parts(src: &str, parts: &[WordPart]) -> String {
    let mut out = String::new();
    for part in parts {
        let text = part.span.text(src);
        match part.kind {
            PartKind::Literal => out.push_str(text),
            PartKind::Escaped => out.push_str(text.get(1..).unwrap_or("")),
            PartKind::LineContinuation => {}
            _ => panic!("this comparison only takes plain text, not {part:?}"),
        }
    }
    out
}

/// `/bin/sh` is the reference for where a here-document ends and which bytes are in it.
#[test]
fn here_doc_text_matches_sh() {
    let lines = [
        "cat <<EOF\nhello\nEOF\n",
        "cat <<EOF\nEOF\n",
        "cat <<'EOF'\nraw $x `y` \\$ \\\\\nEOF\n",
        "cat <<\"EOF\"\nraw $x\nEOF\n",
        "cat <<\\EOF\nraw $x\nEOF\n",
        "cat <<E\"O\"F\nraw $x\nEOF\n",
        "cat <<E\\OF\nraw $x\nEOF\n",
        "cat <<\\\nEOF\nhi\nEOF\n",
        "cat <<$X\nbody\n$X\n",
        "cat <<EOF\n\\$ \\` \\\\ \\\" \\q \"q\" 'q'\nEOF\n",
        "cat <<EOF\nx\\\nEOF\nEOF\n",
        "cat <<EOF\nEOF\\\nEOF\nEOF\n",
        "cat <<EOF\nx\\\\\nEOF\n",
        "cat <<-EOF\n\tone\n\t\ttwo\n\tEOF\n",
        "cat <<-EOF\n\tx\\\n\ty\n\tEOF\n",
        "cat <<-EOF\n\n\tx\nEOF\n",
        "cat <<-'EOF'\n\tone\n\t\ttwo\n\tEOF\n",
        "cat <<EOF\n EOF\nEOF \n\tEOF\nEOF\n",
        "cat <<EOF\nno newline after delimiter\nEOF",
        "cat <<''\nbody\n\nnext",
    ];
    for line in lines {
        let lexed = lex(line);
        assert!(lexed.errors.is_empty(), "{line:?} gave {:?}", lexed.errors);
        let mut ours = String::new();
        for token in &lexed.tokens {
            if let TokenKind::HereDocBody(doc) = &token.kind {
                ours.push_str(&cook_parts(line, &doc.parts));
            }
        }
        // The `next` line in the last case is a separate command for /bin/sh, which fails
        // without printing to stdout.
        assert_eq!(ours, sh(line), "{line:?}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(common::CASES))]

    /// Whatever is typed, the spans stay inside the text and on character boundaries, no text
    /// is lost between tokens, and the two lexing paths agree on the problems.
    #[test]
    fn invariants_hold_for_generated_text(
        src in common::shell_text(
            ALL_GROUPS.iter().flat_map(|(_, lines)| lines.iter().copied()).collect()
        )
    ) {
        assert_invariants(&src);
    }
}

/// The words the editor checks: each one whole, however many pieces it has.
#[test]
fn highlight_words_cover_whole_words() {
    let words = |src: &str| -> Vec<(HighlightKind, String)> {
        lex_for_highlight(src)
            .words
            .iter()
            .map(|w| (w.kind, w.span.text(src).to_owned()))
            .collect()
    };
    assert_eq!(
        words("ls -l \"a b\"c$d"),
        vec![
            (HighlightKind::Command, "ls".to_owned()),
            (HighlightKind::Argument, "-l".to_owned()),
            (HighlightKind::Argument, "\"a b\"c$d".to_owned()),
        ]
    );
    // A word inside `$(...)` ends first; the word that holds it comes after.
    assert_eq!(
        words("echo $(pwd)x"),
        vec![
            (HighlightKind::Command, "echo".to_owned()),
            (HighlightKind::Command, "pwd".to_owned()),
            (HighlightKind::Argument, "$(pwd)x".to_owned()),
        ]
    );
    assert_eq!(
        words("A=1 cmd"),
        vec![
            (HighlightKind::Assignment, "A=1".to_owned()),
            (HighlightKind::Command, "cmd".to_owned()),
        ]
    );
}

/// `;`, `&` and `;;` end a command; `|`, `&&`, `||` and parentheses join or group.
#[test]
fn highlight_separates_command_ends_from_operators() {
    let kinds = |src: &str| -> Vec<HighlightKind> {
        lex_for_highlight(src)
            .spans
            .iter()
            .filter(|s| matches!(s.kind, HighlightKind::Operator | HighlightKind::Separator))
            .map(|s| s.kind)
            .collect()
    };
    assert_eq!(kinds("a ; b & c"), vec![HighlightKind::Separator; 2]);
    assert_eq!(kinds("a | b && c || d"), vec![HighlightKind::Operator; 3]);
    assert_eq!(
        kinds("case x in y) a;; esac"),
        vec![HighlightKind::Operator, HighlightKind::Separator]
    );
}
