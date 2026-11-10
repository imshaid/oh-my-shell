//! Tests for the executor: whole lines go through a shell whose standard output and error are
//! in memory.
//!
//! These shells are always copies (`Shell::subshell`), so `cd` here never calls `chdir`: the
//! working directory belongs to the whole test process and other tests are running. `cd` in
//! the main shell is tested through the binary (`omsh-cli/tests/conformance.rs`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::io::capture::Captured;
use crate::shell::{LineResult, Outcome, Shell};
use crate::vars::Variables;

/// What a line did.
#[derive(Debug)]
struct Done {
    out: String,
    err: String,
    status: u8,
    exited: bool,
}

fn shell() -> Shell {
    Shell::with_variables(Variables::from_pairs([
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/tmp"),
    ]))
    .subshell()
}

fn run(sh: &mut Shell, src: &str) -> Done {
    let mut captured = Captured::new();
    let result = sh.run_source(src, true, &mut captured.io);
    let (status, exited) = match result {
        LineResult::Ran(Outcome::Continue(s)) => (s.code(), false),
        LineResult::Ran(Outcome::Exit(s)) => (s.code(), true),
        other => panic!("not run: {src}: {other:?}"),
    };
    Done {
        out: captured.stdout(),
        err: captured.stderr(),
        status,
        exited,
    }
}

/// Runs a line in a fresh shell and returns what it printed.
fn out(src: &str) -> String {
    run(&mut shell(), src).out
}

/// Runs a line in a fresh shell and returns its status.
fn status(src: &str) -> u8 {
    run(&mut shell(), src).status
}

/// A directory with a few files, removed at the end of the test.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("omsh-exec-{}-{n}", std::process::id()));
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        fs::write(dir.join("sub/inner.txt"), "inner\n").unwrap();
        Self(dir)
    }

    /// A shell that starts in this directory.
    fn shell(&self) -> Shell {
        let mut sh = shell();
        sh.cwd_override = Some(self.0.clone());
        sh.vars
            .set("PWD", self.0.as_os_str().as_encoded_bytes().to_vec())
            .unwrap();
        sh
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.0.join(name)).unwrap()
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------- lists

#[test]
fn a_list_runs_in_order_and_gives_the_last_status() {
    let d = run(&mut shell(), "echo a; echo b; false");
    assert_eq!(d.out, "a\nb\n");
    assert_eq!(d.status, 1);
}

#[test]
fn and_or_lists_run_left_to_right() {
    assert_eq!(out("true && echo yes"), "yes\n");
    assert_eq!(out("false && echo no; echo end"), "end\n");
    assert_eq!(out("false || echo or"), "or\n");
    assert_eq!(out("true || echo no; echo end"), "end\n");
    assert_eq!(out("false || false || echo third"), "third\n");
    assert_eq!(out("true && false || echo rescued"), "rescued\n");
    assert_eq!(status("false && true"), 1);
    assert_eq!(status("true || false"), 0);
}

#[test]
fn a_bang_negates_the_status() {
    assert_eq!(status("! true"), 1);
    assert_eq!(status("! false"), 0);
    assert_eq!(status("! sh -c 'exit 5'"), 0);
    assert_eq!(out("! false && echo ran"), "ran\n");
}

#[test]
fn a_bang_applies_to_the_whole_pipeline() {
    assert_eq!(status("! echo a | grep -q b"), 0);
    assert_eq!(status("! echo a | grep -q a"), 1);
}

// ---------------------------------------------------------------- redirections

#[test]
fn output_redirections_create_truncate_and_append() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "echo one > o.txt");
    assert_eq!(dir.read("o.txt"), "one\n");
    run(&mut sh, "echo two > o.txt");
    assert_eq!(dir.read("o.txt"), "two\n");
    run(&mut sh, "echo three >> o.txt");
    assert_eq!(dir.read("o.txt"), "two\nthree\n");
    run(&mut sh, "echo four >| o.txt");
    assert_eq!(dir.read("o.txt"), "four\n");
}

#[test]
fn an_input_redirection_feeds_the_command() {
    let dir = Dir::new();
    assert_eq!(run(&mut dir.shell(), "cat < a.txt").out, "alpha\n");
    assert_eq!(run(&mut dir.shell(), "cat 0< b.txt").out, "beta\n");
}

#[test]
fn a_read_write_redirection_opens_for_both() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "echo hello 1<>rw.txt");
    assert_eq!(dir.read("rw.txt"), "hello\n");
    assert_eq!(run(&mut sh, "cat <> a.txt").out, "alpha\n");
}

#[test]
fn a_redirection_can_name_any_descriptor() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(
        &mut sh,
        "sh -c 'echo out; echo err >&2; echo three >&3' 3>three.txt 2>err.txt",
    );
    assert_eq!(dir.read("three.txt"), "three\n");
    assert_eq!(dir.read("err.txt"), "err\n");
}

#[test]
fn duplicating_a_descriptor_copies_where_it_points_now() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    // Both go to the file.
    run(&mut sh, "sh -c 'echo o; echo e >&2' >both.txt 2>&1");
    assert_eq!(dir.read("both.txt"), "o\ne\n");
    // The errors go where the output went *before* it was redirected: here, the real stdout.
    let d = run(&mut sh, "sh -c 'echo o; echo e >&2' 2>&1 >only.txt");
    assert_eq!(dir.read("only.txt"), "o\n");
    assert_eq!(d.out, "e\n");
}

#[test]
fn the_shells_own_messages_follow_redirections_of_descriptor_two() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    let d = run(&mut sh, "no-such-command-omsh 2>msg.txt");
    assert_eq!(d.err, "");
    assert_eq!(
        dir.read("msg.txt"),
        "oh-my-shell: no-such-command-omsh: not found\n"
    );
    assert_eq!(d.status, 127);
}

#[test]
fn closing_a_descriptor() {
    let d = run(&mut shell(), "echo hi >&-");
    assert!(d.err.contains("write error"), "{}", d.err);
    assert_eq!(d.status, 1);
    // A program sees descriptor 1 closed.
    assert_ne!(status("sh -c 'echo hi' >&-"), 0);
    assert_eq!(status("cat <&- ; true"), 0);
}

#[test]
fn both_streams_to_a_file_with_ampersand_forms() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "sh -c 'echo o; echo e >&2' &> all.txt");
    assert_eq!(dir.read("all.txt"), "o\ne\n");
    run(&mut sh, "sh -c 'echo more; echo more-e >&2' &>> all.txt");
    assert_eq!(dir.read("all.txt"), "o\ne\nmore\nmore-e\n");
}

#[test]
fn a_here_string_gives_the_word_and_a_line_ending() {
    assert_eq!(out("cat <<< hello"), "hello\n");
    assert_eq!(out("x='a b'; cat <<< \"$x  c\""), "a b  c\n");
}

#[test]
fn a_here_document_expands_unless_its_delimiter_is_quoted() {
    assert_eq!(
        out("x=5; cat <<EOF\nx=$x $(echo sub) $((1+2)) \\$x\nEOF"),
        "x=5 sub 3 $x\n"
    );
    assert_eq!(
        out("x=5; cat <<'EOF'\nx=$x $(echo sub) \\n\nEOF"),
        "x=$x $(echo sub) \\n\n"
    );
    assert_eq!(out("x=5; cat <<\"EOF\"\n$x\nEOF"), "$x\n");
    assert_eq!(out("x=5; cat <<\\EOF\n$x\nEOF"), "$x\n");
}

#[test]
fn a_here_document_keeps_quotes_as_text() {
    assert_eq!(out("cat <<EOF\n\"q\" 'q'\nEOF"), "\"q\" 'q'\n");
}

#[test]
fn a_dash_in_the_operator_strips_leading_tabs() {
    assert_eq!(out("cat <<-EOF\n\tone\n\t\ttwo\n\tEOF"), "one\ntwo\n");
}

#[test]
fn a_big_here_document_does_not_block() {
    // More than a pipe holds; a here-document is a memory file, not a pipe.
    let body = "x".repeat(100);
    let lines = vec![body.as_str(); 1000].join("\n");
    let d = run(&mut shell(), &format!("wc -c <<EOF\n{lines}\nEOF"));
    assert_eq!(d.out.trim(), "101000");
}

#[test]
fn two_here_documents_on_one_command() {
    assert_eq!(
        out("cat 3<<A <&3; cat <<B\nfirst\nA\nsecond\nB"),
        "first\nsecond\n"
    );
}

#[test]
fn a_failed_redirection_stops_the_command_and_gives_2() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    let d = run(&mut sh, "echo ran > nodir/f; echo after");
    assert_eq!(d.out, "after\n");
    assert!(d.err.contains("nodir/f"), "{}", d.err);
    let d = run(&mut sh, "cat < missing.txt");
    assert_eq!(d.status, 2);
    assert!(d.err.contains("missing.txt"), "{}", d.err);
    // Not even the command's output appears.
    assert_eq!(d.out, "");
}

#[test]
fn a_bad_descriptor_is_reported() {
    let d = run(&mut shell(), "echo a >&9");
    assert_eq!(d.status, 2);
    assert!(d.err.contains("bad file descriptor"), "{}", d.err);
    let d = run(&mut shell(), "echo a >&x");
    assert_eq!(d.status, 2);
    assert!(d.err.contains("bad fd number"), "{}", d.err);
}

#[test]
fn a_failed_redirection_of_a_special_builtin_ends_the_line_but_not_the_shell() {
    let mut sh = shell();
    let d = run(&mut sh, ": < missing.txt; echo not-reached");
    assert_eq!(d.out, "");
    assert!(!d.exited);
    assert_eq!(d.status, 2);
}

#[test]
fn the_target_is_expanded_but_not_split_or_globbed() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "n='two words'; echo x > \"$n\"; echo y > $n");
    assert_eq!(dir.read("two words"), "y\n");
    run(&mut sh, "echo z > *.txt");
    assert_eq!(dir.read("*.txt"), "z\n");
    sh.vars
        .set("HOME", dir.path().as_os_str().as_encoded_bytes().to_vec())
        .unwrap();
    run(&mut sh, "echo t > ~/tilde.txt");
    assert_eq!(dir.read("tilde.txt"), "t\n");
}

#[test]
fn a_command_with_only_redirections_makes_the_files() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "> made.txt");
    assert_eq!(dir.read("made.txt"), "");
    let d = run(&mut sh, "x=1 > made2.txt; echo $x");
    assert_eq!(d.out, "1\n");
    assert!(dir.path().join("made2.txt").exists());
}

#[test]
fn redirections_on_compound_commands_apply_to_the_whole_body() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "{ echo a; echo b; } > g.txt");
    assert_eq!(dir.read("g.txt"), "a\nb\n");
    run(&mut sh, "for i in 1 2; do echo $i; done > g.txt");
    assert_eq!(dir.read("g.txt"), "1\n2\n");
    run(&mut sh, "if true; then echo yes; fi >> g.txt");
    assert_eq!(dir.read("g.txt"), "1\n2\nyes\n");
    run(&mut sh, "(echo sub) > g.txt");
    assert_eq!(dir.read("g.txt"), "sub\n");
}

#[test]
fn a_redirection_on_a_function_definition_applies_to_each_call() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "f() { echo body; } > g.txt");
    // Defining the function opens nothing.
    assert!(!dir.path().join("g.txt").exists());
    run(&mut sh, "f");
    assert_eq!(dir.read("g.txt"), "body\n");
}

#[test]
fn builtins_write_where_they_are_told() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "echo to-file > e.txt; pwd >> e.txt");
    assert_eq!(
        dir.read("e.txt"),
        format!("to-file\n{}\n", dir.path().display())
    );
}

// ---------------------------------------------------------------- pipelines

#[test]
fn a_pipeline_connects_the_commands() {
    assert_eq!(out("echo hello | tr a-z A-Z"), "HELLO\n");
    assert_eq!(out("echo a | cat | cat | tr a b"), "b\n");
    assert_eq!(out("printf 'b\\na\\n' | sort"), "a\nb\n");
}

#[test]
fn the_status_of_a_pipeline_is_the_last_commands() {
    assert_eq!(status("false | true"), 0);
    assert_eq!(status("true | false"), 1);
    assert_eq!(status("true | sh -c 'exit 7'"), 7);
}

#[test]
fn builtins_and_functions_can_be_stages() {
    assert_eq!(out("echo one | echo two"), "two\n");
    assert_eq!(out("f() { cat; }; echo piped | f"), "piped\n");
    assert_eq!(out("f() { echo made; }; f | tr a-z A-Z"), "MADE\n");
    assert_eq!(out("echo x | { cat; echo y; }"), "x\ny\n");
    assert_eq!(out("for i in 1 2 3; do echo $i; done | wc -l"), "3\n");
}

#[test]
fn a_lot_of_data_goes_through_a_pipeline() {
    let d = run(&mut shell(), "seq 1 200000 | cat | wc -l");
    assert_eq!(d.out.trim(), "200000");
    // A builtin in the middle that reads and writes the whole stream.
    let d = run(&mut shell(), "f() { cat; }; seq 1 200000 | f | f | tail -1");
    assert_eq!(d.out.trim(), "200000");
}

#[test]
fn a_builtin_that_writes_a_lot_into_a_pipe_is_read_at_the_same_time() {
    let d = run(
        &mut shell(),
        "f() { i=0; while [ $i -lt 3000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done; }; f | wc -l",
    );
    assert_eq!(d.out.trim(), "3000");
}

