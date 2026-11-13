//! `?? text`, `??` / `Alt+?` after a failure, and `cmd | ? question` through the real shell on a
//! terminal (S24).
//!
//! The "model" is a fake server on this machine (`omsh_tests::fakehttp`). Nothing here asks it for
//! a plan: it answers with plain text, which the shell writes to standard output.
//!
//! A typed line is echoed by the terminal, so a test looks for what only the shell prints.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use omsh_tests::fakehttp::Server;
use omsh_tests::{Ctx, Launch, SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_C: u8 = 3;
/// `Alt+?` as a terminal sends it: Escape, then the character.
const ALT_QUESTION: &[u8] = b"\x1b?";

fn configure(ctx: &Ctx, server: &Server, more: &str) -> io::Result<()> {
    let dir: PathBuf = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("config.toml"),
        format!(
            "{more}\n[ai]\nprovider = \"openai-compatible\"\nmodel = \"model-one\"\nbase_url = \"{}\"\n\n\
             [safety]\nblast_radius_max_ms = 60000\n",
            server.url("/v1")
        ),
    )
}

fn launch(ctx: &Ctx) -> Launch {
    ctx.launch()
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/omsh-test-bus",
        )
        .cwd(ctx.home())
}

fn chat_requests(server: &Server) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .filter(|r| r.path().ends_with("/chat/completions"))
        .map(|r| r.body)
        .collect()
}

fn ghp() -> String {
    format!("ghp_{}", "a1B2".repeat(9))
}

