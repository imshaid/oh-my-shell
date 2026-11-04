//! Parser tests through the public API: snapshots per construct, a check that every kind is
//! exercised, invariants on every prefix of every line, strict and tolerant modes, and
//! comparisons with `/bin/sh -n`.

use std::fmt::Write as _;
use std::io::Write as _;
use std::process::{Command as Process, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use omsh_syntax::ast::{
    AndOr, AndOrOp, CaseCommand, Command, CommandKind, ForCommand, FunctionDef, IfCommand, List,
    LoopCommand, PartKind, Pipeline, Redirect, RedirectKind, SimpleCommand, Word, WordPart,
};
use omsh_syntax::{
    Expected, LexErrorKind, ParseError, ParseErrorKind, ReservedWord, Span, lex, parse,
    parse_tolerant,
};

const SIMPLE: &[&str] = &[
    "echo hello world",
    "  echo\t  spaced  ",
    "echo é বাংলা",
    ">f",
    ">f a",
    "a >f b 2>&1 c",
    "echo if then fi done",
    "\"if\" a",
    "\\if b",
    "echo a # trailing\n",
    "time a",
    "[[ a ]]",
    "function f",
];

const ASSIGNMENTS: &[&str] = &[
    "FOO=bar",
    "FOO=bar cmd",
    "A=1 B=\"x y\" cmd A=not-prefix",
    "FOO= cmd",
    "A=1 >f B=2 c D=3",
    ">f A=1",
    "A=$x$(y)z",
    "F\\\nOO=b",
    "FOO\\\n=b",
    "FOO=\\\nb",
    "A=b=c",
    "A=~/x:~/y",
    "A=1 if",
    "1A=b",
    "F\"OO\"=b",
    "FOO\\=b",
];

const PIPELINES: &[&str] = &[
    "a | b",
    "a | b | c",
    "! a",
    "! a | b",
    "!a",
    "a |\n\n b",
    "a |  # comment\nb",
    "(a) | { b; } | c",
    "a && ! b | c",
    "! (a)",
    "! if a; then b; fi",
];

const LISTS: &[&str] = &[
    "a; b",
    "a & b",
    "a &",
    "a;",
    "a && b",
    "a || b",
    "a && b || c",
    "a &&\n b ||\n\n c",
    "a\nb\n\nc\n",
    "a; b & c && d | e",
    "",
    "\n\n",
    "# only a comment",
    "a # trailing\nb",
    "a;#c",
    "a &#c",
    "#a\n#b\nc",
    "a \\\n b",
    "a |\\\n b",
    "a;\\\n b",
];

const REDIRECTIONS: &[&str] = &[
    "a > f",
    "a >> f",
    "a < f",
    "a 2> f",
    "a 2>&1",
    "a >&2",
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
    "a >\\\n> f",
    "a 2\\\n>f",
    "a > if",
    "a >$x$(y)",
    "a >f >g <h",
    "a 12>f",
    "a 2147483647>f",
    "a 2147483648>f",
    "99999999999>f",
];

/// Forms from §6.1 that `dash` reads differently or not at all, so `/bin/sh` cannot judge them.
const BASH_FORMS: &[&str] = &[
    "a &> f",
    "a &>> f",
    "a 2&>f",
    "a <<< word",
    "a <<< \"w $x\"",
    "a 2<<< word",
    "$'tab\\tquote\\'end'",
    "$((a); (b))",
];

const HERE_DOCS: &[&str] = &[
    "cat <<EOF\nhello $x $(y) `z` \\$ \\q \"q\" 'q'\nEOF\n",
    "cat <<'EOF'\nraw $x \\$\nEOF\n",
    "cat <<\"EOF\"\nraw $x\nEOF\n",
    "cat <<\\EOF\nraw $x\nEOF\n",
    "cat <<E\"O\"F\nraw $x\nEOF\n",
    "cat <<-EOF\n\tone\n\t\ttwo $x\n\tEOF\n",
    "cat <<A <<B | grep x # note\nfirst\nA\nsecond\nB\nnext\n",
    "cat 3<<A; cat <<B\nfirst\nA\nsecond\nB\n",
    "cat <<EOF\nno newline after delimiter\nEOF",
    "cat <<EOF\nEOF\n",
    "cat << -EOF\n-EOF\n",
    "if a; then cat <<EOF\nbody\nEOF\nfi",
    "{ cat <<A; } | cat <<B\nfirst\nA\nsecond\nB\n",
    "x=$(cat <<EOF\ninner )\nEOF\n)\n",
    "cat <<EOF\n$(cat <<A\ninner\nA\n)\nEOF\n",
    "cat <<EOF\n`a \\\"b\\\"`\nEOF\n",
    "cat <<E\\\nF\nbody\nEF\n",
    "cat <<A; echo $(cat <<B\ninner\nB\n)\nouter\nA\n",
    "cat <<A # note\nbody\nA\n",
    "while a; do cat <<A; done\nbody\nA\n",
    "f() { cat <<A; }\nbody\nA\n",
];

const SUBSHELLS: &[&str] = &[
    "(a)",
    "(a; b)",
    "(a;)",
    "(a &)",
    "(\n a\n b\n)",
    "((a))",
    "( (a) )",
    "(a) >f 2>&1",
    "(a) &",
    "(a) && (b)",
];

const GROUPS: &[&str] = &[
    "{ a; }",
    "{ a; b; }",
    "{ a & }",
    "{\n a\n}",
    "{ { a; } }",
    "{ a; } >f",
    "{ a; } &",
    "{ (a) }",
    "{(a);}",
    "{ a; } && { b; }",
];

const IFS: &[&str] = &[
    "if a; then b; fi",
    "if a; then b; else c; fi",
    "if a; then b; elif c; then d; else e; fi",
    "if a; then b; elif c; then d; elif e; then f; fi",
    "if a\nthen\n b\nfi",
    "if\n a\nthen\n b\nelse\n c\nfi",
    "if a; b; then c; d; fi",
    "if a; then b; fi >f",
    "if a; then b; fi | c",
    "if a; then if b; then c; fi fi",
    "if ! a; then b; fi",
    "if (a); then { b; }; fi",
    "i\\\nf a; then b; f\\\ni",
    "if # note\n a # note\nthen # note\n b # note\nelse # note\n c # note\nfi # note",
    "if \\\n a; then b; fi",
];

const LOOPS: &[&str] = &[
    "while a; do b; done",
    "until a; do b; done",
    "while a\ndo\n b\ndone",
    "while a; b; do c; d; done",
    "while a; do b; done <f",
    "while a; do b; done | c",
    "while a; do while b; do c; done done",
];

const FORS: &[&str] = &[
    "for i in a b; do c; done",
    "for i in; do c; done",
    "for i in\ndo c; done",
    "for i; do c; done",
    "for i do c; done",
    "for i\ndo c; done",
    "for i\n\nin a\n\ndo c; done",
    "for i;\n\ndo c; done",
    "for i in a\ndo c; done",
    "for do in in; do a; done",
    "for if in then; do a; done",
    "for i in $x \"y z\" *; do c; done",
    "for i in a; do c; done >f",
    "fo\\\nr i\\\nj in a; do c; done",
    "for i in a b # note\ndo # note\n c # note\ndone",
];

const CASES: &[&str] = &[
    "case x in esac",
    "case x in a) esac",
    "case x in a) b; esac",
    "case x in a) b;; esac",
    "case x in a) ;; esac",
    "case x in a|b) c;; d) e;; esac",
    "case x in (a|b) c;; (d) e;; esac",
    "case $x in\n\n  a)\n\n   b\n\n   ;;\n\n  *)\n   c\n\nesac",
    "case x\nin a) b;; esac",
    "case x in (esac) a;; esac",
    "case x in a|esac) b;; esac",
    "case x in a) b;; (esac) c;; esac",
    "case in in in) a;; esac",
    "case x in if) a;; then) b;; esac",
    "case x in a) (b) ;; esac",
    "case x in a) b; c;; d) e & esac",
    "case x in a) b;; esac >f",
    "case x in \"a b\"|$y|*.z) c;; esac",
    "case x in # note\n a) # note\n b;; # note\nesac # note",
    "case x in a) case y in b) c;; esac;; esac",
    "case x in a) if b; then c; fi;; d) { e; };; esac",
    "case x in a) b;; esac; case y in c) d;; esac",
];

const FUNCTIONS: &[&str] = &[
    "f() { a; }",
    "f ( ) { a; }",
    "f()\n\n{ a; }",
    "f() (a)",
    "f() if a; then b; fi",
    "f() { a; } >x",
    "f() a",
    "f() a=1",
    "f() >x",
    "f() a >x | b",
    "f() g() { a; }",
    "f() { a; } | b",
    "f() { a; } && b",
    "f\\\ng() { a; }",
    "_a1() { a; }",
    "echo() { a; }",
    "f() { g() { a; }; }",
    "f() # note\n{ a; }",
    "f(\\\n) { a; }",
    "{ # note\n a; }",
    "( # note\n a )",
];

const WORDS: &[&str] = &[
    "'a b'",
    "\"a b\"",
    "a\\ b \\$x \\\\",
    "\\é",
    "a'b'\"c\"d",
    "\"\\q \\$ \\` \\\" \\\\\"",
    "''  \"\"",
    "a\\\nb",
    "\"a\\\nb\"",
    "'a\\\nb'",
    "~ ~user ~/x a~",
    "* ?.txt [abc] [!abc] \"*\" \\*",
    "$ a$ \"$\"",
    "\"it's\" 'say \"hi\"'",
    "\"a $x `y` $(z) $((1))\"",
];

const EXPANSIONS: &[&str] = &[
    "$x ${x} ${#x} $_a1 $é",
    "$? $$ $! $0 $# $@ $* $- $1 $9 $10 ${10}",
    "${x:-a b} ${x:=$y} ${x:?msg} ${x:+w}",
    "${x-a} ${x=a} ${x?a} ${x+a}",
    "${x#p} ${x##p} ${x%p} ${x%%p}",
    "\"${x:-\"a b\"}\" ${x:-${y:-$(z)}}",
    "$\\\nx ${\\\ny} $z\\\nw",
    "$((1 + 2)) $(( $x * (2 + $(y)) )) $(())",
    "\"$((1 + \"$x\"))\"",
    "${x//a/b} ${x:1:2} ${#x:-y} ${}",
];

const SUBSTITUTIONS: &[&str] = &[
    "$(a)",
    "$()",
    "$( )",
    "$(\n)",
    "$(# only a comment\n)",
    "$(a | b; c)",
    "x$(a)y",
    "\"$(a \"b c\")\"",
    "$(a $(b $(c)))",
    "$(if a; then b; fi)",
    "$(case x in a) b;; esac)",
    "$( (a) )",
    "$(cat <<E\nhi\nE\n)",
    "$(a # )\n)",
    "é$(বাংলা x)",
    // `)` ends a here-document that has not started yet. It gets an empty body.
    "$(cat <<E)",
    "x=$(cat <<E)\nhello\nE\n",
];

/// Each line checks that spans inside backticks point at the source, and that one level of
/// backslashes came off, as `dash` does it.
const BACKTICKS: &[&str] = &[
    "`a`",
    "``",
    "` `",
    "`a | b; c`",
    "x`a`y",
    "`if a; then b; fi`",
    "`a $(b)`",
    "$(a `b`)",
    "`echo \\$x`",
    "`echo $x`",
    "`echo a\\$`",
    "`echo a\\\\b`",
    "`echo a\\\\\\$b`",
    "`echo \\q`",
    "`a \\`b\\``",
    "`a \\`b \\\\\\`c\\\\\\`\\``",
    "`a \\\"b\\\"`",
    "\"`a \\\"b c\\\"`\"",
    "${x:-`a \\\"b\\\"`}",
    "\"${x:-`a \\\"b\\\"`}\"",
    "$((`a \\\"b\\\"`))",
    "`a\\\nb`",
    "`a 'b\\\nc'`",
    "`é \\$x বাংলা`",
    "A=`b`",
    // A closed body is a whole script, which may end in the middle of these.
    "`cat <<E`",
    "`cat <<E\nbody`",
    "`a\\\\`",
];

const ALL_GROUPS: &[(&str, &[&str])] = &[
    ("simple", SIMPLE),
    ("assignments", ASSIGNMENTS),
    ("pipelines", PIPELINES),
    ("lists", LISTS),
    ("redirections", REDIRECTIONS),
    ("bash_forms", BASH_FORMS),
    ("here_docs", HERE_DOCS),
    ("subshells", SUBSHELLS),
    ("groups", GROUPS),
    ("ifs", IFS),
    ("loops", LOOPS),
    ("fors", FORS),
    ("cases", CASES),
    ("functions", FUNCTIONS),
    ("words", WORDS),
    ("expansions", EXPANSIONS),
    ("substitutions", SUBSTITUTIONS),
    ("backticks", BACKTICKS),
];

/// Lines that cannot go on: both the parser and `/bin/sh` reject them straight away.
const HARD_ERRORS: &[&str] = &[
    // Stray operators.
    ";",
    "a;;b",
    "a ; ; b",
    "a;\n;b",
    "& a",
    "a & &",
    "a &; b",
    "| a",
    "|| a",
    "&& a",
    "a | | b",
    "a |& b",
    "a || || b",
    "a &&& b",
    ";;",
    ")",
    // Closing words with nothing open.
    "fi",
    "then",
    "do",
    "done",
    "esac",
    "in",
    "elif",
    "else",
    "}",
    "if a; then b; fi; then",
    "{ a; } }",
    "(a))",
    // Negation.
    "!",
    "! ;",
    "! !",
    "! ! a",
    "! a | ! b",
    "a | ! b",
    "!\ntrue",
    // Empty bodies.
    "()",
    "( )",
    "{ }",
    "{ ; }",
    "if then a; fi",
    "if ; then a; fi",
    "if a; then fi",
    "if a; then ; fi",
    "if a; then b; else fi",
    "if a; then b; elif c; then fi",
    "while do a; done",
    "while a; do done",
    "while a; do ; done",
    "until a; do done",
    "for i in a; do done",
    "for do in in; do done; done",
    // Words that are missing or in the wrong place.
    "if a; fi",
    "if a then b; fi",
    "if a; then b; fi c",
    "if a; then b; else c; else d; fi",
    "if a; then b; else c; elif d; then e; fi",
    "while a; do b; done done",
    "{ a; } ; }",
    "f() { a; } b",
    "a=b() { c; }",
    "function f() { a; }",
    "{ a; } b",
    "(a) b",
    "(echo a) (echo b)",
    "for i in a; do b; done c",
    "a (b)",
    "echo a(",
    "a=(1 2)",
    "A=1 f() { a; }",
    ">x f() { a; }",
    "a b() { c; }",
    "A=1 { a; }",
    ">f { a; }",
    "A=1 (a)",
    ">f (a)",
    "a <(b)",
    "function f { a; }",
    "select i in a; do b; done",
    // `for`.
    "for",
    "for 1a",
    "for \"i\" in a; do b; done",
    "for i in a & do b; done",
    "for i in a >f; do b; done",
    "for i in a; ; do b; done",
    "for i in a\n\n;do b; done",
    // `case`.
    "case",
    "case x; in a) b;; esac",
    "case 2>f in a) b;; esac",
    "case x in esac) a;; esac",
    "case x in ;; esac",
    "case x in a;; esac",
    "case x in a b) c;; esac",
    "case x in |a) c;; esac",
    "case x in a|) c;; esac",
    "case x in () c;; esac",
    "case x in a) b;& c) d;; esac",
    "case x in a) ;; ;; esac",
    "case x in a) b; ; esac",
    "case x in 2>f) b;; esac",
    "case x in a) b;; esac c",
    "case x in a) b; esac; esac",
    "case x in a",
    "case x in (a",
    // Functions.
    "f(",
    "f() ;",
    "1a() { a; }",
    "a-b() { a; }",
    "\"f\"() { a; }",
    "if() { a; }",
    "f() >x { a; }",
    "break() { a; }",
    "continue() { a; }",
    "eval() { a; }",
    "exec() { a; }",
    "exit() { a; }",
    "export() { a; }",
    "readonly() { a; }",
    "return() { a; }",
    "set() { a; }",
    "shift() { a; }",
    "times() { a; }",
    "trap() { a; }",
    "unset() { a; }",
    // Redirections with no word.
    "a >",
    "a <",
    "a <<",
    "a << ;",
    "a >&",
    "a >& ;",
    "a > ;",
    "a >\nb",
    "2>",
    "a > > f",
    "a << 2>f",
    // Inside closed substitutions, where nothing typed later can help.
    "$(if a)",
    "$(a |)",
    "$(;)",
    "$(a;;)",
    "\"$(a;;)\"",
    "${x:-$(a;;)}",
    "$((1 + $(a;;)))",
    "`echo 'a`",
    "`echo \"a`",
    "`echo $(a;;)`",
    "cat <<EOF\n$(a;;)\nEOF\n",
];

/// Lines where `/bin/sh` runs out of input where more could follow. A shell reading from a
/// terminal should read another line.
const UNFINISHED: &[&str] = &[
    "a |",
    "a ||",
    "a &&",
    "a |\n",
    "(",
    "(a",
    "(a;",
    "((a)",
    "{",
    "{ a",
    "{ a }",
    "{ { a; }",
    "if",
    "if a",
    "if a;",
    "if a; then",
    "if a; then b",
    "if a; then b; elif",
    "if a; then b; else",
    "if a; then b; else c",
    "while",
    "while a",
    "while a; do",
    "while a; do b",
    "until a",
    "for i",
    "for i;",
    "for i in",
    "for i in a",
    "for i in a b;",
    "for i do",
    "for i in a b; do",
    "case x",
    "case x in",
    "case x in a)",
    "case x in a) b",
    "case x in a) b esac",
    "case x in a) b;;",
    "case x in a) b;; c) d esac",
    "f()",
    "f( )",
    "f()\n",
    "f() {",
];

/// Lines that `/bin/sh` accepts because it silently drops everything after the `;;`. `bash`
/// reports the error when it runs them, and so does this parser, before anything runs.
const DROPPED_BY_SH: &[&str] = &[
    "`a;;`",
    "\"`a;;`\"",
    "$(( `a;;` ))",
    "`echo \\`a;;\\``",
    "cat <<EOF\n`a;;`\nEOF\n",
];

/// Lines that `dash` accepts or wants more of because it reads whatever token comes after
/// `(` or `|` as a pattern, even a line ending. POSIX and `bash` both reject them.
const LINE_ENDING_AS_PATTERN: &[&str] = &[
    "case x in (",
    "case x in a|",
    "case x in (\n) a;; esac",
    "case x in a|\n) b;; esac",
];

/// Lines where `/bin/sh` reads a closed backtick body as if more could still come. Both reject
/// them, but here they are final, because text typed after the closing backtick cannot reach
/// inside it.
const CLOSED_BACKTICKS: &[&str] = &["`if a`", "`a | `", "`echo $(`"];

/// The lexer's errors reach the caller along with the parser's.
const LEX_ERRORS: &[&str] = &[
    "a 'open",
    "a \"open $x",
    "a $'open",
    "a `open",
    "a $(open",
    "a $((1 +",
    "a ${open",
    "cat <<EOF\nbody",
    "a\\",
    "a \\\n",
    "if 'open",
    "a ;; 'open",
    "cat <<EOF ;; b\nbody",
    "$(if a",
    "$(a ;; b",
    "$(a 'b",
    "\"$(a",
    "`if a",
    "`a ;; b",
    "`a 'b",
];

/// What tolerant mode keeps while the user is still typing, and how it gets past mistakes.
const TOLERANT: &[&str] = &[
    "",
    "a |",
    "a &&",
    "a && b |",
    "a >",
    "a > ; b",
    "if a; then b",
    "if a",
    "if",
    "if a; then b; elif c",
    "if a; then b; else c",
    "while a; do b",
    "for",
    "for i",
    "for i in a b",
    "for i in a; do b",
    "for 1a in x; do b; done",
    "case",
    "case x",
    "case x in a",
    "case x in a) b",
    "case x in a) b;; c",
    "(a",
    "{ a",
    "f()",
    "f() {",
    "1a() { b; }",
    "! ",
    "a ;; b\nc",
    "a | | b; c",
    ") a; b",
    "(a b) c; d",
    "a (b; c)\nd",
    "if a; then b; ) c; fi; d",
    "$(a;;) b; c",
    "`a;;` b; c",
    "a <<EOF ;; b\nbody\nEOF\nc <<EOF\nsecond\nEOF\n",
    "; ; a",
    "fi; a",
    "fi\na",
    "fi & a",
    "a | ; b",
    "| a; b",
    // Regression: an empty list right before a removed backslash stays inside its parent.
    "`for i;\\$x`",
];

/// Writes a tree in a form that can be reviewed: every node with the source text its span
/// covers. Pipelines and and-or lists with one member get no line of their own.
struct Render<'a> {
    src: &'a str,
    out: String,
}

