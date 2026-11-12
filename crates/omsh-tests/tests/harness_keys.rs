//! The harness must not send a key in the same read as the key before it, nor while the editor
//! is drawing (S26). The real shell is the one under test here, because the loss is the line
//! editor's: it reads a batch of keys and, when one of them ends the read (Ctrl+C), the rest of
//! the batch is thrown away.

use std::io;
use std::time::Duration;

use omsh_tests::{SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");
const ROUNDS: usize = 8;

/// Half-typed text, Ctrl+C, and a command in the very next call. Without the gap that the
/// harness keeps after Ctrl+C, the command reaches the editor in the same read and is lost.
fn typed_then_interrupted_then_command(term: &mut Session, n: usize) -> io::Result<bool> {
    term.write_bytes(b"abc")?;
    term.expect("abc", SHORT);
    term.key(3)?;
    term.line(&format!("echo kept{n}-$((1+1))"))?;
    let kept = term.try_expect(&format!("kept{n}-2"), Duration::from_secs(2));
    if kept {
        term.prompt();
    }
    Ok(kept)
}

#[test]
fn the_line_after_ctrl_c_is_not_lost() {
    pty_test(BIN, |ctx| {
        let mut term = ctx.start()?;
        for n in 0..ROUNDS {
            assert!(
                typed_then_interrupted_then_command(&mut term, n)?,
                "round {n}: the line sent right after Ctrl+C never ran:\n{}",
                term.transcript()
            );
        }
        Ok(())
    });
}

#[test]
fn cancel_line_returns_only_when_the_new_prompt_is_drawn() {
    pty_test(BIN, |ctx| {
        for round in 0..ROUNDS {
            let mut term = ctx.start()?;
            term.write_bytes(b"echo 'a b' ; ls | cat && ls > out")?;
            term.expect("out", SHORT);
            let mark = term.transcript().len();
            // The typed line has been drawn, and every redraw of it holds a "$ ". After this
            // call the editor must really be back at an empty prompt: `exit` is read and obeyed.
            term.cancel_line()?;
            // On return: the editor left its read, and its new paint is complete (cursor shown).
            let after = &term.transcript()[mark..];
            let left = after
                .find("\x1b[?2004l")
                .expect("the editor never left its read");
            let hide = after.rfind("\x1b[?25l").expect("no new paint");
            let show = after.rfind("\x1b[?25h").expect("paint not finished");
            assert!(left < hide && hide < show, "round {round}:\n{after:?}");
            term.line("exit")?;
            let status = term.wait_exit(SHORT);
            assert!(status.success(), "round {round}: {status:?}");
        }
        Ok(())
    });
}

#[test]
fn a_key_sent_as_soon_as_the_first_prompt_shows_is_kept() {
    pty_test(BIN, |ctx| {
        for n in 0..ROUNDS {
            let mut term = ctx.start()?;
            term.line(&format!("echo first{n}-$((1+1))"))?;
            term.expect(&format!("first{n}-2"), SHORT);
            term.quit()?;
        }
        Ok(())
    });
}
