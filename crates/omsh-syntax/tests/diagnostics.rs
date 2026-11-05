//! The reports that the user sees: a snapshot of each kind of problem rendered by `miette`, a
//! check that every kind of problem is covered, and checks on what the messages promise.

use std::collections::BTreeSet;

use miette::{GraphicalReportHandler, GraphicalTheme};
use omsh_syntax::{
    Expected, LexErrorKind, ParseErrorKind, ReservedWord, SyntaxDiagnostic, Unsupported, parse,
    parse_tolerant,
};

/// One line for each way a line can fail. The comment says which kind the line is for.
const LINES: &[&str] = &[
    // Unexpected, with each thing that was expected.
    ";",
    "{ a; } b",
    "a > ; b",
    "a << ; b",
    "case ; esac",
    "case x in ; esac",
    "case x in a b) c;; esac",
    "if a; b; fi",
    "if a; then b; done",
    "while a; b; done",
    "while a; do b; fi",
    "case x y",
    "case x in a) b; fi",
    "{ a; fi",
    "a\nfi",
    // The input ends too early.
    "a |",
    "a &&",
    "{ a;",
    "if a; then b",
    "(a",
    "for i in a; do b",
    // Names.
    "export() { a; }",
    "a-b() { c; }",
    "for 1a in x; do b; done",
    "for",
    "for \"i\" in a; do b; done",
    // The lexer's problems.
    "echo 'open",
    "echo $'open",
    "echo \"open $x",
    "echo `open",
    "echo $(open",
    "echo $((1 +",
    "echo ${open",
    "cat <<EOF\nbody",
    "echo a\\",
    // Substitutions that no shell accepts.
    "echo ${}",
    "echo ${#x:-y}",
    // Bash-only syntax. Every `Unsupported` variant has a line here.
    "[[ -f x ]]",
    "diff <(a) <(b)",
    "echo {a,b}",
    "a=(1 2)",
    "select i in a; do b; done",
    "coproc a",
    "shopt -s extglob",
    "echo ${x//a/b}",
    "echo ${x:1:2}",
    "echo ${x^^}",
    "echo ${!x}",
    "echo ${x@Q}",
    "((i++))",
    "function f { a; }",
    "a |& b",
    "case x in a) b;& c) d;; esac",
    "a+=b",
    "echo $(<file)",
    // Several problems at once.
    "echo {a,b} ${x//a/b} 'open",
    // Text that is not ASCII.
    "echo বাংলা {a,b}",
    "echo é ${x//a/b} ✓",
];

/// Lines that nest too deeply. Their reports are checked, but a snapshot of them would only
/// repeat the source over and over.
fn deep_lines() -> Vec<String> {
    vec!["( ".repeat(130), "$(".repeat(100), "\"$(".repeat(100)]
}

fn render(diagnostic: &SyntaxDiagnostic) -> String {
    let mut out = String::new();
    GraphicalReportHandler::new_themed(GraphicalTheme::unicode_nocolor())
        .with_width(80)
        .render_report(&mut out, diagnostic)
        .unwrap_or_else(|error| panic!("cannot render {:?}: {error}", diagnostic.message()));
    out
}

fn failure_report(line: &str) -> SyntaxDiagnostic {
    match parse(line) {
        Ok(_) => panic!("{line:?} should fail"),
        Err(failure) => failure.to_diagnostic(line),
    }
}

#[test]
fn reports_snapshot() {
    let mut out = String::new();
    for line in LINES {
        out.push_str(&format!("=== {line:?}\n"));
        out.push_str(&render(&failure_report(line)));
        out.push('\n');
    }
    insta::assert_snapshot!("reports", out);
}

#[test]
fn every_unsupported_feature_has_a_report() {
    let mut out = String::new();
    for feature in Unsupported::ALL {
        out.push_str(&format!(
            "{feature:?}\n  what: {}\n  help: {}\n",
            feature.what(),
            feature.help()
        ));
    }
    insta::assert_snapshot!("unsupported_texts", out);
}

/// A new variant must be added to the lists below. Matching without a wildcard makes the
/// compiler insist on that.
fn kind_name(kind: ParseErrorKind) -> &'static str {
    match kind {
        ParseErrorKind::Lex(_) => "Lex",
        ParseErrorKind::Unexpected { .. } => "Unexpected",
        ParseErrorKind::UnexpectedEnd { .. } => "UnexpectedEnd",
        ParseErrorKind::BadFunctionName => "BadFunctionName",
        ParseErrorKind::BadForName => "BadForName",
        ParseErrorKind::NestingTooDeep => "NestingTooDeep",
        ParseErrorKind::BadSubstitution => "BadSubstitution",
        ParseErrorKind::Unsupported(_) => "Unsupported",
    }
}

