//! Checks what the tree means, not just its shape.
//!
//! Each line is parsed, the tree is printed back as shell text in a plain canonical style,
//! and `/bin/sh` runs both versions. They must print the same and exit the same. So if the
//! parser put a redirection on the wrong command, gave a here-document the wrong body or
//! removed the wrong backslash, the printed version does something else and this test fails.
//!
//! Everything that `dash` does not have, such as `&>` and `<<<`, is left out, because
//! `/bin/sh` could not judge it.

use std::fs;
use std::path::PathBuf;
use std::process::{Command as Process, Output, Stdio};

use omsh_syntax::ast::{
    AndOr, AndOrOp, CaseCommand, Command, CommandKind, ForCommand, FunctionDef, IfCommand, List,
    ListItem, LoopCommand, ParamOp, Parameter, PartKind, Pipeline, Redirect, RedirectKind,
    RedirectOp, SimpleCommand, Word, WordPart,
};
use omsh_syntax::parse;

/// Every line must be safe to run: it only prints, and only writes in the current directory.
const LINES: &[&str] = &[
    "echo hello world",
    "echo a # comment\necho b",
    "# only a comment\necho c",
    "FOO=bar; echo $FOO",
    "FOO=bar sh -c 'echo $FOO'",
    "a=1 b=\"x y\"; echo \"$a $b\"",
    "A=1 B=$A; echo $A $B",
    "A=b=c; echo $A",
    "A=; echo \"[$A]\"",
    "A=1 >f B=2; echo \"$A $B\"; cat f",
    "F\\\nOO=1; echo $FOO",
    "FOO\\\n=1; echo $FOO",
    "FOO=\\\n1; echo $FOO",
    "A=1; echo A=2; echo $A",
    "echo a | cat | cat",
    "! true; echo $?",
    "! false | true; echo $?",
    "! false; echo $?",
    "true && ! false && echo yes",
    "true && echo a || echo b",
    "false && echo a || echo b",
    "false || true && echo c",
    "false || false || echo d; echo e",
    "true |\n\n cat && \n echo f",
    "echo a; echo b & wait; echo c",
    "(echo a; exit 3) & wait $!; echo $?",
    "echo a\necho b\n\necho c\n",
    "echo a >f; cat f",
    "echo a >f; echo b >>f; cat f",
    "echo a >f; cat <f",
    "echo a >f; cat 0<f",
    "echo a >|f; cat f",
    "echo a <>f; cat f",
    "echo a >f; exec 3<f; cat <&3; exec 3<&-",
    "exec 3>f; echo a >&3; exec 3>&-; cat f",
    "{ echo a; echo b >&2; } 2>&1 | cat",
    "{ echo a; echo b >&2; } >f 2>&1; cat f",
    "{ echo a; echo b >&2; } 2>&1 >f | cat; cat f",
    ">f echo a; cat f",
    "echo a >f b >g c; cat f g",
    "echo 2 >f; cat f",
    "echo a >f >g; cat f; cat g",
    ">f; cat f; echo $?",
    "if true; then >f; fi; cat f",
    "x=f; echo a >$x; cat $x",
    "cat <<EOF\nhello $NOT_SET world\n\\$x `echo b` $(echo c) \\\\ \\q \"q\" 'q'\nEOF\n",
    "cat <<'EOF'\nraw $x `y` \\$ \\\\\nEOF\n",
    "cat <<\"EOF\"\nraw $x\nEOF\n",
    "cat <<\\EOF\nraw $x\nEOF\n",
    "cat <<E\"O\"F\nraw $x\nEOF\n",
    "cat <<-EOF\n\tone\n\t\ttwo\n\tEOF\n",
    "cat <<-'EOF'\n\tone $x\n\tEOF\n",
    "cat <<A; cat <<B\nfirst\nA\nsecond\nB\n",
    "cat <<A <<B\nfirst\nA\nsecond\nB\n",
    "cat <<A | cat\nbody\nA\necho next\n",
    "cat 3<<A <&3\nbody\nA\n",
    "{ cat <<A; } | cat - <<B\nfirst\nA\nsecond\nB\n",
    "cat <<A && cat <<B\nfirst\nA\nsecond\nB\n",
    "if true; then cat <<EOF\nbody\nEOF\nfi",
    "cat <<EOF\nx\\\ny\nEOF\n",
    "x=$(cat <<EOF\ninner )\nEOF\n); echo \"$x\"",
    "cat <<EOF\n$(cat <<A\ninner\nA\n)\nEOF\n",
    "cat <<EOF\n`echo \\\"a\\\"`\nEOF\n",
    "cat <<A; echo $(cat <<B\ninner\nB\n)\nouter\nA\n",
    "f() { cat <<A; }\nbody\nA\nf",
    "i=0; while [ $i -lt 2 ]; do cat <<A; i=$((i+1)); done\nbody\nA\n",
    "cat <<E\\\nF\nbody\nEF\n",
    "x=$(cat <<EOF)\necho after",
    "echo `cat <<EOF`\necho after",
    "echo `cat <<EOF\nbody`",
    "echo `echo a\\\\`",
    "echo \"`echo a\\\\`\"",
    "(echo a; echo b) | cat",
    "(exit 3); echo $?",
    "x=1; (x=2); echo $x",
    "x=1; { x=2; }; echo $x",
    "{ echo a; } >f; cat f",
    "(echo a) >f; cat f",
    "((echo a))",
    "{ { echo a; } }",
    "{ (echo a) }",
    "{(echo a);}",
    "if true; then echo a; fi",
    "if false; then echo a; else echo b; fi",
    "if false; then echo a; elif true; then echo b; else echo c; fi",
    "if false; then echo a; elif false; then echo b; else echo c; fi >f; cat f",
    "if false; true; then echo a; false; true; fi; echo $?",
    "if true; then if false; then echo a; else echo b; fi fi",
    "if true; then echo a; fi | cat",
    "if ! false; then echo a; fi",
    "i=0; while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done",
    "i=0; until [ $i -ge 2 ]; do echo $i; i=$((i+1)); done",
    "i=0; while [ $i -lt 2 ]; do echo $i; i=$((i+1)); done >f; cat f",
    "while read x; do echo got $x; done <<EOF\na\nb\nEOF\n",
    "for i in a b 'c d'; do echo \"$i\"; done",
    "set -- x y; for i; do echo $i; done",
    "set -- x y; for i do echo $i; done",
    "set -- x y\nfor i\ndo echo $i; done",
    "for i in; do echo never; done; echo end",
    "for i in a\ndo echo $i; done",
    "for i\n\nin a\n\ndo echo $i; done",
    "for do in in; do echo $do; done",
    "for i in a; do echo $i; done >f; cat f",
    "case abc in a*) echo one;; *) echo two;; esac",
    "case x in a|x) echo one;; esac",
    "case x in (a|x) echo one; esac",
    "case x in y) echo one;; x) echo two;; esac",
    "case x in x) echo a; echo b;; esac",
    "case esac in (esac) echo good;; esac; echo after",
    "case esac in a|esac) echo good;; esac",
    "case x in a) echo bad;; (esac) echo bad;; x) echo good;; esac",
    "case x in x) ;; esac; echo $?",
    "case x in y) ;; esac; echo $?",
    "case x in esac; echo $?",
    "case x\nin\n x)\n  echo a\n  ;;\nesac",
    "case x in x) echo a;; esac >f; cat f",
    "case in in in) echo a;; esac",
    "x=a; case a in $x) echo var;; esac",
    "case 'a b' in \"a b\") echo quoted;; esac",
    "case x in a) case y in b) echo c;; esac;; x) case y in y) echo d;; esac;; esac",
    "case x in # note\n x) # note\n echo a;; # note\nesac # note",
    "f() { echo in f $1; }; f a; f b",
    "f() { echo a; } >f; f; cat f",
    "f() echo plain; f",
    "f() (echo sub); f",
    "f() if true; then echo a; fi; f",
    "f() g() { echo deep; }; f; g",
    "f () { return 3; }; f; echo $?",
    "f()\n\n{ echo a; }\nf",
    "f() { g() { echo inner; }; }; f; g",
    "f() x=1; f; echo $x",
    "echo() { printf 'mine\\n'; }; echo a",
    "echo 'single $x' \"double $NOT_SET\" \\$x",
    "echo \"a\\qb \\$ \\` \\\" \\\\\"",
    "echo a\\ b \\\\ \\'",
    "echo a'b'\"c\"d",
    "echo 'a\nb'",
    "echo \"a\nb\"",
    "echo a\\\nb",
    "echo \"a\\\nb\"",
    "echo 'a\\\nb'",
    "ech\\\no a",
    "\\echo a",
    "\"echo\" a",
    "echo if then fi done",
    "a=b; echo \"$a\" ${a} ${#a} ${a:-c} ${z:-c} ${z-c} ${a:+d} ${a+d} ${z:=e} $z",
    "a=abcabc; echo ${a#a*} ${a##a*b} ${a%b*} ${a%%b*}",
    "a=; echo \"[${a-x}]\" \"[${a:-x}]\" \"[${a+x}]\" \"[${a:+x}]\"",
    "echo \"${z:-\"a  b\"}\" ${z:-${y:-c}} ${z:-$(echo d)}",
    "set -- a b; echo $# $1 $2 ${2} \"$@\" \"$*\" $0x",
    "(exit 7); echo $?",
    "echo $(echo a $(echo b)) \"$(echo 'c  d')\"",
    "echo $(echo a; echo b)",
    "echo $(case x in x) echo a;; esac)",
    "echo $( (echo a) )",
    "echo $(echo a # )\n)",
    "echo $(if true; then echo a; fi)",
    "echo `echo a` \"`echo 'b  c'`\"",
    "echo `echo \\$NOT_SET x`",
    "echo `echo \\`echo nested\\``",
    "echo `echo \\`echo \\\\\\`echo deep\\\\\\`\\``",
    "echo `echo \\\"a\\\"` \"`echo \\\"a\\\"`\"",
    "echo ${z:-`echo \\\"a\\\"`} \"${z:-`echo \\\"a\\\"`}\"",
    "echo `echo a\\\nb`",
    "echo `echo 'a\\\nb'`",
    "echo `echo a\\\\b`",
    "echo `echo a\\\\\\$b`",
    "echo `echo \\q`",
    "echo `echo a\\$`",
    "echo $((`echo 1` + 2))",
    "echo $(( `echo \\\"1\\\"` ))",
    "echo $(echo `echo a`)",
    "echo `echo $(echo a)`",
    "x=`echo a`; echo $x",
    "echo $((1 + 2 * 3)) $(( (1 + 2) * 3 )) $((1 + $(echo 2)))",
    "x=3; echo $((x + 1)) $(($x + 1))",
    "echo *notfound* ?notfound",
    "echo ~notauser",
    "echo a~ \"~\"",
    ": >a.t; : >b.t; echo *.t",
    "echo $ a$ \"$\"",
    "a=\"x y\"; echo $a; echo \"$a\"",
    "IFS=:; a=x:y; echo $a",
];

