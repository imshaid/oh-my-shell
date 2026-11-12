use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use super::*;
use crate::schema::SECTIONS;

/// A directory for one test, removed at the end.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "omsh-config-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn paths_in(dir: &Scratch) -> Paths {
    let home = dir.0.clone();
    Paths::from_lookup(|name| (name == "HOME").then(|| home.clone().into_os_string()))
}

fn messages(parsed: &Parsed) -> Vec<String> {
    parsed.problems.iter().map(Problem::describe).collect()
}

#[test]
fn an_empty_file_gives_the_defaults() {
    let parsed = parse("");
    assert_eq!(parsed.config, Config::default());
    assert!(parsed.problems.is_empty());
}

#[test]
fn every_setting_can_be_given() {
    let parsed = parse(
        r#"
[ui]
theme = "my-theme_2"
motion = "off"
icons = "ascii"
[editor]
mode = "vi"
hints = false
[editor.abbreviations]
gco = "git checkout"
[prompt]
left = ["cwd"]
right = ["time", "status"]
transient = true
[history]
size = 500
ignore = ["ls", "cd *"]
[safety]
confirm = "caution"
trash_retention_days = 30
blast_radius_max_entries = 1000
blast_radius_max_ms = 50
[ai]
enabled = false
provider = "openai-compatible"
model = "m"
base_url = "https://example.com:8080/v1"
language = "bn"
[ai.context]
os = false
cwd = false
git = false
recent_commands = false
[notify]
enabled = false
min_seconds = 3
ignore = ["make"]
[update]
check = true
channel = "stable"
"#,
    );
    assert!(parsed.problems.is_empty(), "{:?}", messages(&parsed));
    let c = parsed.config;
    assert_eq!(c.ui.theme, "my-theme_2");
    assert_eq!(c.ui.motion, Some(Motion::Off));
    assert_eq!(c.ui.icons, Icons::Ascii);
    assert_eq!(c.editor.mode, EditMode::Vi);
    assert!(!c.editor.hints);
    assert_eq!(c.editor.abbreviations["gco"], "git checkout");
    assert_eq!(c.prompt.left, ["cwd"]);
    assert_eq!(c.prompt.right, ["time", "status"]);
    assert!(c.prompt.transient);
    assert_eq!((c.history.size, c.history.ignore.len()), (500, 2));
    assert_eq!(c.safety.confirm, ConfirmLevel::Caution);
    assert_eq!(c.safety.trash_retention_days, 30);
    assert_eq!(c.safety.blast_radius_max_entries, 1000);
    assert_eq!(c.safety.blast_radius_max_ms, 50);
    assert!(!c.notify.enabled);
    assert_eq!(c.notify.min_seconds, 3);
    assert_eq!(c.notify.ignore, ["make"]);
    assert!(!c.ai.enabled);
    assert_eq!(c.ai.provider, Some(Provider::OpenAiCompatible));
    assert_eq!(c.ai.model.as_deref(), Some("m"));
    assert_eq!(
        c.ai.base_url.as_deref(),
        Some("https://example.com:8080/v1")
    );
    assert_eq!(c.ai.language, AiLanguage::Bangla);
    assert!(!c.ai.context.os && !c.ai.context.cwd && !c.ai.context.git);
    assert!(!c.ai.context.recent_commands);
    assert!(c.update.check);
}

