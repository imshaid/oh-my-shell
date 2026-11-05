//! Tests that run whole lines through a shell and look at the variables they leave behind.
//!
//! `r=$(printf '<%s>' ...)` is the way to see where the fields of a command begin and end:
//! the command's output is captured into a variable that the test reads.

use super::*;
use crate::io::capture::Captured;
use crate::status::ExitStatus;
use crate::vars::Variables;

fn shell() -> Shell {
    Shell::with_variables(Variables::from_pairs([
        ("PATH", "/usr/bin:/bin"),
        ("HOME", "/home/me"),
    ]))
}

/// Runs `src` and returns what the shell printed on its own behalf (descriptor 2).
fn run(shell: &mut Shell, src: &str) -> String {
    let captured = Captured::new();
    let result = shell.run_source(src, true, &captured.io);
    assert!(
        matches!(result, crate::LineResult::Ran(_)),
        "not run: {src}: {result:?}"
    );
    captured.stderr()
}

fn var(shell: &Shell, name: &str) -> String {
    String::from_utf8_lossy(shell.vars.get(name).unwrap_or(b"<unset>")).into_owned()
}

/// The fields of `args` as `<a><b>`. `printf` prints `<>` when it gets no argument at all, so
/// a marker goes first and is cut off again.
fn fields(shell: &mut Shell, args: &str) -> String {
    run(shell, &format!("r=$(printf '<%s>' M {args})"));
    var(shell, "r").strip_prefix("<M>").unwrap().to_owned()
}

#[test]
fn a_variable_is_used_and_an_unset_one_is_empty() {
    let mut sh = shell();
    run(&mut sh, "x=hello");
    assert_eq!(
        fields(&mut sh, "$x ${x} \"$x\" $unset"),
        "<hello><hello><hello>"
    );
    assert_eq!(fields(&mut sh, "\"$unset\""), "<>");
    assert_eq!(fields(&mut sh, "a${unset}b"), "<ab>");
}

#[test]
fn an_unquoted_empty_expansion_vanishes_and_a_quoted_one_stays() {
    let mut sh = shell();
    run(&mut sh, "e=");
    assert_eq!(fields(&mut sh, "a $e b"), "<a><b>");
    assert_eq!(fields(&mut sh, "a \"$e\" b"), "<a><><b>");
    assert_eq!(fields(&mut sh, "a '' b"), "<a><><b>");
    assert_eq!(fields(&mut sh, "$e$e"), "");
}

#[test]
fn splitting_follows_ifs() {
    let mut sh = shell();
    run(&mut sh, "x='a  b c'");
    assert_eq!(fields(&mut sh, "$x"), "<a><b><c>");
    assert_eq!(fields(&mut sh, "\"$x\""), "<a  b c>");
    run(&mut sh, "IFS=:; y=a::b:");
    assert_eq!(fields(&mut sh, "$y"), "<a><><b>");
    run(&mut sh, "IFS=");
    assert_eq!(fields(&mut sh, "$x"), "<a  b c>");
    run(&mut sh, "unset_marker=1");
}

#[test]
fn text_next_to_a_split_expansion_joins_its_ends() {
    let mut sh = shell();
    run(&mut sh, "x=' a b '");
    assert_eq!(fields(&mut sh, "p$x"), "<p><a><b>");
    assert_eq!(fields(&mut sh, "${x}q"), "<a><b><q>");
    assert_eq!(fields(&mut sh, "\"p\"$x\"q\""), "<p><a><b><q>");
}

#[test]
fn default_and_alternative_values() {
    let mut sh = shell();
    run(&mut sh, "set_x=v; empty=");
    assert_eq!(
        fields(&mut sh, "${set_x:-d} ${empty:-d} ${unset:-d}"),
        "<v><d><d>"
    );
    // An unquoted empty value vanishes, even though it is set.
    assert_eq!(
        fields(&mut sh, "${set_x-d} ${empty-d} ${unset-d}"),
        "<v><d>"
    );
    assert_eq!(fields(&mut sh, "\"${empty-d}\""), "<>");
    assert_eq!(
        fields(&mut sh, "${set_x:+a} ${empty:+a} ${unset:+a}"),
        "<a>"
    );
    assert_eq!(
        fields(&mut sh, "${set_x+a} ${empty+a} ${unset+a}"),
        "<a><a>"
    );
    assert_eq!(fields(&mut sh, "${unset:-a b}"), "<a><b>");
    assert_eq!(fields(&mut sh, "\"${unset:-a b}\""), "<a b>");
    assert_eq!(fields(&mut sh, "${unset:-$set_x}"), "<v>");
}