/// Prints a tree as shell text. Every command gets a line of its own, because a here-document
/// body has to start right after a line ending.
#[derive(Default)]
struct Printer {
    out: String,
    /// Here-document bodies waiting for the next line ending.
    pending: Vec<String>,
}

impl Printer {
    fn newline(&mut self) {
        self.out.push('\n');
        for body in std::mem::take(&mut self.pending) {
            self.out.push_str(&body);
        }
    }

    fn list(&mut self, list: &List) {
        for ListItem {
            and_or, background, ..
        } in &list.items
        {
            self.and_or(and_or);
            if *background {
                self.out.push_str(" &");
            }
            self.newline();
        }
    }

    fn and_or(&mut self, and_or: &AndOr) {
        self.pipeline(&and_or.first);
        for part in &and_or.rest {
            self.out.push_str(match part.op {
                AndOrOp::And => " && ",
                AndOrOp::Or => " || ",
            });
            self.pipeline(&part.pipeline);
        }
    }

    fn pipeline(&mut self, pipeline: &Pipeline) {
        if pipeline.negated {
            self.out.push_str("! ");
        }
        for (index, command) in pipeline.commands.iter().enumerate() {
            if index > 0 {
                self.out.push_str(" | ");
            }
            self.command(command);
        }
    }

    fn command(&mut self, command: &Command) {
        match &command.kind {
            CommandKind::Simple(SimpleCommand { assignments, words }) => {
                for assignment in assignments {
                    self.out.push_str(&assignment.name);
                    self.out.push('=');
                    self.word(&assignment.value);
                    self.out.push(' ');
                }
                for word in words {
                    self.word(word);
                    self.out.push(' ');
                }
            }
            CommandKind::Subshell(list) => {
                self.out.push_str("( ");
                self.list(list);
                self.out.push_str(") ");
            }
            CommandKind::Group(list) => {
                self.out.push_str("{ ");
                self.list(list);
                self.out.push_str("} ");
            }
            CommandKind::If(IfCommand {
                branches,
                else_body,
            }) => {
                for (index, branch) in branches.iter().enumerate() {
                    self.out.push_str(if index == 0 { "if " } else { "elif " });
                    self.list(&branch.condition);
                    self.out.push_str("then ");
                    self.list(&branch.body);
                }
                if let Some(body) = else_body {
                    self.out.push_str("else ");
                    self.list(body);
                }
                self.out.push_str("fi ");
            }
            CommandKind::While(body) => self.looping("while ", body),
            CommandKind::Until(body) => self.looping("until ", body),
            CommandKind::For(ForCommand {
                name, words, body, ..
            }) => {
                self.out.push_str("for ");
                self.out.push_str(name);
                if let Some(words) = words {
                    self.out.push_str(" in");
                    for word in words {
                        self.out.push(' ');
                        self.word(word);
                    }
                }
                self.newline();
                self.out.push_str("do ");
                self.list(body);
                self.out.push_str("done ");
            }
            CommandKind::Case(CaseCommand { word, arms }) => {
                self.out.push_str("case ");
                self.word(word);
                self.out.push_str(" in");
                self.newline();
                for arm in arms {
                    self.out.push('(');
                    for (index, pattern) in arm.patterns.iter().enumerate() {
                        if index > 0 {
                            self.out.push('|');
                        }
                        self.word(pattern);
                    }
                    self.out.push(')');
                    self.newline();
                    self.list(&arm.body);
                    self.out.push_str(";;");
                    self.newline();
                }
                self.out.push_str("esac ");
            }
            CommandKind::Function(FunctionDef { name, body, .. }) => {
                self.out.push_str(name);
                self.out.push_str("() ");
                self.command(body);
            }
        }
        for redirect in &command.redirects {
            self.redirect(redirect);
            self.out.push(' ');
        }
    }