impl Render<'_> {
    fn line(&mut self, indent: usize, text: &str) {
        let _ = writeln!(self.out, "{:width$}{text}", "", width = indent * 2);
    }

    fn at(&self, span: Span) -> String {
        format!("{:?}", span.text(self.src))
    }

    fn list(&mut self, list: &List, indent: usize) {
        for item in &list.items {
            let background = if item.background { " background" } else { "" };
            let text = format!("item {}{background}", self.at(item.span));
            self.line(indent, &text);
            self.and_or(&item.and_or, indent + 1);
        }
    }

    fn and_or(&mut self, and_or: &AndOr, indent: usize) {
        if and_or.rest.is_empty() {
            self.pipeline(&and_or.first, indent);
            return;
        }
        let text = format!("and-or {}", self.at(and_or.span));
        self.line(indent, &text);
        self.pipeline(&and_or.first, indent + 1);
        for part in &and_or.rest {
            let op = match part.op {
                AndOrOp::And => "&&",
                AndOrOp::Or => "||",
            };
            self.line(indent + 1, op);
            self.pipeline(&part.pipeline, indent + 1);
        }
    }

    fn pipeline(&mut self, pipeline: &Pipeline, indent: usize) {
        let mut indent = indent;
        if pipeline.negated || pipeline.commands.len() > 1 {
            let negated = if pipeline.negated { " negated" } else { "" };
            let text = format!("pipeline {}{negated}", self.at(pipeline.span));
            self.line(indent, &text);
            indent += 1;
        }
        for command in &pipeline.commands {
            self.command(command, indent);
        }
    }

    fn command(&mut self, command: &Command, indent: usize) {
        let inner = indent + 1;
        match &command.kind {
            CommandKind::Simple(SimpleCommand { assignments, words }) => {
                let text = format!("simple {}", self.at(command.span));
                self.line(indent, &text);
                for assignment in assignments {
                    let text = format!(
                        "assign {} name={:?} at {}",
                        self.at(assignment.span),
                        assignment.name,
                        self.at(assignment.name_span),
                    );
                    self.line(inner, &text);
                    self.word("value", &assignment.value, inner + 1);
                }
                self.words("word", words, inner);
            }
            CommandKind::Subshell(list) => self.body("subshell", command.span, list, indent),
            CommandKind::Group(list) => self.body("group", command.span, list, indent),
            CommandKind::If(IfCommand {
                branches,
                else_body,
            }) => {
                let text = format!("if {}", self.at(command.span));
                self.line(indent, &text);
                for branch in branches {
                    let text = format!("branch {}", self.at(branch.span));
                    self.line(inner, &text);
                    self.line(inner + 1, "condition");
                    self.list(&branch.condition, inner + 2);
                    self.line(inner + 1, "then");
                    self.list(&branch.body, inner + 2);
                }
                if let Some(body) = else_body {
                    self.line(inner, "else");
                    self.list(body, inner + 1);
                }
            }
            CommandKind::While(body) => self.looping("while", command.span, body, indent),
            CommandKind::Until(body) => self.looping("until", command.span, body, indent),
            CommandKind::For(ForCommand {
                name,
                name_span,
                words,
                body,
            }) => {
                let text = format!(
                    "for {} name={name:?} at {}",
                    self.at(command.span),
                    self.at(*name_span)
                );
                self.line(indent, &text);
                match words {
                    Some(words) => {
                        self.line(inner, "in");
                        self.words("word", words, inner + 1);
                    }
                    None => self.line(inner, "no-in"),
                }
                self.line(inner, "do");
                self.list(body, inner + 1);
            }
            CommandKind::Case(CaseCommand { word, arms }) => {
                let text = format!("case {}", self.at(command.span));
                self.line(indent, &text);
                self.word("word", word, inner);
                for arm in arms {
                    let text = format!("arm {}", self.at(arm.span));
                    self.line(inner, &text);
                    self.words("pattern", &arm.patterns, inner + 1);
                    self.list(&arm.body, inner + 1);
                }
            }
            CommandKind::Function(FunctionDef {
                name,
                name_span,
                body,
            }) => {
                let text = format!(
                    "function {} name={name:?} at {}",
                    self.at(command.span),
                    self.at(*name_span)
                );
                self.line(indent, &text);
                self.command(body, inner);
            }
        }
        for redirect in &command.redirects {
            self.redirect(redirect, inner);
        }
    }

    fn body(&mut self, label: &str, span: Span, list: &List, indent: usize) {
        let text = format!("{label} {}", self.at(span));
        self.line(indent, &text);
        self.list(list, indent + 1);
    }

    fn looping(&mut self, label: &str, span: Span, body: &LoopCommand, indent: usize) {
        let text = format!("{label} {}", self.at(span));
        self.line(indent, &text);
        self.line(indent + 1, "condition");
        self.list(&body.condition, indent + 2);
        self.line(indent + 1, "do");
        self.list(&body.body, indent + 2);
    }

    fn redirect(&mut self, redirect: &Redirect, indent: usize) {
        let fd = redirect.fd.map_or(String::new(), |fd| format!(" fd={fd}"));
        match &redirect.kind {
            RedirectKind::Target { op, word } => {
                let text = format!("redirect {} {op:?}{fd}", self.at(redirect.span));
                self.line(indent, &text);
                self.word("target", word, indent + 1);
            }
            RedirectKind::HereDoc(doc) => {
                let text = format!(
                    "here-doc {}{fd} delimiter={:?}{}{}",
                    self.at(redirect.span),
                    doc.delimiter,
                    if doc.quoted { " quoted" } else { "" },
                    if doc.strip_tabs { " strip-tabs" } else { "" },
                );
                self.line(indent, &text);
                let text = format!("body {}", self.at(doc.body_span));
                self.line(indent + 1, &text);
                self.parts(&doc.body, indent + 2);
            }
        }
    }

    fn words(&mut self, label: &str, words: &[Word], indent: usize) {
        for word in words {
            self.word(label, word, indent);
        }
    }

    fn word(&mut self, label: &str, word: &Word, indent: usize) {
        let text = format!("{label} {}", self.at(word.span));
        self.line(indent, &text);
        if let [
            WordPart {
                kind: PartKind::Literal(literal),
                span,
            },
        ] = word.parts.as_slice()
            && literal == span.text(self.src)
            && *span == word.span
        {
            return;
        }
        self.parts(&word.parts, indent + 1);
    }

    fn parts(&mut self, parts: &[WordPart], indent: usize) {
        for part in parts {
            let source = self.at(part.span);
            match &part.kind {
                PartKind::Literal(text) => self.leaf("literal", text, &source, indent),
                PartKind::Escaped(c) => self.leaf("escaped", &c.to_string(), &source, indent),
                PartKind::SingleQuoted(text) => self.leaf("single", text, &source, indent),
                PartKind::AnsiCQuoted(text) => self.leaf("ansi-c", text, &source, indent),
                PartKind::DoubleQuoted(parts) => {
                    self.line(indent, &format!("double {source}"));
                    self.parts(parts, indent + 1);
                }
                PartKind::Arithmetic(parts) => {
                    self.line(indent, &format!("arithmetic {source}"));
                    self.parts(parts, indent + 1);
                }
                PartKind::CommandSub(list) => {
                    self.line(indent, &format!("command-sub {source}"));
                    self.list(list, indent + 1);
                }
                PartKind::Parameter(parameter) => {
                    let mut text = format!(
                        "param {source} name={:?} at {}",
                        parameter.name,
                        self.at(parameter.name_span)
                    );
                    if parameter.length {
                        text.push_str(" length");
                    }
                    if let Some((op, span)) = parameter.op {
                        let _ = write!(text, " {op:?} at {}", self.at(span));
                    }
                    self.line(indent, &text);
                    self.parts(&parameter.arg, indent + 1);
                }
            }
        }
    }

    /// Writes a piece of text. The source is shown only if it says something else, as it
    /// does inside backticks and for quotes.
    fn leaf(&mut self, label: &str, text: &str, source: &str, indent: usize) {
        let shown = format!("{text:?}");
        if shown == source {
            self.line(indent, &format!("{label} {shown}"));
        } else {
            self.line(indent, &format!("{label} {shown} at {source}"));
        }
    }
}

