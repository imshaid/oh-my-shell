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
use omsh_tests::question::{
    Answer, MODES, Mode, answer as respond, pick_model, pick_provider, plan_header, plan_key,
    wait_plan, wait_question,
};
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
    configure_with(ctx, server, "")
}

/// The same, with more text at the end of `config.toml`.
fn configure_with(ctx: &Ctx, server: &Server, more: &str) -> io::Result<()> {
    let dir = config_dir(ctx);
    fs::create_dir_all(&dir)?;
    // The count of what a step touches has a time budget (300 ms by default); a busy machine
    // must not change the numbers these tests look for.
    let safety = if more.contains("[safety]") {
        ""
    } else {
        "[safety]\nblast_radius_max_ms = 60000\n"
    };
    fs::write(
        dir.join("config.toml"),
        format!(
            "[ai]\nprovider = \"openai-compatible\"\nmodel = \"model-one\"\nbase_url = \"{}\"\n{more}{safety}",
            server.url("/v1")
        ),
    )
}

/// A folder `repo` in the home directory that looks like a git repository on `branch`.
fn make_repo(ctx: &Ctx, branch: &str) -> io::Result<()> {
    let git = ctx.home().join("repo/.git");
    fs::create_dir_all(&git)?;
    fs::write(git.join("HEAD"), format!("ref: refs/heads/{branch}\n"))
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
        r#"{{"summary":"Do the thing","steps":[{}]}}"#,
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

#[test]
fn a_plan_waits_for_enter_and_only_then_runs() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let marker = ctx.home().join("marker");
            let answer = plan(&[("Make the marker", "echo made > marker", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? make the marker")?;
            term.expect(plan_header(mode), SHORT);
            term.expect("Make the marker", SHORT);
            wait_plan(&mut term, mode);
            // The panel is up and the step is as harmless as can be, yet it does not run by itself.
            std::thread::sleep(Duration::from_millis(400));
            assert!(!marker.exists(), "nothing may run before a key is pressed");

            plan_key(&mut term, mode, ENTER)?;
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
}

#[test]
fn escape_runs_nothing_and_leaves_status_one() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let marker = ctx.home().join("marker");
            let answer = plan(&[("Make the marker", "echo made > marker", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? make the marker")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ESCAPE)?;
            term.expect("oh-my-shell: cancelled; nothing ran", SHORT);
            term.command("echo st-$?-end")?;
            term.expect("st-1-end", SHORT);
            assert!(!marker.exists());
            Ok(())
        });
    }
}

#[test]
fn a_request_with_an_open_quote_is_sent_on_enter_and_does_not_wait_for_more() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let answer = plan(&[("List", "ls", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? what's in here")?;
            // The panel, not a `> ` continuation prompt.
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ESCAPE)?;
            term.expect("cancelled; nothing ran", SHORT);
            term.sync("alive")?;
            let requests = chat_requests(&server);
            assert_eq!(requests.len(), 1);
            assert!(requests[0].contains("what's in here"), "{}", requests[0]);
            Ok(())
        });
    }
}

#[test]
fn the_request_is_masked_and_carries_only_the_allowed_context() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let token = format!("ghp_{}", "a1B2".repeat(9));
            let answer = plan(&[("List", "ls", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command(&format!("? push with {token} please"))?;
            term.expect("hidden before sending", SHORT);
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ESCAPE)?;
            term.expect("cancelled", SHORT);
            let body = &chat_requests(&server)[0];
            assert!(!body.contains(&token), "the token left the machine: {body}");
            assert!(body.contains("push with [MASKED] please"), "{body}");
            assert!(body.contains("Working directory"), "{body}");
            // Nothing ran yet and the folder is no repository: no recent commands, no branch.
            assert!(!body.contains("Recent commands"), "{body}");
            assert!(body.contains("not inside a repository"), "{body}");
            Ok(())
        });
    }
}

const SECRET_EXPORT: &str = concat!(
    "export GITHUB_TOKEN=gh",
    "p_a1B2a1B2a1B2a1B2a1B2a1B2a1B2a1B2a1B2"
);
const TOKEN: &str = concat!("ghp_", "a1B2a1B2a1B2a1B2a1B2a1B2a1B2a1B2a1B2");