    fn looping(&mut self, keyword: &str, body: &LoopCommand) {
        self.out.push_str(keyword);
        self.list(&body.condition);
        self.out.push_str("do ");
        self.list(&body.body);
        self.out.push_str("done ");
    }

    fn redirect(&mut self, redirect: &Redirect) {
        if let Some(fd) = redirect.fd {
            self.out.push_str(&fd.to_string());
        }
        match &redirect.kind {
            RedirectKind::Target { op, word } => {
                self.out.push_str(match op {
                    RedirectOp::Input => "<",
                    RedirectOp::Output => ">",
                    RedirectOp::Append => ">>",
                    RedirectOp::ReadWrite => "<>",
                    RedirectOp::Clobber => ">|",
                    RedirectOp::DupInput => "<&",
                    RedirectOp::DupOutput => ">&",
                    RedirectOp::OutputAndError | RedirectOp::AppendAndError => {
                        panic!("dash does not have {op:?}")
                    }
                    RedirectOp::HereString => panic!("dash does not have {op:?}"),
                });
                self.word(word);
            }
            RedirectKind::HereDoc(doc) => {
                // The tabs of `<<-` are already gone from the body, so `<<` is enough.
                let delimiter = if doc.quoted {
                    format!("'{}'", doc.delimiter)
                } else {
                    doc.delimiter.clone()
                };
                self.out.push_str("<<");
                self.out.push_str(&delimiter);
                let mut body = Printer::default();
                body.parts(&doc.body);
                // Inside backticks, a body can end with the input and without a line ending.
                if !body.out.is_empty() && !body.out.ends_with('\n') {
                    body.out.push('\n');
                }
                self.pending
                    .push(format!("{}{}\n", body.out, doc.delimiter));
            }
        }
    }

