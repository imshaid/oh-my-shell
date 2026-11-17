use std::time::Duration;

use omsh_config::Motion;

use super::*;
use crate::panels::test_support::caps;

fn facts() -> CallFacts {
    CallFacts {
        provider: "Gemini".to_owned(),
        model: "gemini-2.5-flash".to_owned(),
        effort: None,
        time: Some(Duration::from_millis(3240)),
        input: Some(1204),
        thinking: Some(200),
        output: Some(110),
        cached: Some(64),
        total: Some(1514),
        tokens_per_sec: Some(20.0),
        context_used: Some(1514),
        context_limit: Some(32768),
        limit_note: None,
    }
}

fn fields(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn lines(
    facts: &CallFacts,
    names: &[&str],
    expanded: bool,
    warn: u32,
    unicode: bool,
) -> Vec<String> {
    let c = caps(Motion::Off, unicode);
    let meter = Meter::new(facts.context_used, facts.context_limit, None);
    let f = fields(names);
    info_lines(
        facts,
        &InfoOptions {
            fields: &f,
            thinking_warn: warn,
            expanded,
        },
        &meter,
        &c,
    )
    .iter()
    .map(Line::plain_text)
    .collect()
}

#[test]
fn the_default_line_has_time_in_and_out() {
    assert_eq!(
        lines(&facts(), &["time", "in", "out"], false, 0, true),
        ["3.2 s · in 1,204 · out 110"]
    );
}

#[test]
fn every_field_is_written_in_the_order_given() {
    let all = [
        "model", "effort", "time", "in", "out", "thinking", "cached", "total", "ctx", "speed",
    ];
    assert_eq!(
        lines(&facts(), &all, false, 0, true),
        [
            "gemini-2.5-flash · effort – · 3.2 s · in 1,204 · out 110 · thinking 200 · cached 64 · total 1,514 · ctx ≈ 1,514 / 32,768  4% · speed 20.0 tok/s (derived)"
        ]
    );
    assert_eq!(
        lines(&facts(), &["out", "in"], false, 0, true),
        ["out 110 · in 1,204"]
    );
}

#[test]
fn a_number_the_provider_did_not_send_is_a_dash_never_zero() {
    let mut f = facts();
    f.input = None;
    f.thinking = None;
    f.cached = None;
    f.total = None;
    f.time = None;
    f.tokens_per_sec = None;
    let row = lines(
        &f,
        &["time", "in", "thinking", "cached", "total", "speed"],
        false,
        0,
        true,
    );
    assert_eq!(
        row,
        ["– · in – · thinking – · cached – · total – · speed –"]
    );
    assert!(!row[0].contains('0'), "{}", row[0]);
}

#[test]
fn a_real_zero_is_shown_as_zero() {
    let mut f = facts();
    f.cached = Some(0);
    assert_eq!(lines(&f, &["cached"], false, 0, true), ["cached 0"]);
}

#[test]
fn tab_adds_the_three_fields_that_are_not_there_yet() {
    assert_eq!(
        lines(&facts(), &["time", "in", "out"], true, 0, true),
        ["3.2 s · in 1,204 · out 110 · thinking 200 · cached 64 · total 1,514"]
    );
    assert_eq!(
        lines(&facts(), &["total", "in"], true, 0, true),
        ["total 1,514 · in 1,204 · thinking 200 · cached 64"],
        "a field that is already on the line is not shown twice"
    );
}

#[test]
fn ascii_uses_commas_dashes_and_tilde() {
    let mut f = facts();
    f.input = None;
    let row = lines(&f, &["in", "ctx"], false, 0, false);
    assert_eq!(row, ["in -, ctx ~ 1,514 / 32,768  4%"]);
    assert!(row[0].is_ascii());
}

#[test]
fn no_fields_means_no_line() {
    assert!(lines(&facts(), &[], false, 0, true).is_empty());
}

#[test]
fn a_thinking_spike_warns_in_words_and_in_yellow() {
    let mut f = facts();
    f.thinking = Some(500);
    f.output = Some(100);
    let row = lines(&f, &["out"], false, 4, true);
    assert_eq!(
        row[1],
        "warning: thinking used 5.0× the answer's tokens (500 vs 100)"
    );
    let c = caps(Motion::Off, true);
    let meter = Meter::new(None, None, None);
    let names = fields(&["out"]);
    let all = info_lines(
        &f,
        &InfoOptions {
            fields: &names,
            thinking_warn: 4,
            expanded: false,
        },
        &meter,
        &c,
    );
    assert_eq!(all[1].spans()[0].style.tone, Tone::Warn);
}

#[test]
fn the_warning_has_a_threshold_and_can_be_switched_off() {
    let mut f = facts();
    f.thinking = Some(399);
    f.output = Some(100);
    assert_eq!(
        lines(&f, &["out"], false, 4, true).len(),
        1,
        "3.99 times is under 4"
    );
    f.thinking = Some(400);
    assert_eq!(lines(&f, &["out"], false, 4, true).len(), 2);
    assert_eq!(
        lines(&f, &["out"], false, 0, true).len(),
        1,
        "0 turns it off"
    );
    f.thinking = None;
    assert_eq!(
        lines(&f, &["out"], false, 4, true).len(),
        1,
        "unknown thinking never warns"
    );
}

#[test]
fn an_empty_answer_after_thinking_is_said_plainly() {
    let mut f = facts();
    f.thinking = Some(1024);
    f.output = Some(0);
    let row = lines(&f, &["out"], false, 4, true);
    assert_eq!(
        row[1],
        "warning: all 1,024 output tokens went to thinking and the answer is empty"
    );
    let ascii = lines(
        &{
            let mut g = facts();
            g.thinking = Some(500);
            g.output = Some(100);
            g
        },
        &["out"],
        false,
        4,
        false,
    );
    assert_eq!(
        ascii[1],
        "warning: thinking used 5.0x the answer's tokens (500 vs 100)"
    );
}

#[test]
fn a_hostile_model_name_is_made_harmless() {
    let mut f = facts();
    f.model = "evil\x1b[31m\nname\u{202e}".to_owned();
    let row = lines(&f, &["model"], false, 0, true);
    assert!(
        !row[0].contains('\x1b') && !row[0].contains('\n') && !row[0].contains('\u{202e}'),
        "{:?}",
        row[0]
    );
}
