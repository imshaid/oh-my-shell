//! Process groups, signals and the terminal (S08) on a real terminal.
//!
//! These replace the hand-run pseudo-terminal script of S08.

use std::time::Duration;

use nix::sys::signal::{Signal, kill};
use nix::sys::termios::{LocalFlags, Termios};
use omsh_tests::procfs::{foreground_group, ignored_signals, process_group};
use omsh_tests::{SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_C: u8 = 3;
const CTRL_D: u8 = 4;
const CTRL_BACKSLASH: u8 = 0x1c;

fn bit(signal: Signal) -> u64 {
    1 << (signal as i32 - 1)
}

/// Types `command`, lets it start, presses Ctrl+C and checks that the shell is back, with
/// status 130.
fn interrupt(term: &mut Session, command: &str) -> std::io::Result<()> {
    term.line(command)?;
    term.pump(Duration::from_millis(400));
    term.key(CTRL_C)?;
    term.line("echo status-$?")?;
    term.expect("status-130", SHORT);
    Ok(())
}

/// Ctrl+C ends the job in front and what belongs to it: a program, every stage of a pipeline, a
/// loop of the shell's own and a `$(...)` that is still running. The shell stays.
#[test]
fn ctrl_c_ends_the_foreground_job_in_every_shape() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        interrupt(&mut term, "sleep 30")?;
        term.expect_none("sleep");

        interrupt(&mut term, "sleep 30 | sleep 31 | sleep 32")?;
        term.expect_none("sleep");

        interrupt(&mut term, "while :; do sleep 1; done")?;
        term.expect_none("sleep");

        // A loop that starts no program: only the shell can stop it.
        interrupt(&mut term, "while :; do :; done")?;

        // The command substitution is cut short, and the command around it never runs.
        interrupt(&mut term, "echo a-$(sleep 30)-b")?;
        term.expect_none("sleep");
        assert!(
            !term.transcript().contains("a--b"),
            "the command around $(...) ran\n{}",
            term.transcript()
        );

        // The shell is still the same one, with its jobs in order.
        term.sync("alive")?;
        term.quit()
    });
}

/// Ctrl+C at an empty prompt does not end the shell.
#[test]
fn ctrl_c_at_the_prompt_does_not_end_the_shell() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.key(CTRL_C)?;
        term.pump(Duration::from_millis(200));
        assert!(!term.has_exited()?, "{}", term.transcript());
        term.sync("alive")?;
        term.quit()
    });
}

/// The shell leads a process group of its own, a job gets another one, and the terminal belongs
/// to the job while it runs and to the shell at the prompt. The shell runs under `sh`, so it is
/// not the leader of its session: the harder case.
#[test]
fn the_shell_and_each_job_have_their_own_group_and_the_terminal_follows() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.launch().under_sh().start()?;
        let shell = term.shell_pid();
        assert_ne!(
            shell,
            term.parent_pid(),
            "the shell should be a child of sh"
        );
        let shell_group = process_group(shell);
        assert_eq!(shell_group, shell as i32, "the shell leads its own group");
        assert_ne!(shell_group, process_group(term.parent_pid()));
        assert_eq!(term.foreground_pgrp()?, shell_group);

        term.line("sleep 30")?;
        term.pump(Duration::from_millis(400));
        let sleep = term.in_session("sleep");
        assert_eq!(sleep.len(), 1, "{}", term.transcript());
        let job_group = process_group(sleep[0].as_raw() as u32);
        assert_ne!(job_group, shell_group);
        assert_eq!(term.foreground_pgrp()?, job_group);
        assert_eq!(foreground_group(shell), job_group);

        term.key(CTRL_C)?;
        term.sync("back")?;
        assert_eq!(term.foreground_pgrp()?, shell_group);

        // The shell hands the terminal back before it ends, so that its parent can read.
        // (`exit` without a number gives the status of the last command: the `echo` of `sync`.)
        term.line("exit")?;
        term.expect("parent-saw-0", SHORT);
        assert_eq!(term.foreground_pgrp()?, process_group(term.parent_pid()));
        term.line("go")?;
        term.expect("parent-read-ok", SHORT);
        Ok(())
    });
}

/// Starts `sleep <seconds>`, waits until it runs, and returns the terminal's settings while it
/// does. At the prompt the line editor holds the terminal in raw mode, so the settings the
/// shell keeps between commands can only be seen while a command runs. Then it kills the
/// `sleep` and waits for the prompt.
fn settings_while_a_command_runs(term: &mut Session, seconds: u32) -> std::io::Result<Termios> {
    term.line(&format!("sleep {seconds}"))?;
    let end = std::time::Instant::now() + SHORT;
    while term.in_session("sleep").is_empty() {
        assert!(std::time::Instant::now() < end, "{}", term.transcript());
        term.pump(Duration::from_millis(20));
    }
    let settings = term.termios()?;
    for pid in term.in_session("sleep") {
        kill(pid, Signal::SIGKILL)?;
    }
    term.sync(&format!("after-sleep-{seconds}"))?;
    Ok(settings)
}

