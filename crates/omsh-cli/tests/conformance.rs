//! Runs the same input through `oh-my-shell` and through `/bin/sh` and compares what they do.
//!
//! The input goes in on standard input, which is how the shell reads for now. For each case
//! the standard output and the exit status must be identical. Standard error is compared only
//! as "empty or not", because the wording of the messages differs on purpose.
//!
//! Cases where this shell is meant to behave differently from `/bin/sh` are tested on their
//! own further down.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

const OMSH: &str = env!("CARGO_BIN_EXE_oh-my-shell");
const TARGET_TMP: &str = env!("CARGO_TARGET_TMPDIR");

/// A scratch directory with a few files to run, removed at the end of the test.
struct Sandbox {
    dir: PathBuf,
}

impl Sandbox {
    fn new() -> io::Result<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = Path::new(TARGET_TMP).join(format!("conformance-{}-{n}", std::process::id()));
        fs::create_dir_all(dir.join("bin"))?;
        fs::create_dir_all(dir.join("subdir"))?;
        let sandbox = Self { dir };
        // A text file with no `#!` line: shells run it with `/bin/sh`.
        sandbox.file("script", "echo no-shebang-ran\n", 0o755)?;
        sandbox.file("hashbang", "#!/bin/sh\necho hashbang-ran\n", 0o755)?;
        // A file that is not executable, in the directory and on the PATH.
        sandbox.file("plain", "echo never\n", 0o644)?;
        sandbox.file("bin/plain-in-path", "echo never\n", 0o644)?;
        sandbox.file("bin/tool-in-path", "#!/bin/sh\necho tool-ran\n", 0o755)?;
        // Files for pathname expansion, in `g`. One name is not valid UTF-8.
        fs::create_dir_all(sandbox.dir.join("g/sub"))?;
        fs::create_dir_all(sandbox.dir.join("g/sub-b"))?;
        for name in [
            "a.txt",
            "b.txt",
            "c.rs",
            ".hidden",
            "sp ace",
            "[x]",
            "Z",
            "বাংলা",
            "sub/x",
            "sub-b/y",
        ] {
            File::create(sandbox.dir.join("g").join(name))?;
        }
        File::create(
            sandbox
                .dir
                .join("g")
                .join(OsString::from_vec(b"bad\xffname".to_vec())),
        )?;
        Ok(sandbox)
    }

    fn file(&self, name: &str, text: &str, mode: u32) -> io::Result<()> {
        let path = self.dir.join(name);
        fs::write(&path, text)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode))
    }

    /// Runs `program` in the sandbox with a fixed environment and `input` on standard input.
    fn run(&self, program: &str, input: &[u8]) -> io::Result<Output> {
        let path = format!("{}:/usr/bin:/bin", self.dir.join("bin").display());
        let mut child = Command::new("timeout")
            .arg("10")
            .arg(program)
            .current_dir(&self.dir)
            .env_clear()
            .env("PATH", path)
            .env("LANG", "C")
            .env("HOME", &self.dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            // The program may exit before reading everything (`exit 3`).
            let _ = stdin.write_all(input);
        }
        child.wait_with_output()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Runs `input` in both shells. Returns a description of how they differ, if they do.
fn difference(sandbox: &Sandbox, input: &str) -> io::Result<Option<String>> {
    let ours = sandbox.run(OMSH, input.as_bytes())?;
    let reference = sandbox.run("/bin/sh", input.as_bytes())?;
    let show = |out: &Output| {
        format!(
            "status {:?}, stdout {:?}, stderr {:?}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    };
    let what = if ours.stdout != reference.stdout {
        "stdout differs"
    } else if ours.status.code() != reference.status.code() {
        "status differs"
    } else if ours.stderr.is_empty() != reference.stderr.is_empty() {
        "stderr emptiness differs"
    } else {
        return Ok(None);
    };
    Ok(Some(format!(
        "{what}\ninput {input:?}\n ours: {}\n /bin/sh: {}",
        show(&ours),
        show(&reference)
    )))
}

/// Checks that both shells do the same with `input`.
fn assert_same(sandbox: &Sandbox, input: &str) -> io::Result<()> {
    if let Some(difference) = difference(sandbox, input)? {
        panic!("{difference}");
    }
    Ok(())
}

#[test]
fn simple_commands_behave_like_sh() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    for input in [
        "",
        "true\n",
        "false\n",
        "true\nfalse\n",
        "false\ntrue\n",
        "/bin/echo a 'b c' \"d e\" f\\ g\n",
        "printf '%s\\n' x y\n",
        "printf '%s\\n' 'a  b' \"c\td\"\n",
        "# only a comment\n\n   \n",
        "echo one; echo two\n",
        "echo one;echo two;\n",
        "echo no-newline-at-the-end",
        "echo a\\\nb\n",
        "echo 'two\nlines'\n",
        "echo \"two\nlines\"\n",
        "echo a # not this\n",
        "echo \"quoted # stays\"\n",
        "echo বাংলা\n",
        "echo 'it'\\''s'\n",
        "echo a\"b\"'c'\n",
        "echo ''\n",
        "printf '[%s]\\n' '' x\n",
        "FOO=bar /bin/sh -c 'echo \"$FOO\"'\n",
        "A=1 B=2 /bin/sh -c 'echo \"$A$B\"'\n",
        "A=1 A=2 /bin/sh -c 'echo \"$A\"'\n",
        "A='x y' /bin/sh -c 'echo \"$A\"'\n",
        "/bin/sh -c 'echo \"${FOO-unset}\"'\n",
    ] {
        assert_same(&sandbox, input)?;
    }
    Ok(())
}

#[test]
fn exit_statuses_behave_like_sh() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    for input in [
        "exit 3\n",
        "exit\n",
        "false\nexit\n",
        "exit 259\n",
        "exit 0\necho not-reached\n",
        "echo before; exit 4; echo after\n",
        "/bin/sh -c 'exit 7'\n",
        "/bin/sh -c 'exit 7'\nexit\n",
        "nosuchcmd_omsh\n",
        "nosuchcmd_omsh\ntrue\n",
        "true\nnosuchcmd_omsh\n",
        "./missing\n",
        "/nonexistent/dir/prog\n",
        "./subdir\n",
        "/tmp\n",
        "./plain\n",
        "./plain\ntrue\n",
    ] {
        assert_same(&sandbox, input)?;
    }
    Ok(())
}

#[test]
fn scripts_and_path_lookup_behave_like_sh() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    for input in [
        "./script\n",
        "./hashbang\n",
        "tool-in-path\n",
        "tool-in-path\ntool-in-path\n",
        "./script a b\n",
        "/bin/sh ./script\n",
    ] {
        assert_same(&sandbox, input)?;
    }
    Ok(())
}

#[test]
fn programs_killed_by_a_signal_behave_like_sh() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    for input in [
        "/bin/sh -c 'kill -TERM $$'\n",
        "/bin/sh -c 'kill -KILL $$'\n",
        "/bin/sh -c 'kill -SEGV $$'\n",
        "/bin/sh -c 'kill -HUP $$'\n",
        "/bin/sh -c 'kill -TERM $$'\ntrue\n",
        "/bin/sh -c 'kill -PIPE $$'\n",
    ] {
        assert_same(&sandbox, input)?;
    }
    Ok(())
}

#[test]
fn a_program_killed_by_a_signal_is_named_on_stderr() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"/bin/sh -c 'kill -SEGV $$'\n")?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(139));
    assert!(
        stderr.contains("killed by SIGSEGV (segmentation fault)"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn a_missing_command_is_named_on_stderr() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"nosuchcmd_omsh\n")?;
    assert_eq!(out.status.code(), Some(127));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "oh-my-shell: nosuchcmd_omsh: not found\n"
    );
    Ok(())
}

/// Checks that both shells do the same with each input, and reports every difference at once.
fn assert_all_same(inputs: &[&str]) -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let mut differences = Vec::new();
    for input in inputs {
        differences.extend(difference(&sandbox, input)?);
    }
    assert!(
        differences.is_empty(),
        "{} of {} inputs differ:\n\n{}",
        differences.len(),
        inputs.len(),
        differences.join("\n\n")
    );
    Ok(())
}

// Expansion (S06). In these, `printf '<%s>\n'` shows where each field begins and ends. A
// runtime error such as `${x?msg}` is always on the last line: `/bin/sh` ends a script
// there, while this shell reads on, so the output only agrees up to that point.

#[test]
fn tilde_expansion_behaves_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "echo ~\n",
        "echo ~/x\n",
        "echo ~root\n",
        "echo ~root/x/y\n",
        "echo ~nosuchuser_omsh\n",
        "echo \"~\" '~' \\~ a~\n",
        "echo ~\"/x\"\n",
        "x=1; echo ~$x\n",
        "x=~/a:~/b; echo $x\n",
        "x=a:~; echo $x\n",
        "x=a~; echo $x\n",
        "echo x=~/a\n",
        "HOME=/other; echo ~ ~/z\n",
        "HOME='/a b'; printf '<%s>\\n' ~ ~/z\n",
        "HOME=; printf '<%s>\\n' ~ a\n",
        "echo ${nothing:-~/z}\n",
        "echo \"${nothing:-~/z}\"\n",
    ])
}

#[test]
fn simple_parameters_behave_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "x=hello; echo $x ${x} \"$x\" '$x' \\$x\n",
        "echo $unset_omsh [$unset_omsh]\n",
        "x=a; echo $x$x ${x}b $xb\n",
        "x=a; echo \"$x\"b '$x'b\n",
        "x='a  b'; printf '<%s>\\n' $x \"$x\"\n",
        "x=; printf '<%s>\\n' a $x b \"$x\" ''\n",
        "x=1; y=$x; z=\"$x $y\"; echo $z\n",
        "x=1 y=$x; echo $y\n",
        "x=a y=$x /bin/sh -c 'echo \"$x$y\"'; echo [$x$y]\n",
        "x=1\nx=2 /bin/sh -c 'echo $x'\necho $x\n",
        "x=1\n/bin/sh -c 'echo \"[$x]\"'\n",
        "FOO=1 /bin/sh -c 'echo $FOO'; echo [$FOO]\n",
        "x=a$y\necho \"[$x]\"\n",
        "x='*'; echo \"$x\"\n",
        "echo $1 $2 [$#] $@ $*\n",
        "printf '<%s>\\n' \"$@\" \"$*\" $@ $*\n",
        "printf '<%s>\\n' a\"$@\"b \"a$@b\"\n",
        "echo ${#}\n",
        "echo $-\n",
    ])
}

#[test]
fn parameter_operators_behave_like_sh() -> io::Result<()> {
    let mut inputs = Vec::new();
    // Each operator on a value that is set, empty, and unset.
    for op in [":-", "-", ":+", "+", ":=", "="] {
        for setup in ["v=val", "v=", "unset v"] {
            // Not `unset`: the shell has no such builtin yet. Leaving the variable alone does
            // the same.
            let setup = if setup == "unset v" { "w=1" } else { setup };
            inputs.push(format!(
                "{setup}; printf '<%s>' \"${{v{op}word}}\" ${{v{op}word}}; echo; echo [$v]\n"
            ));
        }
    }
    for op in [":?", "?"] {
        for setup in ["v=val", "v="] {
            inputs.push(format!("{setup}; echo ${{v{op}msg}}\n"));
        }
    }
    let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
    assert_all_same(&refs)?;
    assert_all_same(&[
        "echo ${u:-a b  c}\n",
        "printf '<%s>\\n' ${u:-a b  c} \"${u:-a b  c}\" ${u:-'a b'}\n",
        "x=1; echo ${u:-$x ${x}}\n",
        "echo ${u:-$(echo sub)}\n",
        "echo ${u:-$((1+1))}\n",
        "x=y; echo ${x:+[$x]}\n",
        "echo ${a:=1} $a\n",
        "x=${a:=1}; echo $x $a\n",
        "x=${a=2}; y=${a:=3}; echo $x$y$a\n",
        "echo ${nothing_omsh:?boom}\n",
        "echo ${nothing_omsh?}\n",
        "echo before; echo ${nothing_omsh:?}\n",
        "x=; echo ${x:?}\n",
        "echo ${1:-d} ${#:-d} ${*:-d} ${@:-d}\n",
    ])
}

