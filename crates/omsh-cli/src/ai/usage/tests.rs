use omsh_config::Motion;
use omsh_ui::caps::{Caps, CapsFacts, Out};
use omsh_ui::theme::Theme;

use super::*;

fn ui(unicode: bool) -> MenuUi {
    let facts = CapsFacts {
        stdin_tty: true,
        stdout_tty: true,
        stderr_tty: true,
        utf8: unicode,
        no_color: true,
        size: Some((100, 30)),
        ..CapsFacts::default()
    };
    MenuUi {
        caps: Caps::decide(
            facts,
            Out::Stderr,
            omsh_config::Icons::Auto,
            Some(Motion::Off),
        ),
        theme: Theme::default(),
    }
}

fn usage() -> Usage {
    Usage {
        input: Some(1204),
        output: Some(110),
        thinking: Some(200),
        total: Some(1514),
        ..Usage::default()
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn the_newest_call_of_each_provider_is_kept_in_the_order_of_first_use() {
    let calls = Calls::standard();
    calls.record(Kind::Gemini, "g1", Some(usage()), ms(1000), None);
    calls.record(Kind::Ollama, "o1", None, ms(2000), None);
    calls.record(Kind::Gemini, "g2", Some(usage()), ms(3000), None);
    let records = calls.records();
    let seen: Vec<(Kind, &str)> = records.iter().map(|r| (r.kind, r.model.as_str())).collect();
    assert_eq!(seen, [(Kind::Gemini, "g2"), (Kind::Ollama, "o1")]);
}

#[test]
fn the_meter_eases_from_what_the_provider_showed_before() {
    let calls = Calls::standard();
    let first = calls.record(Kind::Gemini, "g", Some(usage()), ms(1), None);
    assert_eq!(first.before, None);
    let mut bigger = usage();
    bigger.total = Some(3000);
    let second = calls.record(Kind::Gemini, "g", Some(bigger), ms(1), None);
    assert_eq!(second.before, Some(1514));
}

#[test]
fn facts_carry_only_what_the_provider_reported() {
    let calls = Calls::standard();
    let record = calls.record(Kind::Ollama, "m", None, ms(1500), None);
    let facts = calls.facts(&record);
    assert_eq!(facts.provider, "Ollama (local)");
    assert_eq!(facts.time, Some(ms(1500)));
    assert_eq!(
        (
            facts.input,
            facts.output,
            facts.thinking,
            facts.cached,
            facts.total
        ),
        (None, None, None, None, None),
        "nothing reported: nothing shown, and no zero"
    );
    assert_eq!(facts.context_used, None);
    assert_eq!(facts.tokens_per_sec, None);
}

#[test]
fn the_window_is_what_was_found_and_the_config_wins() {
    let found = Limit {
        tokens: 8192,
        source: LimitSource::OllamaLoaded,
    };
    let calls = Calls::standard();
    let record = calls.record(Kind::Ollama, "m", Some(usage()), ms(1), Some(found));
    let facts = calls.facts(&record);
    assert_eq!(facts.context_limit, Some(8192));
    assert!(facts.limit_note.unwrap().contains("loaded model"));

    let ai = Ai {
        context_limit: Some(128_000),
        ..Ai::default()
    };
    let calls = Calls::new(&ai);
    let record = calls.record(Kind::Ollama, "m", Some(usage()), ms(1), Some(found));
    let facts = calls.facts(&record);
    assert_eq!(facts.context_limit, Some(128_000));
    assert!(facts.limit_note.unwrap().contains("[ai] context_limit"));
    let none = calls.record(Kind::Gemini, "g", Some(usage()), ms(1), None);
    assert_eq!(
        calls.facts(&none).context_limit,
        Some(128_000),
        "also where no lookup found one"
    );
}

#[test]
fn an_unknown_window_has_no_limit_and_no_note() {
    let calls = Calls::standard();
    let record = calls.record(Kind::OpenAi, "m", Some(usage()), ms(1), None);
    let facts = calls.facts(&record);
    assert_eq!((facts.context_limit, facts.limit_note), (None, None));
}

#[test]
fn context_used_is_the_total_or_the_input_plus_the_completion() {
    assert_eq!(context_used(Some(&usage())), Some(1514));
    let no_total = Usage {
        total: None,
        ..usage()
    };
    assert_eq!(context_used(Some(&no_total)), Some(1204 + 310));
    let input_only = Usage {
        input: Some(5),
        ..Usage::default()
    };
    assert_eq!(
        context_used(Some(&input_only)),
        None,
        "half a sum is not shown"
    );
    assert_eq!(context_used(None), None);
}

#[test]
fn the_info_text_is_indented_ends_in_a_newline_and_follows_the_configured_fields() {
    let ai = Ai {
        inline: AiInline {
            fields: vec![
                "model".to_owned(),
                "time".to_owned(),
                "in".to_owned(),
                "out".to_owned(),
            ],
            thinking_warn: 0,
        },
        ..Ai::default()
    };
    let calls = Calls::new(&ai);
    let record = calls.record(Kind::Gemini, "flash", Some(usage()), ms(3240), None);
    // The words and numbers are at full weight (faint text is hard to read on some themes);
    // only the separators are faint, and that stays under `NO_COLOR` (an attribute, no color).
    assert_eq!(
        calls.info_text(&ui(true), &record),
        "  flash\x1b[2m · \x1b[0m3.2 s\x1b[2m · \x1b[0min 1,204\x1b[2m · \x1b[0mout 110\n"
    );
    assert_eq!(
        calls.info_text(&ui(false), &record),
        "  flash\x1b[2m, \x1b[0m3.2 s\x1b[2m, \x1b[0min 1,204\x1b[2m, \x1b[0mout 110\n"
    );
}

#[test]
fn a_call_with_no_numbers_still_shows_dashes_and_the_time() {
    let calls = Calls::standard();
    let record = calls.record(Kind::Ollama, "m", None, ms(1200), None);
    assert_eq!(
        calls.info_text(&ui(true), &record),
        "  1.2 s\x1b[2m · \x1b[0min –\x1b[2m · \x1b[0mout –\n"
    );
    assert_eq!(
        calls.info_text(&ui(false), &record),
        "  1.2 s\x1b[2m, \x1b[0min -\x1b[2m, \x1b[0mout -\n"
    );
}

#[test]
fn no_fields_means_no_text() {
    let ai = Ai {
        inline: AiInline {
            fields: Vec::new(),
            thinking_warn: 0,
        },
        ..Ai::default()
    };
    let calls = Calls::new(&ai);
    let record = calls.record(Kind::Ollama, "m", Some(usage()), ms(1), None);
    assert_eq!(calls.info_text(&ui(true), &record), "");
}
