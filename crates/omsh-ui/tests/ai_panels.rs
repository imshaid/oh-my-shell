//! Snapshots of the AI panels (UI upgrade, phase 4): the plan, the question before a step, the
//! details of a call, "AI is not set up", the lists of the setup wizard and the info line, in the
//! looks the spec asks for (normal, `NO_COLOR`, ASCII) and with Latin and Bangla text. The lists
//! also have the numbered form that a pipe or a dumb terminal gets.
//!
//! Every snapshot is checked to be palette-only, and no row is wider than the window.

mod common;

use std::time::Duration;

use omsh_config::{Icons, Motion};
use omsh_ui::caps::{Caps, CapsFacts, Out};
use omsh_ui::panels::choose::Choose;
use omsh_ui::panels::details::Details;
use omsh_ui::panels::facts::CallFacts;
use omsh_ui::panels::meter::Meter;
use omsh_ui::panels::plan::{CallLine, PlanPanel, Risk, StepView};
use omsh_ui::panels::set_up::NotSetUp;
use omsh_ui::panels::step::StepPanel;
use omsh_ui::text::{Line, Tone, render, width};
use omsh_ui::theme::Theme;
use omsh_ui::widgets::list::ListItem;
use omsh_ui::widgets::{Key, Widget};

const WIDTH: u16 = 78;

const GOOD: CapsFacts = CapsFacts {
    stdin_tty: true,
    stdout_tty: true,
    stderr_tty: true,
    dumb: false,
    no_color: false,
    utf8: true,
    ci: false,
    ssh: false,
    tmux: false,
    env_motion: None,
    size: Some((80, 24)),
};

fn look(name: &str) -> Caps {
    let facts = match name {
        "normal" => GOOD,
        "no_color" => CapsFacts {
            no_color: true,
            ..GOOD
        },
        "ascii" => CapsFacts {
            utf8: false,
            ..GOOD
        },
        other => panic!("no look called {other}"),
    };
    // No motion: a snapshot is a still picture of the finished panel.
    Caps::decide(facts, Out::Stderr, Icons::Auto, Some(Motion::Off))
}

const LOOKS: [&str; 3] = ["normal", "no_color", "ascii"];
const LATIN: &str = "build/output.o";
const BANGLA: &str = "রিপোর্ট.txt";

fn written(lines: &[Line], caps: &Caps) -> String {
    let rows = render(lines, &Theme::default(), caps);
    let joined = rows.join("\n");
    common::assert_palette_only(&joined);
    for row in lines {
        assert!(
            width(&row.plain_text()) <= usize::from(WIDTH),
            "a row is wider than the panel: {:?}",
            row.plain_text()
        );
    }
    common::visible(&joined)
}

fn facts() -> CallFacts {
    CallFacts {
        provider: "Gemini".to_owned(),
        model: "gemini-2.5-flash".to_owned(),
        effort: None,
        time: Some(Duration::from_millis(3240)),
        input: Some(1204),
        thinking: Some(200),
        output: Some(110),
        cached: None,
        total: Some(1514),
        tokens_per_sec: None,
        context_used: Some(1514),
        context_limit: Some(32768),
        limit_note: Some("context limit: inputTokenLimit from the Gemini models API".to_owned()),
    }
}

fn call() -> CallLine {
    CallLine {
        facts: facts(),
        fields: ["time", "in", "out", "ctx"].map(str::to_owned).to_vec(),
        thinking_warn: 4,
        meter: Meter::new(Some(1514), Some(32768), None),
    }
}

fn steps(file: &str) -> Vec<StepView> {
    let step = |title: &str, risk: Risk, command: &str, why: &str| StepView {
        title: title.to_owned(),
        risk,
        note: String::new(),
        body: vec![
            Line::new().tone(Tone::Dim, "$ ").tone(Tone::Plain, command),
            Line::plain(why),
        ],
    };
    vec![
        step(
            "Look at the file",
            Risk::Safe,
            &format!("ls -l {file}"),
            "Lists it.",
        ),
        step(
            "Copy it",
            Risk::Caution,
            &format!("cp {file} backup"),
            "Makes a copy.",
        ),
        step(
            "Remove it",
            Risk::Danger,
            &format!("rm -rf {file}"),
            "Deletes the file for good.",
        ),
    ]
}

fn plan_panel(file: &str, with_call: bool) -> PlanPanel {
    PlanPanel::new(
        "Plan from Gemini (gemini-2.5-flash)",
        vec![Line::plain("Back the file up, then remove it.")],
        steps(file),
        Vec::new(),
        with_call.then(call),
    )
}

#[test]
fn the_plan_panel() {
    for (script, file) in [("latin", LATIN), ("bangla", BANGLA)] {
        for name in LOOKS {
            let caps = look(name);
            let lines = plan_panel(file, true).render(&caps, WIDTH);
            insta::assert_snapshot!(format!("ai_plan_{script}_{name}"), written(&lines, &caps));
        }
    }
}