#[test]
fn trimming_behaves_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "v=abcabc; echo ${v#*b} ${v##*b} ${v%b*} ${v%%b*}\n",
        "v=abcabc; echo ${v#a} ${v#b} ${v%c} ${v%b} ${v#abcabc} ${v%%abcabc}\n",
        "v=abcabc; echo ${v#} ${v##} ${v%} ${v%%}\n",
        "v=abcabc; echo ${v#?} ${v##??} ${v%?} ${v%%??}\n",
        "v=abcabc; echo ${v#*} ${v##*} ${v%*} ${v%%*}\n",
        "v=abcabc; echo ${v#[ab]} ${v#[!a]} ${v#[a-c]*} ${v%[bc]}\n",
        "v=/a/b/c.tar.gz; echo ${v##*/} ${v%/*} ${v%%.*} ${v%.*} ${v#/}\n",
        "v='a*b'; echo ${v#a\\*} ${v#a'*'} ${v#a*}\n",
        "v=abc; p='*b'; echo ${v#$p} ${v#\"$p\"} \"${v#$p}\"\n",
        "v=abc; echo ${unset_omsh#a} [${unset_omsh%a}]\n",
        "v=\"a  b\"; echo ${v#a} \"${v%b}\"\n",
        "v=ab; echo ${v#${v%b}} ${v%${v#a}}\n",
    ])
}

#[test]
fn length_behaves_like_sh_for_ascii() -> io::Result<()> {
    assert_all_same(&[
        "v=hello; echo ${#v}\n",
        "v=; echo ${#v} ${#unset_omsh}\n",
        "v='a b  c'; echo ${#v}\n",
        "echo ${#1}\n",
    ])
}

#[test]
fn the_exit_status_parameter_behaves_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "echo $?\n",
        "false; echo $?\n",
        "false; true; echo $?\n",
        "/bin/sh -c 'exit 7'; echo $?; echo $?\n",
        "nosuchcmd_omsh; echo $?\n",
        "./plain; echo $?\n",
        "x=$(exit 4); echo $?\n",
        "false; x=1; echo $?\n",
        "false; echo $(echo $?)\n",
        "echo $(exit 3) $?\n",
        "false; echo \"$?\" ${?} ${?:-d}\n",
    ])
}

#[test]
fn command_substitution_behaves_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "echo $(echo hi)\n",
        "echo `echo hi`\n",
        "echo a$(echo b)c\n",
        "echo $(echo a  b)  \"$(echo a  b)\"\n",
        "printf '<%s>\\n' $(echo a  b) \"$(echo a  b)\" $(printf 'x\\ny')\n",
        "printf '<%s>\\n' \"$(printf 'x\\n\\n\\n')\" \"$(printf '\\n\\nx')\"\n",
        "echo $(echo $(echo deep))\n",
        "echo `echo \\`echo deep\\``\n",
        "echo \"$(echo \"in  side\")\"\n",
        "echo $(true)x$(true)\n",
        "printf '<%s>\\n' $(true) \"$(true)\"\n",
        "x=$(echo a; echo b); echo \"$x\"\n",
        "x=$(echo one) y=$(echo two); echo $x $y\n",
        "echo $(exit 3; echo not)\n",
        "x=1; echo $(x=2; echo $x) $x\n",
        "echo $(echo a; exit 2) $?\n",
        "x=$(echo $PATH_OMSH_UNSET); echo [$x]\n",
        "echo $(echo '$x' \"'\")\n",
        "echo $(printf 'a\\0b')\n",
        "x=$(printf 'tail  \\n'); echo \"[$x]\"\n",
        "echo $(/bin/sh -c 'echo out; echo err >&2')\n",
        "echo $(/bin/sh -c 'kill -TERM $$'); echo $?\n",
        "x=$(/bin/sh -c 'kill -TERM $$'); echo $?\n",
        "x=$(./script); echo $x $?\n",
        "echo $(nosuchcmd_omsh)$?\n",
        "x=$(head -c 200000 /dev/zero); echo ${#x}\n",
        "printf '%s\\n' \"$(printf 'a\\377b')\" $(printf 'c\\377d')\n",
    ])
}

#[test]
fn arithmetic_behaves_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "echo $((1+2)) $((1 + 2 * 3)) $(( (1+2) * 3 )) $((10-2-3))\n",
        "echo $((7/2)) $((-7/2)) $((7%3)) $((-7%3)) $((7%-3))\n",
        "echo $((1<<4)) $((256>>4)) $((6&3)) $((6|3)) $((6^3)) $((~5))\n",
        "echo $((1<2)) $((2<=2)) $((3>4)) $((4>=5)) $((1==1)) $((1!=1))\n",
        "echo $((1&&0)) $((1&&2)) $((0||0)) $((0||3)) $((!0)) $((!5))\n",
        "echo $((1?10:20)) $((0?10:20)) $((1?2?3:4:5))\n",
        "echo $((0x1f)) $((0X10)) $((010)) $((0)) $((00))\n",
        "echo $((-5)) $((- -5)) $((+5)) $((3 - -2)) $((3--2))\n",
        "x=4; echo $((x*x)) $(($x+1)) $((x<5)) $((y+1)) $(( x ))\n",
        "x=7; echo $((x=x+1)) $x $((x+=2)) $x $((x*=2)) $x $((x/=4)) $x $((x%=3)) $x\n",
        "x=3; echo $((x<<=2)) $((x>>=1)) $((x&=4)) $((x|=1)) $((x^=7)) $x\n",
        "echo $((a=b=3)) $a $b\n",
        "x=' 5 '; y=0x10; z=-3; echo $((x+1)) $((y+1)) $((z+1))\n",
        "x=; echo $((x+1))\n",
        "echo $((0 && (1/0))) $((1 || (1/0))) $((1 ? 5 : (1/0)))\n",
        "echo $((9223372036854775807)) $((9223372036854775807 + 0))\n",
        "echo $((2147483647 * 2147483647))\n",
        "echo \"$((1+1))\"x $((1+1))y\n",
        "n=3; echo $(($n*$n)) $((${n}+1))\n",
        "echo $(($(echo 4)+1))\n",
        "echo a$((1))b\n",
        "echo $((\t1\t+\t1\t))\n",
        "echo $((1/0))\n",
        "echo $((1%0))\n",
        "echo before; echo $((2 +))\n",
        "echo $(())\n",
        "x=abc; echo $((x+1))\n",
        "echo $((08))\n",
        "echo $((1 2))\n",
        "echo $((x++))\n",
        "echo $((2**3))\n",
    ])
}

#[test]
fn field_splitting_behaves_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "x=' a  b '; printf '<%s>\\n' $x\n",
        "x='a\tb\nc'; printf '<%s>\\n' $x\n",
        "x=a:b::c:; IFS=:; printf '<%s>\\n' $x\n",
        "IFS=:; x=:a; printf '<%s>\\n' $x\n",
        "IFS=': '; x='a : b  c'; printf '<%s>\\n' $x\n",
        "IFS=': '; x='a :: b'; printf '<%s>\\n' $x\n",
        "IFS=': '; x=' a:b '; printf '<%s>\\n' $x\n",
        "IFS=; x='a b'; printf '<%s>\\n' $x\n",
        "IFS=x; y=axbxc; printf '<%s>\\n' $y \"$y\"\n",
        "IFS=' '; x='a b'; printf '<%s>\\n' $x\n",
        "x=' a '; printf '<%s>\\n' p$x q${x}r \"p\"$x\"q\"\n",
        "x=' '; printf '<%s>\\n' $x a$x$x b\n",
        "x=' '; printf '<%s>\\n' ''$x \"\"$x $x''\n",
        "x='a b'; printf '<%s>\\n' ${x} ${x:-d} ${u:-$x} \"${u:-$x}\"\n",
        "printf '<%s>\\n' $(echo a b) $((1+1))\n",
        "IFS=:; printf '<%s>\\n' $(echo a:b) $(echo \"c d\")\n",
        "x='a b'; y=$x; printf '<%s>\\n' $y\n",
        "x='a b'; printf '<%s>\\n' \"$x\"'c d'\n",
        "x='a*'; printf '<%s>\\n' $x\n",
    ])
}

#[test]
fn pathname_expansion_behaves_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "printf '<%s>\\n' g/*\n",
        "printf '<%s>\\n' g/*.txt g/?.txt g/[ab].txt g/[!a].txt g/[a-b].*\n",
        "printf '<%s>\\n' g/*.nomatch g/nomatch*\n",
        "printf '<%s>\\n' g/c.* g/*.rs g/Z g/[A-Z]\n",
        "printf '<%s>\\n' g/s* g/sp*\n",
        "printf '<%s>\\n' g/*/x g/*/* g/s*/*\n",
        "printf '<%s>\\n' g/*/\n",
        "printf '<%s>\\n' g/*/ g/s?b/\n",
        "printf '<%s>\\n' */\n",
        "printf '<%s>\\n' g/.h* g/.hidden g/.[h]*\n",
        "printf '<%s>\\n' g/????????\n",
        "printf '<%s>\\n' g/ba?\n",
        "printf '<%s>\\n' g/[[]x] g/[[]*] g/\\[x]\n",
        "printf '<%s>\\n' g/[ g/[]x g/[!]\n",
        "printf '<%s>\\n' g/*txt\n",
        "printf '<%s>\\n' g//*.txt g///a.txt\n",
        "printf '<%s>\\n' ./g/*.txt\n",
        "printf '<%s>\\n' g/../g/*.rs\n",
        "printf '<%s>\\n' \"g/*\" 'g/*' g/\\* g/\"*\"\n",
        "printf '<%s>\\n' g/\"*\".txt g/'a'.t?t g/\"?\".txt\n",
        "x='g/*.rs'; printf '<%s>\\n' $x \"$x\"\n",
        "x='g/*.rs'; y=$x; printf '<%s>\\n' $y ${x}\n",
        "x=g; printf '<%s>\\n' $x/*.txt \"$x\"/*.txt\n",
        "printf '<%s>\\n' $(echo 'g/*.rs')\n",
        "printf '<%s>\\n' \"$(echo 'g/*.rs')\"\n",
        "x=g/*.txt; printf '<%s>\\n' \"$x\" $x\n",
        "printf '%s\\n' g/bad*\n",
        "printf '%s\\n' g/b?dname g/*name g/[a-c]??\\?name\n",
        "x=$(printf '%s ' g/*); printf '<%s>\\n' $x\n",
        "[ -f g/a.txt ]; echo $?; [ -f g/*.rs ]; echo $?\n",
        "echo g/[a\n",
        "echo g/*.txt; echo g/*.nomatch\n",
        "echo *.nomatch* ?nomatch\n",
    ])
}

#[test]
fn quote_removal_behaves_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "echo 'a'\"b\"c\\ d\n",
        "printf '<%s>\\n' \"a\\$b\" \"a\\\\b\" \"a\\\"b\" \"a\\nb\" \"a\\`b\"\n",
        "printf '<%s>\\n' '\\n' '\\\\' 'a\"b'\n",
        "printf '<%s>\\n' a\\\\b a\\ b \\\"q\\\" \\'q\\'\n",
        "echo \"it's\" 'say \"hi\"'\n",
        "echo \"$'a'\"\n",
        "echo \"\\$(echo no)\" \\$(echo no)\n",
        "echo \"a  b\"  'c  d'  e\\ \\ f\n",
        "printf '<%s>\\n' \"\" '' \"\"\"\" a\"\"b\n",
        "printf '<%s>\\n' \"a b\"\"c d\" 'x'y'z'\n",
        "echo \"two\nlines\" 'and\nthese'\n",
        "echo a\\\nb\n",
        "echo \"a\\\nb\"\n",
        "echo \"${u:-'q'}\" ${u:-'q'} \"${u:-\\$}\"\n",
        "echo \"${u:-\"in\"}\"\n",
        "echo ${u:+'a  b'}x\n",
    ])
}