#[test]
fn a_loop_that_never_ends_stops_when_its_reader_does() {
    let d = run(&mut shell(), "while :; do echo y; done | head -3");
    assert_eq!(d.out, "y\ny\ny\n");
    let d = run(&mut shell(), "yes | head -2; echo $?");
    assert_eq!(d.out, "y\ny\n0\n");
}

#[test]
fn a_pipeline_stage_is_a_subshell() {
    let mut sh = shell();
    run(&mut sh, "x=1; echo | x=2; echo | { x=3; cd /; }");
    assert_eq!(sh.vars.get("x"), Some(&b"1"[..]));
    let d = run(&mut sh, "echo a | exit 3; echo $?");
    assert_eq!(d.out, "3\n");
    assert!(!d.exited);
}

#[test]
fn an_error_in_a_stage_does_not_end_the_line() {
    let d = run(&mut shell(), "echo ${nothing?boom} | cat; echo after");
    assert_eq!(d.out, "after\n");
    assert!(d.err.contains("boom"), "{}", d.err);
}

#[test]
fn the_end_of_a_pipe_reaches_the_reader_when_the_writer_is_done() {
    // `cat` would wait forever if a copy of the writing end stayed open somewhere.
    assert_eq!(out("echo done | cat"), "done\n");
    assert_eq!(out("{ echo a; } | { cat; } | { cat; }"), "a\n");
}

// ---------------------------------------------------------------- compound commands

#[test]
fn if_takes_the_first_branch_whose_condition_succeeds() {
    assert_eq!(out("if true; then echo t; else echo e; fi"), "t\n");
    assert_eq!(out("if false; then echo t; else echo e; fi"), "e\n");
    assert_eq!(
        out(
            "if false; then echo 1; elif false; then echo 2; elif true; then echo 3; else echo 4; fi"
        ),
        "3\n"
    );
    assert_eq!(
        out("if false; then echo 1; elif false; then echo 2; else echo 4; fi"),
        "4\n"
    );
}

#[test]
fn the_status_of_if_is_that_of_the_branch_or_zero() {
    assert_eq!(status("if false; then true; fi"), 0);
    assert_eq!(status("if true; then false; fi"), 1);
    assert_eq!(status("if false; then true; else sh -c 'exit 6'; fi"), 6);
}

#[test]
fn a_condition_can_be_a_list() {
    assert_eq!(
        out("if echo cond; false; then echo no; else echo else; fi"),
        "cond\nelse\n"
    );
}

#[test]
fn while_and_until_loop_on_the_condition() {
    assert_eq!(
        out("i=0; while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done"),
        "0\n1\n2\n"
    );
    assert_eq!(
        out("i=0; until [ $i -ge 2 ]; do echo $i; i=$((i+1)); done"),
        "0\n1\n"
    );
    assert_eq!(status("while false; do true; done"), 0);
    assert_eq!(out("i=0; until [ $i -ge 1 ]; do i=1; done; echo $i"), "1\n");
}

#[test]
fn for_loops_over_words_and_over_the_parameters() {
    assert_eq!(out("for i in a b c; do echo $i; done"), "a\nb\nc\n");
    assert_eq!(out("for i in; do echo $i; done; echo end"), "end\n");
    assert_eq!(
        out("set -- x 'y z'; for i; do echo \"[$i]\"; done"),
        "[x]\n[y z]\n"
    );
    assert_eq!(
        out("set -- x 'y z'; for i in \"$@\"; do echo \"[$i]\"; done"),
        "[x]\n[y z]\n"
    );
    assert_eq!(out("x='a b'; for i in $x; do echo $i; done"), "a\nb\n");
    let mut sh = shell();
    run(&mut sh, "for i in 1 2 3; do :; done");
    assert_eq!(sh.vars.get("i"), Some(&b"3"[..]));
}

#[test]
fn for_expands_globs() {
    let dir = Dir::new();
    assert_eq!(
        run(&mut dir.shell(), "for f in *.txt; do echo $f; done").out,
        "a.txt\nb.txt\n"
    );
}

#[test]
fn the_status_of_a_loop_is_that_of_its_last_pass() {
    assert_eq!(status("for i in 1 2; do sh -c \"exit $i\"; done"), 2);
    assert_eq!(status("for i in; do false; done"), 0);
}

#[test]
fn break_and_continue() {
    assert_eq!(
        out("for i in 1 2 3; do if [ $i = 2 ]; then break; fi; echo $i; done"),
        "1\n"
    );
    assert_eq!(
        out("for i in 1 2 3; do if [ $i = 2 ]; then continue; fi; echo $i; done"),
        "1\n3\n"
    );
    assert_eq!(
        out(
            "for i in 1 2; do for j in a b; do if [ $j = b ]; then break 2; fi; echo $i$j; done; done; echo end"
        ),
        "1a\nend\n"
    );
    assert_eq!(
        out(
            "for i in 1 2; do for j in a b; do if [ $j = b ]; then continue 2; fi; echo $i$j; done; done"
        ),
        "1a\n2a\n"
    );
    assert_eq!(
        out("i=0; while :; do i=$((i+1)); if [ $i -gt 2 ]; then break; fi; done; echo $i"),
        "3\n"
    );
}

#[test]
fn break_with_too_big_a_number_leaves_every_loop() {
    assert_eq!(
        out("for i in 1 2; do for j in 1 2; do break 9; done; echo no; done; echo end"),
        "end\n"
    );
}

#[test]
fn break_outside_a_loop_does_nothing() {
    let d = run(&mut shell(), "break; echo after");
    assert_eq!(d.out, "after\n");
    assert_eq!(d.status, 0);
}

#[test]
fn break_and_continue_with_a_bad_number() {
    let d = run(&mut shell(), "for i in 1; do break x; done");
    assert_eq!(d.status, 2);
    assert!(d.err.contains("Illegal number"), "{}", d.err);
    assert_eq!(run(&mut shell(), "for i in 1; do break 0; done").status, 2);
}

#[test]
fn case_picks_the_first_matching_pattern() {
    assert_eq!(
        out("case foo in f*) echo star;; *) echo other;; esac"),
        "star\n"
    );
    assert_eq!(
        out("case bar in f*) echo star;; *) echo other;; esac"),
        "other\n"
    );
    assert_eq!(out("case b in a|b|c) echo abc;; esac"), "abc\n");
    assert_eq!(
        out("case x in x) echo first;; x) echo second;; esac"),
        "first\n"
    );
    assert_eq!(out("case ab in a?) echo q;; esac"), "q\n");
    assert_eq!(out("case b in [a-c]) echo range;; esac"), "range\n");
    assert_eq!(out("case '*' in '*') echo literal;; esac"), "literal\n");
    assert_eq!(
        out("case abc in '*') echo literal;; *) echo glob;; esac"),
        "glob\n"
    );
}

#[test]
fn case_expands_the_word_and_the_patterns() {
    assert_eq!(out("x=hello; case $x in hel*) echo yes;; esac"), "yes\n");
    assert_eq!(out("p='h*'; case hello in $p) echo yes;; esac"), "yes\n");
    assert_eq!(
        out("p='h*'; case hello in \"$p\") echo yes;; *) echo no;; esac"),
        "no\n"
    );
    assert_eq!(out("case 'a b' in 'a b') echo space;; esac"), "space\n");
    assert_eq!(out("case $(echo x) in x) echo cmdsub;; esac"), "cmdsub\n");
}

#[test]
fn case_with_no_match_gives_zero() {
    let d = run(&mut shell(), "false; case x in y) echo y;; esac");
    assert_eq!(d.status, 0);
    assert_eq!(d.out, "");
}

#[test]
fn case_gives_the_status_of_the_arm() {
    assert_eq!(status("case x in x) sh -c 'exit 4';; esac"), 4);
}

#[test]
fn a_subshell_changes_nothing_outside() {
    let mut sh = shell();
    let d = run(&mut sh, "x=1; (x=2; cd /; exit 3); echo $x $?");
    assert_eq!(d.out, "1 3\n");
    assert!(!d.exited);
    run(&mut sh, "f() { :; }; (f() { echo changed; }; unset -f f)");
    assert!(sh.functions.contains_key("f"));
}

#[test]
fn a_group_changes_the_current_shell() {
    let mut sh = shell();
    run(&mut sh, "{ x=2; }");
    assert_eq!(sh.vars.get("x"), Some(&b"2"[..]));
}

// ---------------------------------------------------------------- functions

#[test]
fn a_function_gets_arguments_and_leaves_the_callers_untouched() {
    assert_eq!(out("f() { echo \"$1-$2-$#\"; }; f a b c"), "a-b-3\n");
    assert_eq!(
        out("set -- x y; f() { echo \"$1\"; }; f z; echo \"$1 $#\""),
        "z\nx 2\n"
    );
    assert_eq!(
        out("f() { for a; do echo \"[$a]\"; done; }; f 'a b' c"),
        "[a b]\n[c]\n"
    );
    assert_eq!(
        out("f() { echo \"$@\"; shift; echo \"$@\"; }; f 1 2 3"),
        "1 2 3\n2 3\n"
    );
    assert_eq!(out("f() { echo $0; }; f"), "oh-my-shell\n");
}

#[test]
fn return_ends_the_function_with_a_status() {
    assert_eq!(status("f() { return 4; }; f"), 4);
    assert_eq!(
        out("f() { echo in; return; echo no; }; f; echo after"),
        "in\nafter\n"
    );
    assert_eq!(status("f() { false; return; }; f"), 1);
    assert_eq!(status("f() { return 300; }; f"), 44);
    assert_eq!(
        out("f() { if true; then return 3; fi; echo no; }; f; echo $?"),
        "3\n"
    );
    assert_eq!(
        out("f() { for i in 1 2; do return 5; done; }; f; echo $?"),
        "5\n"
    );
}

#[test]
fn return_outside_a_function_is_an_error_and_not_the_end_of_the_shell() {
    let d = run(&mut shell(), "return 3; echo after");
    assert_eq!(d.out, "after\n");
    assert!(d.err.contains("return"), "{}", d.err);
    assert!(!d.exited);
}

#[test]
fn exit_inside_a_function_ends_the_shell() {
    let d = run(&mut shell(), "f() { exit 9; }; f; echo no");
    assert!(d.exited);
    assert_eq!(d.status, 9);
    assert_eq!(d.out, "");
}

#[test]
fn functions_share_the_variables_of_the_caller() {
    let mut sh = shell();
    run(&mut sh, "f() { x=set-in-f; }; f");
    assert_eq!(sh.vars.get("x"), Some(&b"set-in-f"[..]));
}

#[test]
fn functions_can_call_each_other_and_themselves() {
    assert_eq!(out("f() { g; }; g() { echo g; }; f"), "g\n");
    assert_eq!(
        out(
            "fact() { if [ $1 -le 1 ]; then echo 1; else echo $(( $1 * $(fact $(($1-1))) )); fi; }; fact 6"
        ),
        "720\n"
    );
    assert_eq!(out("f() { g() { echo inner; }; }; f; g"), "inner\n");
}

#[test]
fn a_function_defined_later_replaces_an_earlier_one() {
    assert_eq!(out("f() { echo one; }; f() { echo two; }; f"), "two\n");
}

#[test]
fn a_function_comes_before_a_program_and_after_a_special_builtin() {
    assert_eq!(out("cat() { echo mine; }; cat"), "mine\n");
    assert_eq!(out("echo() { printf 'fn\\n'; }; echo x"), "fn\n");
    // A special builtin comes before a function. The parser does not even allow the name.
    let mut captured = Captured::new();
    let result = shell().run_source("export() { echo fn; }", true, &mut captured.io);
    assert!(matches!(result, LineResult::Rejected(_)));
}

#[test]
fn unset_removes_a_function() {
    let d = run(&mut shell(), "f() { echo f; }; unset -f f; f");
    assert_eq!(d.status, 127);
}