#[test]
fn the_branch_and_the_last_commands_are_sent_and_masked() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let answer = plan(&[("List", "ls", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            make_repo(ctx, "feature-x")?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("cd repo")?;
            term.command(SECRET_EXPORT)?;
            term.command("false")?;
            term.command("? list the files")?;
            term.expect("hidden before sending", SHORT);
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ESCAPE)?;
            term.expect("cancelled", SHORT);
            let body = &chat_requests(&server)[0];
            assert!(!body.contains(TOKEN), "the token left the machine: {body}");
            assert!(
                body.contains("inside a repository, on branch feature-x"),
                "{body}"
            );
            assert!(body.contains("Recent commands in this shell"), "{body}");
            assert!(
                body.contains("export GITHUB_TOKEN=[MASKED]  (exit 0)"),
                "{body}"
            );
            assert!(body.contains("false  (exit 1)"), "{body}");
            // The request itself is not a recent command.
            assert!(!body.contains("list the files  (exit"), "{body}");
            Ok(())
        });
    }
}

#[test]
fn switched_off_context_items_are_not_sent() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let answer = plan(&[("List", "ls", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure_with(
                ctx,
                &server,
                "[ai.context]\nos = false\ncwd = false\ngit = false\nrecent_commands = false\n",
            )?;
            make_repo(ctx, "feature-x")?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("cd repo")?;
            term.command("false")?;
            term.command("? list the files")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ESCAPE)?;
            term.expect("cancelled", SHORT);
            let body = &chat_requests(&server)[0];
            for absent in [
                "Git:",
                "feature-x",
                "Recent commands",
                "Working directory",
                "OS:",
                "repo",
            ] {
                assert!(!body.contains(absent), "{absent} was sent: {body}");
            }
            assert!(body.contains("list the files"), "{body}");
            Ok(())
        });
    }
}

#[test]
fn v_shows_what_was_sent_and_runs_nothing() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let marker = ctx.home().join("marker");
            let answer = plan(&[("Touch", &format!("echo x > {}", marker.display()), "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command(&format!("? touch it, key {TOKEN}"))?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, b"v")?;
            term.expect_plain(
                "this was sent to OpenAI-compatible server (model-one)",
                SHORT,
            );
            term.expect_plain("| - Working directory:", SHORT);
            term.expect_plain("| touch it, key [MASKED]", SHORT);
            term.expect_plain("no file contents or command output are sent", SHORT);
            assert!(!marker.exists(), "`v` must not run the plan");
            // The question is still open: Esc cancels it.
            plan_key(&mut term, mode, ESCAPE)?;
            term.expect("cancelled; nothing ran", SHORT);
            assert!(!marker.exists());
            Ok(())
        });
    }
}

#[test]
fn ctrl_c_while_waiting_gives_up_at_once() {
    pty_test(BIN, move |ctx| {
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
    pty_test(BIN, move |ctx| {
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
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let answer = plan(&[
                ("First", "echo first > one", "low"),
                ("Breaks", "false", "low"),
                ("Never", "echo never > three", "low"),
            ]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? three steps")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ENTER)?;
            term.expect("step 2 did not succeed (status 1)", SHORT);
            term.expect("the remaining 1 step did not run", SHORT);
            term.command("echo st-$?-end")?;
            term.expect("st-1-end", SHORT);
            assert!(ctx.home().join("one").exists());
            assert!(!ctx.home().join("three").exists());
            Ok(())
        });
    }
}

#[test]
fn a_command_can_be_edited_in_the_line_editor() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let answer = plan(&[("Make the marker", "echo made > original", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? make the marker")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, b"e")?;
            match mode {
                Mode::Panel => {
                    // The editor opens with the command in the buffer.
                    term.expect_plain("edit> echo made > original", SHORT);
                    term.key(CTRL_U)?;
                }
                // No line editor under `TERM=dumb`: a plain question.
                Mode::Dumb => term.expect_plain("new command (Enter keeps it)> ", SHORT),
            }
            term.line("echo changed > replaced")?;
            // The panel comes back with the new command, marked.
            term.expect_plain(edited_tag(mode), SHORT);
            plan_key(&mut term, mode, ENTER)?;
            term.expect("plan done (1 step)", SHORT);
            term.prompt();
            assert!(ctx.home().join("replaced").exists());
            assert!(!ctx.home().join("original").exists());
            Ok(())
        });
    }
}

#[test]
fn without_a_provider_the_request_says_how_to_set_one_up() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let mut term = mode.launch(launch(ctx)).start()?;
            term.command("? list files")?;
            if mode == Mode::Panel {
                // The panel offers the setup, with "Not now" chosen: Enter alone starts nothing.
                term.expect_plain("Set up AI now", SHORT);
                term.expect_plain("Not now", SHORT);
                plan_key(&mut term, mode, ENTER)?;
            }
            term.expect(
                "oh-my-shell: ?: AI is not set up yet (run: omsh ai setup)",
                SHORT,
            );
            term.command("echo st-$?-end")?;
            term.expect("st-1-end", SHORT);
            assert!(
                !config_dir(ctx).join("credentials").exists(),
                "nothing was set up"
            );
            Ok(())
        });
    }
}