/// A full-screen program turns off line editing and echo. If it is killed it cannot put them
/// back, so the shell does.
///
/// The settings are read while a plain `sleep` runs, before and after the damage. If the shell
/// did not restore them, the line editor would save the damaged settings when it enters raw
/// mode and put them back before the next command, and the second reading would show them.
#[test]
fn the_terminal_settings_come_back_after_a_killed_full_screen_program() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        let before = settings_while_a_command_runs(&mut term, 31)?;
        assert!(
            before
                .local_flags
                .contains(LocalFlags::ICANON | LocalFlags::ECHO),
            "{}",
            term.transcript()
        );

        term.line("sh -c 'stty raw -echo; exec sleep 32'")?;
        // `sleep` replaces the `sh` after `stty` ran, so once it exists the damage is done.
        // (The prompt is no proof: the line editor keeps the terminal raw there.)
        let end = std::time::Instant::now() + SHORT;
        while term.in_session("sleep").is_empty() {
            assert!(std::time::Instant::now() < end, "{}", term.transcript());
            term.pump(Duration::from_millis(20));
        }
        let damaged = term.termios()?;
        assert!(
            !damaged
                .local_flags
                .contains(LocalFlags::ICANON | LocalFlags::ECHO),
            "{}",
            term.transcript()
        );
        let sleep = term.in_session("sleep");
        assert_eq!(sleep.len(), 1, "{}", term.transcript());
        kill(sleep[0], Signal::SIGKILL)?;
        term.sync("restored")?;

        let after = settings_while_a_command_runs(&mut term, 33)?;
        assert_eq!(after, before, "{}", term.transcript());
        term.quit()
    });
}

/// A program that ends normally leaves the settings it chose: `stty -echo` still works. (The
/// line editor puts back the settings it found when it leaves raw mode, so this also checks
/// that it does not undo `stty` with an older copy.)
#[test]
fn settings_chosen_by_a_program_that_ends_normally_are_kept() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("stty -echo")?;
        term.sync("quiet")?;
        let quiet = settings_while_a_command_runs(&mut term, 34)?;
        assert!(!quiet.local_flags.contains(LocalFlags::ECHO));
        term.line("stty echo")?;
        term.sync("loud")?;
        let loud = settings_while_a_command_runs(&mut term, 35)?;
        assert!(loud.local_flags.contains(LocalFlags::ECHO));
        term.quit()
    });
}

/// The program in front reads the keyboard, in raw mode, and the shell does not take the key.
#[test]
fn a_program_in_front_reads_single_keys_in_raw_mode() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        term.line("sh -c 'stty raw -echo; k=$(head -c 1); stty sane; echo key-$k'")?;
        term.pump(Duration::from_millis(500));
        // One key and no line ending: only raw mode delivers it.
        term.key(b'q')?;
        term.expect("key-q", SHORT);
        term.sync("after")?;
        term.quit()
    });
}

/// The shell ignores the signals that would stop or end it, and the programs it starts get the
/// defaults back.
#[test]
fn the_shell_ignores_term_quit_and_tstp_but_its_programs_do_not() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        let shell = term.shell_pid();
        let ignored_by_shell = [
            Signal::SIGTERM,
            Signal::SIGQUIT,
            Signal::SIGTSTP,
            Signal::SIGTTIN,
            Signal::SIGTTOU,
        ];
        let shell_mask = ignored_signals(shell);
        for signal in ignored_by_shell {
            assert_ne!(
                shell_mask & bit(signal),
                0,
                "the shell does not ignore {signal}"
            );
        }
        // The shell does not ignore SIGINT: it catches it.
        assert_eq!(shell_mask & bit(Signal::SIGINT), 0);

        term.line("kill -TERM $$")?;
        term.key(CTRL_BACKSLASH)?;
        term.sync("survived")?;
        assert!(!term.has_exited()?);

        term.line("sleep 30 &")?;
        term.expect("[1] ", SHORT);
        let sleep = term.in_session("sleep");
        assert_eq!(sleep.len(), 1);
        // The signals the shell ignores for itself must not leak into the programs it starts.
        let program_mask = ignored_signals(sleep[0].as_raw() as u32);
        for signal in ignored_by_shell {
            assert_eq!(
                program_mask & bit(signal),
                0,
                "a program inherited the ignored {signal}"
            );
        }
        kill(sleep[0], Signal::SIGKILL)?;

        // And a program really dies of SIGTERM.
        term.line("sh -c 'kill -TERM $$'; echo status-$?")?;
        term.expect("status-143", SHORT);
        term.quit()
    });
}

/// Starts the shell under `sh`, which ends only after reading a line, and checks that the
/// terminal belongs to the parent's group when the shell is gone.
fn leaves_the_terminal_to_the_parent(
    steps: impl FnOnce(&mut Session) -> std::io::Result<()> + Send + 'static,
    parent_saw: &'static str,
) {
    pty_test(BIN, move |ctx| {
        let mut term = ctx.launch().under_sh().start()?;
        let parent = term.parent_pid();
        steps(&mut term)?;
        term.expect(parent_saw, SHORT);
        // The parent is alive (it waits for a line) and the terminal is in its group.
        assert_eq!(term.foreground_pgrp()?, process_group(parent));
        term.line("go")?;
        // If the terminal had been left to a group that is gone, this read would have been
        // stopped by SIGTTIN and the line below would never come.
        term.expect("parent-read-ok", SHORT);
        let status = term.wait_exit(SHORT);
        assert_eq!(status.code(), Some(0));
        Ok(())
    });
}

#[test]
fn exit_gives_the_terminal_back_to_the_parent() {
    leaves_the_terminal_to_the_parent(|term| term.line("exit 3"), "parent-saw-3");
}

#[test]
fn end_of_input_gives_the_terminal_back_to_the_parent() {
    leaves_the_terminal_to_the_parent(|term| term.key(CTRL_D), "parent-saw-0");
}

/// `exec` replaces the shell with a program. The program must be able to read the keyboard, and
/// the parent must get the terminal back when it ends.
#[test]
fn exec_gives_the_program_and_then_the_parent_the_terminal() {
    leaves_the_terminal_to_the_parent(
        |term| {
            term.line("exec sh -c 'read x; echo r-$x'")?;
            term.pump(Duration::from_millis(300));
            term.line("typed")?;
            term.expect("r-typed", SHORT);
            Ok(())
        },
        "parent-saw-0",
    );
}
