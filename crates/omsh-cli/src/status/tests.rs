use std::fs;

use omsh_config::{Config, Provider};
use omsh_ui::caps::{CapsFacts, Out};

use super::*;
use crate::update::daily::Stamp;

fn env(config: Config) -> StatusEnv {
    StatusEnv {
        config,
        paths: Paths::from_lookup(|name| match name {
            "HOME" => Some("/home/someone".into()),
            _ => None,
        }),
        no_ai: false,
        no_config: false,
        state_dir: None,
    }
}

fn facts() -> Facts<'static> {
    Facts {
        caps: Caps::decide(
            CapsFacts {
                stdin_tty: true,
                stdout_tty: true,
                stderr_tty: true,
                utf8: true,
                size: Some((100, 30)),
                ..CapsFacts::default()
            },
            Out::Stderr,
            omsh_config::Icons::Auto,
            None,
        ),
        now: 200 * 86_400,
        current: Version::parse("1.0.0").unwrap(),
        build_epoch: None,
        opt_out: None,
        env_motion: None,
        term: Some("xterm-256color"),
        size: Some((100, 30)),
        sync_output: None,
    }
}

fn value<'a>(rows: &'a [Row], label: &str) -> &'a str {
    &rows
        .iter()
        .find(|row| row.label == label)
        .unwrap_or_else(|| panic!("no row {label}: {rows:?}"))
        .value
}

#[test]
fn a_build_with_no_date_says_unknown_and_never_a_number() {
    let rows = rows(&env(Config::default()), &facts());
    assert_eq!(value(&rows, "Build"), "unknown (not a release build)");
}

#[test]
fn a_dated_build_says_its_age_and_flags_an_old_one_in_words() {
    let mut f = facts();
    f.build_epoch = Some(f.now - 10 * 86_400);
    let rows = rows(&env(Config::default()), &f);
    assert!(value(&rows, "Build").ends_with("10 days old"), "{rows:?}");
    f.build_epoch = Some(f.now - 90 * 86_400);
    let rows = self::rows(&env(Config::default()), &f);
    assert!(value(&rows, "Build").contains("90 days old (old: omsh update --check)"));
}

#[test]
fn the_ai_row_comes_from_the_configuration_alone() {
    let mut config = Config::default();
    assert_eq!(
        value(&rows(&env(config.clone()), &facts()), "AI"),
        "not set up (omsh ai setup)"
    );
    config.ai.provider = Some(Provider::Ollama);
    config.ai.model = Some("llama".to_owned());
    assert_eq!(
        value(&rows(&env(config.clone()), &facts()), "AI"),
        "ready: ollama, llama"
    );
    config.ai.enabled = false;
    assert_eq!(
        value(&rows(&env(config.clone()), &facts()), "AI"),
        "off (ai.enabled = false)"
    );
    let mut flagged = env(Config::default());
    flagged.no_ai = true;
    assert_eq!(
        value(&rows(&flagged, &facts()), "AI"),
        "off for this session (--no-ai)"
    );
}

#[test]
fn the_update_rows_say_off_on_and_why_it_does_not_run() {
    let mut config = Config::default();
    let off = rows(&env(config.clone()), &facts());
    assert!(value(&off, "Update check").starts_with("off"), "{off:?}");
    config.update.check = true;
    let on = rows(&env(config.clone()), &facts());
    assert_eq!(value(&on, "Update check"), "on, once a day");
    let mut f = facts();
    f.opt_out = Some(b"1");
    let skipped = rows(&env(config.clone()), &f);
    assert_eq!(
        value(&skipped, "Update check"),
        "on, but not running here: OMSH_NO_UPDATE_CHECK is set"
    );
    let mut f = facts();
    f.caps.ci = true;
    let ci = rows(&env(config), &f);
    assert!(value(&ci, "Update check").contains("CI run"), "{ci:?}");
}

#[test]
fn the_last_look_comes_from_the_stamp_and_is_never_when_there_is_none() {
    let dir = std::env::temp_dir().join(format!("omsh-status-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let mut e = env(Config::default());
    e.state_dir = Some(dir.clone());
    assert_eq!(value(&rows(&e, &facts()), "Last look"), "never");
    daily::write(
        &dir,
        &Stamp {
            checked: 1_700_000_000,
            latest: Version::parse("1.2.0"),
            noticed: None,
        },
    )
    .unwrap();
    let rows = rows(&e, &facts());
    assert!(
        value(&rows, "Last look").ends_with("newest is 1.2.0, newer than yours"),
        "{rows:?}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn motion_names_what_decided_it() {
    let config = Config::default();
    assert_eq!(
        value(&rows(&env(config.clone()), &facts()), "Motion"),
        "full (automatic)"
    );
    let mut f = facts();
    f.caps.ssh = true;
    f.caps.motion = Motion::Reduced;
    assert_eq!(
        value(&rows(&env(config.clone()), &f), "Motion"),
        "reduced (automatic: ssh)"
    );
    let mut f = facts();
    f.env_motion = Some("off");
    f.caps.motion = Motion::Off;
    assert_eq!(
        value(&rows(&env(config.clone()), &f), "Motion"),
        "off (OMSH_MOTION)"
    );
    let mut f = facts();
    f.caps.out_tty = false;
    f.caps.motion = Motion::Off;
    assert_eq!(
        value(&rows(&env(config), &f), "Motion"),
        "off (not a terminal)"
    );
}

#[test]
fn the_terminal_rows_say_what_was_found_and_what_was_not_asked() {
    let rows = rows(&env(Config::default()), &facts());
    assert_eq!(value(&rows, "Terminal"), "xterm-256color, 100x30");
    assert_eq!(value(&rows, "Streams"), "stdin tty, stdout tty, stderr tty");
    assert_eq!(value(&rows, "Sync output"), "not asked yet");
    let mut f = facts();
    f.sync_output = Some(true);
    f.term = None;
    f.size = None;
    let rows = self::rows(&env(Config::default()), &f);
    assert_eq!(value(&rows, "Sync output"), "yes");
    assert_eq!(value(&rows, "Terminal"), "TERM not set, size unknown");
}

#[test]
fn the_paths_are_shown_and_no_config_says_none() {
    let rows = rows(&env(Config::default()), &facts());
    assert!(value(&rows, "Config file").ends_with("oh-my-shell/config.toml"));
    let mut e = env(Config::default());
    e.no_config = true;
    assert_eq!(
        value(&self::rows(&e, &facts()), "Config file"),
        "none (--no-config)"
    );
}