/// The way a plan with a step that is edited is marked: `[safe, edited]` in the text panel,
/// `safe, edited` in the panel with buttons.
fn edited_tag(mode: Mode) -> &'static str {
    match mode {
        Mode::Panel => "safe, edited",
        Mode::Dumb => "[safe, edited]",
    }
}

const KEY: &str = concat!("sk-", "test-5f3c9a1e77b24d0c");

#[test]
fn set_up_now_runs_the_wizard_and_then_sends_the_question() {
    pty_test(BIN, move |ctx| {
        let marker = ctx.home().join("marker");
        let answer = plan(&[("Make the marker", "echo made > marker", "low")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
        // No `config.toml` yet: the AI is not set up.
        let mut term = launch(ctx).start()?;
        term.command("? make the marker")?;
        term.expect_plain("Set up AI now", SHORT);
        plan_key(&mut term, Mode::Panel, b"s")?;
        pick_provider(&mut term, Mode::Panel, "compat", "5")?;
        term.expect("Server address", SHORT);
        term.line(&server.url("/v1"))?;
        term.expect("API key", SHORT);
        term.line(KEY)?;
        pick_model(&mut term, Mode::Panel, "model-one", "model-one", "1")?;
        term.expect(
            "AI is set up: OpenAI-compatible server, model model-one",
            SHORT,
        );
        // The question that was typed is answered now, with the plan panel.
        term.expect_plain("Make the marker", SHORT);
        wait_plan(&mut term, Mode::Panel);
        assert!(!marker.exists(), "nothing runs before a key is pressed");
        plan_key(&mut term, Mode::Panel, ESCAPE)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        assert!(!marker.exists());
        // The wizard checked the key with a request of its own; the question went once.
        let sent: Vec<String> = chat_requests(&server)
            .into_iter()
            .filter(|body| body.contains("make the marker"))
            .collect();
        assert_eq!(sent.len(), 1, "the typed question was sent once: {sent:?}");
        Ok(())
    });
}

#[test]
fn not_now_and_esc_at_the_setup_panel_send_nothing_and_open_no_socket() {
    for keys in [&b"n"[..], ESCAPE, ENTER] {
        pty_test(BIN, move |ctx| {
            let server = Server::start_answering(Duration::ZERO, false, &["{}"])?;
            let mut term = launch(ctx).start()?;
            term.command("? make the marker")?;
            term.expect_plain("Not now", SHORT);
            plan_key(&mut term, Mode::Panel, keys)?;
            term.expect("AI is not set up yet (run: omsh ai setup)", SHORT);
            term.command("echo st-$?-end")?;
            term.expect("st-1-end", SHORT);
            assert!(server.requests().is_empty(), "{keys:?}: nothing was sent");
            assert!(
                !config_dir(ctx).join("config.toml").exists() || {
                    let text = fs::read_to_string(config_dir(ctx).join("config.toml"))?;
                    !text.contains("provider =") || text.contains("# provider")
                }
            );
            Ok(())
        });
    }
}

#[test]
fn keys_pressed_while_the_model_answers_never_answer_the_plan_panel() {
    pty_test(BIN, move |ctx| {
        let marker = ctx.home().join("marker");
        let answer = plan(&[("Make the marker", "echo made > marker", "low")]);
        // The server answers after a while: the keys below arrive before the panel exists.
        let server = Server::start_answering(Duration::from_millis(700), false, &[&answer])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;
        term.command("? make the marker")?;
        std::thread::sleep(Duration::from_millis(150));
        term.press(b"y\r")?;
        term.press(b"\r")?;
        wait_plan(&mut term, Mode::Panel);
        std::thread::sleep(Duration::from_millis(400));
        term.pump(Duration::from_millis(100));
        assert!(!marker.exists(), "a key typed during the wait ran the plan");
        assert!(
            !term.transcript().contains("plan done"),
            "{}",
            term.transcript()
        );
        // It is still waiting: Esc is the answer now.
        plan_key(&mut term, Mode::Panel, ESCAPE)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        assert!(!marker.exists());
        Ok(())
    });
}

#[test]
fn no_ai_turns_requests_off_for_the_session() {
    pty_test(BIN, move |ctx| {
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
    pty_test(BIN, move |ctx| {
        let mut term = launch(ctx).start()?;
        term.command("?")?;
        term.expect("usage: ? <what you want done>", SHORT);
        // `??` explains; with no AI set up it says so.
        term.command("?? tar")?;
        term.expect("oh-my-shell: ??: AI is not set up yet", SHORT);
        // A glob that happens to start with `?` is still a glob.
        term.command("echo x > ab; echo ?b")?;
        term.expect("ab", SHORT);
        term.sync("end")?;
        Ok(())
    });
}

// -- the blast radius and the question after Enter (S17) -----------------------------------

#[test]
fn a_destructive_step_shows_its_count_and_asks_once_more_after_enter() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let build = ctx.home().join("proj/build");
            fs::create_dir_all(&build)?;
            fs::write(build.join("f"), "x\n")?;
            // The model calls the step harmless; the analyzer does not.
            let answer = plan(&[("Remove build", "rm -rf proj/build", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? remove the build folder")?;
            term.expect_plain("touches 1 file and 1 folder (2 B of file data)", SHORT);
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ENTER)?;
            term.expect_plain("this plan has a dangerous step", SHORT);
            wait_question(&mut term, mode);
            respond(&mut term, mode, Answer::Cancel)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            assert!(build.join("f").exists(), "Enter alone must not delete");

            term.command("? remove the build folder")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ENTER)?;
            wait_question(&mut term, mode);
            respond(&mut term, mode, Answer::Run)?;
            term.expect_plain("plan done (1 step)", SHORT);
            term.prompt();
            assert!(!build.exists());
            Ok(())
        });
    }
}

#[test]
fn a_find_delete_step_no_longer_runs_on_enter_alone() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            // Found by hand: the model's plan deleted a log file with only Enter.
            let logs = ctx.home().join("logs");
            fs::create_dir_all(&logs)?;
            fs::write(logs.join("a.log"), "x\n")?;
            fs::write(logs.join("b.txt"), "x\n")?;
            let answer = plan(&[(
                "Delete the logs",
                "find logs -name '*.log' -delete",
                "medium",
            )]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? delete the log files")?;
            term.expect_plain("searches 2 files and 1 folder", SHORT);
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ENTER)?;
            wait_question(&mut term, mode);
            respond(&mut term, mode, Answer::Cancel)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            assert!(logs.join("a.log").exists());
            Ok(())
        });
    }
}