#[test]
fn a_double_question_mark_explains_and_runs_nothing() {
    pty_test(BIN, |ctx| {
        let marker = ctx.home().join("marker");
        let server = Server::start_answering(Duration::ZERO, false, &["It makes an empty file."])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;

        term.command("?? touch marker")?;
        term.expect("It makes an empty file.", SHORT);
        term.prompt();
        assert!(!marker.exists(), "?? must never run the command");
        let requests = chat_requests(&server);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("Explain this:"), "{}", requests[0]);
        assert!(requests[0].contains("touch marker"), "{}", requests[0]);
        // No plan was asked for.
        assert!(!requests[0].contains("ONE JSON object"), "{}", requests[0]);
        // The answer was a success.
        term.command("echo st-$?-end")?;
        term.expect("st-0-end", SHORT);
        // The request line is in the history.
        term.command("history --all")?;
        term.expect("?? touch marker", SHORT);
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn the_shells_own_check_is_shown_for_a_dangerous_command_that_is_only_explained() {
    pty_test(BIN, |ctx| {
        let keep = ctx.home().join("keep");
        fs::create_dir_all(&keep)?;
        let server = Server::start_answering(Duration::ZERO, false, &["It deletes a folder."])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;

        term.command("?? rm -rf keep")?;
        term.expect("the shell's own check: dangerous", SHORT);
        term.expect("It deletes a folder.", SHORT);
        term.prompt();
        assert!(keep.exists(), "?? must never run the command");
        Ok(())
    });
}

#[test]
fn a_bare_double_question_mark_explains_the_last_failure_with_its_status() {
    pty_test(BIN, |ctx| {
        let server =
            Server::start_answering(Duration::ZERO, false, &["Probably nothing is wrong."])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;

        // Nothing failed yet: nothing is sent.
        term.command("true")?;
        term.command("??")?;
        term.expect("nothing to explain: the last command succeeded", SHORT);
        term.command("echo st-$?-end")?;
        term.expect("st-2-end", SHORT);
        assert!(chat_requests(&server).is_empty());

        term.command("sh -c 'exit 7'")?;
        term.command("??")?;
        term.expect_plain("its error text is not sent", SHORT);
        term.expect("Probably nothing is wrong.", SHORT);
        let requests = chat_requests(&server);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("exit 7"), "{}", requests[0]);
        assert!(requests[0].contains("Exit status: 7"), "{}", requests[0]);
        assert!(requests[0].contains("NOT captured"), "{}", requests[0]);
        // `??` itself is not "the last command": a second `??` explains the same failure.
        term.command("??")?;
        term.expect_plain("explaining `sh -c 'exit 7'`", SHORT);
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn alt_question_mark_explains_the_failure_and_keeps_what_was_typed() {
    pty_test(BIN, |ctx| {
        let server =
            Server::start_answering(Duration::ZERO, false, &["The command exits 1 by design."])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;

        term.command("sh -c 'exit 4'")?;
        term.prompt();
        // Half a line is typed; Alt+? must not run it, and must not lose it.
        term.write_bytes(b"echo kept-$((20+1))")?;
        term.write_bytes(ALT_QUESTION)?;
        term.expect("The command exits 1 by design.", SHORT);
        let requests = chat_requests(&server);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("exit 4"), "{}", requests[0]);
        assert!(requests[0].contains("Exit status: 4"), "{}", requests[0]);
        assert!(
            !requests[0].contains("kept-"),
            "text being typed is not sent: {}",
            requests[0]
        );
        // The typed text is still in the editor: Enter runs it.
        term.write_bytes(b"\r")?;
        term.expect("kept-21", SHORT);
        term.prompt();
        // Alt+? did not change `$?` (the failure before it is what the prompt showed), and it is
        // not in the history.
        term.command("history --all")?;
        term.expect("sh -c 'exit 4'", SHORT);
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn alt_question_mark_with_nothing_to_explain_says_so() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["never"])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;
        term.command("true")?;
        term.prompt();
        term.write_bytes(ALT_QUESTION)?;
        term.expect("nothing to explain: the last command succeeded", SHORT);
        assert!(chat_requests(&server).is_empty());
        // Alt+? is not a line: give the editor a moment to come back before typing again.
        term.pump(Duration::from_millis(500));
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn alt_question_mark_works_in_vi_mode_too() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["Exits 3."])?;
        configure(ctx, &server, "[editor]\nmode = \"vi\"\n")?;
        let mut term = launch(ctx).start()?;
        term.command("sh -c 'exit 3'")?;
        term.prompt();
        term.write_bytes(ALT_QUESTION)?;
        term.expect("Exits 3.", SHORT);
        assert!(chat_requests(&server)[0].contains("exit 3"));
        term.pump(Duration::from_millis(500));
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn the_output_of_a_command_is_sent_with_the_question() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["There are two lines."])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;

        term.command("printf 'alpha\\nbeta\\n' | ? how many lines")?;
        term.expect_plain("sending 11 characters of output", SHORT);
        term.expect("There are two lines.", SHORT);
        let requests = chat_requests(&server);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("how many lines"), "{}", requests[0]);
        assert!(requests[0].contains("alpha\\nbeta\\n"), "{}", requests[0]);
        // The command's output was taken by the pipe, not printed.
        let shown = term.since_cursor().to_owned();
        assert!(!shown.contains("alpha"), "{shown}");
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn secrets_in_the_command_and_in_the_output_are_masked() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["It is a token."])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;
        let key = ghp();
        term.command(&format!("echo \"token={key}\" | ? what is this"))?;
        // What the shell says comes before the answer.
        term.expect_plain("a secret was hidden", SHORT);
        term.expect("It is a token.", SHORT);
        let requests = chat_requests(&server);
        assert_eq!(requests.len(), 1);
        assert!(!requests[0].contains(&key), "{}", requests[0]);
        assert!(requests[0].contains("[MASKED]"), "{}", requests[0]);
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn a_dangerous_command_in_front_of_the_question_mark_is_asked_about_first() {
    pty_test(BIN, |ctx| {
        let doomed = ctx.home().join("doomed");
        fs::create_dir_all(&doomed)?;
        fs::write(doomed.join("f"), "data")?;
        let server = Server::start_answering(Duration::ZERO, false, &["ok"])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;

        term.command("rm -rf doomed | ? did it work")?;
        term.expect("Run it? [y/N]", SHORT);
        term.line("n")?;
        term.expect("cancelled; nothing ran", SHORT);
        term.prompt();
        assert!(
            doomed.join("f").exists(),
            "the cancelled delete must not run"
        );
        assert!(
            chat_requests(&server).is_empty(),
            "nothing is sent for a command that did not run"
        );

        // Confirmed: it runs, and then the question goes out.
        term.command("rm -rf doomed | ? did it work")?;
        term.expect("Run it? [y/N]", SHORT);
        term.line("y")?;
        term.expect("ok", SHORT);
        term.prompt();
        assert!(!doomed.exists());
        assert_eq!(chat_requests(&server).len(), 1);
        Ok(())
    });
}

#[test]
fn without_a_set_up_ai_the_command_in_front_of_the_question_mark_does_not_run() {
    pty_test(BIN, |ctx| {
        let marker = ctx.home().join("m");
        let mut term = launch(ctx).start()?;
        term.command("touch m | ? q")?;
        term.expect("oh-my-shell: ?: AI is not set up yet", SHORT);
        term.prompt();
        assert!(!marker.exists());
        term.command("echo st-$?-end")?;
        term.expect("st-1-end", SHORT);
        Ok(())
    });
}

#[test]
fn a_background_process_holding_the_pipe_does_not_hang_the_shell() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["Done early."])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;
        let started = Instant::now();
        term.command("echo early; sh -c 'sleep 8 &' | ? what happened")?;
        term.expect_plain("was not read", Duration::from_secs(4));
        term.expect("Done early.", Duration::from_secs(4));
        assert!(started.elapsed() < Duration::from_secs(5));
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn ctrl_c_while_waiting_for_the_answer_cancels_and_the_prompt_comes_back() {
    pty_test(BIN, |ctx| {
        // The server waits 20 s before it answers.
        let server = Server::start_answering(Duration::from_secs(20), false, &["too late"])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;
        term.command("?? ls -l")?;
        term.expect("Asking OpenAI-compatible server (model-one)", SHORT);
        term.key(CTRL_C)?;
        term.expect("cancelled", SHORT);
        term.prompt();
        term.command("echo st-$?-end")?;
        term.expect("st-130-end", SHORT);
        Ok(())
    });
}