#[test]
fn assignments_behave_like_sh() -> io::Result<()> {
    assert_all_same(&[
        "x=1\necho $x\n",
        "x=1 y=2\necho $x $y\n",
        "x=1; x=2; echo $x\n",
        "x=\"a b\" y='c d'; echo $x$y\n",
        "x=a\\ b; echo \"$x\"\n",
        "x=\"$(echo sub)\"; echo $x\n",
        "x=$((1+2)); echo $x\n",
        "x=*; echo $x\n",
        "x=g/*; echo \"$x\"\n",
        "x=; echo \"[$x]\"\n",
        "x= ; echo \"[$x]\"\n",
        "x=1\nx=2 /bin/sh -c 'echo $x'\necho $x\n",
        "PATH=/nonexistent_omsh\nls\n",
        "x=1 true\necho \"[$x]\"\n",
        "PATH=/usr/bin:/bin; echo ok\n",
        "a=1 b=$a c=$b; echo $a$b$c\n",
        "echo=1; echo $echo\n",
        "_x1=ok; echo $_x1\n",
        "x=1; $x\n",
        "x=true; $x; echo $?\n",
        "x='echo hi'; $x\n",
        "x='echo hi'; \"$x\"\n",
        "x=; $x; echo $?\n",
        "x=; $x echo hi\n",
        "x=echo; $x hi\n",
        "x=1 $y; echo [$x]\n",
    ])
}

#[test]
fn the_expansion_order_behaves_like_sh() -> io::Result<()> {
    assert_all_same(&[
        // Words first, then the assignments in front of the command.
        "x=1 /bin/sh -c 'echo $x' $x\n",
        "x=a; x=b /bin/sh -c 'echo $x'; echo $x\n",
        // Tilde, parameter, substitution and arithmetic each happen once.
        "x='$(echo no)'; echo $x\n",
        "x='$((1+1))'; echo $x\n",
        "x='~'; echo $x\n",
        "x='$y'; y=1; echo $x\n",
        "x='a b'; echo $x*\n",
        "x=g; echo $x/c.*\n",
        "x='*'; echo g/$x.rs\n",
        "echo $(echo '*')x\n",
    ])
}

// From here on: behavior that is meant to differ from `/bin/sh`.

/// A file found through `PATH` that cannot be run gives 126, as POSIX and bash say and as
/// blueprint section 7.1 requires. dash prints "Permission denied" but exits with 127.
#[test]
fn a_non_executable_file_on_the_path_gives_126() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"plain-in-path\n")?;
    assert_eq!(out.status.code(), Some(126));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "oh-my-shell: plain-in-path: permission denied\n"
    );
    Ok(())
}

/// `/bin/sh` ends a script at its first syntax error. An interactive shell reports it and
/// reads on, so one typing mistake does not end the session.
#[test]
fn a_syntax_error_is_reported_and_the_next_line_still_runs() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"fi\necho after\n")?;
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("fi"),
        "the error names the word"
    );
    Ok(())
}

#[test]
fn a_syntax_error_on_the_last_line_gives_status_2() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"echo before\nfi\n")?;
    assert_eq!(String::from_utf8_lossy(&out.stdout), "before\n");
    assert_eq!(out.status.code(), Some(2));
    Ok(())
}

#[test]
fn an_unfinished_line_at_the_end_of_input_is_an_error() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"echo 'never closed\n")?;
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(2));
    assert!(!out.stderr.is_empty());
    Ok(())
}

/// A line that needs a feature that does not exist yet is refused as a whole: the commands
/// before the unsupported one on the same line must not run either.
#[test]
fn a_line_with_something_not_implemented_runs_nothing() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"touch marker; sleep 0 &\n")?;
    assert!(
        !sandbox.dir.join("marker").exists(),
        "the first command ran"
    );
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("background jobs cannot run yet"));
    Ok(())
}

/// Something this shell cannot run yet is refused wherever it is, also inside `$(...)`.
#[test]
fn something_not_implemented_inside_a_substitution_runs_nothing() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"touch marker; echo $(sleep 0 &)\n")?;
    assert!(
        !sandbox.dir.join("marker").exists(),
        "the first command ran"
    );
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("background jobs cannot run yet"));
    Ok(())
}

/// Runs `input` in this shell and returns its standard output.
fn output_of(input: &str) -> io::Result<String> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, input.as_bytes())?;
    assert_eq!(out.status.code(), Some(0), "{input:?}");
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// dash lists `.` and `..` for `.*`. bash 5.2 and POSIX.1-2024 do not, and `chmod -R x .*`
/// should not climb into the parent directory.
#[test]
fn a_pattern_never_matches_dot_and_dot_dot() -> io::Result<()> {
    assert_eq!(output_of("printf '<%s>\\n' g/.*\n")?, "<g/.hidden>\n");
    assert_eq!(
        output_of("cd_unavailable=1; printf '<%s>\\n' .*\n")?,
        "<.*>\n"
    );
    Ok(())
}

/// In `[^a]` dash takes the `^` as a plain character. bash negates the set, which is what
/// people who paste bash snippets expect.
#[test]
fn a_caret_negates_a_bracket_set() -> io::Result<()> {
    assert_eq!(output_of("printf '<%s>\\n' g/[^a].txt\n")?, "<g/b.txt>\n");
    Ok(())
}

/// dash counts bytes in `${#x}`; bash in a UTF-8 locale counts characters.
#[test]
fn length_counts_characters_not_bytes() -> io::Result<()> {
    assert_eq!(output_of("v=বাংলা; echo ${#v}\n")?, "5\n");
    assert_eq!(output_of("v=বাংলা; echo ${v#?} ${v%?}\n")?, "াংলা বাংল\n");
    Ok(())
}

/// `?` matches one character, not one byte.
#[test]
fn a_question_mark_matches_a_whole_character() -> io::Result<()> {
    // The name has five characters and fifteen bytes. It matches five question marks, next to
    // the names that have five ASCII characters, and not fifteen.
    assert_eq!(
        output_of("printf '<%s>\\n' g/?????\n")?,
        "<g/a.txt>\n<g/b.txt>\n<g/sub-b>\n<g/বাংলা>\n"
    );
    assert_eq!(output_of("printf '<%s>\\n' g/ব?ং??\n")?, "<g/বাংলা>\n");
    Ok(())
}

/// `IFS` holds characters, and dash reads it as bytes.
#[test]
fn ifs_characters_can_be_multibyte() -> io::Result<()> {
    assert_eq!(
        output_of("IFS=ব; x=aবb; printf '<%s>\\n' $x\n")?,
        "<a>\n<b>\n"
    );
    Ok(())
}

#[test]
fn special_parameters_that_differ_from_sh_by_design() -> io::Result<()> {
    assert_eq!(output_of("echo $0\n")?, "oh-my-shell\n");
    // `$$` is this shell's own process id: its child sees it as its parent.
    let out = output_of("echo $$; /bin/sh -c 'echo $PPID'\n")?;
    let mut lines = out.lines();
    let (ours, seen) = (lines.next(), lines.next());
    assert!(ours.is_some_and(|l| l.parse::<u32>().is_ok()), "{out:?}");
    assert_eq!(ours, seen);
    Ok(())
}

/// `$'...'` is bash's. dash has no ANSI-C quoting and prints a `$` and the text.
#[test]
fn ansi_c_quoting_works() -> io::Result<()> {
    assert_eq!(
        output_of("printf '<%s>\\n' $'a\\tb' $'\\x41\\101' $'\\u0985' $'it\\'s' \"$'x'\"\n")?,
        "<a\tb>\n<AA>\n<অ>\n<it's>\n<$'x'>\n"
    );
    Ok(())
}

/// dash divides the smallest number by -1 with a CPU instruction that traps, and dies of
/// SIGFPE. This shell wraps around.
#[test]
fn the_smallest_number_divided_by_minus_one_wraps() -> io::Result<()> {
    assert_eq!(
        output_of(
            "echo $(( (-9223372036854775807-1) / -1 )) $(( (-9223372036854775807-1) % -1 ))\n"
        )?,
        "-9223372036854775808 0\n"
    );
    Ok(())
}

/// A failed expansion stops the line it is on, reports the problem, and the shell reads on.
/// A script run by `/bin/sh` would stop. A line is refused for the same reason by the
/// interactive shells that people use.
#[test]
fn an_expansion_error_ends_the_line_and_not_the_session() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(
        OMSH,
        b"echo one; echo ${nothing_omsh:?it is gone}; echo two\necho $((1/0)); echo three\necho four\n",
    )?;
    assert_eq!(String::from_utf8_lossy(&out.stdout), "one\nfour\n");
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nothing_omsh: it is gone"), "{stderr}");
    assert!(stderr.contains("division by zero"), "{stderr}");
    Ok(())
}

/// The status of a line that failed to expand is 2, also at the end of the input.
#[test]
fn an_expansion_error_on_the_last_line_gives_status_2() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"echo ${nothing_omsh:?}\n")?;
    assert_eq!(out.status.code(), Some(2));
    Ok(())
}

/// A program that fails inside `$(...)` writes its complaint to the terminal, not into the
/// text that is substituted.
#[test]
fn what_a_substitution_prints_on_stderr_is_not_captured() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(
        OMSH,
        b"x=$(/bin/sh -c 'echo out; echo err >&2'); echo \"[$x]\"\n",
    )?;
    assert_eq!(String::from_utf8_lossy(&out.stdout), "[out]\n");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "err\n");
    Ok(())
}

/// A line that is not UTF-8 is skipped with a message, and the next one still runs.
#[test]
fn input_that_is_not_utf8_is_skipped() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"echo \xff\necho after\n")?;
    assert_eq!(String::from_utf8_lossy(&out.stdout), "after\n");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not valid UTF-8"));
    Ok(())
}

/// The shell takes exactly one line at a time from its input, so a program that reads the
/// same input continues after that line. `head -n 1` on a file puts the position back to just
/// after the line it printed, which makes the result exact.
#[test]
fn a_program_reads_the_input_that_follows_its_line() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let script = sandbox.dir.join("input.txt");
    fs::write(&script, "head -n 1\nsecond line\necho done\n")?;

    let out = Command::new("timeout")
        .arg("10")
        .arg(OMSH)
        .current_dir(&sandbox.dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .stdin(File::open(&script)?)
        .output()?;

    assert_eq!(String::from_utf8_lossy(&out.stdout), "second line\ndone\n");
    assert_eq!(out.status.code(), Some(0));
    Ok(())
}

// Execution (S07a). The inputs run in a fresh copy of the same directory for each shell, so
// that a file that one of them writes cannot change what the other sees. The directory name is
// replaced by `<D>` in what they print. A runtime error such as `${x?msg}` in the middle of a
// line is avoided where `/bin/sh` and this shell end the line differently.

/// The sandbox of the other tests plus the files that the execution tests read.
fn fixture() -> io::Result<Sandbox> {
    let sandbox = Sandbox::new()?;
    sandbox.file("f.txt", "one\ntwo\nthree\n", 0o644)?;
    sandbox.file("subdir/inner.txt", "inner\n", 0o644)?;
    Ok(sandbox)
}