fn render_tree(src: &str, list: &List, indent: usize) -> String {
    let mut render = Render {
        src,
        out: String::new(),
    };
    render.list(list, indent);
    render.out
}

/// Renders lines through `parse`, which must accept them.
fn render_lines(lines: &[&str]) -> String {
    let mut out = String::new();
    for src in lines {
        let _ = writeln!(out, "{src:?}");
        match parse(src) {
            Ok(list) => out.push_str(&render_tree(src, &list, 1)),
            Err(failure) => panic!("{src:?} gave {:?}", failure.errors),
        }
        let _ = writeln!(out);
    }
    out
}

fn render_error(src: &str, error: &ParseError) -> String {
    let incomplete = if error.incomplete { " incomplete" } else { "" };
    format!("{:?} at {:?}{incomplete}", error.kind, error.span.text(src))
}

/// Renders what strict mode says about lines it must reject.
fn render_failures(lines: &[&str]) -> String {
    let mut out = String::new();
    for src in lines {
        let _ = writeln!(out, "{src:?}");
        match parse(src) {
            Ok(_) => panic!("{src:?} should fail"),
            Err(failure) => {
                for error in &failure.errors {
                    let _ = writeln!(out, "  {}", render_error(src, error));
                }
            }
        }
    }
    out
}