/// Done-when: a broken file still gives defaults, with a clear error at the right place.
#[test]
fn a_wrong_value_is_reported_where_it_is_and_the_default_stays() {
    let parsed = parse("[ui]\nicons = \"ascii\"\nmotion = \"fast\"\n");
    assert_eq!(parsed.config.ui.motion, None);
    assert_eq!(parsed.config.ui.icons, Icons::Ascii, "the good key applies");
    assert_eq!(
        messages(&parsed),
        [r#"3:10: ui.motion must be "full", "reduced" or "off", not "fast"; using the default"#]
    );
}

#[test]
fn each_kind_of_wrong_value_names_its_setting() {
    let cases = [
        (
            "[history]\nsize = \"lots\"",
            "history.size must be an integer, not a string",
        ),
        (
            "[history]\nsize = 0",
            "history.size must be a whole number from 1 to 10000000, not 0",
        ),
        ("[history]\nsize = -4", "not -4"),
        (
            "[history]\nsize = 99999999999999999999",
            "history.size must be a whole number",
        ),
        (
            "[editor]\nhints = \"yes\"",
            "editor.hints must be true or false, not a string",
        ),
        (
            "[ui]\ntheme = 3",
            "ui.theme must be a string, not an integer",
        ),
        (
            "[ui]\ntheme = \"../x\"",
            "ui.theme must be a name made of letters",
        ),
        (
            "[ui]\ntheme = \"\"",
            "ui.theme must be a name made of letters",
        ),
        ("ui = 3", "ui must be a table, not an integer"),
        (
            "[prompt]\nleft = \"cwd\"",
            "prompt.left must be an array of strings, not a string",
        ),
        (
            "[prompt]\nleft = [\"cwd\", 3]",
            "prompt.left[1] must be a string",
        ),
        (
            "[prompt]\nright = [\"nope\"]",
            "prompt.right has an unknown segment \"nope\"",
        ),
        (
            "[notify]\nmin_seconds = 0",
            "notify.min_seconds must be a whole number from 1 to 86400, not 0",
        ),
        (
            "[notify]\nenabled = \"yes\"",
            "notify.enabled must be true or false",
        ),
        (
            "[safety]\nconfirm = \"never\"",
            "safety.confirm must be \"caution\", \"dangerous\" or \"critical\"",
        ),
        (
            "[ai]\nprovider = \"x\"",
            "ai.provider must be \"ollama\", \"anthropic\", \"openai\", \"gemini\" or \"openai-compatible\"",
        ),
        ("[ai]\nmodel = \" \"", "ai.model must not be empty"),
        (
            "[ai]\nbase_url = \"ftp://x\"",
            "ai.base_url must start with http:// or https://",
        ),
        (
            "[ai]\nbase_url = \"http://\"",
            "ai.base_url must name a host",
        ),
        (
            "[update]\nchannel = \"beta\"",
            "update.channel must be \"stable\"",
        ),
        (
            "[editor.abbreviations]\ngco = 3",
            "editor.abbreviations.gco must be a string",
        ),
        (
            "[editor.abbreviations]\ngco = \"\"",
            "editor.abbreviations.gco must not be empty",
        ),
        (
            "[editor.abbreviations]\n\"a b\" = \"x\"",
            "an abbreviation must be one word",
        ),
    ];
    for (text, expected) in cases {
        let parsed = parse(text);
        let all = messages(&parsed).join("\n");
        assert!(
            all.contains(expected),
            "{text:?} gave {all:?}, wanted {expected:?}"
        );
        assert!(
            parsed.config.ai.base_url.is_none()
                && parsed.config.history.size == 100_000
                && parsed.config.ui.theme == "nord",
            "{text:?} changed a setting"
        );
        assert_eq!(parsed.config.prompt, Config::default().prompt, "{text:?}");
    }
}

#[test]
fn an_address_with_a_password_is_refused_and_not_repeated() {
    let parsed = parse("[ai]\nbase_url = \"https://user:hunter2@host/v1\"\n");
    assert_eq!(parsed.config.ai.base_url, None);
    let all = messages(&parsed).join("\n");
    assert!(
        all.contains("must not contain a user name or password"),
        "{all}"
    );
    assert!(!all.contains("hunter2"), "{all}");
}

#[test]
fn unknown_settings_are_warnings_and_the_rest_applies() {
    let parsed = parse("colour = 1\n[ui]\nicons = \"ascii\"\nfont = \"x\"\n[extra]\na = 1\n");
    assert_eq!(parsed.config.ui.icons, Icons::Ascii);
    assert!(
        parsed
            .problems
            .iter()
            .all(|p| p.severity == Severity::Warning)
    );
    assert_eq!(
        messages(&parsed),
        [
            "1:1: warning: unknown setting colour; ignored",
            "4:1: warning: unknown setting ui.font; ignored",
            "5:2: warning: unknown section [extra]; ignored",
        ]
    );
}

#[test]
fn an_api_key_gets_a_message_that_says_where_keys_go() {
    for key in ["api_key", "token", "OPENAI_API_KEY", "client_secret"] {
        let parsed = parse(&format!("[ai]\n{key} = \"sk-123\"\n"));
        let all = messages(&parsed).join("\n");
        assert!(all.contains("never kept in this file"), "{key}: {all}");
        assert!(
            !all.contains("sk-123"),
            "the value must not be repeated: {all}"
        );
    }
    // Not every name that contains "key" is a secret.
    let parsed = parse("[ai]\nkeyboard = 1\n");
    assert!(messages(&parsed)[0].contains("unknown setting ai.keyboard"));
}

#[test]
fn a_syntax_error_is_reported_and_the_good_settings_around_it_apply() {
    let parsed =
        parse("[ui]\nicons = \"ascii\"\n[history]\nsize = \n[safety]\nconfirm = \"critical\"\n");
    assert_eq!(parsed.config.ui.icons, Icons::Ascii);
    assert_eq!(parsed.config.safety.confirm, ConfirmLevel::Critical);
    assert_eq!(parsed.config.history.size, 100_000);
    assert_eq!(parsed.problems.len(), 1, "{:?}", messages(&parsed));
    assert_eq!(parsed.problems[0].line, Some(4));
    assert_eq!(parsed.problems[0].severity, Severity::Error);
}

/// Found by hand: after an unclosed header the parser guessed that `mode` was a section, and
/// the report said "unknown section [mode]" on top of the real error.
#[test]
fn a_broken_header_does_not_cause_made_up_unknown_sections() {
    let parsed = parse("[editor\nmode = \"vi\"\n");
    assert_eq!(parsed.problems.len(), 1, "{:?}", messages(&parsed));
    assert_eq!(parsed.problems[0].severity, Severity::Error);
    assert_eq!(parsed.problems[0].line, Some(1));
    // Unknown settings are still reported when the file has no syntax error.
    let parsed = parse("[ui]\nfont = 1\n");
    assert_eq!(parsed.problems.len(), 1);
}

#[test]
fn a_file_that_is_all_wrong_still_gives_the_defaults() {
    let parsed = parse("this is [not toml\n=\n[[[\n");
    assert_eq!(parsed.config, Config::default());
    assert!(!parsed.problems.is_empty());
    assert!(parsed.problems.iter().all(|p| p.line.is_some()));
}

#[test]
fn problems_come_in_the_order_of_the_file() {
    let parsed = parse("[safety]\nconfirm = 1\n[ui]\nmotion = 2\n[editor]\nmode = 3\n");
    let lines: Vec<_> = parsed.problems.iter().map(|p| p.line.unwrap()).collect();
    assert_eq!(lines, [2, 4, 6]);
}

#[test]
fn the_column_counts_characters_not_bytes() {
    // Three Bangla letters (three bytes each) come before the bad value on the line.
    let parsed = parse("[ui]\n\"\u{9ac}\u{9be}\u{982}\" = 1\nmotion = \"x\"\n");
    let motion = parsed
        .problems
        .iter()
        .find(|p| p.message.contains("ui.motion"))
        .unwrap();
    assert_eq!((motion.line, motion.column), (Some(3), Some(10)));
    // And on the line with the Bangla letters themselves: the unknown key starts at column 1.
    let unknown = parsed
        .problems
        .iter()
        .find(|p| p.message.contains("unknown"))
        .unwrap();
    assert_eq!((unknown.line, unknown.column), (Some(2), Some(1)));

    // On the line of the Bangla letters themselves: the bad value is the 9th character
    // (it would be the 15th byte).
    let parsed = parse("[editor.abbreviations]\n\"\u{9ac}\u{9be}\u{982}\" = 1\n");
    assert_eq!(
        (parsed.problems[0].line, parsed.problems[0].column),
        (Some(2), Some(9))
    );
}

#[test]
fn crlf_line_endings_work() {
    let parsed = parse("[ui]\r\nmotion = \"x\"\r\nicons = \"ascii\"\r\n");
    assert_eq!(parsed.config.ui.icons, Icons::Ascii);
    assert_eq!(parsed.problems.len(), 1);
    assert_eq!(
        (parsed.problems[0].line, parsed.problems[0].column),
        (Some(2), Some(10))
    );
}

#[test]
fn abbreviations_keep_the_good_entries() {
    let parsed =
        parse("[editor.abbreviations]\ngco = \"git checkout\"\nbad = 1\ngst = \"git status\"\n");
    assert_eq!(parsed.config.editor.abbreviations.len(), 2);
    assert_eq!(parsed.problems.len(), 1);
}

#[test]
fn a_dotted_key_and_an_inline_table_work() {
    let parsed = parse("ui.icons = \"ascii\"\nsafety = { confirm = \"caution\" }\n");
    assert!(parsed.problems.is_empty(), "{:?}", messages(&parsed));
    assert_eq!(parsed.config.ui.icons, Icons::Ascii);
    assert_eq!(parsed.config.safety.confirm, ConfirmLevel::Caution);
}

// The first-run file.

/// Removes the `#` from the lines of the template that are settings at their default.
fn uncommented() -> String {
    first_run::TEMPLATE
        .lines()
        .map(|line| match line.strip_prefix('#') {
            Some(rest) if !rest.is_empty() && !rest.starts_with(' ') => rest,
            _ => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_first_run_file_with_its_comments_removed_is_exactly_the_defaults() {
    let parsed = parse(&uncommented());
    assert!(parsed.problems.is_empty(), "{:?}", messages(&parsed));
    assert_eq!(parsed.config, Config::default());
}

#[test]
fn the_first_run_file_as_written_has_no_problems() {
    let parsed = parse(first_run::TEMPLATE);
    assert!(parsed.problems.is_empty());
    assert_eq!(parsed.config, Config::default());
}

#[test]
fn the_first_run_file_mentions_every_setting() {
    for (section, keys) in SECTIONS {
        assert!(
            first_run::TEMPLATE.contains(&format!("[{section}]")),
            "section {section} is not in the first-run file"
        );
        for key in *keys {
            let sub_table = SECTIONS
                .iter()
                .any(|(name, _)| *name == format!("{section}.{key}"))
                || (*section == "editor" && *key == "abbreviations");
            let wanted = if sub_table {
                format!("[{section}.{key}]")
            } else {
                format!("{key} = ")
            };
            assert!(
                first_run::TEMPLATE.contains(&wanted),
                "{section}.{key} is not in the first-run file"
            );
        }
    }
}

#[test]
fn a_missing_file_is_created_once_with_the_right_modes() {
    let scratch = Scratch::new();
    let paths = paths_in(&scratch);
    let loaded = load(&paths);
    assert!(loaded.created);
    assert!(loaded.problems.is_empty());
    assert_eq!(loaded.config, Config::default());
    let file = paths.config_file().unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), first_run::TEMPLATE);
    let mode = |path: &std::path::Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(file.parent().unwrap()), 0o700);
    assert_eq!(
        mode(&file) & 0o133,
        0,
        "no execute, nothing writable by others"
    );

    // A second start leaves the file alone, even after the user edited it.
    fs::write(&file, "[ui]\nicons = \"ascii\"\n").unwrap();
    let again = load(&paths);
    assert!(!again.created);
    assert_eq!(again.config.ui.icons, Icons::Ascii);
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "[ui]\nicons = \"ascii\"\n"
    );
    // No temporary file stays behind.
    let names: Vec<_> = fs::read_dir(file.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
}

#[test]
fn writers_that_race_leave_one_whole_file() {
    let scratch = Scratch::new();
    let file = scratch.0.join("cfg/config.toml");
    let results: Vec<bool> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| first_run::create(&file, first_run::TEMPLATE).unwrap()))
            .collect();
        workers.into_iter().map(|w| w.join().unwrap()).collect()
    });
    assert_eq!(results.iter().filter(|created| **created).count(), 1);
    assert_eq!(fs::read_to_string(&file).unwrap(), first_run::TEMPLATE);
    assert_eq!(fs::read_dir(file.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn an_existing_file_is_never_overwritten() {
    let scratch = Scratch::new();
    let file = scratch.0.join("config.toml");
    fs::write(&file, "mine").unwrap();
    assert!(!first_run::create(&file, "other").unwrap());
    assert_eq!(fs::read_to_string(&file).unwrap(), "mine");
}

#[test]
fn a_broken_file_on_disk_gives_defaults_and_a_report_with_the_file_name() {
    let scratch = Scratch::new();
    let paths = paths_in(&scratch);
    let file = paths.config_file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(
        &file,
        "[ui]\nmotion = \"fast\"\nicons = \"ascii\"\n[history]\nsize = \"lots\"\n",
    )
    .unwrap();
    let loaded = load(&paths);
    assert!(!loaded.created);
    assert_eq!(loaded.config.ui.icons, Icons::Ascii);
    assert_eq!(loaded.config.history.size, 100_000);
    let report = loaded.report(10);
    assert_eq!(report.len(), 2);
    let name = file.display();
    assert!(
        report[0].starts_with(&format!("{name}:2:10: ")),
        "{report:?}"
    );
    assert!(
        report[1].starts_with(&format!("{name}:5:8: ")),
        "{report:?}"
    );
}

#[test]
fn a_report_is_cut_after_the_limit() {
    let scratch = Scratch::new();
    let paths = paths_in(&scratch);
    let file = paths.config_file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    let text: String = (0..15).map(|i| format!("unknown{i} = 1\n")).collect();
    fs::write(&file, text).unwrap();
    let report = load(&paths).report(10);
    assert_eq!(report.len(), 11);
    assert!(
        report[10].ends_with("... and 5 more problems"),
        "{report:?}"
    );
}

#[test]
fn a_file_that_is_not_utf8_gives_defaults_and_a_place() {
    let scratch = Scratch::new();
    let paths = paths_in(&scratch);
    let file = paths.config_file().unwrap();
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, b"[ui]\nicons = \"ascii\"\n# \xff\xfe\n").unwrap();
    let loaded = load(&paths);
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.problems.len(), 1);
    assert!(loaded.problems[0].message.contains("not valid UTF-8"));
    assert_eq!(loaded.problems[0].line, Some(3));
}