/// Runs `body` on a thread with the stack of a program's main thread: 8 MiB. The threads of
/// tests have 2 MiB, which is not what the depth limit is made for.
fn on_main_thread_stack(body: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(body)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn endless_recursion_ends_with_an_error_and_not_with_a_crash() {
    // Each shape nests differently: through plain calls, `eval`, pipelines, substitutions,
    // subshells, groups, loops and `case`. Together with the 8 MiB stack this shows that the
    // limit (`MAX_DEPTH`) fits, also in a debug build, which uses the most stack per level.
    on_main_thread_stack(|| {
        for src in [
            "f() { f; }; f",
            "f() { f | cat; }; f",
            "f() { echo $(f); }; f",
            "f() { echo $(echo $(f)); }; f",
            "f() { (f); }; f",
            "f() f; f",
            "f() { eval f; }; f",
            "f() { { f; }; }; f",
            "f() { { if true; then while true; do f; done; fi; }; }; f",
            "f() { case x in x) for i in 1; do f; done;; esac; }; f",
        ] {
            let mut sh = shell();
            let mut captured = Captured::new();
            let result = sh.run_source(src, true, &mut captured.io);
            assert!(matches!(result, LineResult::Ran(_)), "{src}: {result:?}");
            assert!(
                captured.stderr().contains("too deeply nested"),
                "{src}: {}",
                captured.stderr()
            );
        }
    });
}

#[test]
fn a_loop_nested_less_deeply_than_the_limit_works() {
    // 100 nested levels of function calls is fine.
    let d = run(
        &mut shell(),
        "f() { if [ $1 -gt 0 ]; then f $(($1 - 1)); else echo bottom; fi; }; f 100",
    );
    assert_eq!(d.out, "bottom\n");
}

// ---------------------------------------------------------------- assignments in front of commands

#[test]
fn an_assignment_in_front_of_a_special_builtin_stays() {
    let mut sh = shell();
    run(&mut sh, "x=1 :");
    assert_eq!(sh.vars.get("x"), Some(&b"1"[..]));
    run(&mut sh, "y=2 eval 'z=3'");
    assert_eq!(sh.vars.get("y"), Some(&b"2"[..]));
}

#[test]
fn an_assignment_in_front_of_anything_else_lasts_for_the_command() {
    let mut sh = shell();
    run(&mut sh, "x=1 true");
    assert_eq!(sh.vars.get("x"), None);
    run(&mut sh, "x=old; x=new echo hi");
    assert_eq!(sh.vars.get("x"), Some(&b"old"[..]));
    assert_eq!(run(&mut sh, "x=1 sh -c 'echo $x'").out, "1\n");
}

#[test]
fn a_function_sees_the_assignment_in_front_of_it_and_its_programs_get_it_too() {
    assert_eq!(out("f() { echo $x; sh -c 'echo $x'; }; x=1 f"), "1\n1\n");
    assert_eq!(out("x=1; f() { echo $x; }; x=2 f; echo $x"), "2\n1\n");
}

#[test]
fn only_the_assigned_names_are_put_back() {
    let mut sh = shell();
    run(&mut sh, "f() { y=2; x=changed; }; x=1 f");
    assert_eq!(sh.vars.get("y"), Some(&b"2"[..]));
    // `x` was assigned in front of the call, so it goes back to unset, whatever `f` did.
    assert_eq!(sh.vars.get("x"), None);
}

#[test]
fn an_exported_variable_stays_exported_after_a_temporary_assignment() {
    let mut sh = shell();
    run(&mut sh, "export x=old; x=new true");
    assert_eq!(sh.vars.get("x"), Some(&b"old"[..]));
    assert!(sh.vars.is_exported("x"));
    assert_eq!(run(&mut sh, "sh -c 'echo $x'").out, "old\n");
}

// ---------------------------------------------------------------- builtins

#[test]
fn the_simple_builtins() {
    assert_eq!(status(":"), 0);
    assert_eq!(status("true"), 0);
    assert_eq!(status("false"), 1);
    assert_eq!(out(": this is ignored $(echo but-expanded >&2)"), "");
}

#[test]
fn echo_in_the_style_of_bash() {
    assert_eq!(out("echo a  b"), "a b\n");
    assert_eq!(out("echo"), "\n");
    assert_eq!(out("echo -n a; echo b"), "ab\n");
    assert_eq!(out("echo 'a\\nb'"), "a\\nb\n");
    assert_eq!(out("echo -e 'a\\nb\\tc'"), "a\nb\tc\n");
    assert_eq!(out("echo -E 'a\\nb'"), "a\\nb\n");
    assert_eq!(out("echo -ne 'a\\n'"), "a\n");
    assert_eq!(out("echo -en 'x\\ty'"), "x\ty");
    assert_eq!(out("echo -e 'a\\cb'"), "a");
    assert_eq!(out("echo -e '\\x41\\0102\\103'"), "ABC\n");
    assert_eq!(out("echo -e 'a\\\\b'"), "a\\b\n");
    assert_eq!(out("echo -e 'q\\qq'"), "q\\qq\n");
    assert_eq!(out("echo -e '\\e' | od -An -c | tr -d ' '"), "033\\n\n");
}

#[test]
fn echo_prints_what_is_not_an_option() {
    assert_eq!(out("echo -x"), "-x\n");
    assert_eq!(out("echo -nx"), "-nx\n");
    assert_eq!(out("echo -"), "-\n");
    assert_eq!(out("echo -n -n a"), "a");
    assert_eq!(out("echo a -n"), "a -n\n");
    assert_eq!(out("echo --"), "--\n");
}

#[test]
fn exit_ends_the_shell_with_its_argument_or_the_last_status() {
    let d = run(&mut shell(), "exit 3");
    assert!(d.exited);
    assert_eq!(d.status, 3);
    let d = run(&mut shell(), "false; exit");
    assert_eq!((d.exited, d.status), (true, 1));
    assert_eq!(run(&mut shell(), "exit 259").status, 3);
    let d = run(&mut shell(), "exit abc");
    assert_eq!((d.exited, d.status), (true, 2));
    assert!(d.err.contains("numeric argument required"));
    assert_eq!(run(&mut shell(), "exit 5; echo no").out, "");
}

#[test]
fn exit_in_a_subshell_leaves_the_shell_alone() {
    let d = run(&mut shell(), "(exit 4); echo $?");
    assert!(!d.exited);
    assert_eq!(d.out, "4\n");
    let d = run(&mut shell(), "x=$(exit 2); echo $?");
    assert_eq!(d.out, "2\n");
}

#[test]
fn export_marks_and_sets_variables() {
    let mut sh = shell();
    run(&mut sh, "export A=1 B; B=2; C=3");
    assert!(sh.vars.is_exported("A"));
    assert!(sh.vars.is_exported("B"));
    assert!(!sh.vars.is_exported("C"));
    let d = run(&mut sh, "sh -c 'echo $A $B ${C-unset}'");
    assert_eq!(d.out, "1 2 unset\n");
    run(&mut sh, "C=4; export C");
    assert_eq!(run(&mut sh, "sh -c 'echo $C'").out, "4\n");
}

#[test]
fn export_lists_the_exported_variables_in_the_form_that_reads_back() {
    let mut sh = shell();
    run(&mut sh, "export B=\"it's\" A='x y'; export NOVALUE");
    let listing = run(&mut sh, "export -p").out;
    assert!(listing.contains("export A='x y'\n"), "{listing}");
    assert!(listing.contains("export B='it'\"'\"'s'\n"), "{listing}");
    assert!(listing.contains("export NOVALUE\n"), "{listing}");
    let a = listing.find("export A=").unwrap();
    let b = listing.find("export B=").unwrap();
    assert!(a < b, "sorted by name");
    assert_eq!(run(&mut sh, "export").out, listing);
}

#[test]
fn export_rejects_a_bad_name_and_goes_on() {
    let mut sh = shell();
    let d = run(&mut sh, "export 1a=x good=y");
    assert_eq!(d.status, 1);
    assert!(d.err.contains("not a valid name"), "{}", d.err);
    assert_eq!(sh.vars.get("good"), Some(&b"y"[..]));
}

#[test]
fn unset_removes_variables() {
    let mut sh = shell();
    run(&mut sh, "a=1; export b=2; unset a b nothing");
    assert_eq!(sh.vars.get("a"), None);
    assert_eq!(sh.vars.get("b"), None);
    assert_eq!(run(&mut sh, "sh -c 'echo ${b-gone}'").out, "gone\n");
    let d = run(&mut sh, "unset 1x");
    assert_eq!(d.status, 1);
}

#[test]
fn set_replaces_the_positional_parameters() {
    assert_eq!(out("set -- a b c; echo $# $1 $3"), "3 a c\n");
    assert_eq!(out("set a b; echo $#"), "2\n");
    assert_eq!(out("set -- a; set --; echo $#"), "0\n");
    assert_eq!(out("set -- -x; echo $1"), "-x\n");
    assert_eq!(
        out("set -- 'a b' c; for i in \"$@\"; do echo \"[$i]\"; done"),
        "[a b]\n[c]\n"
    );
}

#[test]
fn set_without_arguments_lists_variables() {
    let listing = out("zz_var='a b'; set");
    assert!(listing.contains("zz_var='a b'\n"), "{listing}");
}

#[test]
fn set_errexit_ends_the_shell_at_a_failing_command() {
    let d = run(&mut shell(), "set -e; echo a; false; echo b");
    assert_eq!((d.out.as_str(), d.status, d.exited), ("a\n", 1, true));
    assert_eq!(out("set -e; set +e; false; echo after"), "after\n");
}

#[test]
fn errexit_does_not_apply_where_the_failure_is_expected() {
    for line in [
        "false || echo ok",
        "false && echo no; echo ok",
        "! true; echo ok",
        "if false; then :; fi; echo ok",
        "while false; do :; done; echo ok",
        "f() { false; echo in; }; f || echo no; echo ok",
        "f() { false; echo in; }; if f; then :; fi; echo ok",
        "false | true; echo ok",
    ] {
        let d = run(&mut shell(), &format!("set -e; {line}"));
        assert!(d.out.ends_with("ok\n"), "{line}: {d:?}");
        assert!(!d.exited || d.status == 0, "{line}: {d:?}");
    }
}

#[test]
fn errexit_applies_to_the_last_command_of_a_list_and_to_subshells() {
    for line in [
        "true && false; echo no",
        "(false); echo no",
        "true | false; echo no",
        "f() { return 3; }; f; echo no",
        "x=$(false); echo no",
        "{ false; echo no; }",
        "for i in 1; do false; echo no; done",
    ] {
        let d = run(&mut shell(), &format!("set -e; {line}"));
        assert!(d.exited, "{line}: {d:?}");
        assert!(!d.out.contains("no"), "{line}: {d:?}");
    }
}

#[test]
fn pipefail_takes_the_last_failure_of_the_pipeline() {
    assert_eq!(out("false | true; echo $?"), "0\n");
    assert_eq!(
        out("set -o pipefail; false | true; echo $?; true | true; echo $?"),
        "1\n0\n"
    );
    assert_eq!(
        out("set -o pipefail; sh -c 'exit 3' | false | true; echo $?"),
        "1\n"
    );
}

#[test]
fn nounset_makes_an_unset_parameter_an_error_that_ends_the_line() {
    let d = run(&mut shell(), "set -u; echo $nope; echo no");
    assert_eq!((d.out.as_str(), d.status, d.exited), ("", 2, false));
    assert!(d.err.contains("nope: parameter not set"), "{}", d.err);
    // Operators that say what to do with an unset parameter are fine, and so are the
    // special parameters.
    assert_eq!(
        out("set -u; echo [${a-x}${a:-y}${a+z}${a:+w}] $# $? $@ $*; : ${b=1}; echo $b"),
        "[xy] 0 0\n1\n"
    );
    assert!(
        run(&mut shell(), "set -u; echo ${#nope}")
            .err
            .contains("not set")
    );
    assert!(
        run(&mut shell(), "set -u; echo ${nope#a}")
            .err
            .contains("not set")
    );
    assert!(
        run(&mut shell(), "set -u; echo $1")
            .err
            .contains("1: parameter not set")
    );
    assert_eq!(out("set -u; x=; echo [$x]"), "[]\n");
}

#[test]
fn noglob_keeps_patterns_as_they_are() {
    let dir = Dir::new();
    let line = format!(
        "cd {}; set -f; echo *.txt; set +f; echo *.txt",
        dir.path().display()
    );
    assert_eq!(out(&line), "*.txt\na.txt b.txt\n");
}

#[test]
fn xtrace_writes_each_command_to_stderr() {
    let d = run(
        &mut shell(),
        "set -x; x=1; echo \"a b\" c; set +x; echo quiet",
    );
    assert_eq!(d.out, "a b c\nquiet\n");
    assert_eq!(d.err, "+ x=1\n+ echo a b c\n+ set +x\n");
    let d = run(&mut shell(), "PS4='# '; set -x; true");
    assert_eq!(d.err, "# true\n");
}

#[test]
fn noclobber_protects_regular_files_only() {
    let dir = Dir::new();
    let cd = format!("cd {}; set -C;", dir.path().display());
    let d = run(&mut shell(), &format!("{cd} echo x > a.txt; echo after"));
    assert_eq!(d.out, "after\n");
    assert_eq!(d.status, 0);
    assert!(
        d.err.contains("cannot overwrite existing file"),
        "{}",
        d.err
    );
    assert_eq!(out(&format!("{cd} echo x >| a.txt; cat a.txt")), "x\n");
    assert_eq!(
        out(&format!("{cd} echo y > fresh.txt; cat fresh.txt")),
        "y\n"
    );
    assert_eq!(out(&format!("{cd} echo x > /dev/null; echo ok")), "ok\n");
    assert_eq!(
        out(&format!("{cd} echo more >> a.txt; cat a.txt")),
        "x\nmore\n"
    );
}

#[test]
fn set_o_lists_and_sets_options_by_name() {
    let listing = out("set -o errexit -o nounset; set -o");
    assert!(
        listing.starts_with("Current option settings\nerrexit         on\n"),
        "{listing}"
    );
    assert!(listing.contains("nounset         on\n"));
    assert!(listing.contains("xtrace          off\n"));
    let restore = out("set -o xtrace; set +o");
    assert!(
        restore.contains("set -o xtrace\n") && restore.contains("set +o errexit\n"),
        "{restore}"
    );
    assert_eq!(out("set -eu; echo $-; set +e; echo $-"), "use\nus\n");
}

#[test]
fn set_with_options_and_arguments() {
    assert_eq!(
        out("set -e a b; echo $1 $2 $#; set -- ; echo $#"),
        "a b 2\n0\n"
    );
    assert_eq!(out("set a b; set -e; echo $#"), "2\n");
    assert_eq!(out("set a b; set -; echo $#"), "2\n");
    assert_eq!(out("set -- -x; echo $1"), "-x\n");
}

#[test]
fn set_refuses_what_this_shell_does_not_have() {
    for line in ["set -o nosuchname", "set -z", "set +q"] {
        let d = run(&mut shell(), line);
        assert_eq!(d.status, 2, "{line}");
        assert!(
            !d.exited,
            "{line}: an interactive shell survives a typing mistake"
        );
        assert!(!d.err.is_empty(), "{line}");
    }
}

#[test]
fn shift_drops_parameters() {
    assert_eq!(
        out("set -- a b c; shift; echo $@; shift 2; echo $# done"),
        "b c\n0 done\n"
    );
    let d = run(&mut shell(), "set -- a; shift 2; echo $#");
    assert_eq!(d.status, 0);
    assert!(d.err.contains("can't shift"), "{}", d.err);
    assert_eq!(run(&mut shell(), "shift x").status, 2);
}

#[test]
fn eval_runs_its_arguments_as_a_command() {
    assert_eq!(out("eval echo evaled"), "evaled\n");
    assert_eq!(out("x='echo from x'; eval $x"), "from x\n");
    assert_eq!(out("eval 'a=1; echo $a'; echo $a"), "1\n1\n");
    assert_eq!(out("eval 'echo a' 'echo b'"), "a echo b\n");
    assert_eq!(status("eval"), 0);
    assert_eq!(status("eval ''"), 0);
    assert_eq!(status("eval 'exit 5'; true"), 5);
    assert_eq!(out("eval 'echo a; echo b' | tr a-z A-Z"), "A\nB\n");
}

#[test]
fn eval_reports_a_syntax_error_and_gives_2() {
    let d = run(&mut shell(), "eval 'if'; echo after");
    assert_eq!(d.status, 0);
    assert_eq!(d.out, "after\n");
    assert!(d.err.contains("eval"), "{}", d.err);
    assert_eq!(run(&mut shell(), "eval 'fi'").status, 2);
}

#[test]
fn dot_runs_a_file_in_the_current_shell() {
    let dir = Dir::new();
    fs::write(
        dir.path().join("lib.sh"),
        "lib_var=9\nlib_fn() { echo lib; }\necho sourced\n",
    )
    .unwrap();
    let mut sh = dir.shell();
    let d = run(&mut sh, ". ./lib.sh; lib_fn; echo $lib_var");
    assert_eq!(d.out, "sourced\nlib\n9\n");
    let d = run(&mut sh, "source ./lib.sh");
    assert_eq!(d.out, "sourced\n");
}

#[test]
fn dot_with_arguments_and_return() {
    let dir = Dir::new();
    fs::write(
        dir.path().join("args.sh"),
        "echo \"$# $1\"\nreturn 6\necho never\n",
    )
    .unwrap();
    let mut sh = dir.shell();
    let d = run(&mut sh, "set -- outer; . ./args.sh a b; echo $? $1");
    assert_eq!(d.out, "2 a\n6 outer\n");
}

#[test]
fn dot_searches_the_path_for_a_name_without_a_slash() {
    let dir = Dir::new();
    fs::write(dir.path().join("onpath.sh"), "echo found-on-path\n").unwrap();
    let mut sh = dir.shell();
    let d = run(&mut sh, "PATH=/nonexistent:. . onpath.sh");
    assert_eq!(d.out, "found-on-path\n");
    let d = run(&mut sh, ". nosuchfile.sh");
    assert_eq!(d.status, 1);
    assert!(d.err.contains("not found"), "{}", d.err);
    assert_eq!(run(&mut sh, ".").status, 2);
}

#[test]
fn a_syntax_error_in_a_sourced_file_has_a_line_and_a_column() {
    let dir = Dir::new();
    fs::write(
        dir.path().join("bad.sh"),
        "echo one\n\nif true; then\n  echo x\nfi fi\n",
    )
    .unwrap();
    let mut sh = dir.shell();
    let d = run(&mut sh, ". ./bad.sh; echo $?");
    assert_eq!(d.out, "one\n2\n", "what ran before the mistake stays done");
    assert!(d.err.contains("./bad.sh:5:4: "), "{}", d.err);
    // `eval` has no file and says no place.
    let d = run(&mut sh, "eval 'echo a\nfi'");
    assert!(d.err.contains("oh-my-shell: eval: "), "{}", d.err);
}

// ------------------------------------------------------------------ the startup file

/// Runs a startup file and returns what it printed, and what the shell was told to do.
fn startup(sh: &mut Shell, path: &Path) -> (Done, Outcome) {
    let mut captured = Captured::new();
    let outcome = sh.run_startup_file(path, &mut captured.io);
    let done = Done {
        out: captured.stdout(),
        err: captured.stderr(),
        status: sh.last_status().code(),
        exited: matches!(outcome, Outcome::Exit(_)),
    };
    (done, outcome)
}

#[test]
fn the_startup_file_defines_what_the_next_lines_use() {
    let dir = Dir::new();
    let file = dir.path().join("init.omsh");
    fs::write(
        &file,
        "alias ll='echo long'\ngreet() { echo hello $1; }\nexport KEPT=yes\nPLAIN=1\n",
    )
    .unwrap();
    let mut sh = dir.shell();
    let (done, _) = startup(&mut sh, &file);
    assert_eq!((done.out.as_str(), done.err.as_str()), ("", ""));
    let d = run(&mut sh, "ll; greet world; echo $KEPT $PLAIN");
    assert_eq!(d.out, "long\nhello world\nyes 1\n");
    assert!(sh.vars.is_exported("KEPT"));
}

#[test]
fn a_missing_startup_file_is_silent_and_changes_nothing() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    let (done, outcome) = startup(&mut sh, &dir.path().join("init.omsh"));
    assert_eq!((done.out.as_str(), done.err.as_str()), ("", ""));
    assert!(matches!(outcome, Outcome::Continue(_)));
}

