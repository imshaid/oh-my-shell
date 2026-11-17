use std::collections::VecDeque;

use omsh_config::Icons;

use super::*;
use crate::caps::{CapsFacts, Out};
use crate::text::{Style, Tone};
use crate::widgets::Step;
use crate::widgets::buttons::Buttons;

fn facts(width: u16, height: u16) -> CapsFacts {
    CapsFacts {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
        utf8: true,
        size: Some((width, height)),
        ..CapsFacts::default()
    }
}

fn caps() -> Caps {
    Caps::decide(facts(80, 24), Out::Stderr, Icons::Auto, None)
}

fn theme() -> Theme {
    Theme::default()
}

fn lines(rows: &[&str]) -> Vec<Line> {
    rows.iter().map(|row| Line::plain(row)).collect()
}

fn bytes(screen: &mut Screen, rows: &[&str]) -> String {
    String::from_utf8(screen.draw(&lines(rows), &theme(), &caps())).unwrap()
}

/// A terminal that understands only what the screen writes: text, `\r`, `\n`, cursor up, clear
/// line, clear to the end of the screen, and (ignored) colors and private modes. Anything else
/// is a bug in the screen and stops the test.
struct Term {
    rows: Vec<Vec<char>>,
    row: usize,
    column: usize,
}

impl Term {
    /// A terminal with `above` already on screen and the cursor on the empty line below it.
    fn below(above: &[&str]) -> Self {
        let mut rows: Vec<Vec<char>> = above.iter().map(|row| row.chars().collect()).collect();
        rows.push(Vec::new());
        Term {
            row: rows.len() - 1,
            rows,
            column: 0,
        }
    }

    fn feed(&mut self, text: &str) {
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' => self.column = 0,
                '\n' => {
                    self.row += 1;
                    while self.rows.len() <= self.row {
                        self.rows.push(Vec::new());
                    }
                }
                '\x1b' => {
                    assert_eq!(chars.next(), Some('['), "only CSI sequences are written");
                    let mut params = String::new();
                    let final_byte = loop {
                        let next = chars.next().expect("a sequence that ends");
                        if ('\x40'..='\x7e').contains(&next) {
                            break next;
                        }
                        params.push(next);
                    };
                    self.csi(&params, final_byte);
                }
                _ => {
                    let row = &mut self.rows[self.row];
                    while row.len() < self.column {
                        row.push(' ');
                    }
                    if row.len() == self.column {
                        row.push(c);
                    } else {
                        row[self.column] = c;
                    }
                    self.column += 1;
                }
            }
        }
    }

    fn csi(&mut self, params: &str, final_byte: char) {
        let number = params.parse::<usize>().unwrap_or(1);
        match final_byte {
            'A' => {
                assert!(self.row >= number, "moved above the top of the screen");
                self.row -= number;
            }
            'K' => {
                assert_eq!(params, "2", "only the whole line is cleared");
                self.rows[self.row].clear();
            }
            'J' => {
                assert!(params.is_empty(), "only clear-to-end is used");
                self.rows[self.row].truncate(self.column);
                self.rows.truncate(self.row + 1);
            }
            'm' | 'h' | 'l' => {}
            other => panic!("the screen wrote an unexpected sequence: {params:?} {other:?}"),
        }
    }

    /// The text of every row, trailing blanks and trailing empty rows removed.
    fn text(&self) -> Vec<String> {
        let mut rows: Vec<String> = self
            .rows
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_owned())
            .collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows
    }
}

#[test]
fn the_first_drawing_writes_every_row_cleared_first() {
    let mut screen = Screen::new();
    assert_eq!(
        bytes(&mut screen, &["one", "two", "three"]),
        "\r\x1b[2Kone\r\n\x1b[2Ktwo\r\n\x1b[2Kthree"
    );
}

#[test]
fn only_the_row_that_changed_is_written() {
    let mut screen = Screen::new();
    bytes(&mut screen, &["one", "two", "three"]);
    assert_eq!(
        bytes(&mut screen, &["one", "TWO", "three"]),
        "\x1b[2A\r\r\n\x1b[2KTWO\r\n"
    );
}

#[test]
fn a_drawing_that_changes_nothing_writes_nothing() {
    let mut screen = Screen::new();
    bytes(&mut screen, &["one", "two"]);
    assert_eq!(bytes(&mut screen, &["one", "two"]), "");
    assert_eq!(Screen::new().update(Vec::new(), true), Vec::<u8>::new());
}