    fn word(&mut self, word: &Word) {
        self.parts(&word.parts);
    }

    fn parts(&mut self, parts: &[WordPart]) {
        for part in parts {
            match &part.kind {
                PartKind::Literal(text) => self.out.push_str(text),
                PartKind::Escaped(c) => {
                    self.out.push('\\');
                    self.out.push(*c);
                }
                PartKind::SingleQuoted(text) => {
                    self.out.push('\'');
                    self.out.push_str(text);
                    self.out.push('\'');
                }
                PartKind::AnsiCQuoted(_) => panic!("dash does not have $'...'"),
                PartKind::DoubleQuoted(parts) => {
                    self.out.push('"');
                    self.parts(parts);
                    self.out.push('"');
                }
                PartKind::Parameter(parameter) => self.parameter(parameter),
                PartKind::CommandSub(list) => {
                    // Its own printer, because a here-document body waiting outside must not
                    // land inside the substitution.
                    let mut inner = Printer::default();
                    inner.list(list);
                    self.out.push_str("$( ");
                    self.out.push_str(&inner.out);
                    self.out.push(')');
                }
                PartKind::Arithmetic(parts) => {
                    self.out.push_str("$((");
                    self.parts(parts);
                    self.out.push_str("))");
                }
            }
        }
    }