/// Renders what tolerant mode makes of lines with problems: every error, and the tree.
fn render_tolerant(lines: &[&str]) -> String {
    let mut out = String::new();
    for src in lines {
        let _ = writeln!(out, "{src:?}");
        let parsed = parse_tolerant(src);
        for error in &parsed.errors {
            let _ = writeln!(out, "  ERROR {}", render_error(src, error));
        }
        out.push_str(&render_tree(src, &parsed.list, 1));
        let _ = writeln!(out);
    }
    out
}

#[test]
fn ast_snapshot() {
    for (name, lines) in ALL_GROUPS {
        insta::assert_snapshot!(format!("ast_{name}"), render_lines(lines));
    }
}

#[test]
fn strict_errors_snapshot() {
    insta::assert_snapshot!("strict_hard_errors", render_failures(HARD_ERRORS));
    insta::assert_snapshot!("strict_unfinished", render_failures(UNFINISHED));
}

#[test]
fn lex_errors_snapshot() {
    insta::assert_snapshot!("lex_errors", render_failures(LEX_ERRORS));
}

#[test]
fn tolerant_snapshot() {
    insta::assert_snapshot!("tolerant", render_tolerant(TOLERANT));
}

/// Every label the renderer can write must come up in the test lines, so the snapshots do
/// cover every construct.
#[test]
fn every_construct_is_covered() {
    let mut all = String::new();
    for (_, lines) in ALL_GROUPS {
        all.push_str(&render_lines(lines));
    }
    for label in [
        "item ",
        " background",
        "and-or ",
        "&&",
        "||",
        "pipeline ",
        " negated",
        "simple ",
        "assign ",
        "value ",
        "word ",
        "subshell ",
        "group ",
        "if ",
        "branch ",
        "condition",
        "then",
        "else",
        "while ",
        "until ",
        "do",
        "for ",
        "in",
        "no-in",
        "case ",
        "arm ",
        "pattern ",
        "function ",
        "redirect ",
        "target ",
        "fd=",
        "here-doc ",
        "body ",
        " quoted",
        " strip-tabs",
        "literal ",
        "escaped ",
        "single ",
        "ansi-c ",
        "double ",
        "arithmetic ",
        "command-sub ",
        "param ",
        " length",
        // Redirection operators.
        "Input",
        "Output",
        "Append",
        "ReadWrite",
        "Clobber",
        "DupInput",
        "DupOutput",
        "OutputAndError",
        "AppendAndError",
        "HereString",
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
    ] {
        assert!(all.contains(label), "no test line produces {label:?}");
    }

    // Every kind of problem must come up too.
    let mut errors = String::new();
    for src in HARD_ERRORS.iter().chain(UNFINISHED).chain(LEX_ERRORS) {
        for error in parse_tolerant(src).errors {
            let _ = writeln!(errors, "{:?}", error.kind);
        }
    }
    let kinds = [
        ParseErrorKind::BadFunctionName,
        ParseErrorKind::BadForName,
        ParseErrorKind::Lex(LexErrorKind::UnterminatedSingleQuote),
        ParseErrorKind::Lex(LexErrorKind::UnterminatedCommandSub),
    ];
    for kind in kinds {
        let name = format!("{kind:?}");
        assert!(errors.contains(&name), "no test line produces {name}");
    }
    let reserved = |word| Expected::Reserved(word);
    for expected in [
        Expected::Command,
        Expected::CommandEnd,
        Expected::RedirectTarget,
        Expected::HereDocDelimiter,
        Expected::CaseWord,
        Expected::CasePattern,
        Expected::RightParen,
        reserved(ReservedWord::Then),
        reserved(ReservedWord::Fi),
        reserved(ReservedWord::Do),
        reserved(ReservedWord::Done),
        reserved(ReservedWord::In),
        reserved(ReservedWord::Esac),
        reserved(ReservedWord::RightBrace),
    ] {
        let name = format!("{expected:?}");
        assert!(errors.contains(&name), "no test line expects {name}");
    }
}