/// Like [`assert_all_same`], but every input runs in a new copy of the fixture for each shell.
fn assert_all_same_in_fresh_directories(inputs: &[&str]) -> io::Result<()> {
    let mut differences = Vec::new();
    for input in inputs {
        let (mine, theirs) = (fixture()?, fixture()?);
        let ours = mine.run(OMSH, input.as_bytes())?;
        let reference = theirs.run("/bin/sh", input.as_bytes())?;
        let show = |out: &Output, sandbox: &Sandbox| {
            let text = String::from_utf8_lossy(&out.stdout)
                .replace(&sandbox.dir.display().to_string(), "<D>");
            format!(
                "status {:?}, stdout {text:?}, stderr {:?}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr)
            )
        };
        let clean = |out: &Output, sandbox: &Sandbox| {
            String::from_utf8_lossy(&out.stdout).replace(&sandbox.dir.display().to_string(), "<D>")
        };
        let what = if clean(&ours, &mine) != clean(&reference, &theirs) {
            "stdout differs"
        } else if ours.status.code() != reference.status.code() {
            "status differs"
        } else if ours.stderr.is_empty() != reference.stderr.is_empty() {
            "stderr emptiness differs"
        } else {
            continue;
        };
        differences.push(format!(
            "{what}\ninput {input:?}\n ours: {}\n /bin/sh: {}",
            show(&ours, &mine),
            show(&reference, &theirs)
        ));
    }
    assert!(
        differences.is_empty(),
        "{} of {} inputs differ:\n\n{}",
        differences.len(),
        inputs.len(),
        differences.join("\n\n")
    );
    Ok(())
}
#[test]
fn pipelines_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "echo a | cat\n",
        "echo a | cat | tr a b\n",
        "false | true; echo $?\n",
        "true | false; echo $?\n",
        "echo a | exit 3; echo $?\n",
        "yes | head -2\n",
        "yes | head -1 | tr y z\n",
        "echo $(echo a | tr a b)\n",
        "{ echo a; echo b; } | tr a-z A-Z\n",
        "f() { cat; }; echo piped | f\n",
        "while :; do echo y; done | head -2\n",
        "seq 1 50000 | tail -1\n",
        "seq 1 50000 | wc -l\n",
        "echo x | { cat; echo y; }\n",
        "x=1; echo | x=2; echo $x\n",
        "echo a; echo b | cat; echo c\n",
        "echo ${x?boom} | cat; echo after $?\n",
        "for i in 1 2 3; do echo $i; done | wc -l\n",
        "x=$(echo one; echo two | tr t T); echo $x\n",
        "f() { echo made; }; f | tr a-z A-Z\n",
        "echo one | cat | cat | cat | cat | cat\n",
    ])
}

#[test]
fn lists_and_negation_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "! true; echo $?\n",
        "! false; echo $?\n",
        "true && echo yes; false && echo no; echo $?\n",
        "false || echo or; true || echo no2\n",
        "! echo a | grep -q b; echo $?\n",
        "! echo a | grep -q a; echo $?\n",
        "(exit 5) || echo failed $?\n",
        "true && false || echo rescued\n",
        "false || false || echo third\n",
        "echo a; echo b; false; echo $?\n",
        "false && true; echo $?\n",
        "true || false; echo $?\n",
    ])
}

#[test]
fn redirections_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "echo hi > o.txt; cat o.txt\n",
        "echo hi >> o.txt; echo more >> o.txt; cat o.txt\n",
        "cat < f.txt\n",
        "cat 0< f.txt\n",
        "cat < f.txt > g.txt; cat g.txt\n",
        "echo err 2>&1 >/dev/null | cat\n",
        "sh -c 'echo out; echo err >&2' 2>&1 | sort\n",
        "sh -c 'echo out; echo err >&2' >/dev/null 2>&1; echo $?\n",
        "sh -c 'echo o; echo e >&2' >both.txt 2>&1; cat both.txt\n",
        "sh -c 'echo o; echo e >&2' 2>&1 >only.txt | cat; cat only.txt\n",
        "echo hi >&2 2>/dev/null; echo done\n",
        "3>&1 sh -c 'echo three >&3'\n",
        "cat 3<f.txt <&3\n",
        "echo hi 1<>rw.txt; cat rw.txt\n",
        "echo x >| clob.txt; cat clob.txt\n",
        ": > created.txt; ls created.txt\n",
        "> made.txt; ls made.txt\n",
        "x=1 > made.txt; echo $x; ls made.txt\n",
        "echo x > /nonexistent/dir/f; echo $?\n",
        "cat < /nonexistent; echo $?\n",
        "{ echo a; echo b >&2; } 2>&1 | sort\n",
        "{ echo a; echo b; } > g.txt; cat g.txt\n",
        "if true; then echo x; fi > g.txt; cat g.txt\n",
        "f() { echo body; } > g.txt; f; cat g.txt\n",
        "for f in a b; do echo $f; done > g.txt; cat g.txt\n",
        "(echo sub) > g.txt; cat g.txt\n",
        "echo one; echo two 1>&2; echo three\n",
        "n='two words'; echo x > \"$n\"; ls\n",
        "echo z > *.txt; ls\n",
        "x=a; echo hi > $x.txt; ls\n",
        "sh -c 'echo to-three >&3' 3>t.txt; cat t.txt\n",
        "sh -c 'cat <&3' 3<f.txt\n",
        "sh -c 'echo closed >&2' 2>&-; echo $?\n",
        "exec_missing 2>/dev/null; echo $?\n",
        "echo a > one.txt > two.txt; cat one.txt; cat two.txt\n",
    ])
}

#[test]
fn here_documents_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "cat <<EOF\nhello world\nEOF\n",
        "x=5; cat <<EOF\nx is $x and $(echo sub) $((1+2))\nEOF\n",
        "cat <<'EOF'\nno $expand here $(nor this)\nEOF\n",
        "cat <<\"EOF\"\n$x\nEOF\n",
        "cat <<\\EOF\n$x\nEOF\n",
        "cat <<-EOF\n\ttabbed\n\t\ttwo\n\tEOF\n",
        "cat <<EOF | tr a-z A-Z\npiped heredoc\nEOF\n",
        "cat <<EOF >g.txt\nto file\nEOF\ncat g.txt\n",
        "cat 3<<EOF <&3\nfrom fd3\nEOF\n",
        "cat <<A; cat <<B\nfirst\nA\nsecond\nB\n",
        "x=1; cat <<EOF\n\\$x \\\\\\\\ \\` $x\nEOF\n",
        "cat <<EOF\n\"quoted\" 'single'\nEOF\n",
        "f() { cat; }; f <<EOF\nto function\nEOF\n",
        "if true; then cat <<EOF\nin if\nEOF\nfi\n",
        "x=$(cat <<EOF\ninside subst\nEOF\n); echo $x\n",
        "cat <<EOF\nline\n\nafter blank\nEOF\n",
    ])
}

#[test]
fn if_commands_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "if true; then echo t; else echo e; fi\n",
        "if false; then echo t; else echo e; fi\n",
        "if false; then echo t; elif true; then echo elif; else echo e; fi\n",
        "if false; then echo 1; elif false; then echo 2; else echo 3; fi\n",
        "if false; then echo t; fi; echo $?\n",
        "if true; then sh -c 'exit 4'; fi; echo $?\n",
        "if echo cond; false; then echo no; else echo else; fi\n",
        "if true\nthen\n  echo multi\nfi\n",
    ])
}

#[test]
fn loops_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "i=0; while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done\n",
        "i=0; until [ $i -ge 2 ]; do echo u$i; i=$((i+1)); done\n",
        "for i in a b c; do echo $i; done\n",
        "for i in a b c; do if [ $i = b ]; then continue; fi; echo $i; done\n",
        "for i in 1 2 3; do for j in x y; do if [ $j = y ]; then break 2; fi; echo $i$j; done; done\n",
        "for i in 1 2; do for j in a b; do if [ $j = b ]; then continue 2; fi; echo $i$j; done; done\n",
        "for i in *.txt; do echo $i; done\n",
        "set -- p q r; for i; do echo $i; done\n",
        "for i in; do echo $i; done; echo end\n",
        "for i in 1 2 3; do :; done; echo $i\n",
        "for i in 1 2; do echo $i; break; done; echo $?\n",
        "while true; do break; done; echo $?\n",
        "i=0; while [ $i -lt 5 ]; do i=$((i+1)); if [ $i = 3 ]; then continue; fi; echo $i; done\n",
        "for i in 1 2; do for j in 1 2; do break 9; done; echo no; done; echo end\n",
        "for i in 1 2; do sh -c \"exit $i\"; done; echo $?\n",
        "x='a b'; for i in $x; do echo \"[$i]\"; done\n",
        "x='a b'; for i in \"$x\"; do echo \"[$i]\"; done\n",
        "break; echo after-break\n",
        "while false; do :; done; echo $?\n",
    ])
}

#[test]
fn case_commands_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "case foo in f*) echo star;; *) echo other;; esac\n",
        "case bar in f*) echo star;; *) echo other;; esac\n",
        "case x in a|b) echo ab;; x) echo x;; esac\n",
        "case \"a b\" in \"a b\") echo quoted;; esac\n",
        "case x in x) echo first;; x) echo second;; esac\n",
        "case b in [a-c]) echo range;; esac\n",
        "case '*' in '*') echo literal;; esac\n",
        "case abc in '*') echo literal;; *) echo glob;; esac\n",
        "x=hello; case $x in hel*) echo yes;; esac\n",
        "p='h*'; case hello in $p) echo yes;; esac\n",
        "p='h*'; case hello in \"$p\") echo yes;; *) echo no;; esac\n",
        "case x in y) echo y;; esac; echo $?\n",
        "case x in x) sh -c 'exit 4';; esac; echo $?\n",
        "case $(echo x) in x) echo cmdsub;; esac\n",
        "case x in\n  x) echo multi\n     echo line;;\nesac\n",
        "case x in (x) echo paren;; esac\n",
    ])
}

#[test]
fn subshells_and_groups_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "(echo sub; exit 3); echo $?\n",
        "x=1; (x=2); echo $x\n",
        "x=1; { x=2; }; echo $x\n",
        "(cd /; pwd); pwd\n",
        "{ echo a; echo b; } | tr a-z A-Z\n",
        "(echo a; echo b) | tr a-z A-Z\n",
        "f() { :; }; (f() { echo changed; }); f; echo $?\n",
        "(exit 5) ; echo $?\n",
        "( ( echo spaced ) )\n",
        "x=$( (echo in; exit 2) ); echo $x $?\n",
    ])
}

#[test]
fn functions_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "f() { echo \"f:$1:$#\"; }; f a b\n",
        "f() { return 4; }; f; echo $?\n",
        "f() { echo in; return; echo notreached; }; f\n",
        "f() { false; return; }; f; echo $?\n",
        "f() { x=set; }; f; echo $x\n",
        "x=1; f() { echo $x; }; x=2 f; echo $x\n",
        "f() { y=2; }; x=1 f; echo \"$x $y\"\n",
        "f() { g; }; g() { echo g-called; }; f\n",
        "fact() { if [ $1 -le 1 ]; then echo 1; else echo $(( $1 * $(fact $(($1-1))) )); fi; }; fact 5\n",
        "f() { echo \"$@\"; }; f \"a b\" c\n",
        "f() { for x; do echo \"[$x]\"; done; }; f \"a b\" c\n",
        "f() { echo \"$1\"; shift; echo \"$#\"; }; f a b c\n",
        "set -- outer; f() { echo \"$1\"; }; f inner; echo \"$1\"\n",
        "f() { echo one; }; f() { echo two; }; f\n",
        "f() { if true; then return 3; fi; echo no; }; f; echo $?\n",
        "f() { for i in 1 2; do return 5; done; }; f; echo $?\n",
        "f() ( echo subshell body; ); f\n",
        "f() echo oneliner; f\n",
        "cat() { echo mine; }; cat\n",
        "f() { echo f; }; unset -f f; f; echo $?\n",
        "f() { sh -c 'echo $x'; }; x=exported f\n",
        "f() { cat; }; echo in | f | tr a-z A-Z\n",
        "f() { echo $1; }; f $(echo from-subst)\n",
        "f() { x=in-f; echo $(echo $x); }; f\n",
    ])
}

#[test]
fn cd_and_pwd_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "cd subdir; pwd; cd ..; pwd\n",
        "cd /; pwd; cd -\n",
        "(cd /tmp; pwd); pwd\n",
        "x=$(cd /; pwd); echo $x; pwd\n",
        "cd subdir\npwd\ncat ../f.txt | head -1\nls\n",
        "echo $PWD; cd subdir; echo $PWD; echo $OLDPWD\n",
        "cd subdir && cd .. && pwd\n",
        "cd ./subdir/../subdir/.; pwd\n",
        "cd subdir; cd ../subdir; pwd; echo $?\n",
        "cd /; cd ..; pwd\n",
        "cd subdir; echo made > new.txt; cd ..; ls subdir\n",
        "cd subdir; ls; cd ..; ls\n",
        "cd subdir; cat < inner.txt; cd ..\n",
        "mkdir -p lnk_target; ln -s lnk_target lnk; cd lnk; pwd; pwd -P\n",
        "mkdir -p lnk_target; ln -s lnk_target lnk; cd -P lnk; pwd\n",
        "mkdir -p lnk_target; ln -s lnk_target lnk; cd lnk; cd ..; pwd\n",
        "cd \"\"; echo $?\n",
        "cd subdir; echo hi | cat; (cd ..; pwd); pwd\n",
        "pwd -L; pwd -P\n",
        "cd; pwd\n",
    ])
}

