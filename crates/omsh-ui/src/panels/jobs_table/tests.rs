use std::cell::{Cell, RefCell};
use std::rc::Rc;

use omsh_config::Motion;

use super::*;
use crate::panels::test_support::caps;

fn row(number: u32, kind: RowKind, word: &str, text: &str, secs: u64) -> TableRow {
    TableRow {
        number,
        mark: ' ',
        kind,
        word: word.to_owned(),
        text: text.to_owned(),
        elapsed: Duration::from_secs(secs),
    }
}

fn running(number: u32, text: &str) -> TableRow {
    row(number, RowKind::Running, "Running", text, 5)
}

fn stopped(number: u32, text: &str) -> TableRow {
    row(number, RowKind::Stopped, "Stopped", text, 42)
}

fn done(number: u32, text: &str) -> TableRow {
    row(number, RowKind::Done, "Done", text, 72)
}

fn killed(number: u32, text: &str) -> TableRow {
    row(number, RowKind::Failed, "Terminated", text, 3)
}

/// The rows the table reads, which a test changes, and how often they were read.
struct Source {
    rows: Rc<RefCell<Vec<TableRow>>>,
    reads: Rc<Cell<usize>>,
}

fn table(rows: Vec<TableRow>) -> (JobsTable, Source) {
    let rows = Rc::new(RefCell::new(rows));
    let reads = Rc::new(Cell::new(0));
    let (shared, counted) = (Rc::clone(&rows), Rc::clone(&reads));
    let table = JobsTable::new(Box::new(move || {
        counted.set(counted.get() + 1);
        shared.borrow().clone()
    }));
    (table, Source { rows, reads })
}

fn text(table: &JobsTable, unicode: bool, width: u16) -> Vec<String> {
    table
        .render(&caps(Motion::Off, unicode), width)
        .iter()
        .map(Line::plain_text)
        .collect()
}

fn done_with(step: Step<(u32, TableAction)>) -> Option<(u32, TableAction)> {
    match step {
        Step::Done(answer) => Some(answer),
        _ => None,
    }
}

#[test]
fn an_action_fits_only_the_states_it_makes_sense_for() {
    use RowKind::{Done, Failed, Running, Stopped};
    use TableAction::{Background, Foreground, Kill, Stop};
    let table = [
        (Running, [true, false, true, true]),
        (Stopped, [true, true, false, true]),
        (Done, [false, false, false, false]),
        (Failed, [false, false, false, false]),
    ];
    for (kind, want) in table {
        let got = [Foreground, Background, Stop, Kill].map(|action| allowed(kind, action));
        assert_eq!(got, want, "{kind:?}");
    }
}

#[test]
fn the_letters_act_on_the_chosen_job_only_where_they_fit() {
    let (mut t, _) = table(vec![running(1, "sleep 30"), stopped(2, "vim")]);
    // Job 1 runs: Stop fits, Background does not.
    assert_eq!(done_with(t.handle(Key::Char('b'))), None);
    assert_eq!(
        done_with(t.handle(Key::Char('s'))),
        Some((1, TableAction::Stop))
    );
    assert_eq!(
        done_with(t.handle(Key::Char('f'))),
        Some((1, TableAction::Foreground))
    );
    t.handle(Key::Down);
    // Job 2 is stopped: Background fits, Stop does not.
    assert_eq!(done_with(t.handle(Key::Char('s'))), None);
    assert_eq!(
        done_with(t.handle(Key::Char('b'))),
        Some((2, TableAction::Background))
    );
    assert_eq!(
        done_with(t.handle(Key::Char('F'))),
        Some((2, TableAction::Foreground))
    );
}

#[test]
fn a_finished_job_has_no_action() {
    let (mut t, _) = table(vec![done(1, "make"), killed(2, "sleep 99")]);
    for key in [
        Key::Char('f'),
        Key::Char('b'),
        Key::Char('s'),
        Key::Char('k'),
        Key::Enter,
    ] {
        assert_eq!(done_with(t.handle(key)), None, "{key:?}");
        assert!(!t.asking());
    }
    t.handle(Key::Down);
    assert_eq!(done_with(t.handle(Key::Enter)), None);
    assert!(!t.animating());
}

#[test]
fn enter_takes_the_chosen_button_and_the_arrows_skip_the_buttons_that_do_not_fit() {
    let (mut t, _) = table(vec![running(1, "sleep 30")]);
    // The first button that fits is chosen: Foreground.
    assert_eq!(
        done_with(t.handle(Key::Enter)),
        Some((1, TableAction::Foreground))
    );
    // Right skips Background (a running job is not stopped) and lands on Stop.
    t.handle(Key::Right);
    assert_eq!(
        done_with(t.handle(Key::Enter)),
        Some((1, TableAction::Stop))
    );
    t.handle(Key::Tab);
    t.handle(Key::Left);
    t.handle(Key::BackTab);
    assert_eq!(
        done_with(t.handle(Key::Enter)),
        Some((1, TableAction::Foreground))
    );
}