#[test]
fn a_frame_that_shrinks_clears_the_rows_it_no_longer_has_and_goes_back_up() {
    let mut screen = Screen::new();
    bytes(&mut screen, &["one", "two", "three"]);
    assert_eq!(
        bytes(&mut screen, &["one"]),
        "\x1b[2A\r\r\n\x1b[2K\r\n\x1b[2K\x1b[2A"
    );
    assert_eq!(screen.drawn().len(), 1);
}

#[test]
fn a_frame_that_grows_makes_new_rows_below() {
    let mut screen = Screen::new();
    bytes(&mut screen, &["one"]);
    assert_eq!(
        bytes(&mut screen, &["one", "two", "three"]),
        "\r\r\n\x1b[2Ktwo\r\n\x1b[2Kthree"
    );
}

#[test]
fn erasing_goes_to_the_first_row_and_clears_to_the_end_of_the_screen() {
    let mut screen = Screen::new();
    assert_eq!(
        screen.erase(),
        Vec::<u8>::new(),
        "nothing drawn, nothing to erase"
    );
    bytes(&mut screen, &["one", "two", "three"]);
    assert_eq!(
        String::from_utf8(screen.erase()).unwrap(),
        "\x1b[2A\r\x1b[J"
    );
    assert_eq!(screen.erase(), Vec::<u8>::new(), "only once");
    bytes(&mut screen, &["x"]);
    assert_eq!(String::from_utf8(screen.erase()).unwrap(), "\r\x1b[J");
}

#[test]
fn synchronized_output_wraps_a_drawing_and_only_when_asked() {
    let mut with = Screen::new();
    let drawn = String::from_utf8(with.update(vec!["a".into(), "b".into()], true)).unwrap();
    assert!(
        drawn.starts_with("\x1b[?2026h") && drawn.ends_with("\x1b[?2026l"),
        "{drawn:?}"
    );
    let mut without = Screen::new();
    let drawn = String::from_utf8(without.update(vec!["a".into()], false)).unwrap();
    assert!(!drawn.contains("2026"));
}

#[test]
fn the_screen_shows_what_was_wanted_after_any_sequence_of_frames_and_erases_cleanly() {
    // A fixed pseudo-random walk over frames of 0 to 6 rows with changing contents.
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move |bound: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    for sync in [false, true] {
        let mut screen = Screen::new();
        let mut term = Term::below(&["$ ls", "$ rm -rf build"]);
        let top = term.row;
        for _ in 0..400 {
            let count = usize::try_from(next(7)).unwrap();
            let wanted: Vec<String> = (0..count)
                .map(|_| {
                    let word = format!("row {}", next(4));
                    // Some rows carry colors, as real ones do.
                    if next(2) == 0 {
                        format!("\x1b[31m{word}\x1b[0m")
                    } else {
                        word
                    }
                })
                .collect();
            term.feed(&String::from_utf8(screen.update(wanted.clone(), sync)).unwrap());
            let plain: Vec<String> = wanted
                .iter()
                .map(|row| row.replace("\x1b[31m", "").replace("\x1b[0m", ""))
                .collect();
            let mut expected = vec!["$ ls".to_owned(), "$ rm -rf build".to_owned()];
            expected.extend(plain.iter().cloned());
            while expected.last().is_some_and(String::is_empty) {
                expected.pop();
            }
            assert_eq!(term.text(), expected, "after a frame of {count} rows");
            assert_eq!(term.row, top + plain.len().saturating_sub(1));
        }
        term.feed(&String::from_utf8(screen.erase()).unwrap());
        assert_eq!(term.text(), ["$ ls", "$ rm -rf build"], "the menu is gone");
        assert_eq!(
            (term.row, term.column),
            (top, 0),
            "the cursor is where the menu began"
        );
    }
}

#[test]
fn a_frame_is_cut_to_one_column_less_and_one_row_less_than_the_window() {
    let small = Caps::decide(facts(20, 5), Out::Stderr, Icons::Auto, None);
    let wide = Line::plain(&"x".repeat(60));
    let many: Vec<Line> = (0..30).map(|_| wide.clone()).collect();
    let mut screen = Screen::new();
    let written = String::from_utf8(screen.draw(&many, &theme(), &small)).unwrap();
    assert_eq!(
        screen.drawn().len(),
        4,
        "a row is left for the prompt's own line"
    );
    assert!(
        screen
            .drawn()
            .iter()
            .all(|row| row == "xxxxxxxxxxxxxxxxxx…")
    );
    assert!(written.contains("xxxxxxxxxxxxxxxxxx…"));
    let mut term = Term::below(&[]);
    term.feed(&written);
    assert!(term.text().iter().all(|row| row.chars().count() <= 19));
}