#[test]
fn the_status_after_the_startup_file_is_zero() {
    let dir = Dir::new();
    let file = dir.path().join("init.omsh");
    fs::write(&file, "false\n").unwrap();
    let mut sh = dir.shell();
    let (done, _) = startup(&mut sh, &file);
    assert_eq!(done.status, 0);
    assert_eq!(run(&mut sh, "echo $?").out, "0\n");
}

#[test]
fn return_ends_the_startup_file() {
    let dir = Dir::new();
    let file = dir.path().join("init.omsh");
    fs::write(&file, "before=1\nreturn\nafter=1\n").unwrap();
    let mut sh = dir.shell();
    let (done, outcome) = startup(&mut sh, &file);
    assert_eq!(done.err, "");
    assert!(matches!(outcome, Outcome::Continue(_)));
    assert_eq!(
        run(&mut sh, "echo ${before-unset} ${after-unset}").out,
        "1 unset\n"
    );
}

#[test]
fn exit_in_the_startup_file_ends_the_shell() {
    let dir = Dir::new();
    let file = dir.path().join("init.omsh");
    fs::write(&file, "echo hi\nexit 7\necho never\n").unwrap();
    let (done, outcome) = startup(&mut dir.shell(), &file);
    assert_eq!(done.out, "hi\n");
    assert!(matches!(outcome, Outcome::Exit(s) if s.code() == 7));
}

#[test]
fn a_mistake_in_the_startup_file_has_a_place_and_the_shell_goes_on() {
    let dir = Dir::new();
    let file = dir.path().join("init.omsh");
    fs::write(&file, "ok=1\nexport GOOD=yes\nif then\nnever=1\n").unwrap();
    let mut sh = dir.shell();
    let (done, outcome) = startup(&mut sh, &file);
    assert!(matches!(outcome, Outcome::Continue(_)));
    let wanted = format!("{}:3:", file.display());
    assert!(done.err.contains(&wanted), "{}", done.err);
    assert_eq!(
        run(&mut sh, "echo $ok $GOOD ${never-unset}").out,
        "1 yes unset\n",
        "what ran before the mistake stays, what comes after does not run"
    );
}

#[test]
fn a_startup_file_that_cannot_be_used_gives_one_line() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    let file = dir.path().join("init.omsh");
    fs::write(&file, b"echo \xff\n").unwrap();
    let (done, _) = startup(&mut sh, &file);
    assert!(done.err.contains("not valid UTF-8"), "{}", done.err);
    assert_eq!(done.err.lines().count(), 1);

    // A directory.
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    let (done, _) = startup(&mut sh, &file);
    assert_eq!(done.err.lines().count(), 1, "{}", done.err);
}

#[test]
fn a_builtin_that_cannot_write_says_so() {
    let d = run(&mut shell(), "echo hi > /dev/full");
    assert_eq!(d.status, 1);
    assert!(d.err.contains("write error"), "{}", d.err);
}

// ---------------------------------------------------------------- the working directory

#[test]
fn cd_in_a_copy_changes_that_copy_only() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    let sub = dir.path().join("sub");
    let d = run(&mut sh, "cd sub; pwd");
    assert_eq!(d.out, format!("{}\n", sub.display()));
    // The copy remembers it for the next line.
    assert_eq!(run(&mut sh, "pwd").out, format!("{}\n", sub.display()));
    assert_eq!(
        run(&mut sh, "cd ..; pwd").out,
        format!("{}\n", dir.path().display())
    );
}

#[test]
fn programs_started_after_cd_run_in_the_new_directory() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    assert_eq!(run(&mut sh, "cd sub; ls").out, "inner.txt\n");
    assert_eq!(
        run(&mut sh, "pwd -P").out,
        format!(
            "{}\n",
            fs::canonicalize(dir.path().join("sub")).unwrap().display()
        )
    );
    assert_eq!(
        run(&mut sh, "sh -c pwd").out.trim_end(),
        fs::canonicalize(dir.path().join("sub"))
            .unwrap()
            .display()
            .to_string()
    );
}

#[test]
fn a_subshell_and_a_substitution_do_not_move_the_shell() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    let d = run(&mut sh, "(cd sub; pwd); pwd");
    let lines: Vec<_> = d.out.lines().collect();
    assert_eq!(lines[0], dir.path().join("sub").display().to_string());
    assert_eq!(lines[1], dir.path().display().to_string());
    let d = run(&mut sh, "x=$(cd sub; pwd); echo $x; pwd");
    let lines: Vec<_> = d.out.lines().collect();
    assert_eq!(lines[0], dir.path().join("sub").display().to_string());
    assert_eq!(lines[1], dir.path().display().to_string());
    let d = run(&mut sh, "cd sub | cat; pwd");
    assert_eq!(d.out, format!("{}\n", dir.path().display()));
}

#[test]
fn a_copy_uses_its_own_directory_for_redirections_globs_and_commands() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "(cd sub; echo made > new.txt)");
    assert_eq!(dir.read("sub/new.txt"), "made\n");
    assert_eq!(
        run(&mut sh, "(cd sub; echo *.txt)").out,
        "inner.txt new.txt\n"
    );
    assert_eq!(run(&mut sh, "(cd sub; cat < inner.txt)").out, "inner\n");
    fs::write(dir.path().join("sub/tool"), "#!/bin/sh\necho tool-ran\n").unwrap();
    std::process::Command::new("chmod")
        .arg("+x")
        .arg(dir.path().join("sub/tool"))
        .status()
        .unwrap();
    assert_eq!(run(&mut sh, "(cd sub; ./tool)").out, "tool-ran\n");
    assert_eq!(
        run(&mut sh, "(cd sub; PATH=.:/usr/bin tool)").out,
        "tool-ran\n"
    );
    assert_eq!(run(&mut sh, "(cd sub; . ./tool)").out, "tool-ran\n");
}

#[test]
fn cd_updates_pwd_and_oldpwd() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "cd sub");
    assert_eq!(
        String::from_utf8_lossy(sh.vars.get("PWD").unwrap()),
        dir.path().join("sub").display().to_string()
    );
    assert_eq!(
        String::from_utf8_lossy(sh.vars.get("OLDPWD").unwrap()),
        dir.path().display().to_string()
    );
    let d = run(&mut sh, "cd -");
    assert_eq!(d.out, format!("{}\n", dir.path().display()));
}

#[test]
fn cd_with_no_argument_goes_home() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    sh.vars
        .set(
            "HOME",
            dir.path()
                .join("sub")
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
        )
        .unwrap();
    run(&mut sh, "cd");
    assert_eq!(
        run(&mut sh, "pwd").out,
        format!("{}\n", dir.path().join("sub").display())
    );
    sh.vars.unset("HOME").unwrap();
    let d = run(&mut sh, "cd");
    assert_eq!(d.status, 1);
    assert!(d.err.contains("HOME not set"), "{}", d.err);
}

#[test]
fn cd_removes_dots_from_the_path_as_text() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    run(&mut sh, "cd ./sub/../sub/.");
    assert_eq!(
        run(&mut sh, "pwd").out,
        format!("{}\n", dir.path().join("sub").display())
    );
    run(&mut sh, "cd /");
    run(&mut sh, "cd ..");
    assert_eq!(run(&mut sh, "pwd").out, "/\n");
}

