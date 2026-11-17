use omsh_config::Motion;

use super::*;
use crate::panels::test_support::caps;

fn items(names: &[&str]) -> Vec<ListItem> {
    names.iter().map(|n| ListItem::new(n, "")).collect()
}

fn type_text(c: &mut Choose, text: &str) {
    for ch in text.chars() {
        c.handle(Key::Char(ch));
    }
}

fn shown(c: &Choose) -> String {
    c.render(&caps(Motion::Off, true), 80)
        .iter()
        .map(Line::plain_text)
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn enter_returns_the_label_of_the_chosen_row() {
    let mut c = Choose::new("Model", "models", items(&["alpha", "beta", "gamma"]), None);
    c.handle(Key::Down);
    assert!(matches!(c.handle(Key::Enter), Step::Done(name) if name == "beta"));
}

#[test]
fn typing_filters_and_enter_takes_the_best_match() {
    let mut c = Choose::new(
        "Model",
        "models",
        items(&["llama3.2", "qwen3", "gemma2"]),
        None,
    );
    type_text(&mut c, "qwe");
    assert!(matches!(c.handle(Key::Enter), Step::Done(name) if name == "qwen3"));
}

#[test]
fn the_current_choice_is_the_row_that_starts_chosen() {
    let mut c = Choose::new("Model", "models", items(&["a", "b", "c"]), Some("c"));
    assert!(matches!(c.handle(Key::Enter), Step::Done(name) if name == "c"));
    let mut c = Choose::new("Model", "models", items(&["a", "b"]), Some("zzz"));
    assert!(
        matches!(c.handle(Key::Enter), Step::Done(name) if name == "a"),
        "an unknown current is ignored"
    );
}

#[test]
fn esc_cancels() {
    let mut c = Choose::new("Model", "models", items(&["a"]), None);
    assert!(matches!(c.handle(Key::Esc), Step::Cancel));
}

#[test]
fn tab_switches_between_the_chat_models_and_all_models() {
    let mut c = Choose::new("Model", "models", items(&["chat-1", "chat-2"]), None)
        .with_all(items(&["chat-1", "chat-2", "image-1", "tts-1"]));
    assert!(!c.showing_all());
    assert!(
        shown(&c).contains("2 models that chat, of 4 in all"),
        "{}",
        shown(&c)
    );
    assert!(matches!(c.handle(Key::Tab), Step::Stay));
    assert!(c.showing_all());
    assert!(
        shown(&c).contains("all 4 models") && shown(&c).contains("image-1"),
        "{}",
        shown(&c)
    );
    type_text(&mut c, "tts");
    assert!(matches!(c.handle(Key::Enter), Step::Done(name) if name == "tts-1"));
    let mut c =
        Choose::new("Model", "models", items(&["chat-1"]), None).with_all(items(&["chat-1", "x"]));
    c.handle(Key::Tab);
    c.handle(Key::Tab);
    assert!(!c.showing_all(), "Tab again goes back");
}

#[test]
fn without_a_longer_list_tab_does_nothing_and_says_nothing() {
    let mut c = Choose::new("Model", "models", items(&["a", "b"]), None);
    assert!(matches!(c.handle(Key::Tab), Step::Stay));
    assert!(!c.showing_all());
    let all = shown(&c);
    assert!(
        all.contains("2 models") && !all.contains("show all"),
        "{all}"
    );
}

#[test]
fn more_models_than_rows_are_all_reachable_and_none_is_cut_off() {
    let names: Vec<String> = (1..=45).map(|i| format!("model-{i:02}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let mut c = Choose::new("Model", "models", items(&refs), None);
    assert!(shown(&c).contains("45 models"));
    type_text(&mut c, "model-45");
    assert!(matches!(c.handle(Key::Enter), Step::Done(name) if name == "model-45"));
}

#[test]
fn hostile_names_are_made_harmless() {
    let c = Choose::new(
        "Model",
        "models",
        items(&["evil\x1b[2J\nname\u{202e}"]),
        None,
    );
    let all = shown(&c);
    assert!(
        !all.contains('\x1b') && !all.contains('\u{202e}'),
        "{all:?}"
    );
}
