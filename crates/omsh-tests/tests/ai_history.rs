//! `? history <request>` through the real shell on a terminal (S25).
//!
//! The "model" is a fake server on this machine (`omsh_tests::fakehttp`). It answers with a
//! filter; the shell runs the filter on its own history database. A typed line is echoed by the
//! terminal, so a test reads only the part of the transcript that starts at the line the shell
//! prints before the listing.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use omsh_tests::fakehttp::Server;
use omsh_tests::{Ctx, Launch, SHORT, Session, pty_test};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

const CTRL_C: u8 = 3;

/// What the shell prints before the listing.
const DESCRIBED: &str = "? history: commands ";

fn configure(ctx: &Ctx, server: &Server) -> io::Result<()> {
    let dir: PathBuf = ctx.home().join(".config/oh-my-shell");
    fs::create_dir_all(&dir)?;
    fs::write(
        dir.join("config.toml"),
        format!(
            "[ai]\nprovider = \"openai-compatible\"\nmodel = \"model-one\"\nbase_url = \"{}\"\n",
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

/// Everything the shell showed from the line before the listing on.
fn from_description(term: &Session) -> String {
    let all = term.transcript();
    match all.rfind(DESCRIBED) {
        Some(start) => all[start..].to_owned(),
        None => panic!("no description line in:\n{all}"),
    }
}

#[test]
fn a_search_runs_the_models_filter_on_the_local_history() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(
            Duration::ZERO,
            false,
            &[r#"{"all": ["planted-77"], "status": "failed"}"#],
        )?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command("false planted-77 broke")?;
        term.command("echo planted-77 worked")?;
        term.command("? history the ones that went wrong")?;
        term.expect(" shown", SHORT);
        term.prompt();
        let shown = from_description(&term);
        assert!(
            shown.contains("commands containing \"planted-77\", that failed, newest 20"),
            "{shown}"
        );
        assert!(shown.contains("false planted-77 broke"), "{shown}");
        assert!(!shown.contains("echo planted-77 worked"), "{shown}");
        assert!(
            !shown.contains("? history the ones"),
            "the search finds itself: {shown}"
        );
        assert!(shown.contains("1 command shown"), "{shown}");

        // Only the request and the date were sent, never a line of the history.
        let requests = chat_requests(&server);
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].contains("the ones that went wrong"),
            "{}",
            requests[0]
        );
        assert!(!requests[0].contains("planted-77"), "{}", requests[0]);
        assert!(requests[0].contains("Today is "), "{}", requests[0]);

        term.command("echo st-$?-end")?;
        term.expect("st-0-end", SHORT);
        // The search line itself is in the history, like other `?` lines.
        term.command("history --all")?;
        term.expect("? history the ones that went wrong", SHORT);
        term.sync("end")?;
        Ok(())
    });
}

#[test]
fn here_means_this_folder_and_not_the_folders_below() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(
            Duration::ZERO,
            false,
            &[r#"{"all": ["tag-"], "here": true}"#],
        )?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;

        term.command("echo tag-top")?;
        term.command("mkdir sub")?;
        term.command("cd sub")?;
        term.command("echo tag-below")?;
        term.command("cd ..")?;
        term.command("? history tagged commands here")?;
        term.expect(" shown", SHORT);
        term.prompt();
        let shown = from_description(&term);
        assert!(shown.contains("echo tag-top"), "{shown}");
        assert!(!shown.contains("echo tag-below"), "{shown}");
        assert!(
            shown.contains("run in this folder (not its subfolders)"),
            "{shown}"
        );
        Ok(())
    });
}

#[test]
fn a_bare_history_request_is_usage_and_sends_nothing() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["{}"])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;
        term.command("? history")?;
        term.expect("usage: ? history", SHORT);
        term.command("echo st-$?-end")?;
        term.expect("st-2-end", SHORT);
        assert_eq!(chat_requests(&server).len(), 0);
        Ok(())
    });
}

#[test]
fn without_the_ai_nothing_is_sent_and_the_plain_search_is_offered() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(Duration::ZERO, false, &["{}"])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).arg("--no-ai").start()?;
        term.command("? history anything")?;
        term.expect("history search TEXT", SHORT);
        term.command("echo st-$?-end")?;
        term.expect("st-1-end", SHORT);
        assert_eq!(server.requests().len(), 0, "a socket was used");
        Ok(())
    });
}

#[test]
fn a_filter_the_shell_cannot_read_is_said_once_after_one_retry() {
    pty_test(BIN, |ctx| {
        let server = Server::start_answering(
            Duration::ZERO,
            false,
            &[r#"{"sql": "select * from history"}"#],
        )?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;
        term.command("echo something")?;
        term.command("? history something")?;
        term.expect("couldn't read the search", SHORT);
        term.command("echo st-$?-end")?;
        term.expect("st-1-end", SHORT);
        assert_eq!(chat_requests(&server).len(), 2, "one try and one retry");
        Ok(())
    });
}

#[test]
fn ctrl_c_while_waiting_cancels_the_search() {
    pty_test(BIN, |ctx| {
        // The server waits 20 s before it answers.
        let server = Server::start_answering(Duration::from_secs(20), false, &["{}"])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;
        term.command("? history anything")?;
        term.expect("Asking OpenAI-compatible server (model-one)", SHORT);
        term.key(CTRL_C)?;
        term.expect("cancelled; nothing was searched", SHORT);
        term.prompt();
        term.command("echo st-$?-end")?;
        term.expect("st-130-end", SHORT);
        Ok(())
    });
}