#[test]
fn cd_follows_symbolic_links_as_written_unless_told_otherwise() {
    let dir = Dir::new();
    std::os::unix::fs::symlink(dir.path().join("sub"), dir.path().join("link")).unwrap();
    let mut sh = dir.shell();
    run(&mut sh, "cd link");
    assert_eq!(
        run(&mut sh, "pwd").out,
        format!("{}\n", dir.path().join("link").display())
    );
    assert_eq!(
        run(&mut sh, "pwd -P").out,
        format!(
            "{}\n",
            fs::canonicalize(dir.path().join("sub")).unwrap().display()
        )
    );
    let mut sh = dir.shell();
    run(&mut sh, "cd -P link");
    assert_eq!(
        run(&mut sh, "pwd").out,
        format!(
            "{}\n",
            fs::canonicalize(dir.path().join("sub")).unwrap().display()
        )
    );
}

#[test]
fn cd_to_something_that_is_not_a_directory_fails() {
    let dir = Dir::new();
    let mut sh = dir.shell();
    let d = run(&mut sh, "cd nothing");
    assert_eq!(d.status, 1);
    assert!(
        d.err.contains("nothing: No such file or directory"),
        "{}",
        d.err
    );
    let d = run(&mut sh, "cd a.txt");
    assert_eq!(d.status, 1);
    assert!(d.err.contains("Not a directory"), "{}", d.err);
    let d = run(&mut sh, "cd a b");
    assert_eq!(d.status, 1);
    assert!(d.err.contains("too many arguments"), "{}", d.err);
    // The directory did not change.
    assert_eq!(
        run(&mut sh, "pwd").out,
        format!("{}\n", dir.path().display())
    );
}

// ---------------------------------------------------------------- command substitution

#[test]
fn a_substitution_captures_what_builtins_print_too() {
    assert_eq!(out("x=$(echo builtin); echo \"[$x]\""), "[builtin]\n");
    assert_eq!(out("x=$(echo a; echo b); echo \"$x\""), "a\nb\n");
    assert_eq!(out("x=$(echo a | tr a b); echo $x"), "b\n");
    assert_eq!(out("x=$(for i in 1 2; do echo $i; done); echo $x"), "1 2\n");
    assert_eq!(out("f() { echo from-f; }; x=$(f); echo $x"), "from-f\n");
}

#[test]
fn a_substitution_can_hold_a_lot() {
    let d = run(&mut shell(), "x=$(seq 1 100000); echo ${#x}");
    assert_eq!(d.out, "588894\n");
    let d = run(
        &mut shell(),
        "x=$(i=0; while [ $i -lt 2000 ]; do echo 0123456789012345678901234567890123456789; i=$((i+1)); done); echo ${#x}",
    );
    assert_eq!(d.out, "81999\n");
}

#[test]
fn a_substitution_keeps_the_errors_on_descriptor_two() {
    let d = run(&mut shell(), "x=$(echo out; echo err >&2); echo \"[$x]\"");
    assert_eq!(d.out, "[out]\n");
    assert_eq!(d.err, "err\n");
}

#[test]
fn a_substitution_reads_the_standard_input_of_the_command() {
    assert_eq!(out("x=$(cat); echo \"[$x]\" <<EOF\nignored\nEOF"), "[]\n");
    assert_eq!(out("cat <<EOF\n$(echo inside)\nEOF"), "inside\n");
}

/// Process groups: with job control on, the programs of a pipeline form a process group of
/// their own. There is no terminal in a test, so only the groups are visible, which is what
/// matters for Ctrl+C: the terminal sends it to a whole group.
mod job_groups {
    use super::*;

    /// Prints the process group of the program that runs it.
    const GROUP: &str = "cut -d' ' -f5 /proc/self/stat";

    fn jc_shell() -> Shell {
        let mut sh = shell();
        sh.options.monitor = true;
        sh
    }

    fn mine() -> String {
        nix::unistd::getpgrp().to_string()
    }

    fn groups(sh: &mut Shell, src: &str) -> Vec<String> {
        let done = run(sh, src);
        assert_eq!(done.status, 0, "{src}: {}", done.err);
        done.out.lines().map(str::to_owned).collect()
    }

    #[test]
    fn a_pipeline_is_one_group_that_is_not_the_shells() {
        let found = groups(&mut jc_shell(), &format!("{GROUP} | {{ cat; {GROUP}; }}"));
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], found[1]);
        assert_ne!(found[0], mine());
    }

    #[test]
    fn a_group_outlives_its_leader() {
        // The first program is over long before the second one starts. The group must still
        // be there to join, and not be taken over by another process.
        let found = groups(
            &mut jc_shell(),
            &format!("{GROUP} | {{ sleep 0.3; cat; {GROUP}; }}"),
        );
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], found[1]);
        assert_ne!(found[0], mine());
    }

    #[test]
    fn each_command_is_a_job_of_its_own() {
        let found = groups(&mut jc_shell(), &format!("{GROUP}; {GROUP}"));
        assert_eq!(found.len(), 2, "{found:?}");
        assert_ne!(found[0], found[1]);
        assert!(found.iter().all(|g| *g != mine()), "{found:?}");
    }

    #[test]
    fn the_programs_of_a_loop_body_are_separate_jobs() {
        let found = groups(&mut jc_shell(), &format!("for i in 1 2; do {GROUP}; done"));
        assert_eq!(found.len(), 2, "{found:?}");
        assert_ne!(found[0], found[1]);
    }

    #[test]
    fn without_job_control_everything_stays_in_the_shells_group() {
        let found = groups(&mut shell(), &format!("{GROUP}; {GROUP} | cat"));
        assert_eq!(found, vec![mine(), mine()]);
    }

    #[test]
    fn a_substitution_stays_in_the_shells_group() {
        let found = groups(&mut jc_shell(), &format!("echo $({GROUP})"));
        assert_eq!(found, vec![mine()]);
    }

    #[test]
    fn a_substitution_inside_a_stage_joins_the_stage_job() {
        let found = groups(
            &mut jc_shell(),
            &format!("{GROUP} | {{ cat; echo $({GROUP}); }}"),
        );
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0], found[1]);
        assert_ne!(found[0], mine());
    }

    #[test]
    fn a_program_that_cannot_start_does_not_break_the_job() {
        let done = run(&mut jc_shell(), &format!("/nonexistent/omsh | {GROUP}"));
        assert_eq!(done.status, 0);
        assert!(!done.out.trim().is_empty(), "{}", done.err);
        assert_ne!(done.out.trim(), mine());
    }
}

/// Background jobs (`cmd &`), `jobs`, `wait` and `kill`.
///
/// Every test here starts programs that outlive the line, or waits for them. A mistake must not
/// hang the test run or leave processes behind (there is no PTY suite with timeouts until S10),
/// so each test goes through [`guarded`]: the body runs on its own thread with a time limit, and
/// whatever happens (success, a failed assertion, a timeout) every program of every job is
/// killed and collected afterwards, and the test fails if one is left.
mod background {
    use std::any::Any;
    use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use nix::sys::signal::Signal;
    use nix::unistd::Pid;

    use super::*;
    use crate::jobs::Phase;
    use crate::jobtable::Jobs;
    use crate::status::ExitStatus;

    /// How long a test body may take. The slowest ones sleep for about a second.
    const LIMIT: Duration = Duration::from_secs(20);

    fn alive(pid: Pid) -> bool {
        Path::new(&format!("/proc/{pid}")).exists()
    }

    /// Kills every program that the jobs started, waits for the job threads to collect them, and
    /// panics if one is still there.
    fn clean_up(jobs: &Jobs) {
        let mut pids = Vec::new();
        for view in jobs.list() {
            pids.extend(view.job.leader());
            pids.extend(view.job.live_pids());
            // `SIGKILL` works on a stopped process; `SIGCONT` is for the shell's own sake.
            let _ = view.job.signal(Signal::SIGKILL);
            let _ = view.job.signal(Signal::SIGCONT);
        }
        // The thread of each job collects its programs and marks the job finished.
        let end = Instant::now() + Duration::from_secs(5);
        while jobs
            .list()
            .iter()
            .any(|v| !matches!(v.phase, Phase::Finished(_)))
            && Instant::now() < end
        {
            thread::sleep(Duration::from_millis(10));
        }
        for view in jobs.list() {
            jobs.remove(view.number);
        }
        // A leader stays a zombie until its job is dropped, which happens once the job's thread
        // lets go of it.
        let end = Instant::now() + Duration::from_secs(5);
        while pids.iter().any(|pid| alive(*pid)) && Instant::now() < end {
            thread::sleep(Duration::from_millis(10));
        }
        let left: Vec<_> = pids.into_iter().filter(|pid| alive(*pid)).collect();
        assert!(left.is_empty(), "programs were left behind: {left:?}");
    }

    /// Runs `body` on a thread with a time limit and cleans up after it.
    fn guarded(job_control: bool, body: impl FnOnce(&mut Shell) + Send + 'static) {
        let mut sh = shell();
        sh.options.monitor = job_control;
        let jobs = Arc::clone(&sh.jobs);
        let (done, result) = mpsc::channel::<Result<(), Box<dyn Any + Send>>>();
        let thread = thread::spawn(move || {
            let outcome = catch_unwind(AssertUnwindSafe(|| body(&mut sh)));
            let _ = done.send(outcome);
        });
        let outcome = result.recv_timeout(LIMIT);
        clean_up(&jobs);
        match outcome {
            Ok(Ok(())) => thread.join().expect("the test thread ended"),
            Ok(Err(panic)) => resume_unwind(panic),
            Err(_) => panic!("the test did not finish in {LIMIT:?}"),
        }
    }