#[test]
fn the_models_own_dangerous_label_asks_even_when_the_analyzer_sees_nothing() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let answer = plan(&[("Say hi", "echo hi > hi.txt", "high")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? say hi")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ENTER)?;
            wait_question(&mut term, mode);
            respond(&mut term, mode, Answer::Cancel)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            assert!(!ctx.home().join("hi.txt").exists());
            Ok(())
        });
    }
}

#[test]
fn the_limits_in_config_toml_reach_the_plan_panel() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let big = ctx.home().join("big");
            fs::create_dir_all(&big)?;
            for i in 0..10 {
                fs::write(big.join(format!("f{i}")), "x\n")?;
            }
            let answer = plan(&[("Remove big", "rm -rf big", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure_with(
                ctx,
                &server,
                "[safety]\nblast_radius_max_entries = 3\nblast_radius_max_ms = 60000\n",
            )?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? remove the big folder")?;
            term.expect_plain("touches at least 2 files and 1 folder", SHORT);
            term.expect_plain(
                "counting stopped at the limit of 3 entries; there is more",
                SHORT,
            );
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ESCAPE)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            assert_eq!(fs::read_dir(&big)?.count(), 10);
            Ok(())
        });
    }
}

#[test]
fn chat_sends_a_follow_up_and_enter_runs_the_new_plan() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let first = plan(&[("First", "echo one > one", "low")]);
            let second = plan(&[("Second", "echo two > two", "low")]);
            // The last answer repeats, so a third request would get the second plan again.
            let server = Server::start_answering(Duration::ZERO, false, &[&first, &second])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? make a marker")?;
            term.expect_plain("First", SHORT);
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, b"c")?;
            term.expect_plain("chat> ", SHORT);
            term.line("call it two")?;
            term.expect_plain("after 1 chat turn", SHORT);
            term.expect_plain("Second", SHORT);
            assert!(
                !ctx.home().join("one").exists() && !ctx.home().join("two").exists(),
                "nothing may run before Enter"
            );
            plan_key(&mut term, mode, ENTER)?;
            term.expect("plan done (1 step)", SHORT);
            term.prompt();
            assert!(ctx.home().join("two").exists());
            assert!(
                !ctx.home().join("one").exists(),
                "the old plan must not run"
            );

            // The server saw two requests; the second carries the first plan and the message.
            let requests = chat_requests(&server);
            assert_eq!(requests.len(), 2, "{requests:?}");
            assert!(requests[1].contains("echo one"), "{}", requests[1]);
            assert!(requests[1].contains("call it two"), "{}", requests[1]);
            assert!(!requests[0].contains("call it two"));

            // The follow-up is not a command: it is not in the history, the step that ran is.
            let mark = term.transcript().len();
            term.command("history --all")?;
            term.expect("echo two > two", SHORT);
            term.sync("end")?;
            let listing = &term.transcript()[mark..];
            assert!(!listing.contains("call it two"), "{listing}");
            Ok(())
        });
    }
}

