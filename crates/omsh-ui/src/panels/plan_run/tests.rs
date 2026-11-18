use omsh_config::Motion;

use super::*;
use crate::panels::step::StepPanel;
use crate::panels::test_support::caps;

fn row(title: &str, state: RowState) -> RunRow {
    RunRow {
        title: title.to_owned(),
        state,
    }
}

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn text(lines: &[Line]) -> Vec<String> {
    lines.iter().map(Line::plain_text).collect()
}

#[test]
fn the_clock_is_minutes_and_seconds_and_hours_from_an_hour_on() {
    assert_eq!(clock(secs(0)), "0:00");
    assert_eq!(clock(Duration::from_millis(59_999)), "0:59");
    assert_eq!(clock(secs(65)), "1:05");
    assert_eq!(clock(secs(3599)), "59:59");
    assert_eq!(clock(secs(3600)), "1:00:00");
    assert_eq!(clock(secs(3600 + 125)), "1:02:05");
}

#[test]
fn the_bar_counts_the_steps_that_ran_and_not_the_ones_that_were_skipped() {
    let progress = Progress::new(
        vec![
            row("One", RowState::Done),
            row("Two", RowState::Skipped),
            row("Three", RowState::Ask),
        ],
        secs(4),
    );
    let lines = text(&progress.lines(&caps(Motion::Off, true)));
    // One of three ran: three cells of ten.
    assert_eq!(
        lines[0], "███░░░░░░░  1 of 3 done, 1 skipped  0:04",
        "{lines:?}"
    );
    assert_eq!(lines[1], "✓ 1. One");
    assert_eq!(lines[2], "– 2. Two  (skipped)");
    assert_eq!(lines[3], "? 3. Three  (asks first)");
}

#[test]
fn nothing_is_filled_before_a_step_has_run_and_all_is_filled_when_all_ran() {
    let none = Progress::new(vec![row("One", RowState::Ask)], secs(0));
    assert!(text(&none.lines(&caps(Motion::Off, true)))[0].starts_with("░░░░░░░░░░  0 of 1 done"));
    let all = Progress::new(vec![row("One", RowState::Done)], secs(0));
    assert!(text(&all.lines(&caps(Motion::Off, true)))[0].starts_with("██████████  1 of 1 done"));
    let empty = Progress::new(Vec::new(), secs(0));
    assert!(text(&empty.lines(&caps(Motion::Off, true)))[0].starts_with("░░░░░░░░░░  0 of 0 done"));
}

#[test]
fn every_state_has_a_mark_and_a_word_so_color_is_never_the_only_sign() {
    let progress = Progress::new(
        vec![
            row("a", RowState::Done),
            row("b", RowState::Failed),
            row("c", RowState::Paused),
            row("d", RowState::NotRun),
            row("e", RowState::Pending),
        ],
        secs(0),
    );
    let unicode = text(&progress.lines(&caps(Motion::Off, true)));
    assert_eq!(
        &unicode[1..],
        [
            "✓ 1. a",
            "✗ 2. b  (failed)",
            "◼ 3. c  (paused)",
            "– 4. d  (not run)",
            "○ 5. e"
        ]
    );
    let ascii = text(&progress.lines(&caps(Motion::Off, false)));
    assert_eq!(
        ascii[0],
        "####------  1 of 5 done  0:00"
            .replace("####", "##")
            .replace("------", "--------")
    );
    assert_eq!(
        &ascii[1..],
        [
            "+ 1. a",
            "x 2. b  (failed)",
            "# 3. c  (paused)",
            "- 4. d  (not run)",
            "o 5. e"
        ]
    );
}

#[test]
fn the_tone_is_red_for_a_failure_green_when_all_ran_and_blue_otherwise() {
    let tone = |states: &[RowState]| {
        Progress::new(
            states.iter().map(|state| row("x", *state)).collect(),
            secs(0),
        )
        .tone()
    };
    assert_eq!(
        tone(&[RowState::Done, RowState::Failed, RowState::NotRun]),
        Tone::Danger
    );
    assert_eq!(tone(&[RowState::Done, RowState::Skipped]), Tone::Ok);
    assert_eq!(
        tone(&[RowState::Done, RowState::Paused, RowState::Pending]),
        Tone::Info
    );
}

fn pause() -> PausePanel {
    PausePanel::new(
        2,
        3,
        vec![Line::plain("$ sleep 30")],
        Progress::new(
            vec![
                row("One", RowState::Done),
                row("Wait", RowState::Paused),
                row("Three", RowState::Pending),
            ],
            secs(7),
        ),
    )
}