fn expected_name(expected: Expected) -> String {
    match expected {
        Expected::Command => "Command".to_owned(),
        Expected::CommandEnd => "CommandEnd".to_owned(),
        Expected::RedirectTarget => "RedirectTarget".to_owned(),
        Expected::HereDocDelimiter => "HereDocDelimiter".to_owned(),
        Expected::CaseWord => "CaseWord".to_owned(),
        Expected::CasePattern => "CasePattern".to_owned(),
        Expected::Reserved(word) => format!("Reserved({word:?})"),
        Expected::RightParen => "RightParen".to_owned(),
    }
}

fn lex_name(kind: LexErrorKind) -> String {
    format!("{kind:?}")
}

#[test]
fn every_problem_kind_is_covered() {
    let mut kinds = BTreeSet::new();
    let mut expected = BTreeSet::new();
    let mut lexed = BTreeSet::new();
    let mut features = BTreeSet::new();
    let deep = deep_lines();
    for line in LINES.iter().copied().chain(deep.iter().map(String::as_str)) {
        for error in parse_tolerant(line).errors {
            kinds.insert(kind_name(error.kind));
            match error.kind {
                ParseErrorKind::Lex(kind) => {
                    lexed.insert(lex_name(kind));
                }
                ParseErrorKind::Unexpected { expected: e }
                | ParseErrorKind::UnexpectedEnd { expected: e } => {
                    expected.insert(expected_name(e));
                }
                ParseErrorKind::Unsupported(feature) => {
                    features.insert(format!("{feature:?}"));
                }
                _ => {}
            }
        }
    }

    for name in [
        "Lex",
        "Unexpected",
        "UnexpectedEnd",
        "BadFunctionName",
        "BadForName",
        "NestingTooDeep",
        "BadSubstitution",
        "Unsupported",
    ] {
        assert!(kinds.contains(name), "no line produces {name}");
    }
    for name in [
        "Command",
        "CommandEnd",
        "RedirectTarget",
        "HereDocDelimiter",
        "CaseWord",
        "CasePattern",
        "RightParen",
    ] {
        assert!(expected.contains(name), "no line expects {name}");
    }
    for word in [
        ReservedWord::Then,
        ReservedWord::Fi,
        ReservedWord::Do,
        ReservedWord::Done,
        ReservedWord::In,
        ReservedWord::Esac,
        ReservedWord::RightBrace,
    ] {
        let name = expected_name(Expected::Reserved(word));
        assert!(expected.contains(&name), "no line expects {name}");
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
        assert!(lexed.contains(name), "no line produces {name}");
    }
    for feature in Unsupported::ALL {
        let name = format!("{feature:?}");
        assert!(features.contains(&name), "no line produces {name}");
    }
}

/// The lexer's nesting limit is a lexer error, which only a line of quotes can reach.
#[test]
fn both_nesting_limits_have_a_report() {
    let deep = deep_lines();
    let mut codes = BTreeSet::new();
    for line in &deep {
        let diagnostic = failure_report(line);
        assert_eq!(diagnostic.code_name(), "omsh::syntax::nesting", "{line:?}");
        let text = render(&diagnostic);
        assert!(text.contains("nested too deeply"), "{text}");
        assert!(text.contains("at most"), "{text}");
        codes.insert(diagnostic.help_text().map(str::to_owned));
    }
    // One message names the parser's limit, the other the lexer's.
    assert_eq!(codes.len(), 2, "{codes:?}");
}

#[test]
fn every_report_has_the_pieces_a_user_needs() {
    let deep = deep_lines();
    for line in LINES.iter().copied().chain(deep.iter().map(String::as_str)) {
        let diagnostic = failure_report(line);
        let span = diagnostic.span();
        assert!(span.start <= span.end && span.end <= line.len(), "{line:?}");
        assert!(diagnostic.code_name().starts_with("omsh::syntax::"));
        assert!(!diagnostic.message().is_empty(), "{line:?}");
        assert!(!diagnostic.label().is_empty(), "{line:?}");
        assert!(diagnostic.to_string() == diagnostic.message());
        let text = render(&diagnostic);
        // The renderer wraps long messages, so only the start of it is compared.
        let start: String = diagnostic.message().chars().take(30).collect();
        assert!(text.contains(&start), "{line:?}\n{text}");
        // The report quotes the line it is about (the first line, at least).
        if line.len() < 100 {
            let first = line.lines().next().unwrap_or("");
            assert!(text.contains(first), "{line:?}\n{text}");
        }
    }
}

