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
    let out = sandbox.run(OMSH, b"touch marker; echo a | cat\n")?;
    assert!(
        !sandbox.dir.join("marker").exists(),
        "the first command ran"
    );
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pipelines cannot run yet"));
    Ok(())
}

/// Something this shell cannot run yet is refused wherever it is, also inside `$(...)`.
#[test]
fn something_not_implemented_inside_a_substitution_runs_nothing() -> io::Result<()> {
    let sandbox = Sandbox::new()?;
    let out = sandbox.run(OMSH, b"touch marker; echo $(echo a | cat)\n")?;
    assert!(
        !sandbox.dir.join("marker").exists(),
        "the first command ran"
    );
    assert!(out.stdout.is_empty());
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("pipelines cannot run yet"));
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
