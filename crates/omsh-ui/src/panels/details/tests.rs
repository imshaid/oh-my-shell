use omsh_config::Motion;

use super::*;
use crate::panels::test_support::caps;
use crate::text::width;

fn ollama() -> CallFacts {
    CallFacts {
        provider: "Ollama".to_owned(),
        model: "llama3.2".to_owned(),
        effort: None,
        time: Some(Duration::from_millis(3200)),
        input: Some(1204),
        thinking: None,
        output: Some(310),
        cached: None,
        total: Some(1514),
        tokens_per_sec: Some(20.0),
        context_used: Some(1514),
        context_limit: Some(8192),
        limit_note: Some(
            "context limit: context_length of the loaded model (Ollama /api/ps)".to_owned(),
        ),
    }
}

fn gemini() -> CallFacts {
    CallFacts {
        provider: "Gemini".to_owned(),
        model: "gemini-2.5-flash".to_owned(),
        input: Some(100),
        thinking: Some(30),
        output: Some(20),
        cached: Some(40),
        total: Some(150),
        context_used: Some(150),
        ..CallFacts::default()
    }
}

fn rows(d: &Details, motion: Motion, unicode: bool) -> Vec<String> {
    d.render(&caps(motion, unicode), 100)
        .iter()
        .map(Line::plain_text)
        .collect()
}

fn at(d: &mut Details, ms: u64) {
    d.tick(Duration::from_millis(ms));
}

#[test]
fn the_table_has_every_row_and_a_dash_for_what_was_not_reported() {
    let d = Details::new(vec![ollama()], 0);
    let all = rows(&d, Motion::Off, true).join("\n");
    for label in [
        "Provider", "Model", "Effort", "Context", "Input", "Thinking", "Output", "Cached", "Total",
        "Time", "Speed",
    ] {
        assert!(all.contains(label), "{label}\n{all}");
    }
    let row = |label: &str| {
        rows(&d, Motion::Off, true)
            .into_iter()
            .find(|r| r.contains(label))
            .unwrap()
    };
    assert!(
        row("Thinking").contains('–') && !row("Thinking").contains(" 0"),
        "{}",
        row("Thinking")
    );
    assert!(row("Cached").contains('–'));
    assert!(row("Effort").contains('–'));
    assert!(row("Input").contains("1,204"));
    assert!(
        row("Speed").contains("20.0 tok/s (derived)"),
        "{}",
        row("Speed")
    );
    assert!(row("Time").contains("3.2 s"));
    assert!(
        row("Context").contains("ctx ≈ 1,514 / 8,192  18%"),
        "{}",
        row("Context")
    );
}

#[test]
fn a_source_note_says_where_the_limit_came_from_or_that_it_is_unknown() {
    let known = rows(&Details::new(vec![ollama()], 0), Motion::Off, true).join("\n");
    assert!(
        known.contains("context_length of the loaded model"),
        "{known}"
    );
    let unknown = rows(&Details::new(vec![gemini()], 0), Motion::Off, true).join("\n");
    assert!(
        unknown.contains("context limit unknown") && unknown.contains("limit unknown"),
        "{unknown}"
    );
    assert!(unknown.contains("ctx is approximate"));
}

#[test]
fn it_is_a_full_box_and_every_row_has_both_edges_at_the_same_column() {
    let d = Details::new(vec![ollama(), gemini()], 0);
    let all = rows(&d, Motion::Off, true);
    assert!(
        all[0].starts_with('╭') && all[0].ends_with('╮'),
        "{}",
        all[0]
    );
    let w = width(&all[0]);
    assert!(w <= 72);
    for r in &all[1..all.len() - 1] {
        assert!(
            r.starts_with('│') && r.ends_with('│') && width(r) == w,
            "{r:?}"
        );
    }
    let ascii = rows(&d, Motion::Off, false);
    assert!(ascii.iter().all(|r| r.is_ascii()), "{ascii:?}");
    assert!(ascii[0].starts_with('+'));
}