#[test]
fn unsupported_reports_say_so_and_offer_a_way_out() {
    for line in LINES {
        let Err(failure) = parse(line) else {
            panic!("{line:?} should fail");
        };
        let Some(feature) = failure.unsupported() else {
            continue;
        };
        let diagnostic = failure.to_diagnostic(line);
        assert_eq!(diagnostic.code_name(), "omsh::syntax::unsupported");
        assert!(
            diagnostic
                .message()
                .ends_with("is not supported in Oh My Shell"),
            "{line:?}: {}",
            diagnostic.message()
        );
        assert!(diagnostic.message().contains(feature.what()));
        assert_eq!(diagnostic.help_text(), Some(feature.help()));
        assert!(render(&diagnostic).contains("help:"), "{line:?}");
    }
    for feature in Unsupported::ALL {
        assert!(
            feature.what().len() > 5 && feature.help().len() > 10,
            "{feature:?}"
        );
    }
}

#[test]
fn unsupported_only_counts_when_it_is_the_only_problem() {
    let failure = parse("echo {a,b}").unwrap_err();
    assert_eq!(failure.unsupported(), Some(Unsupported::BraceExpansion));
    // The lexer's problem comes along, so the line would not run in bash either.
    let failure = parse("echo {a,b} 'open").unwrap_err();
    assert_eq!(failure.unsupported(), None);
    let failure = parse("echo ${}").unwrap_err();
    assert_eq!(failure.unsupported(), None);
}

#[test]
fn several_problems_make_one_report_with_the_rest_attached() {
    // Strict mode stops at the first problem of the parser, so the lexer's comes with it.
    let line = "echo {a,b} ${x//a/b} 'open";
    let diagnostic = failure_report(line);
    assert_eq!(diagnostic.further().len(), 1);
    let text = render(&diagnostic);
    // Each problem is shown, and each one quotes its own place.
    assert!(text.contains("brace expansion"), "{text}");
    assert!(text.contains("never closed"), "{text}");

    // Tolerant mode lists all of them.
    let tolerant = parse_tolerant(line).to_diagnostic(line).unwrap();
    assert_eq!(tolerant.further().len(), 2);
    let text = render(&tolerant);
    assert!(text.contains("replacement"), "{text}");
    assert!(parse_tolerant("echo hi").to_diagnostic("echo hi").is_none());
}

#[test]
fn a_report_outlives_the_text_it_was_built_from() {
    let diagnostic = {
        let line = String::from("echo ${x//a/b}");
        failure_report(&line)
    };
    let text = render(&diagnostic);
    assert!(text.contains("echo ${x//a/b}"), "{text}");
}

#[test]
fn a_report_works_with_miette_in_general() {
    let diagnostic = failure_report("[[ a ]]");
    let report = miette::Report::new(diagnostic);
    assert!(report.to_string().contains("not supported"));
    assert_eq!(
        report.code().map(|code| code.to_string()),
        Some("omsh::syntax::unsupported".to_owned())
    );
}

#[test]
fn messages_quote_long_and_odd_text_safely() {
    // A very long word is cut short in the message but still marked in full.
    let word = "x".repeat(500);
    let line = format!("{{ a; }} {word}");
    let diagnostic = failure_report(&line);
    assert!(diagnostic.message().len() < 80, "{}", diagnostic.message());
    assert!(diagnostic.message().contains('…'));
    assert_eq!(diagnostic.span().end - diagnostic.span().start, 500);

    // A line ending is called a line ending, not shown as a blank.
    let diagnostic = failure_report("a ;; b");
    assert!(diagnostic.message().starts_with("unexpected `;;`"));
    let diagnostic = failure_report("export() {\n a; }");
    assert!(diagnostic.message().contains("special builtin"));
    let diagnostic = failure_report("a-b() { c; }");
    assert!(diagnostic.message().contains("not a valid function name"));
}