#[test]
fn ctrl_c_at_the_chat_line_goes_back_to_the_panel() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let answer = plan(&[("First", "echo one > one", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&answer])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;

            term.command("? make a marker")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, b"c")?;
            term.expect_plain("chat> ", SHORT);
            plan_key(&mut term, mode, &[CTRL_C])?;
            // The panel is asked again, the old plan is still there, and no second request left.
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ENTER)?;
            term.expect("plan done (1 step)", SHORT);
            term.prompt();
            assert!(ctx.home().join("one").exists());
            assert_eq!(chat_requests(&server).len(), 1);
            Ok(())
        });
    }
}

// -- step by step, details and the info line (UI upgrade, phase 4) ----------------------------

/// A shell with a plan of these steps, the panel up and nothing pressed. `build` is a folder in
/// the home directory and `proj/build` one inside `proj`, each with a file in it.
fn step_shell(
    ctx: &Ctx,
    mode: Mode,
    steps: &[(&str, &str, &str)],
) -> io::Result<omsh_tests::Session> {
    for dir in ["build", "proj/build"] {
        fs::create_dir_all(ctx.home().join(dir))?;
        fs::write(ctx.home().join(dir).join("f"), "x\n")?;
    }
    let json = plan(steps);
    let server = Server::start_answering(Duration::ZERO, false, &[&json])?;
    configure(ctx, &server)?;
    // The server must outlive the shell: the plan is answered once and the flow ends.
    std::mem::forget(server);
    let mut term = mode.launch(launch(ctx)).start()?;
    term.command("? tidy up")?;
    wait_plan(&mut term, mode);
    Ok(term)
}