#[test]
fn a_directory_a_fifo_and_a_huge_file_are_refused_without_hanging() {
    let scratch = Scratch::new();
    let paths = paths_in(&scratch);
    let file = paths.config_file().unwrap();
    let dir = file.parent().unwrap();
    fs::create_dir_all(dir).unwrap();

    fs::create_dir(&file).unwrap();
    let loaded = load(&paths);
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.problems.len(), 1, "{:?}", loaded.problems);
    fs::remove_dir(&file).unwrap();

    let made = std::process::Command::new("mkfifo").arg(&file).status();
    if made.is_ok_and(|s| s.success()) {
        // Reading a FIFO that no one writes to blocks for ever, so the call runs on a thread
        // that the test can give up on.
        let (sender, receiver) = std::sync::mpsc::channel();
        let for_thread = paths.clone();
        std::thread::spawn(move || drop(sender.send(load(&for_thread))));
        let loaded = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("load() blocked on a FIFO");
        assert!(loaded.problems[0].message.contains("not a regular file"));
        fs::remove_file(&file).unwrap();
    }

    fs::write(&file, vec![b'#'; 2 * 1024 * 1024]).unwrap();
    let loaded = load(&paths);
    assert!(loaded.problems[0].message.contains("larger than"));
}

#[test]
fn without_a_home_the_defaults_are_used_with_one_warning() {
    let loaded = load(&Paths::from_lookup(|_| None));
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.problems.len(), 1);
    assert_eq!(loaded.file, None);
    assert!(loaded.report(10)[0].starts_with("config: warning: no home directory"));
}