#[test]
fn moving_to_another_job_goes_back_to_the_first_button_that_fits() {
    let (mut t, _) = table(vec![running(1, "a"), stopped(2, "b")]);
    t.handle(Key::Right);
    t.handle(Key::Down);
    assert_eq!(t.chosen().unwrap().number, 2);
    assert_eq!(
        done_with(t.handle(Key::Enter)),
        Some((2, TableAction::Foreground))
    );
    t.handle(Key::Up);
    assert_eq!(t.chosen().unwrap().number, 1);
    // Up at the top and Down at the bottom stay.
    t.handle(Key::Up);
    assert_eq!(t.chosen().unwrap().number, 1);
    t.handle(Key::Down);
    t.handle(Key::Down);
    assert_eq!(t.chosen().unwrap().number, 2);
}

#[test]
fn kill_asks_first_with_cancel_chosen() {
    let (mut t, _) = table(vec![running(1, "sleep 30")]);
    assert!(matches!(t.handle(Key::Char('k')), Step::Stay));
    assert!(t.asking());
    let all = text(&t, true, 80).join("\n");
    assert!(
        all.contains("Kill this job?") && all.contains("%1  sleep 30"),
        "{all}"
    );
    assert!(all.contains("Sends SIGTERM, as `kill %1`."), "{all}");
    // Enter takes Cancel: back to the table, nothing asked of the shell.
    assert!(matches!(t.handle(Key::Enter), Step::Stay));
    assert!(!t.asking());
    // Esc at the question also only cancels the question.
    t.handle(Key::Char('k'));
    assert!(matches!(t.handle(Key::Esc), Step::Stay));
    assert!(!t.asking());
    t.handle(Key::Char('k'));
    assert!(matches!(t.handle(Key::Char('n')), Step::Stay));
    // `k` at the question kills; so does moving to Kill and pressing Enter.
    t.handle(Key::Char('k'));
    assert_eq!(
        done_with(t.handle(Key::Char('k'))),
        Some((1, TableAction::Kill))
    );
    t.handle(Key::Esc);
    t.handle(Key::Char('k'));
    t.handle(Key::Left);
    assert_eq!(
        done_with(t.handle(Key::Enter)),
        Some((1, TableAction::Kill))
    );
}

#[test]
fn esc_in_the_table_closes_it() {
    let (mut t, _) = table(vec![running(1, "sleep 30")]);
    assert!(matches!(t.handle(Key::Esc), Step::Cancel));
    let (mut empty, _) = table(Vec::new());
    assert!(matches!(empty.handle(Key::Esc), Step::Cancel));
    assert!(matches!(empty.handle(Key::Enter), Step::Stay));
}

#[test]
fn the_rows_are_read_again_once_a_second_and_at_every_key() {
    let (mut t, source) = table(vec![running(1, "sleep 30")]);
    assert_eq!(source.reads.get(), 1);
    t.tick(Duration::from_millis(400));
    t.tick(Duration::from_millis(900));
    assert_eq!(source.reads.get(), 1, "no read before a second has passed");
    source.rows.borrow_mut()[0].elapsed = Duration::from_secs(6);
    t.tick(Duration::from_millis(1000));
    assert_eq!(source.reads.get(), 2);
    assert!(text(&t, true, 80).join("\n").contains("0:06"));
    t.tick(Duration::from_millis(1500));
    assert_eq!(source.reads.get(), 2);
    t.tick(Duration::from_millis(2000));
    assert_eq!(source.reads.get(), 3);
    t.handle(Key::Right);
    assert_eq!(source.reads.get(), 4, "a key reads the rows too");
}

#[test]
fn the_chosen_job_stays_chosen_when_the_rows_change() {
    let (mut t, source) = table(vec![running(1, "a"), running(2, "b"), running(3, "c")]);
    t.handle(Key::Down);
    assert_eq!(t.chosen().unwrap().number, 2);
    // Job 1 is forgotten: job 2 is now the first row, and still chosen.
    source.rows.borrow_mut().remove(0);
    t.tick(Duration::from_secs(1));
    assert_eq!(t.chosen().unwrap().number, 2);
    // Job 2 itself goes: the choice stays on the row at that place.
    source.rows.borrow_mut().remove(0);
    t.tick(Duration::from_secs(2));
    assert_eq!(t.chosen().unwrap().number, 3);
    source.rows.borrow_mut().clear();
    t.tick(Duration::from_secs(3));
    assert!(t.chosen().is_none());
    assert!(text(&t, true, 80).join("\n").contains("No jobs"));
}

