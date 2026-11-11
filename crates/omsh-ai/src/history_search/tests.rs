use super::*;

fn parse(answer: &str) -> Result<HistoryFilter, PlanError> {
    HistoryFilter::parse(answer)
}

fn refused(answer: &str) -> String {
    parse(answer).expect_err(answer).reason
}

#[test]
fn an_empty_object_is_the_newest_twenty() {
    let filter = parse("{}").unwrap();
    assert_eq!(
        filter,
        HistoryFilter {
            all: Vec::new(),
            any: Vec::new(),
            here: false,
            status: Status::Any,
            from: None,
            to: None,
            newest: DEFAULT_NEWEST,
        }
    );
    assert_eq!(filter.describe(), "commands newest 20");
}

#[test]
fn every_field_is_read() {
    let filter = parse(
        r#"Here you go:
        ```json
        {"all": ["docker", "compose"], "any": ["up", "down"], "here": true,
         "status": "failed", "from": "2026-11-09", "to": "2026-11-10", "newest": 5}
        ```"#,
    )
    .unwrap();
    assert_eq!(filter.all, ["docker", "compose"]);
    assert_eq!(filter.any, ["up", "down"]);
    assert!(filter.here);
    assert_eq!(filter.status, Status::Failed);
    assert_eq!(
        filter.from,
        Some(Day {
            year: 2026,
            month: 11,
            day: 9
        })
    );
    assert_eq!(filter.to.unwrap().to_string(), "2026-11-10");
    assert_eq!(filter.newest, 5);
}