    /// Calls `check` until it is true, for at most five seconds.
    fn eventually(what: &str, mut check: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !check() {
            assert!(Instant::now() < end, "{what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// The process group of the program that runs `$!`.
    const GROUP_OF_LAST: &str = "cut -d' ' -f5 /proc/$!/stat";

    #[test]
    fn the_shell_does_not_wait_for_a_job_that_ends_with_an_ampersand() {
        guarded(false, |sh| {
            let start = Instant::now();
            let done = run(sh, "sleep 30 & echo started");
            assert_eq!(done.out, "started\n");
            assert_eq!(done.status, 0);
            assert!(start.elapsed() < Duration::from_secs(10));
        });
    }

    #[test]
    fn wait_gives_the_status_of_the_job() {
        guarded(false, |sh| {
            assert_eq!(run(sh, "sh -c 'exit 3' & wait $!; echo $?").out, "3\n");
            // Shell code only: nothing was started, so `$!` is empty and the job is named.
            assert_eq!(run(sh, "{ exit 4; } & wait %1; echo $?").out, "4\n");
            assert_eq!(run(sh, "{ true; } & wait %1; echo $?").out, "0\n");
        });
    }

    #[test]
    fn dollar_bang_is_the_first_program_of_the_last_job() {
        guarded(false, |sh| {
            assert_eq!(run(sh, "sleep 30 & cat /proc/$!/comm").out, "sleep\n");
            // No program started: nothing to show, and no invented number.
            assert_eq!(run(sh, "{ x=1; } & echo \"[$!]\"").out, "[]\n");
        });
    }

    #[test]
    fn jobs_lists_the_running_jobs_with_marks() {
        guarded(false, |sh| {
            let done = run(sh, "sleep 30 & sleep 31 & jobs");
            assert_eq!(
                done.out,
                format!(
                    "[1]-  {:<27}sleep 30 &\n[2]+  {:<27}sleep 31 &\n",
                    "Running", "Running"
                )
            );
            let done = run(sh, "jobs -p");
            assert_eq!(done.out.lines().count(), 2);
            assert!(
                done.out.lines().all(|l| l.parse::<u32>().is_ok()),
                "{}",
                done.out
            );
            assert!(run(sh, "jobs -l").out.contains("Running"));
            assert_eq!(run(sh, "jobs %2").out.lines().count(), 1);
        });
    }

    #[test]
    fn the_command_text_is_what_was_typed_without_the_ampersand() {
        guarded(false, |sh| {
            let done = run(sh, "echo  hi  &&  sleep 30   &\njobs");
            assert!(
                done.out.contains("echo  hi  &&  sleep 30 &"),
                "{}",
                done.out
            );
            // Inside a function the spans belong to another line: say nothing wrong.
            let done = run(sh, "f() { sleep 31 & }; f; jobs %2");
            assert!(done.out.contains("(background job)"), "{}", done.out);
        });
    }

    #[test]
    fn kill_ends_a_job_and_wait_reports_the_signal() {
        guarded(true, |sh| {
            assert_eq!(run(sh, "sleep 30 & kill %1; wait %1; echo $?").out, "143\n");
        });
        guarded(false, |sh| {
            assert_eq!(
                run(sh, "sleep 30 & kill -9 %1; wait %1; echo $?").out,
                "137\n"
            );
            assert_eq!(
                run(sh, "sleep 30 & kill -s KILL %1; wait %1; echo $?").out,
                "137\n"
            );
        });
    }

    #[test]
    fn kill_on_a_stopped_job_continues_it_so_that_it_can_end() {
        for job_control in [true, false] {
            guarded(job_control, |sh| {
                run(sh, "sleep 30 & kill -STOP %1");
                eventually("the job was not noted as stopped", || {
                    run(sh, "jobs").out.contains("Stopped")
                });
                // `wait` does not hang on a stopped job. It says so, as bash does.
                let done = run(sh, "wait %1; echo $?");
                assert_eq!(done.out, "147\n");
                assert!(done.err.contains("job 1 is stopped"), "{}", done.err);
                // TERM alone would stay pending on a stopped process: the shell sends CONT.
                let done = run(sh, "kill %1; wait %1; echo $?");
                assert_eq!(done.out, "143\n", "{}", done.err);
            });
        }
    }

    #[test]
    fn a_continued_job_is_running_again() {
        guarded(true, |sh| {
            run(sh, "sleep 30 & kill -STOP %1");
            eventually("the job was not noted as stopped", || {
                run(sh, "jobs").out.contains("Stopped")
            });
            run(sh, "kill -CONT %1");
            assert!(run(sh, "jobs").out.contains("Running"));
        });
    }

    #[test]
    fn shell_code_without_a_program_is_stopped_by_kill() {
        guarded(false, |sh| {
            let done = run(sh, "while :; do :; done & kill %1; wait %1; echo $?");
            assert_eq!(done.out, "143\n", "{}", done.err);
        });
    }

    #[test]
    fn compound_commands_lists_functions_and_pipelines_can_run_in_the_background() {
        guarded(false, |sh| {
            assert_eq!(run(sh, "{ echo a; echo b; } & wait").out, "a\nb\n");
            assert_eq!(run(sh, "true && echo and-list & wait").out, "and-list\n");
            assert_eq!(run(sh, "f() { echo in-f; }; f & wait").out, "in-f\n");
            assert_eq!(run(sh, "echo hi | cat & wait").out, "hi\n");
            assert_eq!(
                run(sh, "for i in 1 2; do echo $i; done & wait").out,
                "1\n2\n"
            );
            // Changes made in the job stay in the job: it is a subshell.
            assert_eq!(run(sh, "x=1; { x=2; } & wait; echo $x").out, "1\n");
        });
    }

    #[test]
    fn plain_wait_waits_for_every_job() {
        guarded(false, |sh| {
            let start = Instant::now();
            let done = run(sh, "sleep 0.3 & sleep 0.3 & wait; echo $?");
            assert_eq!(done.out, "0\n");
            assert!(start.elapsed() >= Duration::from_millis(250));
            // Nothing left to tell about: `wait` has seen them end.
            assert_eq!(run(sh, "jobs").out, "");
        });
    }

    #[test]
    fn exit_in_a_background_job_ends_that_job_and_not_the_shell() {
        guarded(false, |sh| {
            let done = run(sh, "{ exit 4; } & wait %1; echo $?");
            assert!(!done.exited);
            assert_eq!(done.out, "4\n");
        });
    }

    #[test]
    fn a_job_that_ends_is_reported_once_before_the_next_prompt() {
        guarded(false, |sh| {
            run(sh, "sh -c 'exit 3' &");
            let told = Captured::new();
            eventually("no notice", || {
                sh.job_notices(&told.io);
                !told.stderr().is_empty()
            });
            assert_eq!(
                told.stderr(),
                format!("[1]+  {:<27}sh -c 'exit 3'\n", "Exit 3")
            );
            sh.job_notices(&told.io);
            assert_eq!(told.stderr().lines().count(), 1);
            assert_eq!(run(sh, "jobs").out, "");
        });
    }

    #[test]
    fn a_killed_job_is_reported_with_the_signal() {
        guarded(false, |sh| {
            run(sh, "sleep 30 & kill %1");
            let told = Captured::new();
            eventually("no notice", || {
                sh.job_notices(&told.io);
                !told.stderr().is_empty()
            });
            assert_eq!(
                told.stderr(),
                format!("[1]+  {:<27}sleep 30\n", "Terminated")
            );
        });
    }

    #[test]
    fn a_background_job_has_a_process_group_of_its_own_when_there_is_job_control() {
        guarded(true, |sh| {
            let done = run(sh, &format!("sleep 30 & echo $!; {GROUP_OF_LAST}"));
            let lines: Vec<_> = done.out.lines().collect();
            assert_eq!(lines.len(), 2, "{}", done.err);
            // It leads its own group, which is not the shell's.
            assert_eq!(lines[0], lines[1]);
            assert_ne!(lines[1], nix::unistd::getpgrp().to_string());
        });
        guarded(false, |sh| {
            let done = run(sh, &format!("sleep 30 & {GROUP_OF_LAST}"));
            assert_eq!(done.out.trim(), nix::unistd::getpgrp().to_string());
        });
    }

    #[test]
    fn without_job_control_a_background_job_cannot_read_the_keyboard_or_be_interrupted() {
        guarded(false, |sh| {
            // Standard input is a pipe that never gets any data and is never closed.
            let (reader, _writer) = nix::unistd::pipe().unwrap();
            let mut captured = Captured::new();
            captured.io.set(0, Arc::new(reader));
            let result = sh.run_source("cat & wait %1; echo $?", true, &mut captured.io);
            assert!(matches!(result, LineResult::Ran(_)));
            assert_eq!(captured.stdout(), "0\n");

            // Ctrl+C and Ctrl+\ are ignored by the program, and so is a signal sent to it.
            let done = run(
                sh,
                "sh -c 'kill -INT $$; kill -QUIT $$; echo alive' & wait %1",
            );
            assert_eq!(done.out, "alive\n");
        });
    }

    #[test]
    fn eval_and_dot_can_start_background_jobs_and_show_their_text() {
        guarded(false, |sh| {
            let done = run(sh, "eval 'sleep 30 &'; jobs");
            assert_eq!(done.status, 0, "{}", done.err);
            // The text is the one `eval` ran, not a piece of the line that called it.
            assert!(done.out.contains("sleep 30 &"), "{}", done.out);
            // Plain `wait` waits for every job, so end the first one before.
            assert_eq!(run(sh, "kill %1; eval 'echo ok & wait'").out, "ok\n");
        });
    }

    #[test]
    fn a_subshell_has_no_jobs() {
        guarded(false, |sh| {
            let done = run(sh, "sleep 30 & (jobs; wait; echo $?)");
            assert_eq!(done.out, "0\n");
            assert_eq!(run(sh, "jobs").out.lines().count(), 1);
        });
    }

    #[test]
    fn a_job_that_cannot_start_ends_with_127_and_does_not_hang_the_table() {
        guarded(false, |sh| {
            let done = run(sh, "/nonexistent/omsh & wait %1; echo $?");
            assert_eq!(done.out, "127\n");
        });
    }

    #[test]
    fn errors_from_jobs_wait_and_kill() {
        guarded(false, |sh| {
            let done = run(sh, "kill %9");
            assert_eq!(done.status, 1);
            assert!(done.err.contains("kill: %9: no such job"), "{}", done.err);

            let done = run(sh, "kill -s NOSUCH 1");
            assert_eq!(done.status, 1);
            assert!(done.err.contains("invalid signal"), "{}", done.err);

            assert_eq!(run(sh, "kill").status, 2);
            assert_eq!(run(sh, "kill abc").status, 1);

            let done = run(sh, "wait %9");
            assert_eq!(done.status, 127);
            assert!(done.err.contains("no such job"), "{}", done.err);

            let done = run(sh, "wait 999999");
            assert_eq!(done.status, 127);
            assert!(done.err.contains("not a child"), "{}", done.err);
            assert_eq!(run(sh, "wait abc").status, 2);

            assert_eq!(run(sh, "jobs %3").status, 1);
            assert_eq!(run(sh, "jobs -z").status, 2);
            // The same process ids work with `kill`: signal 0 only checks.
            assert_eq!(run(sh, "kill -0 $$").status, 0);
        });
    }

    #[test]
    fn kill_lists_and_translates_signals() {
        guarded(false, |sh| {
            assert!(run(sh, "kill -l").out.starts_with("HUP INT QUIT"));
            assert_eq!(run(sh, "kill -l 15").out, "TERM\n");
            assert_eq!(run(sh, "kill -l 143").out, "TERM\n");
            assert_eq!(run(sh, "kill -l TERM").out, "15\n");
            assert_eq!(run(sh, "kill -l SIGKILL").out, "9\n");
            assert_eq!(run(sh, "kill -l NOSUCH").status, 1);
        });
    }

    #[test]
    fn kill_sends_signals_to_plain_process_ids_too() {
        guarded(false, |sh| {
            let done = run(sh, "sleep 30 & kill $!; wait $!; echo $?");
            assert_eq!(done.out, "143\n");
        });
    }

    // ------------------------------------------------------------------ stopped jobs
    //
    // A program stops itself with `kill -STOP $$`; `kill -STOP 0` stops its whole group. (There
    // is no terminal in a unit test, so no Ctrl+Z. The terminal part is in `omsh-tests`.)

    /// A file the test's programs write to, because a program that outlives `run` no longer
    /// writes to that call's captured output.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("omsh-s09b-{}-{name}", std::process::id()));
            let _ = std::fs::remove_file(&path);
            Self(path)
        }

        fn path(&self) -> &str {
            self.0.to_str().unwrap()
        }

        fn text(&self) -> String {
            std::fs::read_to_string(&self.0).unwrap_or_default()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Waits until the only job in the table has `programs` programs and all are stopped.
    fn all_stopped(sh: &Shell, programs: usize) {
        eventually("the job did not stop", || {
            sh.jobs.list().first().is_some_and(|view| {
                matches!(view.job.phase(), Phase::Stopped(_))
                    && view.job.live_pids().len() == programs
            })
        });
    }

    #[test]
    fn a_stopped_program_keeps_its_job_and_the_shell_goes_on() {
        guarded(true, |sh| {
            let done = run(sh, "sh -c 'kill -TSTP $$'; echo after $?");
            assert_eq!(done.out, "after 148\n");
            assert!(
                done.err
                    .contains("[1]+  Stopped                    sh -c 'kill -TSTP $$'"),
                "{}",
                done.err
            );
            let listed = run(sh, "jobs");
            assert_eq!(
                listed.out,
                format!("[1]+  {:<27}sh -c 'kill -TSTP $$'\n", "Stopped")
            );
            // Told once: not again before the next prompt.
            let captured = crate::io::capture::Captured::new();
            sh.job_notices(&captured.io);
            assert_eq!(captured.stderr(), "");
        });
    }

    #[test]
    fn fg_continues_a_stopped_job_and_gives_its_status() {
        guarded(true, |sh| {
            let file = Scratch::new("fg");
            run(
                sh,
                &format!(
                    "sh -c 'kill -STOP $$; echo resumed > {}; exit 3'",
                    file.path()
                ),
            );
            assert_eq!(file.text(), "");
            let done = run(sh, "fg");
            assert_eq!(done.status, 3);
            // `fg` shows which job it continued.
            assert!(
                done.out.starts_with("sh -c 'kill -STOP $$; echo resumed"),
                "{}",
                done.out
            );
            assert_eq!(file.text(), "resumed\n");
            // The job is gone, and `fg` has nothing to continue.
            assert_eq!(run(sh, "jobs").out, "");
            let none = run(sh, "fg");
            assert_eq!(none.status, 1);
            assert!(
                none.err.contains("fg: current: no such job"),
                "{}",
                none.err
            );
        });
    }

    #[test]
    fn a_job_can_be_stopped_and_continued_again() {
        guarded(true, |sh| {
            let file = Scratch::new("twice");
            run(
                sh,
                &format!(
                    "sh -c 'kill -STOP $$; echo one >> {0}; kill -STOP $$; echo two >> {0}'",
                    file.path()
                ),
            );
            let second = run(sh, "fg");
            assert_eq!(second.status, 147, "{}", second.err);
            assert_eq!(file.text(), "one\n");
            assert_eq!(run(sh, "fg").status, 0);
            assert_eq!(file.text(), "one\ntwo\n");
        });
    }

    #[test]
    fn bg_continues_a_stopped_job_in_the_background() {
        guarded(true, |sh| {
            run(sh, "sh -c 'kill -STOP $$; exit 4'");
            let done = run(sh, "bg");
            assert_eq!(done.status, 0);
            assert_eq!(done.err, "[1]+ sh -c 'kill -STOP $$; exit 4' &\n");
            assert_eq!(run(sh, "wait %1; echo $?").out, "4\n");
            // A job that already runs is left alone.
            run(sh, "sleep 30 &");
            let again = run(sh, "bg %1");
            assert!(
                again.err.contains("job 1 already in background"),
                "{}",
                again.err
            );
            assert_eq!(again.status, 0);
        });
    }

    #[test]
    fn fg_and_bg_need_job_control() {
        guarded(false, |sh| {
            for line in ["fg", "bg"] {
                let done = run(sh, line);
                assert_eq!(done.status, 1);
                assert!(done.err.contains("no job control"), "{}", done.err);
            }
        });
    }

    #[test]
    fn a_pipeline_stops_as_one_job_and_fg_finishes_it() {
        guarded(true, |sh| {
            let file = Scratch::new("pipeline");
            // The first program stops only after the second has started (it waits for the
            // marker), so that this test is about stopping two programs together. A stage that
            // has not started its program no longer makes the job count as stopped (see
            // `a_job_is_not_stopped_before_every_stage_has_started_its_program`).
            let marker = Scratch::new("pipeline-started");
            let done = run(
                sh,
                &format!(
                    "sh -c 'while [ ! -e {m} ]; do :; done; kill -STOP $$; echo one' \
                     | sh -c ': > {m}; kill -STOP $$; cat > {}'",
                    file.path(),
                    m = marker.path()
                ),
            );
            assert_eq!(done.err.matches("Stopped").count(), 1, "{}", done.err);
            all_stopped(sh, 2);
            assert_eq!(run(sh, "jobs").out.lines().count(), 1);
            let resumed = run(sh, "fg");
            assert_eq!(resumed.status, 0, "{}", resumed.err);
            assert_eq!(file.text(), "one\n");
        });
    }

    #[test]
    fn a_job_is_not_stopped_while_one_of_its_programs_runs() {
        guarded(true, |sh| {
            // Only the first program stops; the second runs until it is told to end. The first
            // waits for a marker from the second, so that the second's program is live when the
            // job is looked at: this test is about a running program keeping the job running.
            let marker = Scratch::new("not-stopped-started");
            let done = run(
                sh,
                &format!(
                    "sh -c 'while [ ! -e {m} ]; do :; done; kill -STOP $$' \
                     | sh -c ': > {m}; exec sleep 30' &\nsleep 0.3; jobs",
                    m = marker.path()
                ),
            );
            assert!(done.out.contains("Running"), "{}", done.out);
            assert!(!done.out.contains("Stopped"), "{}", done.out);
        });
    }

    /// The state letter of a process in `/proc/<pid>/stat` (`T` is stopped).
    fn process_state(pid: i32) -> Option<char> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        text.rsplit_once(')')?.1.trim().chars().next()
    }

    #[test]
    fn a_job_is_not_stopped_before_every_stage_has_started_its_program() {
        guarded(true, |sh| {
            // The race, made certain: the second stage opens a FIFO for `cat` before it can
            // fork `cat`, and nobody writes to the FIFO until this test says so. So the first
            // program stops while the second stage is on its way to a program, for as long as
            // we like. The shell must not give the job up then. (Before the fix `run` came back
            // at once with status 147 and a "Stopped" notice.)
            let pid_file = Scratch::new("race-pid");
            let fifo = Scratch::new("race-fifo");
            nix::unistd::mkfifo(&fifo.0, nix::sys::stat::Mode::S_IRWXU).expect("mkfifo");
            let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let helper = {
                let released = Arc::clone(&released);
                let pid_path = pid_file.0.clone();
                let fifo_path = fifo.0.clone();
                thread::spawn(move || {
                    let mut pid = 0;
                    eventually("the first program did not stop", || {
                        pid = std::fs::read_to_string(&pid_path)
                            .ok()
                            .and_then(|text| text.trim().parse().ok())
                            .unwrap_or(0);
                        pid != 0 && process_state(pid) == Some('T')
                    });
                    // Long enough for a wrong implementation to have given the job up.
                    thread::sleep(Duration::from_millis(300));
                    released.store(true, Ordering::SeqCst);
                    nix::sys::signal::kill(Pid::from_raw(pid), Signal::SIGCONT).expect("SIGCONT");
                    // Read and write, so that opening does not wait for anybody.
                    let mut writer = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&fifo_path)
                        .expect("open the fifo");
                    std::io::Write::write_all(&mut writer, b"x\n").expect("write");
                })
            };
            let done = run(
                sh,
                &format!(
                    "sh -c 'echo $$ > {pid}; kill -STOP $$' | cat < {fifo}; echo after $?",
                    pid = pid_file.path(),
                    fifo = fifo.path()
                ),
            );
            let released_when_back = released.load(Ordering::SeqCst);
            helper.join().expect("the helper thread ended");
            assert!(
                released_when_back,
                "the shell gave the job up before cat had started: {done:?}"
            );
            assert_eq!(done.out, "x\nafter 0\n", "{}", done.err);
            assert!(!done.err.contains("Stopped"), "{}", done.err);
            assert!(sh.jobs.list().is_empty());
        });
    }

    #[test]
    fn a_job_is_not_stopped_by_a_program_that_was_continued_from_outside() {
        guarded(true, |sh| {
            // The first program stops, then something else continues it (`kill -CONT` from
            // another shell). Nobody tells the job. The first program then ends only after the
            // second stage is over, so the job sees `cat` end while the first program is still
            // recorded as stopped: every live program "stopped", yet nothing is. The shell must
            // ask the kernel and let the job run to its end, not give it up as stopped (status
            // 147). This is the CI flake of
            // `a_job_is_not_stopped_before_every_stage_has_started_its_program`, made certain.
            let pid_file = Scratch::new("cont-pid");
            let fifo = Scratch::new("cont-fifo");
            let done_file = Scratch::new("cont-done");
            nix::unistd::mkfifo(&fifo.0, nix::sys::stat::Mode::S_IRWXU).expect("mkfifo");
            let pid_path = pid_file.0.clone();
            let fifo_path = fifo.0.clone();
            let helper = thread::spawn(move || {
                let mut pid = 0;
                eventually("the first program did not stop", || {
                    pid = std::fs::read_to_string(&pid_path)
                        .ok()
                        .and_then(|text| text.trim().parse().ok())
                        .unwrap_or(0);
                    pid != 0 && process_state(pid) == Some('T')
                });
                // Let the job record the stop first.
                thread::sleep(Duration::from_millis(200));
                nix::sys::signal::kill(Pid::from_raw(pid), Signal::SIGCONT).expect("SIGCONT");
                eventually("the first program did not continue", || {
                    process_state(pid) != Some('T')
                });
                let mut writer = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&fifo_path)
                    .expect("open the fifo");
                std::io::Write::write_all(&mut writer, b"x\n").expect("write");
            });
            // The first program waits for the second stage to be over, and then stays alive
            // for a while, so the stale record is the only thing that can make the job stopped.
            let done = run(
                sh,
                &format!(
                    "sh -c 'echo $$ > {pid}; kill -STOP $$; \
                     while [ ! -e {d} ]; do sleep 0.01; done; sleep 0.5' \
                     | {{ cat < {fifo}; : > {d}; }}; echo after $?",
                    pid = pid_file.path(),
                    fifo = fifo.path(),
                    d = done_file.path()
                ),
            );
            helper.join().expect("the helper thread ended");
            assert_eq!(done.out, "x\nafter 0\n", "{}", done.err);
            assert!(!done.err.contains("Stopped"), "{}", done.err);
            assert!(sh.jobs.list().is_empty());
        });
    }