#[test]
fn step_by_step_asks_before_each_step_and_run_skip_and_stop_do_what_they_say() {
    pty_test(BIN, move |ctx| {
        let mut term = step_shell(
            ctx,
            Mode::Panel,
            &[
                ("First", "echo one > one", "low"),
                ("Second", "echo two > two", "low"),
                ("Third", "echo three > three", "low"),
            ],
        )?;
        plan_key(&mut term, Mode::Panel, b"s")?;
        term.expect_plain("Step 1 of 3", SHORT);
        plan_key(&mut term, Mode::Panel, b"y")?;
        term.expect_plain("[1/3] First", SHORT);
        term.expect_plain("Step 2 of 3", SHORT);
        plan_key(&mut term, Mode::Panel, b"s")?;
        term.expect_plain("2. Second (skipped)", SHORT);
        term.expect_plain("Step 3 of 3", SHORT);
        plan_key(&mut term, Mode::Panel, b"y")?;
        term.expect_plain("plan done (3 steps, 1 skipped)", SHORT);
        term.prompt();
        assert!(ctx.home().join("one").exists());
        assert!(
            !ctx.home().join("two").exists(),
            "a skipped step must not run"
        );
        assert!(ctx.home().join("three").exists());
        Ok(())
    });
}

#[test]
fn stop_ends_the_plan_before_that_step_and_runs_nothing_more() {
    pty_test(BIN, move |ctx| {
        let mut term = step_shell(
            ctx,
            Mode::Panel,
            &[
                ("First", "echo one > one", "low"),
                ("Second", "echo two > two", "low"),
            ],
        )?;
        plan_key(&mut term, Mode::Panel, b"s")?;
        term.expect_plain("Step 1 of 2", SHORT);
        plan_key(&mut term, Mode::Panel, b"y")?;
        term.expect_plain("Step 2 of 2", SHORT);
        plan_key(&mut term, Mode::Panel, b"n")?;
        term.expect_plain("2. Second (not run)", SHORT);
        term.expect_plain("stopped before step 2", SHORT);
        term.command("echo st-$?-end")?;
        term.expect("st-1-end", SHORT);
        assert!(ctx.home().join("one").exists());
        assert!(!ctx.home().join("two").exists());
        Ok(())
    });
}

#[test]
fn enter_on_a_step_runs_it_and_esc_stops_the_plan() {
    pty_test(BIN, move |ctx| {
        let mut term = step_shell(
            ctx,
            Mode::Panel,
            &[
                ("First", "echo one > one", "low"),
                ("Second", "echo two > two", "low"),
            ],
        )?;
        plan_key(&mut term, Mode::Panel, b"s")?;
        term.expect_plain("Step 1 of 2", SHORT);
        plan_key(&mut term, Mode::Panel, ENTER)?;
        term.expect_plain("Step 2 of 2", SHORT);
        plan_key(&mut term, Mode::Panel, ESCAPE)?;
        term.expect_plain("stopped before step 2", SHORT);
        assert!(ctx.home().join("one").exists());
        assert!(!ctx.home().join("two").exists());
        Ok(())
    });
}

#[test]
fn a_step_that_depended_on_a_skipped_one_is_judged_again_and_still_asks() {
    pty_test(BIN, move |ctx| {
        // The model says both steps are harmless. Step 1 is a `cd`; step 2 deletes a folder
        // that is `proj/build` after the `cd` and `build` here. Skipping the `cd` changes what
        // step 2 touches: it is looked at again, against the shell as it is, and it asks.
        let mut term = step_shell(
            ctx,
            Mode::Panel,
            &[
                ("Go into proj", "cd proj", "low"),
                ("Remove build", "rm -rf build", "low"),
            ],
        )?;
        plan_key(&mut term, Mode::Panel, b"s")?;
        term.expect_plain("Step 1 of 2", SHORT);
        plan_key(&mut term, Mode::Panel, b"s")?;
        term.expect_plain("1. Go into proj (skipped)", SHORT);
        term.expect_plain("Step 2 of 2", SHORT);
        plan_key(&mut term, Mode::Panel, b"y")?;
        wait_question(&mut term, Mode::Panel);
        plan_key(&mut term, Mode::Panel, b"n")?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        assert!(
            ctx.home().join("build/f").exists(),
            "the question was skipped"
        );
        assert!(ctx.home().join("proj/build/f").exists());
        Ok(())
    });
}