#[test]
fn null_dates_and_the_two_other_statuses_are_fine() {
    let filter = parse(r#"{"from": null, "to": null, "status": "succeeded"}"#).unwrap();
    assert_eq!(filter.from, None);
    assert_eq!(filter.to, None);
    assert_eq!(filter.status, Status::Succeeded);
    assert_eq!(parse(r#"{"status": "any"}"#).unwrap().status, Status::Any);
}

#[test]
fn describe_is_built_from_the_checked_fields() {
    let filter = parse(
        r#"{"all": ["docker", "compose"], "here": true, "status": "failed",
            "from": "2026-11-10", "to": "2026-11-10", "newest": 7}"#,
    )
    .unwrap();
    assert_eq!(
        filter.describe(),
        "commands containing \"docker\" and \"compose\", run in this folder (not its \
         subfolders), that failed, from 2026-11-10 to 2026-11-10, both included, newest 7"
    );
    let any = parse(r#"{"any": ["git"], "from": "2026-01-02"}"#).unwrap();
    assert_eq!(
        any.describe(),
        "commands containing \"git\", from 2026-01-02 on, newest 20"
    );
    let many = parse(r#"{"any": ["a", "b"], "to": "2026-01-02", "status": "succeeded"}"#).unwrap();
    assert_eq!(
        many.describe(),
        "commands containing at least one of \"a\", \"b\", that succeeded, up to 2026-01-02, \
         included, newest 20"
    );
}

#[test]
fn text_that_is_not_a_filter_is_refused() {
    assert!(refused("no json here").contains("no JSON object"));
    assert!(refused("{ not json }").contains("does not match the schema"));
    // A filter is an object, not a list of lines from the history.
    assert!(refused(r#"{"all": "docker"}"#).contains("does not match the schema"));
    assert!(refused(r#"{"status": "broken"}"#).contains("does not match the schema"));
    assert!(refused(r#"{"newest": -1}"#).contains("does not match the schema"));
}

#[test]
fn unknown_keys_are_refused() {
    // The model has no way to add a field that the shell would not check: no SQL, no paths.
    for key in ["sql", "where", "cwd", "command", "limit"] {
        let answer = format!(r#"{{"{key}": "x"}}"#);
        assert!(refused(&answer).contains("unknown field"), "{key}");
    }
}

#[test]
fn newest_is_one_to_a_hundred() {
    assert_eq!(parse(r#"{"newest": 1}"#).unwrap().newest, 1);
    assert_eq!(parse(r#"{"newest": 100}"#).unwrap().newest, 100);
    assert!(refused(r#"{"newest": 0}"#).contains("newest"));
    assert!(refused(r#"{"newest": 101}"#).contains("newest"));
}

#[test]
fn dates_must_be_real_days_in_one_format() {
    for good in ["2026-11-11", "2024-02-29", "1970-01-01", "9999-12-31"] {
        assert!(Day::parse(good).is_ok(), "{good}");
    }
    for bad in [
        "2026-11-31",
        "2026-02-29",
        "2100-02-29",
        "2026-13-01",
        "2026-00-10",
        "2026-11-00",
        "1969-12-31",
        "2026-1-1",
        "26-11-11",
        "2026/11/11",
        " 2026-11-11",
        "2026-11-11T10:00",
        "+026-11-11",
        "yesterday",
        "",
    ] {
        assert!(Day::parse(bad).is_err(), "{bad:?}");
    }
    assert!(Day::parse("2000-02-29").is_ok(), "2000 is a leap year");
    assert!(refused(r#"{"from": "2026-02-30"}"#).contains("YYYY-MM-DD"));
    assert!(refused(r#"{"to": "soon"}"#).contains("YYYY-MM-DD"));
}

#[test]
fn a_date_in_an_error_is_short_and_printable() {
    let long = "x".repeat(300);
    let error = Day::parse(&long).unwrap_err().reason;
    assert!(error.len() < 120, "{error}");
    let error = Day::parse("\u{1b}[31m2026").unwrap_err().reason;
    assert!(!error.chars().any(char::is_control), "{error:?}");
}

#[test]
fn from_after_to_is_refused_and_one_day_is_fine() {
    assert!(refused(r#"{"from": "2026-11-11", "to": "2026-11-10"}"#).contains("after"));
    assert!(parse(r#"{"from": "2026-11-11", "to": "2026-11-11"}"#).is_ok());
}

#[test]
fn words_are_limited_in_number_and_length() {
    let eight = ["a"; 8].join("\",\"");
    assert!(parse(&format!(r#"{{"all": ["{eight}"]}}"#)).is_ok());
    let nine = ["a"; 9].join("\",\"");
    assert!(refused(&format!(r#"{{"all": ["{nine}"]}}"#)).contains("most allowed"));
    assert!(refused(&format!(r#"{{"any": ["{nine}"]}}"#)).contains("most allowed"));
    let long = "w".repeat(MAX_WORD_CHARS);
    assert!(parse(&format!(r#"{{"all": ["{long}"]}}"#)).is_ok());
    let too_long = "w".repeat(MAX_WORD_CHARS + 1);
    assert!(refused(&format!(r#"{{"all": ["{too_long}"]}}"#)).contains("longer"));
    assert!(refused(r#"{"all": [""]}"#).contains("empty"));
    assert!(refused(r#"{"any": ["   "]}"#).contains("empty"));
}

#[test]
fn a_word_may_hold_spaces_and_quotes() {
    let filter = parse(r#"{"all": ["git push", "-m \"fix\""]}"#).unwrap();
    assert_eq!(filter.all, ["git push", "-m \"fix\""]);
}

#[test]
fn control_characters_in_words_are_refused() {
    for ch in [
        "\\u001b", "\\u0007", "\\r", "\\n", "\\t", "\\u0000", "\\u0085",
    ] {
        let answer = format!(r#"{{"all": ["a{ch}b"]}}"#);
        assert!(refused(&answer).contains("control character"), "{ch}");
    }
}

#[test]
fn invisible_and_reordering_characters_in_words_are_refused() {
    // U+202E reverses what follows on the screen, U+2066 isolates text, U+200B hides, U+061C and
    // U+FEFF are invisible marks, U+2028 breaks a line, U+E0041 is a hidden "tag" letter.
    for code in [
        "202e", "202a", "202c", "2066", "2069", "200b", "200f", "061c", "feff", "2028", "2029",
        "2060", "00ad", "e0041",
    ] {
        let escaped = if code.len() > 4 {
            // JSON writes a character above U+FFFF as a surrogate pair; use the raw character.
            let ch = char::from_u32(u32::from_str_radix(code, 16).unwrap()).unwrap();
            ch.to_string()
        } else {
            format!("\\u{code}")
        };
        let answer = format!(r#"{{"any": ["rm{escaped}"]}}"#);
        let reason = refused(&answer);
        assert!(reason.contains("invisible"), "{code}: {reason}");
    }
    // Ordinary words, also with accents and other scripts, pass.
    for word in ["docker", "café", "日本", "ls -l", "grep \\\"x\\\""] {
        let answer = format!(r#"{{"all": ["{word}"]}}"#);
        assert!(parse(&answer).is_ok(), "{word}");
    }
}

#[test]
fn the_request_has_only_the_masked_text_and_the_date() {
    let key = format!("ghp_{}", "a1B2".repeat(9));
    let request = history_search_request(
        &format!("what did I do with {key} yesterday"),
        "2026-11-11 (Wednesday)",
    );
    assert!(request.json);
    assert_eq!(request.max_tokens, HISTORY_MAX_TOKENS);
    assert_eq!(request.messages.len(), 1);
    let sent = &request.messages[0].text;
    assert!(!sent.contains("ghp_"), "{sent}");
    assert!(sent.contains("[MASKED]"), "{sent}");
    let system = request.system.unwrap();
    assert!(system.contains("2026-11-11 (Wednesday)"));
    // Nothing about the machine: no folder, no OS, no earlier commands.
    for forbidden in ["cwd", "Operating system", "Recent commands", "Git:"] {
        assert!(!system.contains(forbidden), "{forbidden}");
    }
    assert!(history_request_hides_secret(&format!("see {key}")));
    assert!(!history_request_hides_secret("git push yesterday"));
}

#[test]
fn the_request_text_is_cleaned_and_limited() {
    let request = history_search_request("a\u{1b}[31mb\u{202e}c\nd", "2026-11-11");
    let sent = &request.messages[0].text;
    assert!(
        !sent.chars().any(|c| c.is_control() || is_invisible(c)),
        "{sent:?}"
    );
    let long = history_search_request(&"x".repeat(REQUEST_CHARS * 3), "2026-11-11");
    assert_eq!(long.messages[0].text.chars().count(), REQUEST_CHARS);
}

#[test]
fn a_refused_answer_can_be_retried_with_the_standard_message() {
    let first = history_search_request("git yesterday", "2026-11-11");
    let error = refused(r#"{"newest": 0}"#);
    let again = crate::retry_request(&first, "{\"newest\": 0}", &PlanError::new(error));
    assert_eq!(again.messages.len(), 3);
    assert!(again.messages[2].text.contains("not usable"));
}