    #[test]
    fn a_stage_that_runs_only_shell_code_does_not_hold_back_a_stop() {
        // A stage that never starts a program (`while read`, `read`, a command not found) must not keep the job
        // from counting as stopped, or Ctrl+Z on `cat file | while read l; do :; done` would
        // never bring the prompt back.
        for line in [
            "sh -c 'kill -STOP $$' | while read l; do :; done; echo after $?",
            "sh -c 'kill -STOP $$' | read x; echo after $?",
            "sh -c 'kill -STOP $$' | x=1; echo after $?",
            // Ends without ever starting a program.
            "sh -c 'kill -STOP $$' | nosuch-command-xyz 2>/dev/null; echo after $?",
        ] {
            guarded(true, move |sh| {
                let done = run(sh, line);
                assert_eq!(done.out, "after 147\n", "{line}: {}", done.err);
                assert_eq!(done.err.matches("Stopped").count(), 1, "{}", done.err);
                all_stopped(sh, 1);
            });
        }
    }

    #[test]
    fn a_subshell_stops_as_a_whole_and_fg_goes_on_inside_it() {
        guarded(true, |sh| {
            let file = Scratch::new("subshell");
            let done = run(
                sh,
                &format!(
                    "( sh -c 'kill -STOP $$'; echo x > {} ); echo after $?",
                    file.path()
                ),
            );
            assert_eq!(done.out, "after 147\n");
            all_stopped(sh, 1);
            assert!(
                run(sh, "jobs")
                    .out
                    .contains("( sh -c 'kill -STOP $$'; echo x")
            );
            assert_eq!(file.text(), "");
            assert_eq!(run(sh, "fg").status, 0);
            assert_eq!(file.text(), "x\n");
        });
    }

    #[test]
    fn after_a_stop_a_list_goes_on_a_loop_is_left_and_a_function_goes_on() {
        guarded(true, |sh| {
            let done = run(
                sh,
                "for i in 1 2; do sh -c 'kill -STOP $$'; echo in $i; done; echo out $?",
            );
            assert_eq!(done.out, "out 147\n");
            assert_eq!(run(sh, "jobs").out.lines().count(), 1);
            let done = run(
                sh,
                "f() { sh -c 'kill -STOP $$'; echo in f $?; echo f2; }; f; echo out $?",
            );
            assert_eq!(done.out, "in f 147\nf2\nout 0\n");
            assert_eq!(run(sh, "jobs").out.lines().count(), 2);
        });
    }

    #[test]
    fn a_nested_loop_is_left_whole() {
        guarded(true, |sh| {
            let done = run(
                sh,
                "while true; do for i in 1 2; do sh -c 'kill -STOP $$'; echo in; done; echo again; done; echo out $?",
            );
            assert_eq!(done.out, "out 147\n");
        });
    }

    #[test]
    fn a_job_started_in_a_function_is_named_after_its_words() {
        guarded(true, |sh| {
            run(sh, "f() { sh -c 'kill -STOP $$'; }; f");
            let listed = run(sh, "jobs").out;
            assert!(
                listed.contains("Stopped") && listed.contains("sh -c kill -STOP "),
                "{listed}"
            );
        });
    }

    #[test]
    fn a_stop_ends_the_shell_under_errexit_like_any_failure() {
        guarded(true, |sh| {
            let done = run(sh, "set -e; sh -c 'kill -STOP $$'; echo not");
            assert!(done.exited);
            assert_eq!(done.status, 147);
            assert_eq!(done.out, "");
        });
    }

    /// The signals that a program started by `command` ignores, from `/proc`.
    fn ignored_by(sh: &mut Shell, command: &str) -> u64 {
        let status = run(sh, command).out;
        status
            .lines()
            .find_map(|line| line.strip_prefix("SigIgn:"))
            .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
            .unwrap_or_else(|| panic!("no SigIgn in {status:?}"))
    }

    #[test]
    fn a_substitution_ignores_the_stop_signals() {
        // Ctrl+Z must not stop a program whose output the shell is waiting for. (The signals
        // are checked in the program's mask: whether the kernel would really stop a process
        // that signals itself depends on how the test run was started.)
        let stops = (1_u64 << (Signal::SIGTSTP as i32 - 1))
            | (1_u64 << (Signal::SIGTTIN as i32 - 1))
            | (1_u64 << (Signal::SIGTTOU as i32 - 1));
        guarded(true, move |sh| {
            let inside = ignored_by(sh, "echo \"$(cat /proc/self/status)\"");
            assert_eq!(inside & stops, stops, "ignored inside $(...): {inside:#x}");
            // A command of its own is a job and keeps the defaults: Ctrl+Z must stop it.
            let direct = ignored_by(sh, "cat /proc/self/status");
            assert_eq!(direct & stops, 0, "ignored by a job: {direct:#x}");
        });
    }

    #[test]
    fn jobs_can_be_listed_by_state() {
        guarded(true, |sh| {
            run(sh, "sleep 30 &");
            run(sh, "sh -c 'kill -STOP $$'");
            assert_eq!(run(sh, "jobs -s").out.lines().count(), 1);
            assert!(run(sh, "jobs -s").out.contains("Stopped"));
            assert_eq!(run(sh, "jobs -r").out.lines().count(), 1);
            assert!(run(sh, "jobs -r").out.contains("Running"));
            assert_eq!(run(sh, "jobs").out.lines().count(), 2);
        });
    }

    #[test]
    fn kill_ends_a_stopped_job() {
        guarded(true, |sh| {
            run(sh, "sh -c 'kill -STOP $$'");
            assert_eq!(run(sh, "kill %1; wait %1; echo $?").out, "143\n");
        });
    }

    #[test]
    fn disown_forgets_a_job() {
        guarded(true, |sh| {
            run(sh, "sleep 30 & p1=$!; sleep 31 & p2=$!");
            assert_eq!(run(sh, "disown %1; jobs").out.lines().count(), 1);
            let done = run(sh, "disown %9");
            assert_eq!(done.status, 1);
            assert!(done.err.contains("no such job"), "{}", done.err);
            // `disown -a` and `-r`.
            assert_eq!(run(sh, "disown -r; jobs").out, "");
            // The disowned programs are not the table's any more: end them here.
            run(sh, "kill $p1 $p2");
        });
    }

    #[test]
    fn disown_h_keeps_the_job_listed_but_safe_from_hangup() {
        guarded(true, |sh| {
            run(sh, "sleep 30 &");
            run(sh, "disown -h");
            assert!(run(sh, "jobs").out.contains("Running"));
            assert!(sh.jobs.list()[0].no_hup);
        });
    }

    #[test]
    fn disowning_a_stopped_job_warns() {
        guarded(true, |sh| {
            run(sh, "sh -c 'kill -STOP $$'");
            let pid = sh.jobs.list()[0].job.leader().unwrap();
            let done = run(sh, "disown");
            assert!(
                done.err.contains(&format!(
                    "warning: deleting stopped job 1 with process group {pid}"
                )),
                "{}",
                done.err
            );
            assert_eq!(run(sh, "jobs").out, "");
            // Not the table's now: end it here.
            let _ = nix::sys::signal::kill(pid, Signal::SIGKILL);
            let _ = nix::sys::signal::kill(pid, Signal::SIGCONT);
        });
    }

    #[test]
    fn monitor_is_an_option_and_shows_in_dollar_dash() {
        guarded(false, |sh| {
            assert_eq!(run(sh, "set -m; echo $-").out, "sm\n");
            assert_eq!(run(sh, "set -o | grep monitor").out, "monitor         on\n");
            assert_eq!(run(sh, "set +m; echo $-").out, "s\n");
            assert_eq!(run(sh, "set -o monitor; echo $-").out, "sm\n");
        });
    }