#[test]
fn the_buttons_follow_a_job_that_changes_state() {
    let (mut t, source) = table(vec![running(1, "a")]);
    assert_eq!(done_with(t.handle(Key::Char('b'))), None);
    source.rows.borrow_mut()[0] = stopped(1, "a");
    t.tick(Duration::from_secs(1));
    assert_eq!(
        done_with(t.handle(Key::Char('b'))),
        Some((1, TableAction::Background))
    );
}

#[test]
fn a_job_that_goes_while_the_question_is_open_closes_the_question() {
    let (mut t, source) = table(vec![running(1, "a")]);
    t.handle(Key::Char('k'));
    assert!(t.asking());
    source.rows.borrow_mut().clear();
    t.tick(Duration::from_secs(1));
    assert!(!t.asking());
}

#[test]
fn only_a_table_with_a_live_job_asks_for_frames() {
    let (t, _) = table(vec![done(1, "a"), killed(2, "b")]);
    assert!(!t.animating());
    let (t, _) = table(vec![done(1, "a"), stopped(2, "b")]);
    assert!(t.animating());
    let (t, _) = table(Vec::new());
    assert!(!t.animating());
}

#[test]
fn the_table_has_the_state_words_and_the_time_before_the_command() {
    let mut rows = vec![
        stopped(1, "vim notes.txt"),
        running(2, "sleep 30"),
        done(3, "make"),
        killed(4, "sleep 99"),
        row(5, RowKind::Failed, "Exit 3", "false", 0),
    ];
    rows[0].mark = '+';
    rows[1].mark = '-';
    let (t, _) = table(rows);
    let lines = text(&t, true, 80);
    assert!(lines[0].starts_with("╭─ Jobs"), "{lines:?}");
    assert_eq!(lines[1], "│ ❯ %1+  ◼ Stopped      0:42  vim notes.txt");
    assert_eq!(lines[2], "│   %2-  ● Running      0:05  sleep 30");
    assert_eq!(lines[3], "│   %3   ✓ Done         1:12  make");
    assert_eq!(lines[4], "│   %4   ✘ Terminated   0:03  sleep 99");
    assert_eq!(lines[5], "│   %5   ✘ Exit 3       0:00  false");
    // The chosen button is padded and the others bracketed; Stop does not fit a stopped job, and
    // an action that does not fit is in parentheses (not in a weaker color).
    assert_eq!(lines[7], "│  Foreground   [Background]  (Stop)  [Kill]");
}

#[test]
fn the_ascii_table_uses_plain_marks_and_the_same_words() {
    let (t, _) = table(vec![running(1, "sleep 30"), done(2, "make")]);
    let lines = text(&t, false, 80);
    assert!(lines[0].starts_with("+- Jobs"), "{lines:?}");
    assert_eq!(lines[1], "| > %1   * Running   0:05  sleep 30");
    assert_eq!(lines[2], "|   %2   + Done      1:12  make");
    assert!(lines.last().unwrap().starts_with("+-"));
    assert!(lines.iter().all(|line| line.is_ascii()), "{lines:?}");
}

#[test]
fn a_long_command_is_cut_at_the_end_and_never_takes_the_time_with_it() {
    let long = "x".repeat(200);
    let (t, _) = table(vec![running(1, &long)]);
    for width in [30_u16, 50, 100] {
        let lines = text(&t, true, width);
        for line in &lines {
            assert!(crate::text::width(line) <= usize::from(width), "{line:?}");
        }
        assert!(lines[1].contains("0:05"), "{}", lines[1]);
    }
}

#[test]
fn bangla_and_hostile_commands_are_kept_and_made_inert() {
    let (t, _) = table(vec![
        running(1, "cat রিপোর্ট.txt"),
        running(2, "echo \x1b[31mred\u{202e}"),
    ]);
    let all = text(&t, true, 60).join("\n");
    assert!(all.contains("cat রিপোর্ট.txt"), "{all}");
    assert!(
        !all.contains('\x1b') && !all.contains('\u{202e}'),
        "{all:?}"
    );
}

#[test]
fn many_jobs_scroll_in_a_window_with_counts() {
    let rows: Vec<TableRow> = (1..=12).map(|n| running(n, &format!("job{n}"))).collect();
    let (mut t, _) = table(rows);
    let first = text(&t, true, 80).join("\n");
    assert!(first.contains("job8") && !first.contains("job9"), "{first}");
    assert!(
        first.contains("↓ 4 more") && first.matches("more").count() == 1,
        "{first}"
    );
    for _ in 0..11 {
        t.handle(Key::Down);
    }
    assert_eq!(t.chosen().unwrap().number, 12);
    let last = text(&t, true, 80).join("\n");
    assert!(last.contains("job12") && !last.contains("job4\n"), "{last}");
    assert!(
        last.contains("↑ 4 more") && last.matches("more").count() == 1,
        "{last}"
    );
}