#[test]
fn a_directory_that_cannot_be_written_is_one_warning() {
    let scratch = Scratch::new();
    // A read-only `.config`: the file is missing and cannot be created.
    let config = scratch.0.join(".config");
    fs::create_dir(&config).unwrap();
    fs::set_permissions(&config, fs::Permissions::from_mode(0o500)).unwrap();
    let loaded = load(&paths_in(&scratch));
    fs::set_permissions(&config, fs::Permissions::from_mode(0o700)).unwrap();
    if loaded.created {
        return; // running as root: permissions do not apply
    }
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.problems.len(), 1);
    assert_eq!(loaded.problems[0].severity, Severity::Warning);
    assert!(loaded.problems[0].message.contains("could not be written"));
}

#[test]
fn a_path_through_a_regular_file_is_one_error_and_the_defaults() {
    let scratch = Scratch::new();
    fs::write(scratch.0.join(".config"), "x").unwrap();
    let loaded = load(&paths_in(&scratch));
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.problems.len(), 1);
    assert!(loaded.problems[0].message.contains("cannot read the file"));
}

#[test]
fn notifications_are_on_after_ten_seconds_and_skip_the_interactive_programs() {
    let c = Config::default();
    assert!(c.notify.enabled);
    assert_eq!(c.notify.min_seconds, 10);
    for name in ["vim", "less", "ssh", "tmux", "man", "htop", "bash"] {
        assert!(c.notify.ignore.iter().any(|n| n == name), "{name}");
    }
    // A bad list keeps the default for that key only.
    let parsed = parse("[notify]\nignore = 3\nmin_seconds = 5\n");
    assert_eq!(parsed.config.notify.min_seconds, 5);
    assert_eq!(parsed.config.notify.ignore, Config::default().notify.ignore);
    assert_eq!(parsed.problems.len(), 1);
}