#[test]
fn variable_builtins_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "export FOO=bar; sh -c 'echo $FOO'\n",
        "FOO=1; sh -c 'echo ${FOO-unset}'; export FOO; sh -c 'echo ${FOO-unset}'\n",
        "export A; sh -c 'echo ${A-unset}'; A=1; sh -c 'echo ${A-unset}'\n",
        "export B=1; unset B; echo ${B-gone}\n",
        "export B=1; unset B; sh -c 'echo ${B-gone}'\n",
        "A=1 B=2; export A B; sh -c 'echo $A $B'\n",
        "export A=1 B=2; sh -c 'echo $A $B'\n",
        "export X='a b'; sh -c 'echo \"$X\"'\n",
        "x=1; unset x; echo ${x-unset}; echo $?\n",
        "unset nothing_here; echo $?\n",
        "set -- a b c; shift; echo $@; shift 2; echo $#\n",
        "set -- a b c; echo $#; set --; echo $#\n",
        "set a b; echo $# $1\n",
        "set -- 'a b' c; for i in \"$@\"; do echo \"[$i]\"; done\n",
        "set -- a b c; shift 0; echo $#\n",
        "set -- -x; echo $1\n",
        "x=1 :; echo $x\n",
        "x=1 true; echo ${x-unset}\n",
        "x=1 eval 'echo $x'; echo $x\n",
        "x=1; x=2 sh -c 'echo $x'; echo $x\n",
        "export P=1; P=2 sh -c 'echo $P'; sh -c 'echo $P'\n",
        "export FOO=bar\nsh -c 'echo $FOO'\n",
        "A=1\nexport A\nsh -c 'echo $A'\n",
        "export A=1\nunset A\nsh -c 'echo ${A-gone}'\n",
        "f() { echo fn; }\nf\nf\n",
        "x=1\n(x=2)\necho $x\n",
        "set -- a b\nshift\necho $1\n",
        "cd subdir\nexport HERE=$PWD\ncd ..\nsh -c 'echo $HERE'\npwd\n",
    ])
}

#[test]
fn eval_and_dot_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "eval 'echo evaled'; eval \"x=1; echo \\$x\"\n",
        "eval echo a b\n",
        "eval 'echo a' 'echo b'\n",
        "eval; echo $?\n",
        "eval ''; echo $?\n",
        "x='echo from x'; eval $x\n",
        "eval 'exit 5'; echo not\n",
        "eval 'a=1; echo $a'; echo $a\n",
        "echo 'echo sourced; y=9' > s.sh; . ./s.sh; echo $y\n",
        "echo 'return 5; echo no' > r.sh; . ./r.sh; echo $?\n",
        "printf 'f() { echo from-lib; }\\n' > lib.sh; . ./lib.sh; f\n",
        "printf 'echo on-path\\n' > onpath.sh; PATH=.:$PATH . onpath.sh\n",
        "echo 'cd subdir' > c.sh; . ./c.sh; pwd\n",
        "eval 'f() { echo defined; }'; f\n",
        "eval 'for i in 1 2; do echo $i; done'\n",
    ])
}

#[test]
fn simple_builtins_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        ":; echo $?\n",
        "true; echo $?\n",
        "false; echo $?\n",
        ": a b c; echo $?\n",
        "echo a b   c\n",
        "echo -n x; echo y\n",
        "echo\n",
        "echo \"\"\n",
        "echo a > /dev/null; echo $?\n",
        "exit 3\n",
        "false; exit\n",
        "(exit 5); echo $?\n",
        "x=$(exit 3); echo $?\n",
        "echo one; exit 2; echo not\n",
    ])
}

/// `read` (S07b). A loop reads from a here-document, a file and a pipe.
#[test]
fn read_behaves_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "printf 'a b\\nc d\\n' | while read x y; do echo \"[$x][$y]\"; done\n",
        "while read l; do echo \"<$l>\"; done <<'E'\none two\n  three  \nE\n",
        "while read -r l; do printf '<%s>\\n' \"$l\"; done <<'E'\none\\two\nthree\nE\n",
        "while read l; do printf '<%s>\\n' \"$l\"; done <<'E'\none\\two\nthree\nE\n",
        "while read l; do echo \"[$l]\"; done < f.txt\n",
        "IFS=: read a b <<'E'\nx:y::\nE\necho \"[$a][$b]\"\n",
        "IFS=: read a b <<'E'\nx:y:\nE\necho \"[$a][$b]\"\n",
        "IFS=: read a b c <<'E'\n:x::\nE\necho \"[$a][$b][$c]\"\n",
        "IFS=' :' read a b <<'E'\n a : b c \nE\necho \"[$a][$b]\"\n",
        "printf 'no newline' | { read v; echo \"st=$? [$v]\"; }\n",
        "printf 'a\\\\\\nb\\n' | { read v; echo \"[$v]\"; }\n",
        "printf 'a\\\\ b c\\n' | { read x y; echo \"[$x][$y]\"; }\n",
        "{ read a; cat; } <<'E'\nfirst\nsecond\nthird\nE\n",
        "{ read a; read b; echo \"$a-$b\"; } <<'E'\nfirst\nsecond\nE\n",
        "read x < /dev/null; echo \"st=$? [$x]\"\n",
        "read a b c <<'E'\nonly\nE\necho \"[$a][$b][$c]\"\n",
        "read a <<'E'\n  lead and trail  \nE\necho \"[$a]\"\n",
        "IFS= read a <<'E'\n  keep  \nE\necho \"[$a]\"\n",
        "n=0; while read l; do n=$((n+1)); done < f.txt; echo $n\n",
        "cat f.txt | while read l; do echo \"$l\"; done | wc -l\n",
        "read 1x </dev/null; echo st=$?\n",
    ])
}

/// `test` and `[` (S07b): each operator, and the rules by number of arguments.
#[test]
fn test_behaves_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "test ; echo \"test:$?\"\n[  ]; echo \"bracket:$?\"\n",
        "test x; echo \"test:$?\"\n[ x ]; echo \"bracket:$?\"\n",
        "test -n x; echo \"test:$?\"\n[ -n x ]; echo \"bracket:$?\"\n",
        "test -z x; echo \"test:$?\"\n[ -z x ]; echo \"bracket:$?\"\n",
        "test -n ''; echo \"test:$?\"\n[ -n '' ]; echo \"bracket:$?\"\n",
        "test -z ''; echo \"test:$?\"\n[ -z '' ]; echo \"bracket:$?\"\n",
        "test ! x; echo \"test:$?\"\n[ ! x ]; echo \"bracket:$?\"\n",
        "test ! ''; echo \"test:$?\"\n[ ! '' ]; echo \"bracket:$?\"\n",
        "test a = a; echo \"test:$?\"\n[ a = a ]; echo \"bracket:$?\"\n",
        "test a = b; echo \"test:$?\"\n[ a = b ]; echo \"bracket:$?\"\n",
        "test a != b; echo \"test:$?\"\n[ a != b ]; echo \"bracket:$?\"\n",
        "test a '<' b; echo \"test:$?\"\n[ a '<' b ]; echo \"bracket:$?\"\n",
        "test b '>' a; echo \"test:$?\"\n[ b '>' a ]; echo \"bracket:$?\"\n",
        "test 10 -gt 9; echo \"test:$?\"\n[ 10 -gt 9 ]; echo \"bracket:$?\"\n",
        "test 10 '>' 9; echo \"test:$?\"\n[ 10 '>' 9 ]; echo \"bracket:$?\"\n",
        "test 1 -eq 1; echo \"test:$?\"\n[ 1 -eq 1 ]; echo \"bracket:$?\"\n",
        "test 1 -ne 1; echo \"test:$?\"\n[ 1 -ne 1 ]; echo \"bracket:$?\"\n",
        "test 1 -lt 2; echo \"test:$?\"\n[ 1 -lt 2 ]; echo \"bracket:$?\"\n",
        "test 1 -le 1; echo \"test:$?\"\n[ 1 -le 1 ]; echo \"bracket:$?\"\n",
        "test 2 -ge 3; echo \"test:$?\"\n[ 2 -ge 3 ]; echo \"bracket:$?\"\n",
        "test -1 -lt 0; echo \"test:$?\"\n[ -1 -lt 0 ]; echo \"bracket:$?\"\n",
        "test ' 7' -eq '7 '; echo \"test:$?\"\n[ ' 7' -eq '7 ' ]; echo \"bracket:$?\"\n",
        "test -d .; echo \"test:$?\"\n[ -d . ]; echo \"bracket:$?\"\n",
        "test -d f.txt; echo \"test:$?\"\n[ -d f.txt ]; echo \"bracket:$?\"\n",
        "test -f f.txt; echo \"test:$?\"\n[ -f f.txt ]; echo \"bracket:$?\"\n",
        "test -f subdir; echo \"test:$?\"\n[ -f subdir ]; echo \"bracket:$?\"\n",
        "test -e f.txt; echo \"test:$?\"\n[ -e f.txt ]; echo \"bracket:$?\"\n",
        "test -e nosuch; echo \"test:$?\"\n[ -e nosuch ]; echo \"bracket:$?\"\n",
        "test -s f.txt; echo \"test:$?\"\n[ -s f.txt ]; echo \"bracket:$?\"\n",
        "test -s /dev/null; echo \"test:$?\"\n[ -s /dev/null ]; echo \"bracket:$?\"\n",
        "test -r f.txt; echo \"test:$?\"\n[ -r f.txt ]; echo \"bracket:$?\"\n",
        "test -w f.txt; echo \"test:$?\"\n[ -w f.txt ]; echo \"bracket:$?\"\n",
        "test -x script; echo \"test:$?\"\n[ -x script ]; echo \"bracket:$?\"\n",
        "test -x f.txt; echo \"test:$?\"\n[ -x f.txt ]; echo \"bracket:$?\"\n",
        "test -L f.txt; echo \"test:$?\"\n[ -L f.txt ]; echo \"bracket:$?\"\n",
        "test -h f.txt; echo \"test:$?\"\n[ -h f.txt ]; echo \"bracket:$?\"\n",
        "test -p f.txt; echo \"test:$?\"\n[ -p f.txt ]; echo \"bracket:$?\"\n",
        "test -c /dev/null; echo \"test:$?\"\n[ -c /dev/null ]; echo \"bracket:$?\"\n",
        "test -b /dev/null; echo \"test:$?\"\n[ -b /dev/null ]; echo \"bracket:$?\"\n",
        "test -S f.txt; echo \"test:$?\"\n[ -S f.txt ]; echo \"bracket:$?\"\n",
        "test -O f.txt; echo \"test:$?\"\n[ -O f.txt ]; echo \"bracket:$?\"\n",
        "test -G f.txt; echo \"test:$?\"\n[ -G f.txt ]; echo \"bracket:$?\"\n",
        "test -t 99; echo \"test:$?\"\n[ -t 99 ]; echo \"bracket:$?\"\n",
        "test f.txt -ef f.txt; echo \"test:$?\"\n[ f.txt -ef f.txt ]; echo \"bracket:$?\"\n",
        "test f.txt -ef ./f.txt; echo \"test:$?\"\n[ f.txt -ef ./f.txt ]; echo \"bracket:$?\"\n",
        "test a = a -a b = b; echo \"test:$?\"\n[ a = a -a b = b ]; echo \"bracket:$?\"\n",
        "test a = b -o b = b; echo \"test:$?\"\n[ a = b -o b = b ]; echo \"bracket:$?\"\n",
        "test a = b -a b = b; echo \"test:$?\"\n[ a = b -a b = b ]; echo \"bracket:$?\"\n",
        "test ! a = b; echo \"test:$?\"\n[ ! a = b ]; echo \"bracket:$?\"\n",
        "test ! a = a; echo \"test:$?\"\n[ ! a = a ]; echo \"bracket:$?\"\n",
        "test '(' a = a ')'; echo \"test:$?\"\n[ '(' a = a ')' ]; echo \"bracket:$?\"\n",
        "test ! '(' a = b ')'; echo \"test:$?\"\n[ ! '(' a = b ')' ]; echo \"bracket:$?\"\n",
        "test '(' a = b ')' -o c = c; echo \"test:$?\"\n[ '(' a = b ')' -o c = c ]; echo \"bracket:$?\"\n",
        "test -n x -a -n y; echo \"test:$?\"\n[ -n x -a -n y ]; echo \"bracket:$?\"\n",
        "test -z x -o -z y; echo \"test:$?\"\n[ -z x -o -z y ]; echo \"bracket:$?\"\n",
        "test -d . -a -f f.txt; echo \"test:$?\"\n[ -d . -a -f f.txt ]; echo \"bracket:$?\"\n",
        "test ! -d nosuch; echo \"test:$?\"\n[ ! -d nosuch ]; echo \"bracket:$?\"\n",
        "test ! -z ''; echo \"test:$?\"\n[ ! -z '' ]; echo \"bracket:$?\"\n",
        "test '!' = '!'; echo \"test:$?\"\n[ '!' = '!' ]; echo \"bracket:$?\"\n",
        "test -n = -n; echo \"test:$?\"\n[ -n = -n ]; echo \"bracket:$?\"\n",
        "test -f = -f; echo \"test:$?\"\n[ -f = -f ]; echo \"bracket:$?\"\n",
        "test x -a y; echo \"test:$?\"\n[ x -a y ]; echo \"bracket:$?\"\n",
        "[ a = a; echo st=$?\n",
        "[ 1 -eq x ]; echo st=$?\n",
        "[ a = a ] && echo yes || echo no\n",
        "if [ -f f.txt ] && [ ! -d f.txt ]; then echo file; fi\n",
        "i=0; while [ $i -lt 3 ]; do echo $i; i=$((i+1)); done\n",
        "x=; [ -z \"$x\" ] && echo empty\n",
        "x='a b'; [ \"$x\" = 'a b' ] && echo same\n",
        "[ ]; echo $?\n",
        "[ '' ]; echo $?\n",
        "test; echo $?\n",
    ])
}