fn assert_span_valid(src: &str, span: Span) {
    assert!(span.start <= span.end, "{span:?} backwards in {src:?}");
    assert!(span.end <= src.len(), "{span:?} past the end of {src:?}");
    assert!(
        src.is_char_boundary(span.start) && src.is_char_boundary(span.end),
        "{span:?} splits a character in {src:?}"
    );
}

fn assert_inside(src: &str, outer: Span, inner: Span) {
    assert!(
        outer.start <= inner.start && inner.end <= outer.end,
        "{inner:?} leaves {outer:?} in {src:?}"
    );
}

/// Checks that nodes stay inside their parents and in order.
fn check_list(src: &str, outer: Span, list: &List) {
    assert_inside(src, outer, list.span);
    let mut cursor = list.span.start;
    for item in &list.items {
        assert!(
            item.span.start >= cursor,
            "{item:?} out of order in {src:?}"
        );
        cursor = item.span.end;
        assert_inside(src, list.span, item.span);
        assert_inside(src, item.span, item.and_or.span);
        check_pipeline(src, item.and_or.span, &item.and_or.first);
        for part in &item.and_or.rest {
            check_pipeline(src, item.and_or.span, &part.pipeline);
        }
    }
}

fn check_pipeline(src: &str, outer: Span, pipeline: &Pipeline) {
    assert_inside(src, outer, pipeline.span);
    assert!(!pipeline.commands.is_empty(), "{pipeline:?} in {src:?}");
    let mut cursor = pipeline.span.start;
    for command in &pipeline.commands {
        assert!(
            command.span.start >= cursor,
            "{command:?} out of order in {src:?}"
        );
        cursor = command.span.end;
        check_command(src, pipeline.span, command);
    }
}