    fn parameter(&mut self, parameter: &Parameter) {
        self.out.push_str("${");
        if parameter.length {
            self.out.push('#');
        }
        self.out.push_str(&parameter.name);
        if let Some((op, _)) = parameter.op {
            self.out.push_str(match op {
                ParamOp::ColonDash => ":-",
                ParamOp::ColonEquals => ":=",
                ParamOp::ColonQuestion => ":?",
                ParamOp::ColonPlus => ":+",
                ParamOp::Dash => "-",
                ParamOp::Equals => "=",
                ParamOp::Question => "?",
                ParamOp::Plus => "+",
                ParamOp::Hash => "#",
                ParamOp::DoubleHash => "##",
                ParamOp::Percent => "%",
                ParamOp::DoublePercent => "%%",
                ParamOp::Unsupported => panic!("not a supported form: {parameter:?}"),
            });
        }
        self.parts(&parameter.arg);
        self.out.push('}');
    }
}

fn print_tree(list: &List) -> String {
    let mut printer = Printer::default();
    printer.list(list);
    printer.out
}

/// Runs a script in `dir`, which it empties first. `timeout` keeps a wrong loop from hanging.
fn run(script: &str, dir: &PathBuf) -> Output {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).unwrap_or_else(|error| panic!("cannot create {dir:?}: {error}"));
    Process::new("timeout")
        .args(["10", "/bin/sh", "-c", script])
        .current_dir(dir)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("cannot run /bin/sh: {error}"))
}

/// The test's own folder, so that parallel runs do not meet.
fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join(format!("omsh-roundtrip-{}", std::process::id()))
}

fn assert_same_behavior(line: &str, printed: &str, dir: &PathBuf) {
    let original = run(line, dir);
    let again = run(printed, dir);
    let shown = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    assert_eq!(
        (original.status.code(), shown(&original.stdout)),
        (again.status.code(), shown(&again.stdout)),
        "\nline:    {line:?}\nprinted: {printed:?}\nstderr of printed: {}",
        shown(&again.stderr),
    );
}

#[test]
fn printed_trees_behave_like_the_source() {
    let dir = scratch_dir();
    for line in LINES {
        let list = parse(line).unwrap_or_else(|failure| panic!("{line:?} gave {failure:?}"));
        let printed = print_tree(&list);
        assert_same_behavior(line, &printed, &dir);

        // Printing is stable, so the printed text means the same tree to the parser too.
        let again = parse(&printed).unwrap_or_else(|failure| panic!("{printed:?}: {failure:?}"));
        assert_eq!(print_tree(&again), printed, "{line:?} is not stable");
    }
    let _ = fs::remove_dir_all(&dir);
}
