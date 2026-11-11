//! `[c] chat`: a follow-up message to the model, and a new plan (S23, §13.6).

use super::*;

fn marker_plan(dir: &Dir, name: &str) -> String {
    json(&[("Mark", &format!("echo {name} > {}", dir.file(name)), "low")])
}

fn typed(text: &str) -> Result<String, ReadError> {
    Ok(text.to_owned())
}

fn roles_and_texts(request: &ChatRequest) -> Vec<(Role, String)> {
    request
        .messages
        .iter()
        .map(|m| (m.role, m.text.clone()))
        .collect()
}

#[test]
fn chat_then_enter_runs_the_new_plan_and_not_the_old_one() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    let second = marker_plan(&dir, "two");
    let ran = go(
        says(&first).says(&[&second]),
        "make a marker",
        Setup {
            keys: vec![ch('c'), enter()],
            lines: vec![typed("name it two instead")],
            ..Setup::default()
        },
    );
    assert!(dir.has("two") && !dir.has("one"), "{}", ran.err);
    assert_eq!(ran.status(), 0);
    assert_eq!(ran.line_prompts, ["chat> "]);
    assert!(ran.err.contains("after 1 chat turn"), "{}", ran.err);
    assert!(ran.err.contains("chat turn 1..."), "{}", ran.err);
}

#[test]
fn the_second_request_carries_the_request_the_plan_and_the_message_in_order() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    let second = marker_plan(&dir, "two");
    let ran = go(
        says(&first).says(&[&second]),
        "make a marker",
        Setup {
            keys: vec![ch('c'), Ok(Key::Escape)],
            lines: vec![typed("name it two instead")],
            ..Setup::default()
        },
    );
    assert_eq!(ran.requests.len(), 2);
    let thread = roles_and_texts(&ran.requests[1]);
    assert_eq!(thread.len(), 3, "{thread:?}");
    assert_eq!(thread[0], (Role::User, "make a marker".to_owned()));
    assert_eq!(thread[1].0, Role::Assistant);
    assert!(thread[1].1.contains("echo one"), "{}", thread[1].1);
    assert_eq!(thread[2], (Role::User, "name it two instead".to_owned()));
    // The instructions are the same for every turn.
    assert_eq!(ran.requests[1].system, ran.requests[0].system);
    assert!(ran.requests[1].json);
}

#[test]
fn a_secret_in_the_message_is_masked_before_sending() {
    let key = format!("ghp_{}", "a1B2".repeat(9));
    let first = json(&[("Hi", "echo hi", "low")]);
    let ran = go(
        says(&first).says(&[&first]),
        "x",
        Setup {
            keys: vec![ch('c'), Ok(Key::Escape)],
            lines: vec![typed(&format!("use {key} please"))],
            ..Setup::default()
        },
    );
    let sent = &ran.requests[1].messages.last().unwrap().text;
    assert_eq!(sent, "use [MASKED] please");
    assert!(
        ran.err.contains("a secret in your message was hidden"),
        "{}",
        ran.err
    );
}

#[test]
fn a_command_edited_by_hand_is_told_to_the_model() {
    let first = json(&[("Hi", "echo hi", "low")]);
    let ran = go(
        says(&first).says(&[&first]),
        "x",
        Setup {
            keys: vec![ch('e'), ch('c'), Ok(Key::Escape)],
            edits: vec![Edited::Text("echo mine".to_owned())],
            lines: vec![typed("and then sort it")],
            ..Setup::default()
        },
    );
    let sent = &ran.requests[1].messages.last().unwrap().text;
    assert_eq!(
        sent,
        "Note: before this message I changed the command of step 1 to: echo mine\n\
         and then sort it"
    );
    // After the edit the panel says so instead of repeating the model's explanation.
    assert!(
        ran.err
            .contains("(your edit; the model's explanation was for its own command)"),
        "{}",
        ran.err
    );
    assert_eq!(
        ran.err.matches("Because.").count(),
        2,
        "the first panel and the plan from the chat show the explanation; the edited one does not: {}",
        ran.err
    );
}