fn check_command(src: &str, outer: Span, command: &Command) {
    assert_inside(src, outer, command.span);
    let span = command.span;
    match &command.kind {
        CommandKind::Simple(SimpleCommand { assignments, words }) => {
            for assignment in assignments {
                assert_inside(src, span, assignment.span);
                assert_inside(src, assignment.span, assignment.name_span);
                check_word(src, assignment.span, &assignment.value);
            }
            check_words(src, span, words);
        }
        CommandKind::Subshell(list) | CommandKind::Group(list) => check_list(src, span, list),
        CommandKind::If(IfCommand {
            branches,
            else_body,
        }) => {
            assert!(!branches.is_empty(), "{command:?} in {src:?}");
            for branch in branches {
                assert_inside(src, span, branch.span);
                check_list(src, branch.span, &branch.condition);
                check_list(src, branch.span, &branch.body);
            }
            if let Some(body) = else_body {
                check_list(src, span, body);
            }
        }
        CommandKind::While(body) | CommandKind::Until(body) => {
            check_list(src, span, &body.condition);
            check_list(src, span, &body.body);
        }
        CommandKind::For(ForCommand {
            name_span,
            words,
            body,
            ..
        }) => {
            assert_inside(src, span, *name_span);
            if let Some(words) = words {
                check_words(src, span, words);
            }
            check_list(src, span, body);
        }
        CommandKind::Case(CaseCommand { word, arms }) => {
            check_word(src, span, word);
            for arm in arms {
                assert_inside(src, span, arm.span);
                check_words(src, arm.span, &arm.patterns);
                check_list(src, arm.span, &arm.body);
            }
        }
        CommandKind::Function(FunctionDef {
            name_span, body, ..
        }) => {
            assert_inside(src, span, *name_span);
            check_command(src, span, body);
        }
    }
    for redirect in &command.redirects {
        assert_inside(src, span, redirect.span);
        match &redirect.kind {
            RedirectKind::Target { word, .. } => check_word(src, redirect.span, word),
            RedirectKind::HereDoc(doc) => check_parts(src, doc.body_span, &doc.body),
        }
    }
}

fn check_words(src: &str, outer: Span, words: &[Word]) {
    for word in words {
        check_word(src, outer, word);
    }
}

fn check_word(src: &str, outer: Span, word: &Word) {
    assert_inside(src, outer, word.span);
    check_parts(src, word.span, &word.parts);
}

fn check_parts(src: &str, outer: Span, parts: &[WordPart]) {
    let mut cursor = outer.start;
    for part in parts {
        assert!(
            part.span.start >= cursor,
            "{part:?} out of order in {src:?}"
        );
        assert_inside(src, outer, part.span);
        cursor = part.span.end;
        match &part.kind {
            PartKind::DoubleQuoted(parts) | PartKind::Arithmetic(parts) => {
                check_parts(src, part.span, parts);
            }
            PartKind::Parameter(parameter) => {
                assert_inside(src, part.span, parameter.name_span);
                if let Some((_, span)) = parameter.op {
                    assert_inside(src, part.span, span);
                }
                check_parts(src, part.span, &parameter.arg);
            }
            PartKind::CommandSub(list) => check_list(src, part.span, list),
            _ => {}
        }
    }
}

/// Checks what must hold for any input at all.
fn assert_invariants(src: &str) {
    let parsed = parse_tolerant(src);
    let mut copy = parsed.list.clone();
    copy.map_spans(&mut |span| {
        assert_span_valid(src, span);
        span
    });
    assert_eq!(copy, parsed.list, "map_spans changed a tree for {src:?}");
    check_list(src, Span::new(0, src.len()), &parsed.list);

    let mut cursor = 0;
    for error in &parsed.errors {
        assert_span_valid(src, error.span);
        assert!(
            error.span.start >= cursor,
            "{error:?} out of order in {src:?}"
        );
        cursor = error.span.start;
    }

    // The two modes agree on whether there is a problem, and on the tree if there is none.
    match parse(src) {
        Ok(list) => {
            assert_eq!(
                parsed.errors,
                [],
                "only tolerant mode complains about {src:?}"
            );
            assert_eq!(
                list, parsed.list,
                "the modes build different trees for {src:?}"
            );
        }
        Err(failure) => {
            assert!(!failure.errors.is_empty(), "no errors for {src:?}");
            assert!(
                !parsed.errors.is_empty(),
                "only strict mode complains about {src:?}"
            );
            assert_eq!(
                failure.is_incomplete(),
                parsed.is_incomplete(),
                "the modes disagree on whether {src:?} can go on"
            );
        }
    }
}

fn all_lines() -> impl Iterator<Item = &'static str> {
    ALL_GROUPS
        .iter()
        .flat_map(|(_, lines)| lines.iter())
        .chain(HARD_ERRORS)
        .chain(UNFINISHED)
        .chain(LEX_ERRORS)
        .chain(TOLERANT)
        .copied()
}

/// Text being typed is a prefix of a finished line, so every prefix has to work.
#[test]
fn invariants_hold_for_every_prefix() {
    for src in all_lines() {
        for end in (0..=src.len()).filter(|&end| src.is_char_boundary(end)) {
            assert_invariants(src.get(..end).unwrap_or(""));
        }
    }
}

/// Returns the commands of the substitution that is the first thing in `list`.
fn first_substitution(list: &List) -> &List {
    let CommandKind::Simple(simple) = &list.items[0].and_or.first.commands[0].kind else {
        panic!("{list:?}");
    };
    let word = simple.words.first().unwrap_or_else(|| panic!("{list:?}"));
    let PartKind::CommandSub(body) = &word.parts[0].kind else {
        panic!("{word:?}");
    };
    body
}

/// Backticks move spans through a table, so check that a nested body lands on the very
/// bytes it came from.
#[test]
fn spans_inside_backticks_point_at_the_source() {
    let src = "`a \\`b\\$c\\`` d";
    let list = parse(src).unwrap();
    let body = first_substitution(&list);
    let CommandKind::Simple(inner) = &body.items[0].and_or.first.commands[0].kind else {
        panic!("{body:?}");
    };
    assert_eq!(inner.words[0].span.text(src), "a");
    let PartKind::CommandSub(nested) = &inner.words[1].parts[0].kind else {
        panic!("{inner:?}");
    };
    // A removed backslash at the very start of a span stays outside it.
    assert_eq!(inner.words[1].span.text(src), "`b\\$c\\`");
    assert_eq!(nested.span.text(src), "b\\$c");
    let CommandKind::Simple(deepest) = &nested.items[0].and_or.first.commands[0].kind else {
        panic!("{nested:?}");
    };
    let word = &deepest.words[0];
    assert_eq!(word.parts.len(), 2);
    assert_eq!(word.parts[1].span.text(src), "$c");
    assert!(matches!(
        &word.parts[1].kind,
        PartKind::Parameter(parameter) if parameter.name == "c"
    ));
}