#[test]
fn a_dangerous_step_asks_at_its_turn_even_when_it_was_run_after_its_predecessor() {
    pty_test(BIN, move |ctx| {
        let mut term = step_shell(
            ctx,
            Mode::Panel,
            &[
                ("Go into proj", "cd proj", "low"),
                ("Remove build", "rm -rf build", "low"),
            ],
        )?;
        plan_key(&mut term, Mode::Panel, b"s")?;
        term.expect_plain("Step 1 of 2", SHORT);
        plan_key(&mut term, Mode::Panel, b"y")?;
        term.expect_plain("Step 2 of 2", SHORT);
        plan_key(&mut term, Mode::Panel, b"y")?;
        wait_question(&mut term, Mode::Panel);
        // Not the "riskier than the plan showed" case: the plan showed it. Step by step asks at
        // every step that reaches the level that asks.
        plan_key(&mut term, Mode::Panel, b"y")?;
        term.expect_plain("plan done (2 steps)", SHORT);
        term.prompt();
        assert!(!ctx.home().join("proj/build").exists());
        assert!(
            ctx.home().join("build/f").exists(),
            "after the cd, `build` is proj/build"
        );
        Ok(())
    });
}

#[test]
fn an_edited_step_is_analyzed_and_asked_about_again() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            fs::create_dir_all(ctx.home().join("proj/build"))?;
            fs::write(ctx.home().join("proj/build/f"), "x\n")?;
            let json = plan(&[("Say hi", "echo hi", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&json])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;
            term.command("? say hi")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, b"e")?;
            match mode {
                Mode::Panel => {
                    term.expect_plain("edit> echo hi", SHORT);
                    term.key(CTRL_U)?;
                }
                Mode::Dumb => term.expect_plain("new command (Enter keeps it)> ", SHORT),
            }
            // A harmless step became a delete: the analyzer sees it, whatever the model said.
            term.line("rm -rf proj/build")?;
            term.expect_plain("edited", SHORT);
            plan_key(&mut term, mode, ENTER)?;
            wait_question(&mut term, mode);
            respond(&mut term, mode, Answer::Cancel)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            assert!(
                ctx.home().join("proj/build/f").exists(),
                "{mode:?}: the edit ran unasked"
            );
            Ok(())
        });
    }
}

#[test]
fn i_shows_the_details_of_the_call_and_the_plan_comes_back() {
    pty_test(BIN, move |ctx| {
        let json = plan(&[("Say hi", "echo hi", "low")]);
        let server = Server::start_answering(Duration::ZERO, false, &[&json])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;
        term.command("? say hi")?;
        wait_plan(&mut term, Mode::Panel);
        plan_key(&mut term, Mode::Panel, b"i")?;
        term.expect_plain("AI call details", SHORT);
        term.expect_plain("OpenAI-compatible server", SHORT);
        term.expect_plain("Esc close", SHORT);
        term.expect_plain("model-one", SHORT);
        plan_key(&mut term, Mode::Panel, ESCAPE)?;
        // Back at the plan, still waiting for an answer: nothing ran.
        wait_plan(&mut term, Mode::Panel);
        plan_key(&mut term, Mode::Panel, ESCAPE)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        Ok(())
    });
}

