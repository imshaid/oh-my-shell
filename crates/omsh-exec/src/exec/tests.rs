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
    for line in [
        "set -m",
        "set -o monitor",
        "set -o nosuchname",
        "set -z",
        "set +q",
    ] {
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
    // Something that cannot run yet is refused in `eval` too.
    let d = run(&mut shell(), "eval 'sleep 0 &'");
    assert_eq!(d.status, 2);
    assert!(d.err.contains("background jobs"), "{}", d.err);
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