#[test]
fn a_failed_plan_step_offers_the_explanation() {
    pty_test(BIN, |ctx| {
        let plan = r#"{"summary":"Fail","steps":[{"title":"Fail","command":"sh -c 'exit 5'","explanation":"x","risk":"low","needs_root":false}]}"#;
        let server = Server::start_answering(Duration::ZERO, false, &[plan, "It exited with 5."])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;
        term.command("? make it fail")?;
        term.expect("[Enter] run", SHORT);
        term.write_bytes(b"\r")?;
        term.expect_plain("type ?? (or press Alt+?) to ask why it failed", SHORT);
        term.command("??")?;
        term.expect("It exited with 5.", SHORT);
        let requests = chat_requests(&server);
        assert_eq!(requests.len(), 2);
        assert!(requests[1].contains("exit 5"), "{}", requests[1]);
        term.sync("end")?;
        Ok(())
    });
}

/// A 429 that says `Retry-After: 1` is waited out: the second request comes at least a second
/// after the first (the shell's own pause is half a second), and the answer is shown.
#[test]
fn a_rate_limit_with_retry_after_is_waited_out() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(
            Duration::ZERO,
            false,
            &["@error 429 retry-after=1", "Waited and answered."],
        )?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;
        term.command("?? touch marker")?;
        term.expect("Waited and answered.", SHORT);
        term.prompt();
        let chats: Vec<_> = server
            .requests()
            .into_iter()
            .filter(|r| r.path().ends_with("/chat/completions"))
            .collect();
        assert_eq!(chats.len(), 2, "one failure, one answer");
        let gap = chats[1].at.duration_since(chats[0].at);
        assert!(gap >= Duration::from_millis(950), "{gap:?}");
        term.quit()
    });
}

/// A server that asks for a long wait is not waited for: one request, and the line says how long
/// it asked.
#[test]
fn a_retry_after_over_ten_seconds_is_reported_and_not_waited_for() {
    pty_test(BIN, |ctx| {
        let server =
            Server::start_answering(Duration::ZERO, false, &["@error 429 retry-after=90"])?;
        configure(ctx, &server, "")?;
        let mut term = launch(ctx).start()?;
        term.command("?? touch marker")?;
        term.expect("it asks to wait 90 s", SHORT);
        term.prompt();
        let chats = server
            .requests()
            .into_iter()
            .filter(|r| r.path().ends_with("/chat/completions"))
            .count();
        assert_eq!(chats, 1, "no second try");
        term.quit()
    });
}