#[test]
fn assigning_defaults() {
    let mut sh = shell();
    // The words of a substitution run in a copy of the shell, so assign outside of one.
    run(&mut sh, "x=${a:=1}");
    assert_eq!(var(&sh, "a"), "1");
    assert_eq!(var(&sh, "x"), "1");
    assert_eq!(fields(&mut sh, "${a:=9} $a"), "<1><1>");
    run(&mut sh, "b=");
    assert_eq!(fields(&mut sh, "${b=2} [$b]"), "<[]>");
    assert_eq!(fields(&mut sh, "${b:=2} $b"), "<2><2>");
}

#[test]
fn a_missing_value_with_a_question_mark_is_an_error() {
    let mut sh = shell();
    let captured = Captured::new();
    let result = sh.run_source("x=1; echo ${nope:?it is gone}; x=2", true, &captured.io);
    assert!(matches!(result, crate::LineResult::Ran(_)));
    assert_eq!(captured.stderr(), "oh-my-shell: nope: it is gone\n");
    // The line stopped there.
    assert_eq!(var(&sh, "x"), "1");
    assert_eq!(sh.last_status(), ExitStatus::USAGE);
    let err = run(&mut sh, "echo ${nope?}");
    assert_eq!(err, "oh-my-shell: nope: parameter not set\n");
    let err = run(&mut sh, "e=; echo ${e:?}");
    assert_eq!(err, "oh-my-shell: e: parameter null or not set\n");
    assert_eq!(run(&mut sh, "echo ${e?}"), "");
}

#[test]
fn trimming() {
    let mut sh = shell();
    run(&mut sh, "v=abcabc");
    assert_eq!(
        fields(&mut sh, "${v#*b} ${v##*b} ${v%b*} ${v%%b*}"),
        "<cabc><c><abca><a>"
    );
    assert_eq!(
        fields(&mut sh, "${v#x} ${v%x} ${v#} ${v%}"),
        "<abcabc><abcabc><abcabc><abcabc>"
    );
    assert_eq!(fields(&mut sh, "${v#a?} ${v%?c}"), "<cabc><abca>");
    run(&mut sh, "p='*'");
    assert_eq!(fields(&mut sh, "${v#$p} \"${v#$p}\""), "<abcabc><abcabc>");
    // A quoted pattern character is plain.
    assert_eq!(fields(&mut sh, "${v#\"*\"}"), "<abcabc>");
    run(&mut sh, "s='a*b'");
    assert_eq!(fields(&mut sh, "${s#a\"*\"}"), "<b>");
    assert_eq!(fields(&mut sh, "${unset#a}"), "");
}

#[test]
fn length_counts_characters() {
    let mut sh = shell();
    run(&mut sh, "v=বাংলা; e=");
    assert_eq!(fields(&mut sh, "${#v} ${#e} ${#unset}"), "<5><0><0>");
}

#[test]
fn special_parameters() {
    let mut sh = shell();
    run(&mut sh, "false");
    assert_eq!(fields(&mut sh, "$? $#"), "<1><0>");
    assert_eq!(fields(&mut sh, "$0"), "<oh-my-shell>");
    assert_eq!(fields(&mut sh, "$$"), format!("<{}>", std::process::id()));
    assert_eq!(fields(&mut sh, "\"$@\" \"$*\""), "<>");
    assert_eq!(fields(&mut sh, "$@ $*"), "");
    assert_eq!(fields(&mut sh, "$-"), "<s>");
    sh.set_interactive(true);
    assert_eq!(fields(&mut sh, "$-"), "<si>");
}

/// What `/bin/sh` makes of the same positional parameters.
fn sh_fields(params: &[&str], args: &str) -> String {
    let script = format!("printf '<%s>' M {args}");
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(&script)
        .arg("name")
        .args(params)
        .output()
        .unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .strip_prefix("<M>")
        .unwrap()
        .to_owned()
}

#[test]
fn positional_parameters_behave_like_sh() {
    for params in [
        &[][..],
        &["a"],
        &["a", "b c"],
        &["", "x"],
        &[" a ", ""],
        &["a b", "c d"],
    ] {
        for args in [
            "$@",
            "\"$@\"",
            "$*",
            "\"$*\"",
            "x\"$@\"y",
            "\"x$@y\"",
            // dash makes one empty field of `"$@$@"` with no parameters, bash and POSIX
            // make none, and so do we. That case is therefore not compared.
            "$1",
            "\"$1\"",
            "$2",
            "${1:-d}",
            "${3:-d}",
            "${#}",
            "$# ${#1}",
            "\"${@:-none}\"",
            "${*:+set}",
            "${1#a} ${@#a}",
        ] {
            let mut sh = shell();
            sh.positional = params.iter().map(|p| p.as_bytes().to_vec()).collect();
            let ours = fields(&mut sh, args);
            let theirs = sh_fields(params, args);
            assert_eq!(ours, theirs, "params {params:?}, args {args}");
        }
    }
}