#[test]
fn left_and_right_switch_the_provider_and_wrap() {
    let mut d = Details::new(vec![ollama(), gemini()], 0);
    assert!(rows(&d, Motion::Off, true).join("\n").contains("llama3.2"));
    d.handle(Key::Right);
    assert_eq!(d.selected(), 1);
    assert!(
        rows(&d, Motion::Off, true)
            .join("\n")
            .contains("gemini-2.5-flash")
    );
    d.handle(Key::Right);
    assert_eq!(d.selected(), 0, "wraps");
    d.handle(Key::Left);
    assert_eq!(d.selected(), 1);
    let mut one = Details::new(vec![ollama()], 0);
    one.handle(Key::Right);
    assert_eq!(one.selected(), 0);
}

#[test]
fn enter_q_and_esc_close_it() {
    for key in [Key::Enter, Key::Char('q'), Key::Esc] {
        let mut d = Details::new(vec![ollama()], 0);
        assert!(
            matches!(d.handle(key), Step::Done(()) | Step::Cancel),
            "{key:?}"
        );
    }
    let mut d = Details::new(vec![ollama()], 0);
    assert!(matches!(d.handle(Key::Char('x')), Step::Stay));
}

#[test]
fn with_full_motion_the_rows_appear_one_by_one_and_the_numbers_count_up() {
    let mut d = Details::new(vec![ollama()], 0);
    at(&mut d, 0);
    let start = rows(&d, Motion::Full, true);
    let count = |rows: &[String]| {
        rows.iter()
            .filter(|r| r.contains("Provider") || r.contains("Model") || r.contains("Input"))
            .count()
    };
    assert_eq!(
        count(&start),
        1,
        "only the first row at the start: {start:?}"
    );
    assert_eq!(
        start.len(),
        rows(&d, Motion::Off, true).len(),
        "the box does not grow"
    );
    at(&mut d, 400);
    let mid = rows(&d, Motion::Full, true);
    let input = mid.iter().find(|r| r.contains("Input")).unwrap();
    assert!(!input.contains("1,204"), "still counting: {input}");
    at(&mut d, 3000);
    assert_eq!(
        rows(&d, Motion::Full, true),
        rows(&d, Motion::Off, true),
        "settles on the exact table"
    );
}

#[test]
fn reduced_and_off_motion_show_the_whole_table_at_once_and_never_animate() {
    for motion in [Motion::Reduced, Motion::Off] {
        let mut d = Details::new(vec![ollama()], 0);
        at(&mut d, 0);
        assert_eq!(
            rows(&d, motion, true),
            rows(&d, Motion::Off, true),
            "{motion:?}"
        );
        assert!(!d.animating(), "{motion:?}");
    }
}

#[test]
fn it_animates_while_the_rows_appear_and_a_key_never_waits_for_it() {
    let mut d = Details::new(vec![ollama(), gemini()], 0);
    at(&mut d, 100);
    let _ = rows(&d, Motion::Full, true);
    assert!(d.animating());
    d.handle(Key::Right);
    assert!(d.animating(), "a new tab starts the motion again");
    assert!(
        matches!(d.handle(Key::Esc), Step::Cancel),
        "Esc acts at once"
    );
    let mut d = Details::new(vec![ollama()], 0);
    at(&mut d, 100);
    let _ = rows(&d, Motion::Full, true);
    assert!(
        matches!(d.handle(Key::Enter), Step::Done(())),
        "Enter acts at once"
    );
    at(&mut d, 5000);
    assert!(!d.animating(), "idle again: no timer");
}

#[test]
fn no_call_yet_says_so() {
    let d = Details::new(Vec::new(), 0);
    assert!(
        rows(&d, Motion::Off, true)
            .join("\n")
            .contains("no AI call yet")
    );
}

#[test]
fn hostile_model_names_are_made_harmless_and_the_box_keeps_its_shape() {
    let mut f = ollama();
    f.model = "evil\x1b[2J\nমডেল\u{202e}".to_owned();
    let all = rows(&Details::new(vec![f], 0), Motion::Off, true);
    for r in &all {
        assert!(!r.contains('\x1b') && !r.contains('\u{202e}'), "{r:?}");
    }
    assert!(all.join("\n").contains("মডেল"));
}
