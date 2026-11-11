//! `? request` through the real shell on a terminal (S21).
//!
//! The "model" is a fake server on this machine that answers with a plan chosen by the test
//! (`omsh_tests::fakehttp`). The keyring is never touched (see `ai.rs`). The shell is
//! configured by writing `config.toml` before it starts, so no test depends on the setup
//! wizard.
//!
//! A typed line is echoed by the terminal, so a test never looks for text it typed itself:
//! it looks for what only the shell prints.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use omsh_tests::fakehttp::Server;
use omsh_tests::{Ctx, Launch, SHORT, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const ENTER: &[u8] = b"\r";
const ESCAPE: &[u8] = b"\x1b";
const CTRL_C: u8 = 3;
const CTRL_U: u8 = 0x15;

fn config_dir(ctx: &Ctx) -> PathBuf {
    ctx.home().join(".config/oh-my-shell")
}

/// Points the shell at the fake server.
fn configure(ctx: &Ctx, server: &Server) -> io::Result<()> {
    let dir = config_dir(ctx);
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("config.toml"),
        format!(
            "[ai]\nprovider = \"openai-compatible\"\nmodel = \"model-one\"\nbase_url = \"{}\"\n",
            server.url("/v1")
        ),
    )
}

/// A launch whose keyring does not exist, in the home directory (so relative paths in the
/// plan's commands land there).
fn launch(ctx: &Ctx) -> Launch {
    ctx.launch()
        .env(
            "DBUS_SESSION_BUS_ADDRESS",
            "unix:path=/nonexistent/omsh-test-bus",
        )
        .cwd(ctx.home())
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A plan as the model would send it: (title, command, risk) for each step.
fn plan(steps: &[(&str, &str, &str)]) -> String {
    let steps: Vec<String> = steps
        .iter()
        .map(|(title, command, risk)| {
            format!(
                r#"{{"title":"{}","command":"{}","explanation":"Because.","risk":"{risk}","needs_root":false}}"#,
                escape(title),
                escape(command)
            )
        })
        .collect();
    format!(
        r#"{{"summary":"Do the thing","steps":[{}],"language":"en"}}"#,
        steps.join(",")
    )
}

fn chat_requests(server: &Server) -> Vec<String> {
    server
        .requests()
        .into_iter()
        .filter(|r| r.path().ends_with("/chat/completions"))
        .map(|r| r.body)
        .collect()
}

const PANEL_KEYS: &str = "[Enter] run  [e] edit  [Esc] cancel";

#[test]
fn a_plan_waits_for_enter_and_only_then_runs() {
    pty_test(BIN, |ctx| {
        let marker = ctx.home().join("marker");
        let answer = plan(&[("Make the marker", "echo made > marker", "low")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command("? make the marker")?;
        term.expect("plan from OpenAI-compatible server (model-one)", SHORT);
        term.expect("Make the marker", SHORT);
        term.expect(PANEL_KEYS, SHORT);
        // The panel is up and the step is as harmless as can be, yet it does not run by itself.
        std::thread::sleep(Duration::from_millis(400));
        assert!(!marker.exists(), "nothing may run before a key is pressed");

        term.write_bytes(ENTER)?;
        term.expect("plan done (1 step)", SHORT);
        term.prompt();
        assert_eq!(fs::read_to_string(&marker)?, "made\n");

        // The request line and the step that ran are both in the history.
        term.command("history --all")?;
        term.expect("echo made > marker", SHORT);
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn escape_runs_nothing_and_leaves_status_one() {
    pty_test(BIN, |ctx| {
        let marker = ctx.home().join("marker");
        let answer = plan(&[("Make the marker", "echo made > marker", "low")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command("? make the marker")?;
        term.expect(PANEL_KEYS, SHORT);
        term.write_bytes(ESCAPE)?;
        term.expect("oh-my-shell: cancelled; nothing ran", SHORT);
        term.command("echo st-$?-end")?;
        term.expect("st-1-end", SHORT);
        assert!(!marker.exists());
        Ok(())
    });
}

#[test]
fn a_request_with_an_open_quote_is_sent_on_enter_and_does_not_wait_for_more() {
    pty_test(BIN, |ctx| {
        let answer = plan(&[("List", "ls", "low")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command("? what's in here")?;
        // The panel, not a `> ` continuation prompt.
        term.expect(PANEL_KEYS, SHORT);
        term.write_bytes(ESCAPE)?;
        term.expect("cancelled; nothing ran", SHORT);
        term.sync("alive")?;
        let requests = chat_requests(&server);
        assert_eq!(requests.len(), 1);
        assert!(requests[0].contains("what's in here"), "{}", requests[0]);
        Ok(())
    });
}

#[test]
fn the_request_is_masked_and_carries_only_the_allowed_context() {
    pty_test(BIN, |ctx| {
        let token = format!("ghp_{}", "a1B2".repeat(9));
        let answer = plan(&[("List", "ls", "low")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command(&format!("? push with {token} please"))?;
        term.expect("a secret in the request was hidden", SHORT);
        term.expect(PANEL_KEYS, SHORT);
        term.write_bytes(ESCAPE)?;
        term.expect("cancelled", SHORT);
        let body = &chat_requests(&server)[0];
        assert!(!body.contains(&token), "the token left the machine: {body}");
        assert!(body.contains("push with [MASKED] please"), "{body}");
        assert!(body.contains("Working directory"), "{body}");
        // No history, no git facts, no environment.
        assert!(!body.contains("Recent commands"), "{body}");
        Ok(())
    });
}

#[test]
fn ctrl_c_while_waiting_gives_up_at_once() {
    pty_test(BIN, |ctx| {
        let answer = plan(&[("List", "ls", "low")]);
        // The server holds its answer for a long time.
        let server = Server::start_answering(Duration::from_secs(30), false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command("? something slow")?;
        term.expect("Asking OpenAI-compatible server (model-one)", SHORT);
        term.key(CTRL_C)?;
        term.expect("cancelled; nothing ran", SHORT);
        term.command("echo st-$?-end")?;
        term.expect("st-130-end", SHORT);
        Ok(())
    });
}

#[test]
fn a_model_that_cannot_make_a_plan_gets_one_more_try_and_then_a_clean_message() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["Sure! Just run ls."])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command("? list files")?;
        term.expect("couldn't make a plan: the answer has no JSON object", SHORT);
        term.command("echo st-$?-end")?;
        term.expect("st-1-end", SHORT);
        assert_eq!(chat_requests(&server).len(), 2, "one retry, no more");
        Ok(())
    });
}

#[test]
fn a_failed_step_stops_the_plan_and_the_shell_goes_on() {
    pty_test(BIN, |ctx| {
        let answer = plan(&[
            ("First", "echo first > one", "low"),
            ("Breaks", "false", "low"),
            ("Never", "echo never > three", "low"),
        ]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command("? three steps")?;
        term.expect(PANEL_KEYS, SHORT);
        term.write_bytes(ENTER)?;
        term.expect("step 2 did not succeed (status 1)", SHORT);
        term.expect("the remaining 1 step did not run", SHORT);
        term.command("echo st-$?-end")?;
        term.expect("st-1-end", SHORT);
        assert!(ctx.home().join("one").exists());
        assert!(!ctx.home().join("three").exists());
        Ok(())
    });
}

#[test]
fn a_command_can_be_edited_in_the_line_editor() {
    pty_test(BIN, |ctx| {
        let answer = plan(&[("Make the marker", "echo made > original", "low")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command("? make the marker")?;
        term.expect(PANEL_KEYS, SHORT);
        term.write_bytes(b"e")?;
        // The editor opens with the command in the buffer.
        term.expect_plain("edit> echo made > original", SHORT);
        term.key(CTRL_U)?;
        term.line("echo changed > replaced")?;
        // The panel comes back with the new command, marked.
        term.expect("[safe, edited]", SHORT);
        term.write_bytes(ENTER)?;
        term.expect("plan done (1 step)", SHORT);
        term.prompt();
        assert!(ctx.home().join("replaced").exists());
        assert!(!ctx.home().join("original").exists());
        Ok(())
    });
}

#[test]
fn without_a_provider_the_request_says_how_to_set_one_up() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx).start()?;
        term.command("? list files")?;
        term.expect(
            "oh-my-shell: ?: AI is not set up yet (run: omsh ai setup)",
            SHORT,
        );
        term.command("echo st-$?-end")?;
        term.expect("st-1-end", SHORT);
        Ok(())
    });
}

#[test]
fn no_ai_turns_requests_off_for_the_session() {
    pty_test(BIN, |ctx| {
        let answer = plan(&[("List", "ls", "low")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).arg("--no-ai").start()?;
        term.command("? list files")?;
        term.expect("AI is off for this session (--no-ai)", SHORT);
        term.sync("alive")?;
        assert!(chat_requests(&server).is_empty(), "no request may go out");
        Ok(())
    });
}

#[test]
fn a_question_mark_alone_and_a_glob_are_not_confused() {
    pty_test(BIN, |ctx| {
        let mut term = launch(ctx).start()?;
        term.command("?")?;
        term.expect("usage: ? <what you want done>", SHORT);
        // `??` explains, which is not there yet.
        term.command("?? tar")?;
        term.expect("comes in a later version", SHORT);
        // A glob that happens to start with `?` is still a glob.
        term.command("echo x > ab; echo ?b")?;
        term.expect("ab", SHORT);
        term.sync("end")?;
        Ok(())
    });
}