#[test]
fn star_joins_with_the_first_ifs_character() {
    let mut sh = shell();
    sh.positional = vec![b"a".to_vec(), b"b".to_vec()];
    run(&mut sh, "IFS=:");
    assert_eq!(fields(&mut sh, "\"$*\" \"$@\""), "<a:b><a><b>");
    run(&mut sh, "IFS=");
    assert_eq!(fields(&mut sh, "\"$*\""), "<ab>");
    run(&mut sh, "unset_marker=1");
}

#[test]
fn command_substitution() {
    let mut sh = shell();
    assert_eq!(
        fields(&mut sh, "$(printf 'a  b') \"$(printf 'a  b')\""),
        "<a><b><a  b>"
    );
    assert_eq!(fields(&mut sh, "$(printf 'x\\n\\n\\n')"), "<x>");
    assert_eq!(fields(&mut sh, "\"$(printf '\\n\\nx\\n')\""), "<\n\nx>");
    assert_eq!(fields(&mut sh, "`echo hi`"), "<hi>");
    assert_eq!(fields(&mut sh, "$(echo $(echo deep))"), "<deep>");
    assert_eq!(fields(&mut sh, "a$(echo b)c"), "<abc>");
    assert_eq!(fields(&mut sh, "$(true)"), "");
}

#[test]
fn command_substitution_has_its_own_variables_and_exit() {
    let mut sh = shell();
    run(&mut sh, "x=outer; y=$(x=inner; echo $x; exit 3)");
    assert_eq!(var(&sh, "x"), "outer");
    assert_eq!(var(&sh, "y"), "inner");
    assert_eq!(sh.last_status(), ExitStatus(3));
    assert_eq!(fields(&mut sh, "$(exit 5; echo no)"), "");
}

#[test]
fn an_assignment_only_command_has_the_status_of_its_substitution() {
    let mut sh = shell();
    run(&mut sh, "x=$(false)");
    assert_eq!(sh.last_status(), ExitStatus(1));
    run(&mut sh, "x=$(true)");
    assert_eq!(sh.last_status(), ExitStatus(0));
    run(&mut sh, "false; x=1");
    assert_eq!(sh.last_status(), ExitStatus(0));
    // A command's own status wins over its substitutions.
    run(&mut sh, "true $(false)");
    assert_eq!(sh.last_status(), ExitStatus(0));
}

#[test]
fn a_substitution_sees_the_current_status() {
    let mut sh = shell();
    run(&mut sh, "false; r=$(echo $?)");
    assert_eq!(var(&sh, "r"), "1");
}

#[test]
fn output_that_is_not_utf8_passes_through() {
    let mut sh = shell();
    run(&mut sh, "r=$(printf 'a\\377b')");
    assert_eq!(sh.vars.get("r"), Some(&b"a\xffb"[..]));
}

#[test]
fn a_nul_byte_in_the_output_is_dropped() {
    let mut sh = shell();
    run(&mut sh, "r=$(printf 'a\\0b')");
    assert_eq!(var(&sh, "r"), "ab");
}

#[test]
fn arithmetic() {
    let mut sh = shell();
    assert_eq!(
        fields(&mut sh, "$((1+2*3)) $(( (1+2)*3 )) $((7/2)) $((-7%3))"),
        "<7><9><3><-1>"
    );
    run(&mut sh, "n=4");
    assert_eq!(
        fields(&mut sh, "$((n*n)) $(($n+1)) $((n<5)) $((n==4?10:20))"),
        "<16><5><1><10>"
    );
    assert_eq!(fields(&mut sh, "$((m=3)) $m $((m+=2)) $m"), "<3><3><5><5>");
    assert_eq!(fields(&mut sh, "$((0x1f)) $((010))"), "<31><8>");
    assert_eq!(fields(&mut sh, "\"$((1+1))\"x"), "<2x>");
}

#[test]
fn an_arithmetic_error_stops_the_line() {
    let mut sh = shell();
    let err = run(&mut sh, "echo $((1/0)); x=1");
    assert!(err.contains("division by zero"), "{err}");
    assert_eq!(sh.vars.get("x"), None);
    assert_eq!(sh.last_status(), ExitStatus::USAGE);
}

