//! Snapshots of the panels of UI upgrade phase 6: the jobs table, the question after a step was
//! stopped with Ctrl+Z, the step question with the steps of the plan above it, and the summary of
//! a plan that has ended. Each is drawn in the looks the spec asks for (normal, `NO_COLOR`,
//! ASCII) and with Latin and Bangla text.
//!
//! These panels have no numbered form: where nobody can press an arrow key the shell keeps the
//! plain text it had before (the `jobs` list, the plan's lines), and its tests pin that.
//!
//! Every snapshot is checked to be palette-only, and no row is wider than the window.

mod common;

use std::time::Duration;

use omsh_config::{Icons, Motion};
use omsh_ui::caps::{Caps, CapsFacts, Out};
use omsh_ui::panels::jobs_table::{JobsTable, RowKind, TableRow};
use omsh_ui::panels::plan::Risk;
use omsh_ui::panels::plan_run::{PausePanel, Progress, RowState, RunRow, summary};
use omsh_ui::panels::step::StepPanel;
use omsh_ui::text::{Line, Tone, render, width};
use omsh_ui::theme::Theme;
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
const SCRIPTS: [(&str, &str); 2] = [("latin", LATIN), ("bangla", BANGLA)];

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

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn run_row(title: &str, state: RowState) -> RunRow {
    RunRow {
        title: title.to_owned(),
        state,
    }
}

/// The plan of the examples: three steps, the second one is the file's.
fn progress(file: &str, second: RowState, third: RowState) -> Progress {
    Progress::new(
        vec![
            run_row("Go into the project folder", RowState::Done),
            run_row(&format!("Compress {file}"), second),
            run_row("Copy the archive", third),
        ],
        secs(74),
    )
}

fn table_row(number: u32, mark: char, kind: RowKind, word: &str, text: &str, s: u64) -> TableRow {
    TableRow {
        number,
        mark,
        kind,
        word: word.to_owned(),
        text: text.to_owned(),
        elapsed: secs(s),
    }
}

fn every_state(file: &str) -> Vec<TableRow> {
    vec![
        table_row(
            1,
            '+',
            RowKind::Stopped,
            "Stopped",
            &format!("vim {file}"),
            42,
        ),
        table_row(2, '-', RowKind::Running, "Running", "sleep 300", 5),
        table_row(
            3,
            ' ',
            RowKind::Done,
            "Done",
            &format!("tar czf {file}.gz {file}"),
            72,
        ),
        table_row(4, ' ', RowKind::Failed, "Terminated", "sleep 99", 3),
        table_row(5, ' ', RowKind::Failed, "Exit 3", "make", 0),
    ]
}

fn table(rows: Vec<TableRow>) -> JobsTable {
    JobsTable::new(Box::new(move || rows.clone()))
}

#[test]
fn the_jobs_table() {
    for (script, file) in SCRIPTS {
        let t = table(every_state(file));
        for name in LOOKS {
            let caps = look(name);
            let lines = t.render(&caps, WIDTH);
            insta::assert_snapshot!(
                format!("jobs_table_{script}_{name}"),
                written(&lines, &caps)
            );
        }
    }
}

#[test]
fn the_jobs_table_on_a_running_job_a_finished_job_and_when_empty() {
    let caps = look("normal");
    let mut t = table(every_state(LATIN));
    t.handle(Key::Down);
    insta::assert_snapshot!(
        "jobs_table_running_chosen",
        written(&t.render(&caps, WIDTH), &caps)
    );
    for _ in 0..2 {
        t.handle(Key::Down);
    }
    insta::assert_snapshot!(
        "jobs_table_finished_chosen",
        written(&t.render(&caps, WIDTH), &caps)
    );
    insta::assert_snapshot!(
        "jobs_table_empty",
        written(&table(Vec::new()).render(&caps, WIDTH), &caps)
    );
}

#[test]
fn the_question_before_a_kill() {
    for (script, file) in SCRIPTS {
        for name in LOOKS {
            let caps = look(name);
            let mut t = table(every_state(file));
            t.handle(Key::Down);
            t.handle(Key::Char('k'));
            let lines = t.render(&caps, WIDTH);
            insta::assert_snapshot!(format!("jobs_kill_{script}_{name}"), written(&lines, &caps));
        }
    }
}

#[test]
fn the_question_after_ctrl_z_stopped_a_step() {
    for (script, file) in SCRIPTS {
        for name in LOOKS {
            let caps = look(name);
            let panel = PausePanel::new(
                2,
                3,
                vec![
                    Line::new()
                        .tone(Tone::Dim, "$ ")
                        .tone(Tone::Plain, &format!("tar czf {file}.gz {file}")),
                ],
                progress(file, RowState::Paused, RowState::Pending),
            );
            let lines = panel.render(&caps, WIDTH);
            insta::assert_snapshot!(
                format!("plan_pause_{script}_{name}"),
                written(&lines, &caps)
            );
        }
    }
}

#[test]
fn the_pause_question_with_another_button_chosen() {
    let caps = look("normal");
    let mut panel = PausePanel::new(
        2,
        3,
        vec![Line::plain("$ sleep 300")],
        progress(LATIN, RowState::Paused, RowState::Pending),
    );
    panel.handle(Key::Left);
    insta::assert_snapshot!(
        "plan_pause_skip_chosen",
        written(&panel.render(&caps, WIDTH), &caps)
    );
}

#[test]
fn the_step_question_with_the_plan_above_it() {
    for (script, file) in SCRIPTS {
        for name in LOOKS {
            let caps = look(name);
            let panel = StepPanel::new(
                2,
                3,
                &format!("Compress {file}"),
                Risk::Safe,
                vec![Line::plain(&format!("$ tar czf {file}.gz {file}"))],
            )
            .with_progress(progress(file, RowState::Ask, RowState::Pending));
            let lines = panel.render(&caps, WIDTH);
            insta::assert_snapshot!(format!("plan_step_{script}_{name}"), written(&lines, &caps));
        }
    }
}

#[test]
fn the_summary_of_a_plan_that_has_ended() {
    for (script, file) in SCRIPTS {
        for name in LOOKS {
            let caps = look(name);
            let ended = [
                ("done", RowState::Done, RowState::Done),
                ("failed", RowState::Failed, RowState::NotRun),
                ("stopped", RowState::Paused, RowState::NotRun),
                ("skipped", RowState::Skipped, RowState::Done),
            ];
            for (how, second, third) in ended {
                let lines = summary(&progress(file, second, third), &caps, WIDTH);
                insta::assert_snapshot!(
                    format!("plan_summary_{how}_{script}_{name}"),
                    written(&lines, &caps)
                );
            }
        }
    }
}