/// `printf` (S07b): conversions, flags, escapes, reuse of the format, and bad numbers.
#[test]
fn printf_behaves_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "printf '[\\c]\\n'\n",
        "printf '\\x41|A|\\e|\\q|\\101|\\0101|\\1234|\\8|\\\"|\\'\"'\"'\\n'\n",
        "printf '%b|' 'a\\cb' 'x\\0101y' 'p\\101q' '\\x41'; echo\n",
        "printf '[%c][%c][%5c]\\n' abc '' z\n",
        "printf '[%s][%5s][%-5s][%.2s][%05s][%5.1s]\\n' abc abc abc abc abc abc\n",
        "printf '[%d][%i][%5d][%-5d][%05d][%+d][% d][%.3d][%5.3d][%x][%#x][%#o][%X][%u]\\n' 42 42 42 42 42 42 42 7 7 255 255 8 255 -1\n",
        "printf '%d %d %d\\n' 0x1f 010 '\"a'\n",
        "printf '%d\\n' 12abc; echo st=$?\n",
        "printf '%d\\n' ' 12' ''; echo st=$?\n",
        "printf '%d\\n' 99999999999999999999; echo st=$?\n",
        "printf '%f|%.2f|%e|%E|%g|%G|%10.3f|%-10.1f|%+f|%010.2f\\n' 3.14159 3.14159 31415.9 0.00031 100000 1e-10 2.5 2.5 1 -2.5\n",
        "printf '%g %g %g %g %g %#g\\n' 0 100000 1000000 0.0001 0.00001 1\n",
        "printf '%f %f %s\\n' inf nan x\n",
        "printf '%s %s\\n' a\n",
        "printf '%*d|%-*d|%.*f\\n' 5 1 5 2 2 3.14159\n",
        "printf '%5%|\\n'; echo st=$?\n",
        "printf 'a%sb%sc\\n' 1 2 3 4 5\n",
        "printf '%.0f %.0f %.0f %.0f\\n' 0.5 1.5 2.5 3.5\n",
        "printf '%x %o %u\\n' -1 -1 -1\n",
        "printf '%.3d|%.0d|%5.0d|%x|%#X|%#x\\n' 5 0 0 0 255 0\n",
        "printf '%e %e\\n' 0 123456789\n",
        "printf '%G %g %g\\n' 0.000012345 123456789 1.5\n",
        "printf '%10.4e|%-12.2E|\\n' 3.14159 31415.9\n",
        "printf '%.10g %.3g %g\\n' 3.14159265358979 1234.5678 -0\n",
        "printf '%f\\n' 0x10 ' 2.5' .5 5. 1e2 -.5e1\n",
        "printf '%d\\n' 1.5; echo st=$?\n",
        "printf '%d %s\\n' 1 2 3\n",
        "printf 'ab%z\\n' 1; echo st=$?\n",
        "printf '%s\\n' 'a\\nb'\n",
        "printf '%b\\n' 'a\\\\b' '\\q' '\\n'\n",
        "printf '\\0\\n' | od -c\n",
        "printf '%b' 'a\\0b' | od -c\n",
        "printf '%%|%5s|%-3d|\\n' x 7\n",
        "printf '%s\\n' a b c\n",
        "printf '%s %s\\n' a b c\n",
        "printf '%d%%\\n' 50\n",
        "printf '[%10s][%-10s]\\n' right left\n",
        "printf '%c%c%c\\n' a b c\n",
        "printf '%05.1f|%+.2e|%G\\n' 2.55 12345.678 0.0000123\n",
        "printf '%o %x %X %u\\n' 64 255 255 3\n",
        "printf '%s\\n' \"$(printf 'x\\ny')\"\n",
        "printf -- '--\\n'\n",
        "printf; echo st=$?\n",
    ])
}

/// Aliases (S07b): they are replaced in the text of a line, so a definition works from the next line on.
#[test]
fn aliases_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "alias e=echo\ne hi\n",
        "alias e=echo; e hi\n",
        "alias e='echo a;'\ne b\n",
        "alias ll='ls -d'\nll /tmp\n",
        "alias ls='ls -d'\nls /tmp\n",
        "alias a=b b=a\nb 2>/dev/null; echo $?\n",
        "alias e=echo\n\\e hi 2>/dev/null; echo $?\n'e' hi 2>/dev/null; echo $?\n",
        "alias e=echo\necho e\n",
        "alias e=echo\nif e x; then e y; fi\n",
        "alias e=echo\nf() { e in-func; }\nf\n",
        "alias e=echo\nx=$(e sub); echo $x\n",
        "alias e=echo\n( e sub )\n{ e grp; }\n",
        "alias e=echo\ne a | cat\n",
        "alias s='e '\nalias e=echo x=XX\ns x\n",
        "alias s='echo '\nalias x=XX\ns x\n",
        "alias s='echo'\nalias x=XX\ns x\n",
        "alias e=echo\nunalias e\ne hi 2>/dev/null; echo $?\n",
        "alias e=echo f='echo f'\nalias\n",
        "alias e=echo\nalias e\nalias nosuch 2>/dev/null; echo $?\n",
        "alias e='echo it'\"'\"'s'\nalias e\n",
        "unalias nosuch 2>/dev/null; echo $?\n",
        "alias e=echo\neval 'e from-eval'\n",
        "alias e=echo\ne=1\necho $e\n",
        "alias e=echo\nfor i in 1 2; do e $i; done\n",
        "alias e=echo\n e   spaced\n",
        "alias if=echo\nif true; then echo ok; fi\n",
        "alias and='true &&'\nand echo yes\n",
        "alias e=echo\ncase x in x) e case;; esac\n",
        "alias e=echo\nX=1 e hi\n",
        "alias e=echo\n>/dev/null e hi\n",
        "alias e=echo\nalias | wc -l\n",
        "alias ll=echo\n$(:)ll hi\nx\\ll hi\n\"x\"ll hi\n",
        "printf 'alias e=echo\\ne from-file\\n' > a.sh\n. ./a.sh\ne after\n",
        "printf 'echo one\\nfi\\necho three\\n' > bad.sh\n. ./bad.sh\n",
        "printf 'f() { echo defined; }\\nf\\nif true; then\\n  f\\nfi\\n' > ok.sh\n. ./ok.sh\nf\n",
        "alias e=echo\nprintf 'e in-dot\\n' > b.sh\n. ./b.sh\n",
        "printf 'alias e=echo\\ne same-file\\nunalias e\\ne gone 2>/dev/null; echo $?\\n' > c.sh\n. ./c.sh\n",
    ])
}

/// `set` options (S07b): `-e` in every place it does and does not apply, `-u`, `-f`, `-C`, `-x`, `-o`, `$-`.
#[test]
fn set_options_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "set -e\nfalse\necho not\n",
        "set -e\ntrue\necho yes\n",
        "set -e\nfalse || echo handled\necho after\n",
        "set -e\nfalse && echo no\necho after\n",
        "set -e\ntrue && false\necho not\n",
        "set -e\n! true\necho after\n",
        "set -e\nif false; then :; fi\necho after\n",
        "set -e\nif false; then :; else echo e; fi\necho after\n",
        "set -e\nwhile false; do :; done\necho after\n",
        "set -e\nuntil true; do :; done\necho after\n",
        "set -e\nf() { false; echo in-f; }\nf\necho not\n",
        "set -e\nf() { false; echo in-f; }\nf || echo handled\necho after\n",
        "set -e\nf() { false; echo in-f; }\nif f; then echo then; fi\necho after\n",
        "set -e\n(false; echo sub)\necho not\n",
        "set -e\n(false) || echo handled\necho after\n",
        "set -e\n{ false; echo grp; }\necho not\n",
        "set -e\nfalse | true\necho after\n",
        "set -e\ntrue | false\necho not\n",
        "set -e\nx=$(false)\necho not\n",
        "set -e\nx=$(false; echo hi)\necho after $x\n",
        "set -e\nfor i in 1 2; do false; echo $i; done\necho not\n",
        "set -e\nfor i in 1 2; do echo $i; done\necho after\n",
        "set -e\ncase x in x) false; echo c;; esac\necho not\n",
        "set -e\nset +e\nfalse\necho after\n",
        "set -e\nfalse; echo x\n",
        "set -e\nexit 3\n",
        "set -e\nfalse\n",
        "set -e\nf() { return 4; }\nf\necho not\n",
        "set -ex\nfalse\n",
        "set -e\neval false\necho not\n",
        "set -e\neval 'false; echo in-eval'\necho not\n",
        "set -e\nf() { false && true; echo ok; }\nf\n",
        "set -e\nfalse &&  true || true\necho ok\n",
        "set -e\n! false\necho ok\n",
        "set -e\nx=1 false\necho not\n",
        "set -u\necho ${x-d} ${x:-d} ${x+d} ${x:+d}\necho $@ $* $# $?\necho after\n",
        "set -u\ny=\necho [$y]\n",
        "set -u\n: ${z=1}\necho $z\n",
        "set -eu\necho $x\n",
        "set -u\necho $1\n",
        "set -u\nx=1\necho $x ${#x}\necho ${#y}\n",
        "set -u\necho ${y#a}\n",
        "set -f\necho /bin/s*\necho *\nset +f\necho /bi[n]\n",
        "set -f\nx='*'\necho $x\n",
        "set -C\necho a > cl.txt\necho b > cl.txt 2>/dev/null; echo st=$?\necho c >| cl.txt\ncat cl.txt\n",
        "set -C\necho a > /dev/null; echo st=$?\n",
        "set -C\necho a >> /dev/null; echo st=$?\n",
        "set -x\necho hi\nx=1\nset +x\necho no-trace\n",
        "set -eu\necho $-\nset +eu\necho $-\nset -C -f\necho $-\n",
        "set -o errexit\necho $-\nset +o errexit\necho $-\n",
        "set -o noglob -o nounset\necho $-\n",
        "set -o | head -3\nset -o errexit\nset -o | head -2\n",
        "set -e -- a b\necho $1 $2 $#\n",
        "set a b c\nset -e\necho $#\n",
        "set -- -x y\necho $1 $2\n",
        "set -eo nounset\necho $-\n",
        "set +e\n",
        "set +o | head -5\n",
        "set -- a b\nset -\necho $#\n",
    ])
}