#[test]
fn colors_come_from_the_theme_and_are_dropped_for_a_pipe() {
    let colored = vec![Line::new().then("bad", Style::of(Tone::Danger))];
    let mut screen = Screen::new();
    let written = String::from_utf8(screen.draw(&colored, &theme(), &caps())).unwrap();
    assert!(written.contains("\x1b[31mbad\x1b[0m"));
}

struct ScriptedKeys {
    inputs: VecDeque<io::Result<Input>>,
    waits: Vec<Option<Duration>>,
    /// How many keys were read when each `discard` happened.
    discards: Vec<usize>,
    read: usize,
}

impl ScriptedKeys {
    fn new(inputs: Vec<Input>) -> Self {
        ScriptedKeys {
            inputs: inputs.into_iter().map(Ok).collect(),
            waits: Vec::new(),
            discards: Vec::new(),
            read: 0,
        }
    }
}

impl KeySource for ScriptedKeys {
    fn next(&mut self, wait: Option<Duration>) -> io::Result<Input> {
        self.waits.push(wait);
        self.read += 1;
        self.inputs.pop_front().unwrap_or(Ok(Input::Closed))
    }

    fn discard(&mut self) {
        self.discards.push(self.read);
    }
}

fn buttons() -> Buttons {
    Buttons::new(
        &[("Move to trash", 't'), ("Run anyway", 'y'), ("Cancel", 'n')],
        2,
    )
}

fn run_buttons(inputs: Vec<Input>) -> (Option<usize>, String, ScriptedKeys) {
    let mut keys = ScriptedKeys::new(inputs);
    let mut out = Vec::new();
    let answer = run(&mut buttons(), &mut keys, &mut out, &theme(), &caps()).unwrap();
    (answer, String::from_utf8(out).unwrap(), keys)
}

#[test]
fn run_returns_the_choice_and_leaves_the_screen_as_it_found_it() {
    let (answer, written, _) = run_buttons(vec![Input::Key(Key::Left), Input::Key(Key::Enter)]);
    assert_eq!(answer, Some(1));
    assert!(
        written.starts_with("\x1b[?25l"),
        "the cursor is hidden first"
    );
    assert!(written.ends_with("\x1b[?25h"), "and shown last");
    let mut term = Term::below(&["$ rm -rf build"]);
    term.feed(&written);
    assert_eq!(term.text(), ["$ rm -rf build"]);
    assert_eq!((term.row, term.column), (1, 0));
}

#[test]
fn run_ends_on_esc_a_letter_or_a_closed_terminal_each_with_its_answer() {
    assert_eq!(run_buttons(vec![Input::Key(Key::Esc)]).0, None);
    assert_eq!(run_buttons(vec![Input::Key(Key::Char('t'))]).0, Some(0));
    assert_eq!(
        run_buttons(vec![Input::Key(Key::Enter)]).0,
        Some(2),
        "the safe one is the default"
    );
    assert_eq!(run_buttons(vec![Input::Tick, Input::Closed]).0, None);
    assert_eq!(
        run_buttons(vec![Input::Key(Key::Char('x'))]).0,
        None,
        "then closed"
    );
}

#[test]
fn a_menu_that_does_not_move_waits_for_ever_with_no_timer() {
    let (_, _, keys) = run_buttons(vec![
        Input::Key(Key::Right),
        Input::Key(Key::Right),
        Input::Key(Key::Esc),
    ]);
    assert_eq!(keys.waits, [None, None, None]);
}

struct Pulse {
    elapsed: Duration,
}

impl Widget for Pulse {
    type Output = ();

    fn handle(&mut self, key: Key) -> Step<()> {
        if key == Key::Enter {
            Step::Done(())
        } else {
            Step::Stay
        }
    }

    fn render(&self, caps: &Caps, _width: u16) -> Vec<Line> {
        vec![Line::plain(caps.spinner().frame(self.elapsed))]
    }

    fn animating(&self) -> bool {
        true
    }

    fn tick(&mut self, since_open: Duration) {
        self.elapsed = since_open;
    }
}