#[test]
fn a_refused_chat_answer_is_asked_again_and_stays_out_of_the_thread() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    let second = marker_plan(&dir, "two");
    let third = marker_plan(&dir, "three");
    let ran = go(
        says(&first)
            .says(&["no json here"])
            .says(&[&second])
            .says(&[&third]),
        "x",
        Setup {
            keys: vec![ch('c'), ch('c'), enter()],
            lines: vec![typed("again"), typed("once more")],
            ..Setup::default()
        },
    );
    assert!(dir.has("three"), "{}", ran.err);
    // first, chat (refused), chat retry, second chat
    assert_eq!(ran.requests.len(), 4);
    let retry = roles_and_texts(&ran.requests[2]);
    assert!(retry.iter().any(|(_, t)| t == "no json here"));
    let last = roles_and_texts(&ran.requests[3]);
    assert!(
        !last
            .iter()
            .any(|(_, t)| t.contains("no json here") || t.contains("not usable")),
        "{last:?}"
    );
    // request, plan one, "again", plan two, "once more"
    assert_eq!(last.len(), 5, "{last:?}");
    assert!(last[3].1.contains("echo two"));
}

#[test]
fn a_chat_turn_that_fails_leaves_the_old_plan_to_run() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    let ran = go(
        says(&first).fails(ProviderError::new(ErrorKind::Auth, "401")),
        "x",
        Setup {
            keys: vec![ch('c'), enter()],
            lines: vec![typed("change it")],
            ..Setup::default()
        },
    );
    assert!(dir.has("one"), "{}", ran.err);
    assert_eq!(ran.status(), 0);
    assert!(ran.err.contains("chat:"), "{}", ran.err);
    assert!(
        ran.err.contains("the previous plan is unchanged"),
        "{}",
        ran.err
    );
    // The panel was shown twice: before the chat and again after it.
    assert_eq!(ran.err.matches("plan from").count(), 2, "{}", ran.err);
}

#[test]
fn two_bad_chat_answers_end_with_the_old_plan_still_there() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    let ran = go(
        says(&first).says(&["nope"]).says(&["still nope"]),
        "x",
        Setup {
            keys: vec![ch('c'), enter()],
            lines: vec![typed("change it")],
            ..Setup::default()
        },
    );
    assert!(dir.has("one"), "{}", ran.err);
    assert!(
        ran.err.contains("chat: couldn't make a plan"),
        "{}",
        ran.err
    );
}

#[test]
fn a_chat_turn_that_is_cancelled_leaves_the_old_plan() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    // The fake gives no second answer; a cancelled request is shown the same way.
    let ran = go(
        says(&first).fails(ProviderError::new(ErrorKind::Cancelled, "interrupted")),
        "x",
        Setup {
            keys: vec![ch('c'), Ok(Key::Escape)],
            lines: vec![typed("change it")],
            ..Setup::default()
        },
    );
    assert!(!dir.has("one"));
    assert!(ran.err.contains("chat: cancelled"), "{}", ran.err);
    assert_eq!(ran.err.matches("plan from").count(), 2, "{}", ran.err);
    assert_eq!(ran.status(), 1);
}

#[test]
fn nothing_typed_at_the_chat_line_sends_nothing() {
    for answer in [
        typed(""),
        typed("   "),
        Err(ReadError::Interrupted),
        Err(ReadError::EndOfInput),
    ] {
        let dir = Dir::new();
        let first = marker_plan(&dir, "one");
        let ran = go(
            says(&first),
            "x",
            Setup {
                keys: vec![ch('c'), enter()],
                lines: vec![answer],
                ..Setup::default()
            },
        );
        assert_eq!(ran.requests.len(), 1);
        assert!(dir.has("one"), "{}", ran.err);
        assert_eq!(ran.err.matches("plan from").count(), 2, "{}", ran.err);
    }
}

#[test]
fn a_huge_message_is_refused_without_sending() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    let ran = go(
        says(&first),
        "x",
        Setup {
            keys: vec![ch('c'), enter()],
            lines: vec![typed(&"x".repeat(omsh_ai::MAX_FOLLOW_UP_CHARS + 1))],
            ..Setup::default()
        },
    );
    assert_eq!(ran.requests.len(), 1);
    assert!(ran.err.contains("nothing was sent"), "{}", ran.err);
    assert!(dir.has("one"));
}