/// `set -o pipefail` is not compared with `/bin/sh`: dash only has it since Debian's 0.5.12-7
/// (Ubuntu 24.04, which CI runs on, ships an older build that says "Illegal option -o pipefail").
/// The expected output is what bash prints.
#[test]
fn pipefail_reports_the_last_failing_stage() -> io::Result<()> {
    assert_eq!(
        output_of(
            "set -o pipefail\nfalse | true; echo $?\ntrue | false | true; echo $?\n\
             /bin/sh -c 'exit 3' | true; echo $?\n/bin/sh -c 'exit 3' | false | true; echo $?\n"
        )?,
        "1\n1\n3\n1\n"
    );
    assert_eq!(
        output_of("set -o pipefail\nset +o pipefail\nfalse | true; echo $?\n")?,
        "0\n"
    );
    assert_eq!(
        output_of("set -o pipefail\nset -o | grep pipefail\nset +o | grep pipefail\n")?,
        "pipefail        on\nset -o pipefail\n"
    );
    Ok(())
}

/// With `-e`, a failing stage ends the shell when `pipefail` is on.
#[test]
fn errexit_with_pipefail_ends_the_shell() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"set -eo pipefail\nfalse | true\necho not\n")?;
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(1));
    Ok(())
}

/// `test f -nt g` and `-ot` with a missing file follow POSIX.1-2024 and bash: an existing file
/// is newer than a missing one. They are not compared with `/bin/sh`, because dash changed this
/// in Debian's 0.5.12-10 and older builds (Ubuntu 24.04, used by CI) answer 1.
#[test]
fn newer_and_older_than_follow_posix_2024_for_missing_files() -> io::Result<()> {
    let out = output_of(
        "touch f.txt\ntouch -d 2001-01-01 old.txt\n\
         test f.txt -nt nosuch; echo $?\n[ nosuch -ot f.txt ]; echo $?\n\
         test nosuch -nt f.txt; echo $?\ntest f.txt -ot nosuch; echo $?\n\
         test nosuch -nt nosuch2; echo $?\n\
         test f.txt -nt old.txt; echo $?\ntest old.txt -nt f.txt; echo $?\n\
         test old.txt -ot f.txt; echo $?\n",
    )?;
    assert_eq!(out, "0\n0\n1\n1\n1\n0\n1\n0\n");
    Ok(())
}

/// `type`, `command` and `hash` (S07b). The "tracked alias" wording of dash is avoided here: see the direct test.
#[test]
fn type_command_and_hash_behave_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "type cd\n",
        "type export\n",
        "type if\n",
        "type nosuch 2>/dev/null; echo $?\n",
        "type nosuch cd 2>/dev/null; echo $?\n",
        "f() { :; }\ntype f\n",
        "alias a=b\ntype a\n",
        "type echo printf test '['\n",
        "command -v cd\ncommand -v if\ncommand -v nosuch; echo $?\n",
        "f() { :; }\ncommand -v f\ncommand -V f\n",
        "alias a='b c'\ncommand -v a\ncommand -V a\n",
        "command -V cd\ncommand -V export\ncommand -V if\n",
        "command -V nosuch 2>/dev/null; echo $?\n",
        "command -v /bin/sh\n",
        "command -v\ncommand; echo $?\ncommand -p; echo $?\n",
        "command echo hi\ncommand -- echo hi\n",
        "f() { echo func; }\ncommand f 2>/dev/null; echo $?\nf\n",
        "echo() { printf 'fn\\n'; }\necho a\ncommand echo b\n",
        "command exit 3\n",
        "command nosuch 2>/dev/null; echo $?\n",
        "command -p echo hi\n",
        "PATH=/nonexistent\ncommand -p ls /bin >/dev/null; echo $?\n",
        "x=1 command true; echo [$x]\n",
        "x=1 command export y=2; echo [$x] [$y]\n",
        "hash -r\nhash nosuch 2>/dev/null; echo $?\nhash ls cat\nhash\nhash -r\nhash\n",
        "hash cd; echo $?\n",
        "command -pv ls\n",
    ])
}

/// `umask` (S07b), also in subshells, which must not change the umask of the shell or of each other.
#[test]
fn umask_behaves_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "umask 022\numask\numask -S\n",
        "umask 027; umask; umask -S\n",
        "umask 027\numask u=rwx,g=rx,o=; umask\numask g+w; umask -S\numask a=; umask\n",
        "umask 0777; umask -S\numask 0; umask -S\n",
        "umask -x 2>/dev/null; echo $?\n",
        "umask 22\n(umask 077; umask)\numask\n",
        "umask 22\nx=$(umask 077; umask); echo $x; umask\n",
        "umask 22\numask 077 | cat; umask\n",
        "umask 077\n: > u1\nls -l u1 | cut -c1-10\n",
        "umask 000\n: > u2\n: > u3\nls -l u2 u3 | cut -c1-10\n",
        "umask 022\n(umask 077; : > u4; echo a > u4a)\n: > u5\nls -l u4 u4a u5 | cut -c1-10\n",
        "umask 022\n(umask 000; : > u6)\n( umask 077; sh -c ': > u7' )\nls -l u6 u7 | cut -c1-10\n",
        "umask 077\n(umask 000; umask 000 ; sh -c 'umask')\nsh -c umask\n",
        "umask 022\n: > u8\n(umask 077; : >> u8; : > u8)\nls -l u8 | cut -c1-10\n",
        "umask 027\n(umask 000; umask)\n( : > u9 ) \nls -l u9 | cut -c1-10\n",
        "umask 999 2>/dev/null; echo $?\n",
    ])
}

/// `exec` (S07b): replacing the shell, and redirections that last from one command to the next.
#[test]
fn exec_behaves_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "exec echo hi\necho not\n",
        "exec sh -c 'exit 3'\n",
        "exec sh -c 'echo $0 $1' a b\n",
        "x=1 exec sh -c 'echo [$x]'\n",
        "(exec echo sub)\necho after\n",
        "(exec sh -c 'exit 4'); echo $?\n",
        "echo hi | exec cat\n",
        "exec true\necho not\n",
        "exec 3>out1\necho one >&3\necho two >&3\nexec 3>&-\ncat out1\n",
        "exec >out2\necho captured\necho more\nexec >&2\ncat out2\n",
        "exec 2>out3\nls /nonexistent-omsh\nexec 2>&1\ncat out3 | wc -l\n",
        "exec 3<<E\nline1\nline2\nE\nread a <&3\nread b <&3\necho $a $b\n",
        "echo data > out4\nexec 4<out4\nread x <&4\necho got $x\nexec 4<&-\n",
        "{ exec 3>out5; }\necho in-group >&3\ncat out5\n",
        "f() { exec 3>out6; }\nf\necho in-f >&3\ncat out6\n",
        "(exec 3>out7; echo sub >&3)\necho outer >&3 2>/dev/null; echo st=$?\ncat out7\n",
        "exec 3>out8 && echo ok >&3\ncat out8\n",
        "exec 3>out9\nsh -c 'echo child >&3'\ncat out9\n",
        "exec 3>out10\nexec 3>&-\necho x >&3 2>/dev/null; echo st=$?\n",
        "exec 3>out11\nx=$(echo hi >&3; echo sub)\necho $x\ncat out11\n",
        "exec 3>out12\necho a | { cat >&3; }\ncat out12\n",
        "exec 5>out13\nwhile read l; do echo $l >&5; done <<E\nA\nB\nE\ncat out13\n",
        "exec\necho still\n",
        "exec 2>/dev/null\nls /nonexistent-omsh\necho st=$?\n",
        "exec >/dev/null\necho hidden\n",
        "exec 3>out14\n(echo in-sub >&3)\ncat out14\n",
    ])
}

/// `trap` (S07b): the `EXIT` trap, signals sent to the shell itself, ignored signals, and traps in subshells.
#[test]
fn trap_behaves_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "trap 'echo bye' EXIT\necho hi\n",
        "trap 'echo bye' EXIT\nexit 3\n",
        "trap 'echo bye' 0\necho hi\n",
        "trap 'echo bye; exit 5' EXIT\necho hi\n",
        "trap 'echo bye' EXIT\ntrap - EXIT\necho hi\n",
        "trap 'echo status=$?' EXIT\nfalse\n",
        "trap 'echo status=$?' EXIT\nexit 7\n",
        "trap 'echo first' EXIT\ntrap 'echo second' EXIT\n",
        "trap 'echo bye' EXIT\ntrap\n",
        "trap '' INT\ntrap\n",
        "trap 'echo a b' INT TERM\ntrap\n",
        "trap 'echo got' USR1\nkill -USR1 $$\necho after\n",
        "trap 'echo got' USR1\nkill -USR1 $$\necho after\nkill -USR1 $$\necho again\n",
        "trap 'echo got $?' USR1\nfalse\nkill -USR1 $$\necho after $?\n",
        "trap 'echo hup' HUP\nkill -HUP $$\n",
        "trap 'echo term' TERM\nkill -TERM $$\necho after\n",
        "trap '' TERM\nkill -TERM $$\necho survived\n",
        "trap '' USR1\nsh -c 'kill -USR1 $PPID; echo child-done'\necho survived\n",
        "trap 'echo caught' USR2\nsh -c 'kill -USR2 $PPID'\necho after\n",
        "trap 'echo x' USR1\ntrap - USR1\ntrap\n",
        "trap 'echo x' USR1\ntrap USR1\ntrap\n",
        "trap 'echo x' 10\ntrap\n",
        "(trap 'echo sub-exit' EXIT; echo in-sub)\necho after\n",
        "trap 'echo outer' EXIT\n(echo in-sub)\necho after\n",
        "x=$(trap 'echo from-sub' EXIT; echo val); echo [$x]\n",
        "trap 'echo A' EXIT\ntrap 'echo got' USR1\nkill -USR1 $$\n",
        "f() { trap 'echo in-f' EXIT; }\nf\necho end\n",
        "trap 'echo bye' EXIT\nexec true\n",
        "trap 'echo caught' INT\nkill -INT $$\necho after\n",
        "trap 'kill -USR1 $$' USR2\ntrap 'echo nested' USR1\nkill -USR2 $$\necho end\n",
        "trap 'echo 1' USR1\nfor i in 1 2 3; do kill -USR1 $$; echo loop $i; done\n",
        "trap 'exit 4' USR1\nkill -USR1 $$\necho not\n",
        "trap 'echo x' NOSUCH 2>/dev/null; echo st=$?\n",
    ])
}

/// `readonly` (S07b). A violation is always on the last line: dash ends a script there.
#[test]
fn readonly_behaves_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "readonly R=1\nreadonly -p | grep '^readonly R='\n",
        "readonly R=1 S='a b'\nreadonly | grep -E '^readonly [RS]='\n",
        "readonly R\nreadonly -p | grep -c '^readonly R$'\necho [${R-unset}]\n",
        "readonly R=1\nR=2\n",
        "readonly R=1\nR=2 true\n",
        "readonly R=1\nunset R\n",
        "readonly R=1\nexport R=3\n",
        "readonly R=1\nexport R\nsh -c 'echo [$R]'\n",
        "readonly R=1\nfor R in a b; do echo $R; done\n",
        "readonly R=1\nread R </dev/null\n",
        "readonly R=1\n: ${R:=2}\necho [$R]\n",
        "readonly R=1\necho $((R=2))\n",
        "readonly R=1\nf() { R=2; }\nf\n",
        "readonly R=1\n(R=2)\necho [$R]\n",
        "readonly R=1\nreadonly R=1\n",
        "readonly R=1\nreadonly R=2\n",
        "readonly R=1\necho $R\nreadonly R\necho $R\n",
        "readonly 1x=3\n",
        "x=5\nreadonly x\necho $x\n",
        "readonly R=1\neval 'echo [$R]'\n",
        "readonly R=1\nsh -c 'echo [$R]'\n",
    ])
}