    #[test]
    fn the_first_exit_with_stopped_jobs_only_warns() {
        guarded(true, |sh| {
            sh.in_subshell = false;
            sh.interactive = true;
            run(sh, "sh -c 'kill -STOP $$'");
            let first = run(sh, "exit");
            assert!(!first.exited);
            assert!(
                first.err.contains("There are stopped jobs."),
                "{}",
                first.err
            );
            // Straight away again: it goes through.
            assert!(run(sh, "exit").exited);
        });
    }

    #[test]
    fn another_command_in_between_brings_the_warning_back() {
        guarded(true, |sh| {
            sh.in_subshell = false;
            sh.interactive = true;
            run(sh, "sh -c 'kill -STOP $$'");
            assert!(!run(sh, "exit").exited);
            assert!(!run(sh, "true").exited);
            let third = run(sh, "exit");
            assert!(!third.exited);
            assert!(third.err.contains("There are stopped jobs."));
        });
    }

    #[test]
    fn no_warning_without_stopped_jobs_or_outside_an_interactive_shell() {
        guarded(true, |sh| {
            sh.in_subshell = false;
            sh.interactive = true;
            run(sh, "sleep 30 &");
            assert!(
                run(sh, "exit").exited,
                "a running job does not hold the shell back"
            );
            sh.interactive = false;
            run(sh, "sh -c 'kill -STOP $$'");
            assert!(run(sh, "exit").exited);
        });
    }

    /// True if this test process ignores `SIGHUP` (it was started under `nohup`). Programs it
    /// starts inherit that, and then a hangup does nothing to them, which is right.
    fn hup_is_ignored() -> bool {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("SigIgn:"))
                    .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
            })
            .is_some_and(|mask| mask & 1 != 0)
    }

    #[test]
    fn ending_the_session_hangs_up_stopped_jobs_but_not_running_ones() {
        if hup_is_ignored() {
            eprintln!("skipped: SIGHUP is ignored here (started under nohup?)");
            return;
        }
        guarded(true, |sh| {
            run(sh, "sleep 30 &");
            run(sh, "sh -c 'kill -STOP $$'");
            sh.end_session();
            // The stopped job got SIGHUP (and SIGCONT, so that it could act on it).
            eventually("the stopped job did not end", || {
                matches!(sh.jobs.list()[1].job.phase(), Phase::Finished(_))
            });
            assert_eq!(
                sh.jobs.list()[1].job.phase(),
                Phase::Finished(ExitStatus(129))
            );
            assert_eq!(sh.jobs.list()[0].job.phase(), Phase::Running);
        });
    }

    #[test]
    fn a_disowned_with_h_stopped_job_survives_the_end_of_the_session() {
        guarded(true, |sh| {
            run(sh, "sh -c 'kill -STOP $$'");
            run(sh, "disown -h");
            sh.end_session();
            thread::sleep(Duration::from_millis(200));
            assert!(matches!(sh.jobs.list()[0].job.phase(), Phase::Stopped(_)));
        });
    }

    #[test]
    fn the_shell_code_of_a_stopped_job_waits_where_it_looks_for_ctrl_c() {
        // A loop looks at `interrupted()` on every pass: for a stopped job it must sleep there
        // and not spin until the job runs again.
        let bell = Jobs::new().bell();
        let job = crate::jobs::Job::foreground(bell, 1, false);
        job.pretend_started(&[Pid::from_raw(900_003)]);
        job.pretend_stopped(Pid::from_raw(900_003), Signal::SIGTSTP as i32);
        let mut sh = shell();
        sh.job = Some(Arc::clone(&job));
        let (sender, receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let interrupted = sh.interrupted();
            let _ = sender.send(interrupted);
        });
        assert!(receiver.recv_timeout(Duration::from_millis(300)).is_err());
        job.mark_continued();
        let interrupted = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("the shell code did not wake up");
        assert!(!interrupted);
        thread.join().unwrap();
    }

    #[test]
    fn a_background_job_is_not_stopped_by_the_ctrl_c_flag() {
        // The flag is global; the shell code of a background job reads its own instead.
        let bell = Jobs::new().bell();
        let job = crate::jobs::Job::background(false, bell);
        let mut sh = shell();
        sh.job = Some(Arc::clone(&job));
        assert!(!sh.interrupted());
        job.signal(Signal::SIGTERM).unwrap();
        assert!(sh.interrupted());
        assert_eq!(job.cancelled_by(), Some(Signal::SIGTERM as i32));
    }
}

mod history {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{BuiltinOutput, HistoryCommand};

    /// Answers with what it was asked, and remembers it.
    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<(Vec<String>, String, bool)>>,
    }

    impl HistoryCommand for Recorder {
        fn run(&self, args: &[Vec<u8>], cwd: &[u8], to_terminal: bool) -> BuiltinOutput {
            let args: Vec<String> = args
                .iter()
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect();
            let cwd = String::from_utf8_lossy(cwd).into_owned();
            self.calls
                .lock()
                .unwrap()
                .push((args.clone(), cwd, to_terminal));
            if args.first().is_some_and(|arg| arg == "bad") {
                return BuiltinOutput {
                    out: Vec::new(),
                    err: b"history: bad: no such thing\n".to_vec(),
                    status: 2,
                };
            }
            BuiltinOutput {
                out: format!("asked {}\nsecond line\n", args.join(",")).into_bytes(),
                err: Vec::new(),
                status: 0,
            }
        }
    }

    fn shell_with_history() -> (Shell, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let mut sh = shell();
        sh.set_history(Arc::clone(&recorder) as Arc<dyn HistoryCommand>);
        (sh, recorder)
    }

    #[test]
    fn without_a_history_the_builtin_says_so() {
        let done = run(&mut shell(), "history");
        assert_eq!(done.status, 1);
        assert_eq!(done.out, "");
        assert_eq!(
            done.err,
            "oh-my-shell: history: not available in this shell\n"
        );
    }

    #[test]
    fn the_builtin_passes_its_words_and_the_directory_on() {
        let (mut sh, recorder) = shell_with_history();
        let dir = Dir::new();
        let done = run(
            &mut sh,
            &format!("cd {}; history --here search 'a b'", dir.path().display()),
        );
        assert_eq!(done.status, 0);
        assert_eq!(done.out, "asked --here,search,a b\nsecond line\n");
        assert_eq!(done.err, "");
        let calls = recorder.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].1, dir.path().display().to_string());
        // The test's output is captured in a file, which is not a terminal.
        assert!(!calls[0].2);
    }

    #[test]
    fn an_error_goes_to_standard_error_with_its_status() {
        let (mut sh, _) = shell_with_history();
        let done = run(&mut sh, "history bad; echo status $?");
        assert_eq!(done.out, "status 2\n");
        assert_eq!(done.err, "history: bad: no such thing\n");
        let quiet = run(&mut sh, "history bad 2>/dev/null");
        assert_eq!((quiet.status, quiet.err.as_str()), (2, ""));
    }

    #[test]
    fn the_builtin_works_in_pipelines_substitutions_and_redirections() {
        let (mut sh, recorder) = shell_with_history();
        assert_eq!(
            run(&mut sh, "history a | /usr/bin/head -n 1").out,
            "asked a\n"
        );
        assert_eq!(
            run(&mut sh, "x=$(history b); echo \"[$x]\"").out,
            "[asked b\nsecond line]\n"
        );
        assert_eq!(run(&mut sh, "history c >/dev/null").out, "");
        // Copies of the shell share the one history.
        assert_eq!(recorder.calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn history_is_a_regular_builtin() {
        let (mut sh, recorder) = shell_with_history();
        assert_eq!(
            run(&mut sh, "type history").out,
            "history is a shell builtin\n"
        );
        assert!(crate::is_builtin("history"));
        // A function of the same name comes first, as for every regular builtin.
        let done = run(
            &mut sh,
            "history() { echo mine; }; history; command history z",
        );
        assert_eq!(done.out, "mine\nasked z\nsecond line\n");
        assert_eq!(recorder.calls.lock().unwrap().len(), 1);
    }
}

mod omsh {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{OmshCommand, ReadError, Terminal};

    /// A command that does what its first word says, and remembers its words.
    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<Vec<String>>>,
    }

    impl OmshCommand for Recorder {
        fn run(&self, args: &[Vec<u8>], terminal: &mut dyn Terminal) -> u8 {
            let words: Vec<String> = args
                .iter()
                .map(|arg| String::from_utf8_lossy(arg).into_owned())
                .collect();
            self.calls.lock().unwrap().push(words.clone());
            let mut line = Vec::new();
            match words.first().map(String::as_str) {
                Some("say") => {
                    terminal.out("to-out\n");
                    terminal.err("to-err\n");
                    0
                }
                Some(which @ ("ask" | "secret")) => {
                    let hidden = which == "secret";
                    match terminal.read_line("Q? ", hidden, &mut line) {
                        Ok(()) => {
                            terminal.out(&format!("got:{}\n", String::from_utf8_lossy(&line)))
                        }
                        Err(ReadError::EndOfInput) => terminal.out("end of input\n"),
                        Err(other) => terminal.out(&format!("failed:{other:?}\n")),
                    }
                    0
                }
                Some("terminal") => {
                    terminal.out(&format!("{}\n", terminal.input_is_terminal()));
                    0
                }
                Some("seven") => 7,
                _ => 0,
            }
        }
    }

    fn shell_with_omsh() -> (Shell, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let mut sh = shell();
        sh.set_omsh(Arc::clone(&recorder) as Arc<dyn OmshCommand>);
        (sh, recorder)
    }

    #[test]
    fn without_a_command_the_builtin_says_so() {
        let done = run(&mut shell(), "omsh ai status");
        assert_eq!(done.status, 1);
        assert_eq!(done.err, "oh-my-shell: omsh: not available in this shell\n");
    }

    #[test]
    fn it_is_a_builtin_that_a_program_of_the_same_name_does_not_replace() {
        assert!(crate::is_builtin("omsh"));
        let (mut sh, _) = shell_with_omsh();
        let done = run(&mut sh, "type omsh");
        assert!(done.out.contains("builtin"), "{}", done.out);
    }

    #[test]
    fn the_words_and_the_status_pass_through() {
        let (mut sh, recorder) = shell_with_omsh();
        let done = run(
            &mut sh,
            "omsh ai 'two words' x; echo status $?; omsh seven; echo status $?",
        );
        assert_eq!(done.out, "status 0\nstatus 7\n");
        let calls = recorder.calls.lock().unwrap();
        assert_eq!(calls[0], ["ai", "two words", "x"]);
    }

    #[test]
    fn output_and_messages_use_the_right_descriptors() {
        let (mut sh, _) = shell_with_omsh();
        let done = run(&mut sh, "omsh say");
        assert_eq!(
            (done.out.as_str(), done.err.as_str()),
            ("to-out\n", "to-err\n")
        );
        let quiet = run(&mut sh, "omsh say 2>/dev/null | cat");
        assert_eq!((quiet.out.as_str(), quiet.err.as_str()), ("to-out\n", ""));
    }

    #[test]
    fn a_line_is_read_from_standard_input_and_nothing_more() {
        let (mut sh, _) = shell_with_omsh();
        let done = run(
            &mut sh,
            "{ omsh ask; cat; } <<'EOF'\nfirst\nsecond\nthird\nEOF",
        );
        assert_eq!(done.out, "got:first\nsecond\nthird\n");
        assert_eq!(done.err, "Q? ");
    }

    #[test]
    fn a_last_line_without_a_line_ending_counts() {
        let (mut sh, _) = shell_with_omsh();
        let done = run(&mut sh, "printf abc | omsh ask");
        assert_eq!(done.out, "got:abc\n");
    }

    #[test]
    fn the_end_of_the_input_is_reported() {
        let (mut sh, _) = shell_with_omsh();
        assert_eq!(run(&mut sh, "omsh ask </dev/null").out, "end of input\n");
        assert_eq!(run(&mut sh, "printf '' | omsh ask").out, "end of input\n");
    }

    #[test]
    fn an_empty_line_is_an_empty_answer_and_not_the_end() {
        let (mut sh, _) = shell_with_omsh();
        assert_eq!(run(&mut sh, "printf '\\n' | omsh ask").out, "got:\n");
    }

    #[test]
    fn a_hidden_read_from_a_pipe_prints_no_extra_line() {
        let (mut sh, _) = shell_with_omsh();
        let done = run(&mut sh, "printf 'sekret\\n' | omsh secret");
        assert_eq!(done.out, "got:sekret\n");
        // Only the prompt: with no terminal there was no echo to turn off and no line to end.
        assert_eq!(done.err, "Q? ");
    }

    #[test]
    fn standard_input_is_not_a_terminal_in_a_test() {
        let (mut sh, _) = shell_with_omsh();
        assert_eq!(run(&mut sh, "omsh terminal </dev/null").out, "false\n");
    }

    #[test]
    fn it_works_in_a_substitution_and_a_function() {
        let (mut sh, _) = shell_with_omsh();
        let done = run(
            &mut sh,
            "f() { omsh say 2>/dev/null; }; x=$(f); echo \"[$x]\"",
        );
        assert_eq!(done.out, "[to-out]\n");
    }
}