#[test]
fn a_menu_that_moves_wakes_up_at_the_frame_rate_and_not_when_motion_is_off() {
    let mut keys = ScriptedKeys::new(vec![Input::Tick, Input::Key(Key::Enter)]);
    let mut out = Vec::new();
    run(
        &mut Pulse {
            elapsed: Duration::ZERO,
        },
        &mut keys,
        &mut out,
        &theme(),
        &caps(),
    )
    .unwrap();
    assert!(
        keys.waits
            .iter()
            .all(|wait| wait.is_some_and(|w| w <= Duration::from_millis(100)))
    );
    assert_eq!(keys.waits.len(), 2);

    let still = Caps::decide(
        facts(80, 24),
        Out::Stderr,
        Icons::Auto,
        Some(omsh_config::Motion::Off),
    );
    let mut keys = ScriptedKeys::new(vec![Input::Key(Key::Enter)]);
    run(
        &mut Pulse {
            elapsed: Duration::ZERO,
        },
        &mut keys,
        &mut Vec::new(),
        &theme(),
        &still,
    )
    .unwrap();
    assert_eq!(keys.waits, [None], "no motion, no timer");
}

#[test]
fn run_cleans_up_when_the_keys_fail() {
    struct Broken;
    impl KeySource for Broken {
        fn next(&mut self, _: Option<Duration>) -> io::Result<Input> {
            Err(io::Error::other("the terminal went away"))
        }
    }
    let mut out = Vec::new();
    let error = run(&mut buttons(), &mut Broken, &mut out, &theme(), &caps()).unwrap_err();
    assert_eq!(error.to_string(), "the terminal went away");
    let written = String::from_utf8(out).unwrap();
    assert!(written.starts_with("\x1b[?25l") && written.ends_with("\x1b[?25h"));
    let mut term = Term::below(&[]);
    term.feed(&written);
    assert_eq!(term.text(), Vec::<String>::new());
}

#[test]
fn run_writes_nothing_and_refuses_when_nobody_can_answer() {
    for facts in [
        CapsFacts {
            stdin_tty: false,
            ..facts(80, 24)
        },
        CapsFacts {
            stderr_tty: false,
            ..facts(80, 24)
        },
    ] {
        let caps = Caps::decide(facts, Out::Stderr, Icons::Auto, None);
        let mut out = Vec::new();
        let error = run(
            &mut buttons(),
            &mut ScriptedKeys::new(vec![]),
            &mut out,
            &theme(),
            &caps,
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert!(out.is_empty());
    }
}

#[test]
fn ctrl_c_is_told_from_esc_by_run_menu() {
    let run_with = |input: Input| {
        let mut keys = ScriptedKeys::new(vec![input]);
        let mut out = Vec::new();
        run_menu(&mut buttons(), &mut keys, &mut out, &theme(), &caps()).unwrap()
    };
    assert_eq!(run_with(Input::Interrupted), Ended::Interrupted);
    assert_eq!(run_with(Input::Key(Key::Esc)), Ended::Cancelled);
    assert_eq!(run_with(Input::Closed), Ended::Closed);
    assert_eq!(run_with(Input::Key(Key::Enter)), Ended::Chosen(2));
}

#[test]
fn waiting_keys_are_discarded_before_the_first_drawing_and_after_the_menu_is_gone() {
    let mut keys = ScriptedKeys::new(vec![Input::Key(Key::Left), Input::Key(Key::Enter)]);
    let mut out = Vec::new();
    run_menu(&mut buttons(), &mut keys, &mut out, &theme(), &caps()).unwrap();
    // Once before any key was read (so nothing typed before the menu can answer it), once after
    // the last one (so nothing typed for it reaches the next prompt).
    assert_eq!(keys.discards, [0, 2]);
}

#[test]
fn the_discard_after_the_menu_also_happens_when_the_menu_fails() {
    struct Failing(Vec<usize>);
    impl KeySource for Failing {
        fn next(&mut self, _wait: Option<Duration>) -> io::Result<Input> {
            Err(io::Error::other("the terminal broke"))
        }
        fn discard(&mut self) {
            self.0.push(0);
        }
    }
    let mut keys = Failing(Vec::new());
    let mut out = Vec::new();
    let result = run_menu(&mut buttons(), &mut keys, &mut out, &theme(), &caps());
    assert!(result.is_err());
    assert_eq!(keys.0.len(), 2);
    assert!(String::from_utf8(out).unwrap().ends_with("\x1b[?25h"));
}

#[test]
fn a_ctrl_c_event_is_an_interrupt_and_a_plain_c_is_a_letter() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let key = |code, modifiers| Event::Key(KeyEvent::new(code, modifiers));
    assert_eq!(
        input_of(&key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Some(Input::Interrupted)
    );
    assert_eq!(
        input_of(&key(KeyCode::Char('c'), KeyModifiers::NONE)),
        Some(Input::Key(Key::Char('c')))
    );
    assert_eq!(
        input_of(&key(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        None
    );
    assert_eq!(input_of(&Event::FocusGained), None);
}