#[test]
fn tilde() {
    let mut sh = shell();
    assert_eq!(fields(&mut sh, "~ ~/x"), "</home/me></home/me/x>");
    assert_eq!(fields(&mut sh, "\"~\" '~' \\~ a~"), "<~><~><~><a~>");
    // A quote inside the prefix turns the expansion off.
    assert_eq!(fields(&mut sh, "~\"/x\""), "<~/x>");
    assert_eq!(fields(&mut sh, "~nosuchuser-omsh"), "<~nosuchuser-omsh>");
    run(&mut sh, "HOME='/a b'");
    assert_eq!(fields(&mut sh, "~"), "</a b>");
    assert_eq!(fields(&mut sh, "${unset:-~/z}"), "</a b/z>");
    assert_eq!(fields(&mut sh, "\"${unset:-~/z}\""), "<~/z>");
}

#[test]
fn tilde_in_an_assignment_also_works_after_a_colon() {
    let mut sh = shell();
    run(&mut sh, "p=~/a:~/b:x~");
    assert_eq!(var(&sh, "p"), "/home/me/a:/home/me/b:x~");
    run(&mut sh, "q=~");
    assert_eq!(var(&sh, "q"), "/home/me");
}

#[test]
fn an_assignment_value_is_neither_split_nor_matched() {
    let mut sh = shell();
    run(&mut sh, "x='a  b'; y=$x; z=*");
    assert_eq!(var(&sh, "y"), "a  b");
    assert_eq!(var(&sh, "z"), "*");
}

#[test]
fn ansi_c_quotes() {
    let mut sh = shell();
    assert_eq!(
        fields(&mut sh, "$'a\\tb' $'\\x41\\101' $'it\\'s'"),
        "<a\tb><AA><it's>"
    );
    assert_eq!(fields(&mut sh, "$'a b'"), "<a b>");
}

#[test]
fn globbing() {
    let dir = std::env::temp_dir().join(format!("omsh-expand-glob-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["b.txt", "a.txt", "c.rs", ".hid", "sp ace.txt"] {
        std::fs::write(dir.join(name), "").unwrap();
    }
    let mut sh = shell();
    run(&mut sh, &format!("d={}", dir.display()));
    let strip = |s: String| s.replace(&format!("{}/", dir.display()), "");
    assert_eq!(
        strip(fields(&mut sh, "$d/*.txt")),
        "<a.txt><b.txt><sp ace.txt>"
    );
    assert_eq!(strip(fields(&mut sh, "$d/?.rs")), "<c.rs>");
    assert_eq!(strip(fields(&mut sh, "$d/.*")), "<.hid>");
    assert_eq!(strip(fields(&mut sh, "\"$d\"/[ab].*")), "<a.txt><b.txt>");
    // Quoted pattern characters stay text, and so does a pattern with no match.
    assert_eq!(
        strip(fields(&mut sh, "\"$d/*.txt\"")),
        format!("<{}/*.txt>", dir.display()).replace(&format!("{}/", dir.display()), "")
    );
    assert!(fields(&mut sh, "$d/*.nomatch").ends_with("*.nomatch>"));
    // A pattern that comes from an expansion is a pattern.
    run(&mut sh, "pat='*.rs'");
    assert_eq!(strip(fields(&mut sh, "$d/$pat")), "<c.rs>");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn assignments_are_applied_in_order_and_those_before_a_command_are_taken_back() {
    let mut sh = shell();
    run(&mut sh, "a=1 b=$a c=$b");
    assert_eq!(var(&sh, "c"), "1");
    run(&mut sh, "a=x b=$a true");
    assert_eq!(var(&sh, "a"), "1");
    assert_eq!(var(&sh, "b"), "1");
    // A failing value takes back the ones before it, too.
    let err = run(&mut sh, "a=changed b=${nothing:?} true");
    assert!(err.contains("nothing"), "{err}");
    assert_eq!(var(&sh, "a"), "1");
}

#[test]
fn the_number_of_positional_parameters_is_the_length_of_at_and_star() {
    let mut sh = shell();
    sh.positional = vec![b"a b".to_vec(), b"c".to_vec()];
    assert_eq!(fields(&mut sh, "${#@} ${#*} $# ${#1}"), "<2><2><2><3>");
}

#[test]
fn a_list_of_parameters_is_trimmed_one_by_one() {
    let mut sh = shell();
    sh.positional = vec![b"xa".to_vec(), b"xb".to_vec()];
    assert_eq!(fields(&mut sh, "\"${@#x}\" ${*%b}"), "<a><b><xa><x>");
}