#[test]
fn omsh_ai_details_says_so_before_any_call_and_shows_the_call_after() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            let json = plan(&[("Say hi", "echo hi", "low")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&json])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;
            term.command("omsh ai details")?;
            term.expect_plain("no AI call has been made in this shell session yet", SHORT);
            term.command("echo st-$?-end")?;
            term.expect("st-1-end", SHORT);

            term.command("? say hi")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ESCAPE)?;
            term.expect_plain("cancelled; nothing ran", SHORT);
            term.prompt();
            term.command("omsh ai details")?;
            match mode {
                Mode::Panel => {
                    term.expect_plain("AI call details", SHORT);
                    plan_key(&mut term, mode, ESCAPE)?;
                }
                Mode::Dumb => {
                    term.expect_plain("model     model-one", SHORT);
                    term.expect_plain("context   - / limit unknown", SHORT);
                    term.expect_plain("input     -", SHORT);
                }
            }
            term.prompt();
            Ok(())
        });
    }
}

#[test]
fn the_plan_panel_carries_the_numbers_of_the_call_and_tab_shows_more() {
    pty_test(BIN, move |ctx| {
        // 1,204 tokens in, 310 out of which 200 were thinking: 110 are the answer.
        let json = format!(
            "@usage 1204 310 200 | {}",
            plan(&[("Say hi", "echo hi", "low")])
        );
        let server = Server::start_answering(Duration::ZERO, false, &[&json])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).start()?;
        term.command("? say hi")?;
        // The default line: time, in and out. The thinking is not counted in `out` twice.
        term.expect_plain("in 1,204, out 110", SHORT);
        wait_plan(&mut term, Mode::Panel);
        // A number nobody sent is not there.
        assert!(
            !term.transcript().contains("thinking 200"),
            "not before Tab"
        );
        plan_key(&mut term, Mode::Panel, b"\t")?;
        term.expect_plain("thinking 200, cached -, total 1,514", SHORT);
        plan_key(&mut term, Mode::Panel, ESCAPE)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        term.prompt();
        // The details have the same numbers, and `-` where the server said nothing.
        term.command("omsh ai details")?;
        term.expect_plain("Esc close", SHORT);
        term.expect_plain("1,204", SHORT);
        plan_key(&mut term, Mode::Panel, ESCAPE)?;
        term.prompt();
        Ok(())
    });
}

#[test]
fn without_a_key_terminal_the_tokens_line_stays_as_it_was() {
    pty_test(BIN, move |ctx| {
        let json = format!(
            "@usage 1204 310 200 | {}",
            plan(&[("Say hi", "echo hi", "low")])
        );
        let server = Server::start_answering(Duration::ZERO, false, &[&json])?;
        configure(ctx, &server)?;
        let mut term = launch(ctx).dumb().start()?;
        term.command("? say hi")?;
        // The plain line: the completion as the server counted it, thinking and all.
        term.expect_plain(", 1,204 tokens in, 310 out", SHORT);
        wait_plan(&mut term, Mode::Dumb);
        plan_key(&mut term, Mode::Dumb, ESCAPE)?;
        term.expect_plain("cancelled; nothing ran", SHORT);
        assert!(
            !term.transcript().contains("thinking"),
            "{}",
            term.transcript()
        );
        Ok(())
    });
}

#[test]
fn a_critical_plan_is_not_answered_with_a_button() {
    for mode in MODES {
        pty_test(BIN, move |ctx| {
            // `~` is this test's own home directory. The plan is cancelled at the typed question;
            // it must never run.
            let keep = ctx.home().join("keep");
            fs::write(&keep, "x\n")?;
            let json = plan(&[("Wipe home", "rm -rf ~", "high")]);
            let server = Server::start_answering(Duration::ZERO, false, &[&json])?;
            configure(ctx, &server)?;
            let mut term = mode.launch(launch(ctx)).start()?;
            term.command("? wipe my home")?;
            wait_plan(&mut term, mode);
            plan_key(&mut term, mode, ENTER)?;
            term.expect_plain("this plan has a critical step", SHORT);
            // A critical question is typed, in both forms: `yes` or the target.
            term.expect_plain("to run it:", SHORT);
            assert!(
                !term.transcript().contains("Run anyway"),
                "{mode:?}: a critical plan got a button: {}",
                term.transcript()
            );
            term.line("no")?;
            term.expect_plain("cancelled", SHORT);
            assert!(keep.exists(), "{mode:?}: the plan ran");
            Ok(())
        });
    }
}