/// `CDPATH` (S07b): a directory found through it is shown, one found through the empty entry is not.
#[test]
fn cdpath_behaves_like_sh() -> io::Result<()> {
    assert_all_same_in_fresh_directories(&[
        "mkdir -p cda/sub cdb/sub cdc 'cdd e/x'\nCDPATH=cda:cdb\ncd sub\npwd\n",
        "mkdir -p cda/sub cdc\nCDPATH=:cda\ncd cdc\npwd\n",
        "mkdir -p cda cdc\nCDPATH=cda\ncd ./cdc\npwd\n",
        "mkdir -p cda cdc\nCDPATH=cda\ncd cdc\npwd\n",
        "mkdir -p cda/sub\nCDPATH=.:cda\ncd sub\npwd\n",
        "mkdir -p cdc\nCDPATH=.\ncd cdc\npwd\n",
        "mkdir -p cda/sub\nCDPATH=cda/\ncd sub\npwd\n",
        "mkdir -p 'cdd e/x'\nCDPATH='cdd e'\ncd x\npwd\n",
        "mkdir -p cda/sub\nCDPATH=cda\n(cd sub; pwd)\npwd\n",
        "mkdir -p cda/sub\nCDPATH=cda\ncd sub\ncd -\npwd\n",
        "mkdir -p cda/sub\nCDPATH=cda\nx=$(cd sub); echo [$x]\n",
    ])
}

// Differences from `/bin/sh` that are on purpose in S07b. Each has a row in the Decision Log.

/// `read` with no name keeps the line in `REPLY`, as in bash. dash needs a name.
#[test]
fn read_without_a_name_sets_reply() -> io::Result<()> {
    assert_eq!(
        output_of("echo 'a b' | { read; echo \"[$REPLY]\"; }\n")?,
        "[a b]\n"
    );
    Ok(())
}

/// `type` says "is /usr/bin/ls" where dash says "is a tracked alias for /usr/bin/ls": there is
/// no tracked alias here, only the path cache.
#[test]
fn type_does_not_talk_about_tracked_aliases() -> io::Result<()> {
    let out = output_of("hash -r; command -v ls >/dev/null; type ls\n")?;
    assert!(out.starts_with("ls is /"), "{out}");
    assert!(!out.contains("tracked"), "{out}");
    Ok(())
}

/// `trap` accepts a name with the `SIG` prefix, as bash does. dash calls it a bad trap.
#[test]
fn trap_accepts_the_sig_prefix() -> io::Result<()> {
    assert_eq!(
        output_of("trap 'echo got' SIGUSR1\nkill -USR1 $$\necho after\n")?,
        "got\nafter\n"
    );
    Ok(())
}

/// Mistakes that end a script do not end the session: an unset variable under `-u`, an
/// assignment to a read only variable, an unknown option of `set`, a program that `exec` cannot
/// find. The line ends, the status is the same as dash's, and the next line runs.
#[test]
fn the_new_mistakes_that_end_a_script_do_not_end_the_session() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    for input in [
        "set -u\necho $nope\necho next\n",
        "readonly R=1\nR=2\necho next\n",
        "set -z\necho next\n",
        "exec nosuch-omsh\necho next\n",
        "trap 'echo x' KILL\necho next\n",
    ] {
        let out = sandbox.run(OMSH, input.as_bytes())?;
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "next\n",
            "{input:?}: the line after the mistake must run"
        );
        assert!(
            !out.stderr.is_empty(),
            "{input:?}: the mistake must be reported"
        );
    }
    Ok(())
}

/// `set` refuses options this shell does not have, instead of quietly ignoring them.
#[test]
fn set_refuses_options_that_do_not_exist_yet() -> io::Result<()> {
    assert_eq!(output_of("set -m 2>/dev/null; echo $?\n")?, "2\n");
    assert_eq!(output_of("set -o monitor 2>/dev/null; echo $?\n")?, "2\n");
    assert_eq!(output_of("set -o nosuch 2>/dev/null; echo $?\n")?, "2\n");
    Ok(())
}

/// `SHLVL` goes up by one and `OMSH_VERSION` is exported. `SHELL` is left alone.
#[test]
fn shlvl_and_the_version_are_exported() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(
        OMSH,
        b"echo \"$SHLVL $OMSH_VERSION\"\nsh -c 'echo \"$SHLVL $OMSH_VERSION\"'\necho \"[$SHELL]\"\n",
    )?;
    let version = env!("CARGO_PKG_VERSION");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("1 {version}\n1 {version}\n[]\n")
    );
    Ok(())
}

/// A copy of the shell keeps its own `umask` and passes it on. The shell itself keeps the
/// mask it had.
#[test]
fn a_subshell_umask_does_not_leak() -> io::Result<()> {
    assert_eq!(
        output_of("umask 022; (umask 077; sh -c umask); umask; sh -c umask\n")?,
        "0077\n0022\n0022\n"
    );
    Ok(())
}

/// Changes made by `exec` to the files last from one line to the next in an interactive session.
#[test]
fn exec_redirections_last_across_lines() -> io::Result<()> {
    assert_eq!(
        output_of("exec 3>fd3.txt\necho a >&3\necho b >&3\nexec 3>&-\ncat fd3.txt\n")?,
        "a\nb\n"
    );
    Ok(())
}

// Differences from `/bin/sh` that are on purpose. Each has a row in the Decision Log.

/// `echo` follows bash: escape sequences only with `-e`. dash always reads them.
#[test]
fn echo_does_not_read_escapes_unless_asked() -> io::Result<()> {
    assert_eq!(output_of("echo 'a\\nb'\n")?, "a\\nb\n");
    assert_eq!(output_of("echo -e 'a\\tb'\n")?, "a\tb\n");
    assert_eq!(output_of("echo -n a; echo b\n")?, "ab\n");
    Ok(())
}

/// `&>`, `&>>` and `<<<` are bash forms that dash does not have.
#[test]
fn the_bash_redirections_work() -> io::Result<()> {
    let out = output_of(
        "sh -c 'echo o; echo e >&2' &> all.txt\n\
         sh -c 'echo o2; echo e2 >&2' &>> all.txt\n\
         cat all.txt\n\
         cat <<< 'a here string'\n",
    )?;
    assert_eq!(out, "o\ne\no2\ne2\na here string\n");
    Ok(())
}

/// `. file args` sets `$1`, `$2`, ... while the file runs, as in bash. dash ignores the
/// arguments.
#[test]
fn dot_passes_its_arguments_to_the_file() -> io::Result<()> {
    let out =
        output_of("printf 'echo \"$# $1\"\\n' > a.sh\nset -- outer\n. ./a.sh p q\necho $1\n")?;
    assert_eq!(out, "2 p\nouter\n");
    Ok(())
}

/// `return` outside a function is an error that does not end the session (dash ends the
/// script). A mistake in a redirection of a special builtin does not end it either.
#[test]
fn mistakes_that_end_a_script_do_not_end_the_session() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(
        OMSH,
        b"return 3\necho after-return\n: < missing\necho after-redirect\n",
    )?;
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "after-return\nafter-redirect\n"
    );
    assert_eq!(out.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("return"), "{stderr}");
    assert!(stderr.contains("missing"), "{stderr}");
    Ok(())
}

/// A status is only the low eight bits: `return 300` is 44, as in bash and POSIX. dash prints
/// 300.
#[test]
fn a_status_has_eight_bits() -> io::Result<()> {
    assert_eq!(output_of("f() { return 300; }; f; echo $?\n")?, "44\n");
    Ok(())
}

/// A failed `cd` gives 1, as in bash. dash gives 2.
#[test]
fn a_failed_cd_gives_1() -> io::Result<()> {
    assert_eq!(
        output_of("cd /nonexistent-omsh 2>/dev/null; echo $?\n")?,
        "1\n"
    );
    Ok(())
}

/// A program receives the descriptors of its table and nothing else, whatever pipes, here
/// documents and substitutions the shell has open around it.
///
/// A descriptor that the test environment itself hands down without close-on-exec (the CI
/// runner passes two, 142 and 145) reaches every program, as it does in bash and dash: a
/// shell does not close what it inherited. So the test asks `/bin/sh` which numbers above 2
/// the environment provides and does not count those as strays.
#[test]
fn a_program_inherits_no_stray_descriptors() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let list = |program: &str, input: &str| -> io::Result<Vec<String>> {
        let out = sandbox.run(program, input.as_bytes())?;
        assert_eq!(out.status.code(), Some(0), "{input:?}");
        Ok(String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_owned)
            .collect())
    };
    let inherited: Vec<String> = list("/bin/sh", "ls /proc/$$/fd\n")?
        .into_iter()
        .filter(|fd| !matches!(fd.as_str(), "0" | "1" | "2"))
        .collect();
    let seen = list(
        OMSH,
        "sh -c 'ls /proc/$$/fd'\n\
         echo x | sh -c 'ls /proc/$$/fd'\n\
         sh -c 'ls /proc/$$/fd' <<EOF\nbody\nEOF\n\
         y=$(sh -c 'ls /proc/$$/fd'); echo $y\n\
         sh -c 'ls /proc/$$/fd' 3>/dev/null\n",
    )?;
    let strays: Vec<&str> = seen
        .iter()
        .map(String::as_str)
        .filter(|fd| !inherited.iter().any(|known| known == fd))
        .collect();
    assert_eq!(
        strays,
        [
            "0", "1", "2", "0", "1", "2", "0", "1", "2", "0", "1", "2", "0", "1", "2", "3"
        ],
        "all: {seen:?}, provided by the environment: {inherited:?}"
    );
    Ok(())
}

/// The commands of a line that cannot run yet (a background job) are refused as a whole
/// also when they are nested in a compound command or a function.
#[test]
fn a_background_job_inside_a_compound_command_runs_nothing() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"touch marker; if true; then sleep 0 & fi\n")?;
    assert!(!sandbox.dir.join("marker").exists());
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("background jobs cannot run yet"));
    Ok(())
}

/// Runs `shell` with standard input from `input` and the given signals ignored, as under
/// `nohup` or a background `&`. A signal that is ignored when a program starts stays ignored.
fn run_with_ignored(shell: &str, ignored: &str, input: &str) -> io::Result<Output> {
    let script = format!("trap '' {ignored}; exec \"$0\"");
    let mut child = Command::new("/bin/sh")
        .args(["-c", &script, shell])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    child.wait_with_output()
}

/// A signal that was ignored when the shell started stays ignored: the shell cannot trap it
/// and the programs it starts inherit the setting (S08). The shell must not reset the signals
/// it knows about for its programs without looking at this.
#[test]
fn signals_ignored_on_entry_stay_ignored() -> io::Result<()> {
    for (ignored, input) in [
        ("HUP", "trap 'echo caught' HUP\nkill -HUP $$\necho after\n"),
        ("HUP", "sh -c 'kill -HUP $$; echo alive'\n"),
        ("TERM", "sh -c 'kill -TERM $$; echo alive'\n"),
        (
            "INT QUIT",
            "sh -c 'kill -INT $$; echo alive-int'\nsh -c 'kill -QUIT $$; echo alive-quit'\n",
        ),
        (
            "USR1",
            "trap 'echo caught' USR1\nkill -USR1 $$\necho after\n",
        ),
        // Not ignored on entry: the signal is still trapped, and the program still dies.
        (
            "HUP",
            "trap 'echo caught' USR1\nkill -USR1 $$\nsh -c 'kill -TERM $$; echo not-alive'\necho end\n",
        ),
    ] {
        let ours = run_with_ignored(OMSH, ignored, input)?;
        let reference = run_with_ignored("/bin/sh", ignored, input)?;
        assert_eq!(
            String::from_utf8_lossy(&ours.stdout),
            String::from_utf8_lossy(&reference.stdout),
            "stdout differs, ignored {ignored}, input {input:?}"
        );
        assert_eq!(
            ours.status.code(),
            reference.status.code(),
            "status differs, ignored {ignored}, input {input:?}"
        );
    }
    Ok(())
}