#[test]
fn stop_plan_is_chosen_when_the_pause_panel_opens() {
    let mut p = pause();
    assert_eq!(p.chosen(), PauseChoice::Stop);
    assert!(matches!(
        p.handle(Key::Enter),
        Step::Done(PauseChoice::Stop)
    ));
}

#[test]
fn the_letters_and_esc_choose_at_once_in_the_pause_panel() {
    for (key, want) in [
        (Key::Char('c'), PauseChoice::Continue),
        (Key::Char('s'), PauseChoice::Skip),
        (Key::Char('n'), PauseChoice::Stop),
        (Key::Char('b'), PauseChoice::Background),
        (Key::Esc, PauseChoice::Stop),
    ] {
        match pause().handle(key) {
            Step::Done(got) => assert_eq!(got, want, "{key:?}"),
            _ => panic!("{key:?}"),
        }
    }
}

#[test]
fn the_arrows_move_between_the_four_buttons() {
    let mut p = pause();
    p.handle(Key::Right);
    assert_eq!(p.chosen(), PauseChoice::Background);
    p.handle(Key::Right);
    assert_eq!(p.chosen(), PauseChoice::Continue);
    p.handle(Key::Left);
    assert_eq!(p.chosen(), PauseChoice::Background);
    assert!(matches!(
        p.handle(Key::Enter),
        Step::Done(PauseChoice::Background)
    ));
}

#[test]
fn the_pause_panel_names_the_step_shows_the_plan_and_says_what_the_chosen_button_does() {
    let mut p = pause();
    let rows = text(&p.render(&caps(Motion::Off, true), 90));
    assert!(rows[0].starts_with("╭─ Paused: step 2 of 3"), "{}", rows[0]);
    let all = rows.join("\n");
    assert!(all.contains("◼ 2. Wait  (paused)"), "{all}");
    assert!(all.contains("$ sleep 30"), "{all}");
    assert!(
        all.contains("Continue") && all.contains("To background"),
        "{all}"
    );
    assert!(
        all.contains("leave it stopped (see jobs); the rest of the plan does not run"),
        "{all}"
    );
    p.handle(Key::Left);
    let all = text(&p.render(&caps(Motion::Off, true), 90)).join("\n");
    assert!(
        all.contains("end this step and go on with the next one"),
        "{all}"
    );
}

#[test]
fn the_pause_panel_fits_a_narrow_window_and_makes_hostile_titles_harmless() {
    let p = PausePanel::new(
        1,
        1,
        vec![Line::plain("$ x")],
        Progress::new(
            vec![row("evil\x1b[31m title\u{202e}", RowState::Paused)],
            secs(0),
        ),
    );
    for width in [30_u16, 50, 100] {
        for line in p.render(&caps(Motion::Off, true), width) {
            let shown = line.plain_text();
            assert!(
                !shown.contains('\x1b') && !shown.contains('\u{202e}'),
                "{shown:?}"
            );
            assert!(
                crate::text::width(&shown) <= usize::from(width),
                "{shown:?}"
            );
        }
    }
}

#[test]
fn the_summary_is_an_open_panel_without_buttons() {
    let progress = Progress::new(
        vec![row("One", RowState::Done), row("Two", RowState::Failed)],
        secs(3),
    );
    let rows = text(&summary(&progress, &caps(Motion::Off, true), 60));
    assert!(rows[0].starts_with("╭─ Plan"), "{}", rows[0]);
    assert!(rows.last().unwrap().starts_with('╰'));
    assert!(
        rows.iter()
            .all(|row| !row.contains('╮') && !row.contains('╯'))
    );
    assert!(rows.join("\n").contains("✗ 2. Two  (failed)"));
}

#[test]
fn the_step_question_shows_the_plan_above_it_only_when_it_is_given_the_progress() {
    let plain = StepPanel::new(
        2,
        3,
        "Second",
        crate::panels::plan::Risk::Safe,
        vec![Line::plain("$ ls")],
    );
    let before = text(&plain.render(&caps(Motion::Off, true), 80));
    assert!(!before.join("\n").contains("of 3 done"));
    let with = StepPanel::new(
        2,
        3,
        "Second",
        crate::panels::plan::Risk::Safe,
        vec![Line::plain("$ ls")],
    )
    .with_progress(Progress::new(
        vec![
            row("First", RowState::Done),
            row("Second", RowState::Ask),
            row("Third", RowState::Pending),
        ],
        secs(2),
    ));
    let after = text(&with.render(&caps(Motion::Off, true), 80)).join("\n");
    assert!(after.contains("1 of 3 done"), "{after}");
    assert!(after.contains("? 2. Second  (asks first)"), "{after}");
    assert!(after.contains("Run step"), "{after}");
}