#[test]
fn the_plan_panel_expanded_and_with_the_step_question() {
    let caps = look("normal");
    let mut expanded = plan_panel(LATIN, true).with_expanded(true);
    insta::assert_snapshot!(
        "ai_plan_expanded",
        written(&expanded.render(&caps, WIDTH), &caps)
    );
    expanded.handle(Key::Char('e'));
    insta::assert_snapshot!(
        "ai_plan_which_step",
        written(&expanded.render(&caps, WIDTH), &caps)
    );
    let one = PlanPanel::new(
        "Plan from Gemini (gemini-2.5-flash)",
        Vec::new(),
        steps(LATIN).into_iter().take(1).collect(),
        Vec::new(),
        None,
    );
    insta::assert_snapshot!(
        "ai_plan_one_step_no_call",
        written(&one.render(&caps, WIDTH), &caps)
    );
}

#[test]
fn the_question_before_a_step() {
    for (script, file) in [("latin", LATIN), ("bangla", BANGLA)] {
        for name in LOOKS {
            let caps = look(name);
            let panel = StepPanel::new(
                3,
                3,
                "Remove it",
                Risk::Danger,
                vec![
                    Line::new()
                        .tone(Tone::Dim, "$ ")
                        .tone(Tone::Plain, &format!("rm -rf {file}")),
                    Line::plain("Deletes the file for good."),
                ],
            );
            insta::assert_snapshot!(
                format!("ai_step_{script}_{name}"),
                written(&panel.render(&caps, WIDTH), &caps)
            );
        }
    }
}

#[test]
fn the_details_of_a_call() {
    let mut second = facts();
    second.provider = "Ollama (local)".to_owned();
    second.model = "llama3.2".to_owned();
    second.thinking = None;
    second.tokens_per_sec = Some(20.0);
    second.context_limit = None;
    second.limit_note = None;
    for name in LOOKS {
        let caps = look(name);
        let panel = Details::new(vec![facts(), second.clone()], 0);
        insta::assert_snapshot!(
            format!("ai_details_{name}"),
            written(&panel.render(&caps, WIDTH), &caps)
        );
    }
    let caps = look("normal");
    let mut panel = Details::new(vec![facts(), second], 0);
    panel.handle(Key::Right);
    insta::assert_snapshot!(
        "ai_details_second_tab",
        written(&panel.render(&caps, WIDTH), &caps)
    );
}

#[test]
fn ai_is_not_set_up() {
    for name in LOOKS {
        let caps = look(name);
        insta::assert_snapshot!(
            format!("ai_not_set_up_{name}"),
            written(&NotSetUp::new().render(&caps, WIDTH), &caps)
        );
    }
}

fn models(names: &[&str]) -> Vec<ListItem> {
    names.iter().map(|n| ListItem::new(n, "")).collect()
}

#[test]
fn the_lists_of_the_setup_wizard() {
    for name in LOOKS {
        let caps = look(name);
        let panel = Choose::new(
            "Choose a model",
            "models",
            models(&["gemini-2.5-flash", "gemini-2.5-pro"]),
            Some("gemini-2.5-pro"),
        )
        .with_all(models(&[
            "gemini-2.5-flash",
            "gemini-2.5-flash-image",
            "gemini-2.5-flash-preview-tts",
            "gemini-2.5-pro",
            "imagen-4.0-generate-001",
        ]))
        .intro(vec![Line::plain(
            "Type to filter. Esc lets you type a name that is not in the list.",
        )]);
        insta::assert_snapshot!(
            format!("ai_choose_{name}"),
            written(&panel.render(&caps, WIDTH), &caps)
        );
    }
    // Showing all of them, and a Bangla model name.
    let caps = look("normal");
    let mut panel = Choose::new("Choose a model", "models", models(&["a"]), None)
        .with_all(models(&["a", "মডেল-এক", "tts-1"]));
    panel.handle(Key::Tab);
    insta::assert_snapshot!(
        "ai_choose_all_bangla",
        written(&panel.render(&caps, WIDTH), &caps)
    );
}

#[test]
fn the_lists_have_a_numbered_form_for_a_pipe() {
    use omsh_ui::widgets::list::List;
    let list = List::new(
        vec![
            ListItem::new("Ollama (local)", "runs on this computer, needs no key"),
            ListItem::new("Anthropic", ""),
            ListItem::new("মডেল-এক", ""),
        ],
        6,
    );
    let text = list.to_numbered("Choose a provider").render().join("\n");
    assert!(text.is_ascii() || text.contains("মডেল"), "{text}");
    insta::assert_snapshot!("ai_list_numbered", text);
}