/// With no backslashes to remove, `` `P` `` and `$(P)` hold the same tree, one byte apart, for
/// every construct. This catches a span that the move back to the source forgets.
#[test]
fn backtick_trees_match_command_sub_trees() {
    for (_, lines) in ALL_GROUPS {
        for line in lines.iter().filter(|line| !line.contains(['`', '\\', '#'])) {
            let backtick_src = format!("`{line}\n`");
            let command_sub_src = format!("$({line}\n)");
            let backtick = parse(&backtick_src).unwrap();
            let command_sub = parse(&command_sub_src).unwrap();
            let mut moved = first_substitution(&backtick).clone();
            moved.map_spans(&mut |span| Span::new(span.start + 1, span.end + 1));
            assert_eq!(&moved, first_substitution(&command_sub), "{line:?}");
        }
    }
}

#[test]
fn errors_inside_backticks_point_at_the_source() {
    let src = "x `a \\$b ;; c` y";
    let failure = parse(src).unwrap_err();
    assert_eq!(failure.errors.len(), 1);
    assert_eq!(failure.errors[0].span.text(src), ";;");
    assert!(!failure.is_incomplete());
}

#[test]
fn strict_reports_one_parser_error_and_tolerant_reports_them_all() {
    let src = "a ;; b\nc ;; d\ne";
    let failure = parse(src).unwrap_err();
    assert_eq!(failure.errors.len(), 1);
    assert_eq!(failure.errors[0].span, Span::new(2, 4));

    let parsed = parse_tolerant(src);
    assert_eq!(parsed.errors.len(), 2);
    assert_eq!(parsed.errors[1].span, Span::new(9, 11));
    // Tolerant mode gets to the commands after both mistakes.
    let items: Vec<&str> = parsed
        .list
        .items
        .iter()
        .map(|item| item.span.text(src))
        .collect();
    assert_eq!(items, ["a", "b", "c", "d", "e"]);
}

#[test]
fn a_tree_that_outlives_its_source_keeps_its_text() {
    let list = {
        let src = String::from("f() { echo \"a $b\" c\\\nd; }");
        parse(&src).unwrap()
    };
    let CommandKind::Function(function) = &list.items[0].and_or.first.commands[0].kind else {
        panic!("{list:?}");
    };
    assert_eq!(function.name, "f");
    let CommandKind::Group(group) = &function.body.kind else {
        panic!("{function:?}");
    };
    let CommandKind::Simple(simple) = &group.items[0].and_or.first.commands[0].kind else {
        panic!("{group:?}");
    };
    assert!(matches!(
        &simple.words[2].parts[..],
        [WordPart { kind: PartKind::Literal(text), .. }] if text == "cd"
    ));
}

/// `/bin/sh -n` parses without running anything.
fn sh_accepts(line: &str) -> bool {
    Process::new("/bin/sh")
        .args(["-n", "-c", line])
        .stderr(Stdio::null())
        .status()
        .unwrap_or_else(|error| panic!("cannot run /bin/sh: {error}"))
        .success()
}

/// Whether `/bin/sh` runs out of input on this line, even after its line ending.
fn sh_wants_more(line: &str) -> bool {
    let mut child = Process::new("/bin/sh")
        .arg("-n")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("cannot run /bin/sh: {error}"));
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(format!("{line}\n").as_bytes());
    }
    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("cannot wait for /bin/sh: {error}"));
    String::from_utf8_lossy(&output.stderr).contains("end of file unexpected")
}

#[test]
fn accepted_lines_match_sh() {
    for (name, lines) in ALL_GROUPS {
        if *name == "bash_forms" {
            continue;
        }
        for line in *lines {
            let parsed = parse(line);
            assert!(parsed.is_ok(), "{line:?} gave {parsed:?}");
            assert!(sh_accepts(line), "only the parser accepts {line:?}");
        }
    }
}

#[test]
fn rejected_lines_match_sh() {
    for line in HARD_ERRORS {
        assert!(!sh_accepts(line), "only the parser rejects {line:?}");
        assert!(!sh_wants_more(line), "/bin/sh wants more after {line:?}");
        let failure = parse(line).unwrap_err();
        assert!(!failure.is_incomplete(), "{line:?} gave {failure:?}");
    }
}

#[test]
fn unfinished_lines_match_sh() {
    for line in UNFINISHED {
        assert!(
            sh_wants_more(line),
            "/bin/sh does not want more after {line:?}"
        );
        let failure = parse(line).unwrap_err();
        assert!(failure.is_incomplete(), "{line:?} gave {failure:?}");
    }
}

#[test]
fn known_differences_from_sh() {
    let lines = DROPPED_BY_SH
        .iter()
        .chain(CLOSED_BACKTICKS)
        .chain(LINE_ENDING_AS_PATTERN);
    for line in lines {
        let failure = parse(line).unwrap_err();
        assert!(!failure.is_incomplete(), "{line:?} gave {failure:?}");
    }
}

/// A terminal hands over a line without its line ending, so a backslash at the very end shows
/// up as a word. It stands for a line continuation, and the next line decides what happens.
#[test]
fn a_lone_backslash_at_the_end_can_still_go_on() {
    for (line, finished) in [
        ("{ a; } \\", "{ a; } \\\n>f"),
        ("(a) \\", "(a) \\\n>f"),
        ("if a; then b; fi \\", "if a; then b; fi \\\n>f"),
        ("for \\", "for \\\ni in a; do b; done"),
        ("case \\", "case \\\nx in esac"),
        ("case x \\", "case x \\\nin esac"),
        ("a \\", "a \\\nb"),
        ("a | \\", "a | \\\nb"),
    ] {
        let failure = parse(line).unwrap_err();
        assert!(failure.is_incomplete(), "{line:?} gave {failure:?}");
        assert!(parse(finished).is_ok(), "{finished:?}");
    }
    // The next line cannot help with these, because the word is there already.
    for line in ["{ a; } b\\", "(a) b\\", "for 1\\"] {
        let failure = parse(line).unwrap_err();
        assert!(!failure.is_incomplete(), "{line:?} gave {failure:?}");
    }
}

#[test]
fn a_big_number_before_a_redirection_is_an_ordinary_word() {
    let word_count = |src: &str| {
        let list = parse(src).unwrap();
        let command = &list.items[0].and_or.first.commands[0];
        let CommandKind::Simple(simple) = &command.kind else {
            panic!("{list:?}");
        };
        (simple.words.len(), command.redirects[0].fd)
    };
    assert_eq!(word_count("a 2147483647>f"), (1, Some(2_147_483_647)));
    assert_eq!(word_count("a 2147483648>f"), (2, None));
    assert_eq!(word_count("a 99999999999999999999>f"), (2, None));
}

/// Runs `check` on a thread with the default test stack of 2 MiB, less than the shell's main
/// thread gets, so this also shows that the depth limit leaves enough room.
fn on_small_stack(check: fn()) {
    let thread = thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(check)
        .unwrap_or_else(|error| panic!("cannot start a thread: {error}"));
    if let Err(panic) = thread.join() {
        std::panic::resume_unwind(panic);
    }
}