#[test]
fn the_hint_comes_at_the_fifth_turn_and_chat_still_works_after_it() {
    let first = json(&[("Hi", "echo hi", "low")]);
    let mut fake = says(&first);
    for _ in 0..6 {
        fake = fake.says(&[&first]);
    }
    let mut keys = vec![ch('c'); 6];
    keys.push(Ok(Key::Escape));
    let ran = go(
        fake,
        "x",
        Setup {
            keys,
            lines: (1..=6).map(|n| typed(&format!("turn {n}"))).collect(),
            ..Setup::default()
        },
    );
    assert_eq!(ran.requests.len(), 7);
    let hint = |n: usize| format!("(chat turn {n}: if only a command needs changing");
    for before in 1..=4 {
        assert!(!ran.err.contains(&hint(before)), "turn {before}");
    }
    assert!(ran.err.contains(&hint(5)), "{}", ran.err);
    assert!(ran.err.contains(&hint(6)), "{}", ran.err);
    assert!(ran.err.contains("after 6 chat turns"), "{}", ran.err);
}

#[test]
fn a_view_after_a_chat_lists_the_whole_conversation_and_no_model_text() {
    let first = json(&[("Hi", "echo hi-from-the-model", "low")]);
    let ran = go(
        says(&first).says(&[&first]),
        "show me hi",
        Setup {
            keys: vec![ch('c'), ch('v'), Ok(Key::Escape)],
            lines: vec![typed("shorter please")],
            ..Setup::default()
        },
    );
    let at = ran.err.rfind("this was sent to").expect("[v] printed");
    let view = &ran.err[at..];
    for needle in [
        "request:",
        "show me hi",
        "the model's earlier plan (",
        "your message:",
        "shorter please",
    ] {
        assert!(view.contains(needle), "{needle}: {view}");
    }
    assert!(!view.contains("hi-from-the-model"), "{view}");
}

#[test]
fn a_plan_from_the_chat_goes_through_the_analyzer_like_any_other() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    let risky = json(&[("Wipe", "shred /nonexistent-omsh-chat-test", "low")]);
    let ran = go(
        says(&first).says(&[&risky]),
        "x",
        Setup {
            keys: vec![ch('c'), enter()],
            lines: vec![typed("wipe it"), typed("no")],
            ..Setup::default()
        },
    );
    assert!(ran.err.contains("[critical]"), "{}", ran.err);
    assert_eq!(ran.status(), 1);
    assert!(!ran.err.contains("[1/1]"), "{}", ran.err);
    assert!(!dir.has("one"));
}

#[test]
fn every_request_gets_its_own_provider_and_none_outlives_the_flow() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    let second = marker_plan(&dir, "two");
    let ran = go(
        says(&first).says(&[&second]),
        "x",
        Setup {
            keys: vec![ch('c'), enter()],
            lines: vec![typed("again")],
            ..Setup::default()
        },
    );
    assert_eq!(ran.connects, 2);
    assert_eq!(ran.alive, 0, "a provider (a connection) was kept");
}

#[test]
fn the_chat_message_is_not_stored_in_the_history_but_the_steps_that_ran_are() {
    let dir = Dir::new();
    let first = marker_plan(&dir, "one");
    let second = marker_plan(&dir, "two");
    let history = History {
        store: Store::in_memory(),
        filter: Filter::default(),
    };
    go(
        says(&first).says(&[&second]),
        "x",
        Setup {
            keys: vec![ch('c'), enter()],
            lines: vec![typed("a unique follow-up message")],
            history: Some(history.clone()),
            ..Setup::default()
        },
    );
    history.store.settle(Duration::from_secs(2));
    let seen: Vec<String> = history
        .store
        .query(&Query::default())
        .into_iter()
        .map(|entry| entry.command)
        .collect();
    assert_eq!(seen, [format!("echo two > {}", dir.file("two"))]);
}

#[test]
fn a_plan_from_the_chat_gets_its_count_of_what_it_would_touch() {
    let dir = super::two_folders();
    let harmless = json(&[("Hi", "echo hi", "low")]);
    let command = format!("rm -rf {}", dir.file("a"));
    let removal = json(&[("Remove a", &command, "high")]);
    let ran = go(
        says(&harmless).says(&[&removal]),
        "x",
        Setup {
            keys: vec![ch('c'), Ok(Key::Escape)],
            lines: vec![typed("remove folder a instead")],
            ..Setup::default()
        },
    );
    assert!(
        ran.err
            .contains("       touches 2 files and 1 folder (10 B of file data)\n"),
        "{}",
        ran.err
    );
    assert!(dir.has("a/f0"), "counting must not delete anything");
}