fn is_too_deep(error: &ParseError) -> bool {
    matches!(
        error.kind,
        ParseErrorKind::NestingTooDeep | ParseErrorKind::Lex(LexErrorKind::NestingTooDeep)
    )
}

#[test]
fn deep_nesting_stops_with_an_error_and_keeps_the_stack() {
    on_small_stack(|| {
        let openers = [
            "(",
            "( ",
            "{ ",
            "if ",
            "if a; then ",
            "if a; then b; else ",
            "if a; then b; elif ",
            "while ",
            "while a; do ",
            "until ",
            "for i do ",
            "for i in a; do ",
            "case x in a) ",
            "case x in (a|b) c;; d) ",
            "f() ",
            "f() { ",
            "f() (",
            "! (",
            "a | (",
            "a && (",
            "$( (",
            "\"$( (",
        ];
        for opener in openers {
            let src = opener.repeat(100_000);
            let errors = parse_tolerant(&src).errors;
            assert!(errors.iter().any(is_too_deep), "{opener:?} gave {errors:?}");
            let errors = parse(&src).unwrap_err().errors;
            assert!(errors.iter().any(is_too_deep), "{opener:?} gave {errors:?}");
        }
    });
}

/// The lexer never looks inside backticks, so its limit starts over in each body. The parser's
/// limit is the one that has to hold across them. These are also the shapes that use the most
/// stack per level, because each level goes through word conversion and a nested parse.
#[test]
fn the_nesting_limit_applies_across_backticks() {
    on_small_stack(|| {
        for (opener, closer) in [
            ("$(", ")"),
            ("$(a ", ")"),
            ("$(a >", ")"),
            ("A=$(", ")"),
            ("$(cat <<E\n", "\nE\n)"),
            ("$(if a; then ", "; fi)"),
            ("$(case x in a) ", ";; esac)"),
        ] {
            let mut src = String::from("a");
            for _ in 0..3 {
                let body = src.replace('\\', "\\\\").replace('`', "\\`");
                src = format!("{}`{body}`{}", opener.repeat(60), closer.repeat(60));
            }
            assert!(
                !lex(&src)
                    .errors
                    .iter()
                    .any(|error| error.kind == LexErrorKind::NestingTooDeep)
            );
            let errors = parse(&src).unwrap_err().errors;
            assert!(errors.iter().any(is_too_deep), "{opener:?} gave {errors:?}");
            let errors = parse_tolerant(&src).errors;
            assert!(errors.iter().any(is_too_deep), "{opener:?} gave {errors:?}");
        }
    });
}

/// Everything after this point would only repeat the error.
#[test]
fn being_too_deep_ends_the_parse() {
    on_small_stack(|| {
        let deep = format!("{}a{}", "(".repeat(130), ")".repeat(130));
        let parsed = parse_tolerant(&format!("{deep}\n{deep}\n;;"));
        let kinds: Vec<_> = parsed.errors.iter().map(|error| error.kind).collect();
        assert_eq!(kinds, [ParseErrorKind::NestingTooDeep]);

        // The lexer's limit works the same way.
        let parsed = parse_tolerant(&"$(".repeat(100_000));
        let kinds: Vec<_> = parsed.errors.iter().map(|error| error.kind).collect();
        assert_eq!(kinds, [ParseErrorKind::Lex(LexErrorKind::NestingTooDeep)]);
        assert_eq!(parsed.list.items, []);
    });
}

/// Depth counts how far in the parser is, not how much it has seen, so siblings cost nothing.
#[test]
fn siblings_do_not_count_as_nesting() {
    for sibling in [
        "(a); ",
        "{ a; }; ",
        "if a; then b; fi; ",
        "while a; do b; done; ",
        "for i in a; do b; done; ",
        "case x in a) b;; esac; ",
        "f() { a; }; ",
        "a \"b\" ",
        "a ${x:-y} ",
        "a $((1)) ",
        "a $(b) ",
        "a `b` ",
    ] {
        let src = sibling.repeat(1_000);
        let parsed = parse(&src);
        assert!(parsed.is_ok(), "{sibling:?} gave {parsed:?}");
    }
}

#[test]
fn nesting_below_the_limit_is_fine() {
    on_small_stack(|| {
        let subs = format!("{}a{}", "$(".repeat(60), ")".repeat(60));
        assert!(parse(&subs).is_ok());
        let parens = format!("{}a{}", "(".repeat(120), ")".repeat(120));
        assert!(parse(&parens).is_ok());
        let groups = format!("{}a;{}", "{ ".repeat(120), " }".repeat(120));
        assert!(parse(&groups).is_ok());
        let mixed = format!("{}a{}", "$( (".repeat(60), ") )".repeat(60));
        assert!(parse(&mixed).is_ok());
    });
}

/// Long input with no nesting must not use stack per element.
#[test]
fn long_flat_input_uses_no_stack() {
    on_small_stack(|| {
        let cases = [
            "a; ".repeat(50_000),
            "a\n".repeat(50_000),
            format!("a{}", " | a".repeat(50_000)),
            format!("a{}", " && a".repeat(50_000)),
            format!("a{}", " a".repeat(50_000)),
            format!("a{}", " >f".repeat(50_000)),
            format!("{}a", "A=1 ".repeat(50_000)),
            format!("if a; then b; {}fi", "elif c; then d; ".repeat(50_000)),
            format!("case x in {}esac", "a) b;; ".repeat(50_000)),
            format!("for i in{}; do a; done", " b".repeat(50_000)),
        ];
        for src in &cases {
            let parsed = parse(src);
            assert!(parsed.is_ok(), "{:?}", src.get(..20));
        }
    });
}

/// Work must not explode on awkward input, because this can run on half-typed text. The time
/// limit is generous on purpose; it catches exponential behavior, not slow machines.
#[test]
fn awkward_input_stays_fast() {
    let cases = [
        "(".repeat(200_000),
        ")".repeat(200_000),
        "; ".repeat(100_000),
        "fi ".repeat(100_000),
        "fi\n".repeat(100_000),
        ") ".repeat(100_000),
        "a | ".repeat(100_000),
        "a ;; ".repeat(50_000),
        "<<A ".repeat(5_000),
        format!("{}\n", "cat <<A; ".repeat(2_000)),
        "$(a) ".repeat(20_000),
        "`a` ".repeat(20_000),
        "`a;;` ".repeat(20_000),
        "$(a;;) ".repeat(20_000),
        "\"$(a)\" ".repeat(20_000),
        format!("{}a{}", "$(".repeat(60), ")".repeat(60)).repeat(1_000),
        format!("{}a{}", "(".repeat(120), ")".repeat(120)).repeat(1_000),
        "if a; then ".repeat(50_000),
        "case x in a) ".repeat(50_000),
        "f() ".repeat(100_000),
    ];
    for src in &cases {
        let started = Instant::now();
        let _ = parse(src);
        let _ = parse_tolerant(src);
        let elapsed = started.elapsed();
        let head: String = src.chars().take(12).collect();
        assert!(
            elapsed < Duration::from_secs(5),
            "{head:?} took {elapsed:?}"
        );
    }
}
