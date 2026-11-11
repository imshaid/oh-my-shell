use std::fs::{self, File};
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use super::*;

/// A lookup that knows a fixed set of names and counts how often it was asked.
struct Fake {
    known: Vec<&'static str>,
    asked: Arc<AtomicUsize>,
}

impl CommandLookup for Fake {
    fn is_command(&self, name: &str) -> bool {
        self.asked.fetch_add(1, Ordering::Relaxed);
        self.known.contains(&name)
    }
}

/// A directory that is removed when it goes out of scope.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("omsh-hl-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn file(&self, name: &str) {
        File::create(self.0.join(name)).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const KNOWN: [&str; 4] = ["ls", "cat", "echo", "grep"];

struct Setup {
    highlighter: ShellHighlighter,
    context: Arc<Mutex<Context>>,
    asked: Arc<AtomicUsize>,
}

impl Setup {
    fn new(theme: Theme, cwd: &Path, home: Option<&Path>) -> Self {
        let context = Arc::new(Mutex::new(Context::new()));
        let setup = Self {
            highlighter: ShellHighlighter::new(theme, Arc::clone(&context)),
            context,
            asked: Arc::new(AtomicUsize::new(0)),
        };
        setup.new_prompt(cwd, home);
        setup
    }

    fn new_prompt(&self, cwd: &Path, home: Option<&Path>) {
        let lookup = Fake {
            known: KNOWN.to_vec(),
            asked: Arc::clone(&self.asked),
        };
        self.context.lock().unwrap().reset(
            Box::new(lookup),
            cwd.to_path_buf(),
            home.map(Path::to_path_buf),
        );
    }

    /// Each piece of the line with the SGR codes it is drawn with (sorted).
    fn pieces(&self, line: &str) -> Vec<(String, Vec<u8>)> {
        self.highlighter
            .highlight(line, 0)
            .buffer
            .iter()
            .map(|(style, text)| (text.clone(), codes(*style)))
            .collect()
    }

    /// The codes of the piece whose text is exactly `text`.
    fn codes_of(&self, line: &str, text: &str) -> Vec<u8> {
        let pieces = self.pieces(line);
        pieces
            .iter()
            .find(|(piece, _)| piece == text)
            .unwrap_or_else(|| panic!("no piece {text:?} in {pieces:?}"))
            .1
            .clone()
    }
}

fn setup() -> Setup {
    Setup::new(Theme::default(), Path::new("/"), None)
}

/// The SGR codes of a style, as numbers, in order. Leading zeros (a legacy format) are fine.
fn codes(style: Style) -> Vec<u8> {
    let prefix = style.prefix().to_string();
    let inner = prefix
        .strip_prefix("\x1b[")
        .and_then(|rest| rest.strip_suffix('m'))
        .unwrap_or("");
    let mut codes: Vec<u8> = inner.split(';').filter_map(|c| c.parse().ok()).collect();
    codes.sort_unstable();
    codes
}

const RED: u8 = 31;
const GREEN: u8 = 32;
const YELLOW: u8 = 33;
const CYAN: u8 = 36;
const DIM: u8 = 2;
const UNDERLINE: u8 = 4;

#[test]
fn a_command_that_is_not_found_is_red() {
    let s = setup();
    assert_eq!(s.codes_of("nosuch x", "nosuch"), vec![RED]);
}

#[test]
fn a_command_that_exists_has_no_color() {
    let s = setup();
    assert_eq!(s.codes_of("ls", "ls"), Vec::<u8>::new());
    assert_eq!(s.codes_of("echo hi", "echo"), Vec::<u8>::new());
}

#[test]
fn a_reserved_word_has_no_color_and_is_not_looked_up() {
    let s = setup();
    assert_eq!(s.codes_of("if ls; then ls; fi", "if"), Vec::<u8>::new());
    assert_eq!(s.codes_of("if ls; then ls; fi", "then"), Vec::<u8>::new());
    assert_eq!(s.asked.load(Ordering::Relaxed), 1, "only `ls`");
}

#[test]
fn options_and_arguments_have_their_own_roles() {
    // Distinct colors for the two roles, so the test sees which one was used.
    let mut theme = Theme::default();
    theme.set(
        Role::Option,
        RoleStyle {
            color: Some(AnsiColor::Green),
            ..RoleStyle::PLAIN
        },
    );
    theme.set(
        Role::Param,
        RoleStyle {
            color: Some(AnsiColor::Blue),
            ..RoleStyle::PLAIN
        },
    );
    let s = Setup::new(theme, Path::new("/"), None);
    assert_eq!(s.codes_of("ls -la foo", "-la"), vec![GREEN]);
    assert_eq!(s.codes_of("ls -la foo", "foo"), vec![34]);
    assert_eq!(s.codes_of("ls --color=auto", "--color=auto"), vec![GREEN]);
}

#[test]
fn quotes_are_yellow_and_the_rest_of_the_word_is_not() {
    let s = setup();
    assert_eq!(s.codes_of("echo 'a b'", "'a b'"), vec![YELLOW]);
    assert_eq!(s.codes_of("echo \"a b\"", "\"a b\""), vec![YELLOW]);
    let pieces = s.pieces("echo x'y'z");
    assert_eq!(
        pieces[2..],
        [
            ("x".to_owned(), vec![CYAN]),
            ("'y'".to_owned(), vec![YELLOW]),
            ("z".to_owned(), vec![CYAN])
        ]
    );
}

#[test]
fn separators_are_green_and_operators_cyan() {
    let s = setup();
    let line = "ls ; ls & ls | cat && ls || ls";
    assert_eq!(s.codes_of(line, ";"), vec![GREEN]);
    assert_eq!(s.codes_of(line, "&"), vec![GREEN]);
    assert_eq!(s.codes_of(line, "|"), vec![CYAN]);
    assert_eq!(s.codes_of(line, "&&"), vec![CYAN]);
    assert_eq!(s.codes_of(line, "||"), vec![CYAN]);
}

#[test]
fn redirections_are_cyan() {
    let s = setup();
    let line = "ls > out 2>&1 < in >> log";
    for text in [">", "2", ">&", "<", ">>"] {
        assert_eq!(s.codes_of(line, text), vec![CYAN], "{text}");
    }
}

#[test]
fn a_comment_is_red_and_dim() {
    let s = setup();
    assert_eq!(s.codes_of("ls # why", "# why"), vec![DIM, RED]);
}

#[test]
fn variables_and_substitutions_are_expansions_and_commands_inside_are_checked() {
    let s = setup();
    assert_eq!(s.codes_of("echo $HOME", "$HOME"), vec![CYAN]);
    assert_eq!(s.codes_of("echo $(nosuch)", "nosuch"), vec![RED]);
    assert_eq!(s.codes_of("echo $(ls)", "ls"), Vec::<u8>::new());
}

#[test]
fn a_command_with_an_expansion_is_not_checked() {
    let s = setup();
    for line in ["$cmd x", "\"$cmd\" x", "$(which ls) x", "`which ls` x"] {
        let first = &s.pieces(line)[0];
        assert!(!first.1.contains(&RED), "{line}: {first:?}");
    }
}

#[test]
fn a_quoted_command_that_is_not_found_is_red_all_over() {
    let s = setup();
    assert_eq!(s.codes_of("\"nosuch\"", "\"nosuch\""), vec![RED]);
    // Quotes are taken away before the lookup.
    assert_eq!(s.codes_of("'ls'", "'ls'"), vec![YELLOW]);
    assert_eq!(s.codes_of("l\\s", "l"), Vec::<u8>::new());
}

#[test]
fn an_existing_file_is_underlined_and_keeps_its_color() {
    let dir = TempDir::new();
    dir.file("a.txt");
    fs::create_dir(dir.0.join("sub dir")).unwrap();
    let s = Setup::new(Theme::default(), &dir.0, None);
    assert_eq!(
        s.codes_of("cat a.txt missing", "a.txt"),
        vec![UNDERLINE, CYAN]
    );
    assert_eq!(s.codes_of("cat a.txt missing", "missing"), vec![CYAN]);
    assert_eq!(s.codes_of("ls ./a.txt", "./a.txt"), vec![UNDERLINE, CYAN]);
    // A directory counts, and so does a quoted name with a blank.
    assert_eq!(
        s.codes_of("ls 'sub dir'", "'sub dir'"),
        vec![UNDERLINE, YELLOW]
    );
    assert_eq!(s.codes_of("ls sub\\ dir", "sub"), vec![UNDERLINE, CYAN]);
    // A redirection target is checked as well.
    assert_eq!(s.codes_of("ls < a.txt", "a.txt"), vec![UNDERLINE, CYAN]);
}

#[test]
fn a_path_is_checked_from_the_prompts_directory_and_with_the_home_for_a_tilde() {
    let dir = TempDir::new();
    dir.file("in-home");
    let elsewhere = TempDir::new();
    let s = Setup::new(Theme::default(), &elsewhere.0, Some(&dir.0));
    assert_eq!(
        s.codes_of("cat ~/in-home", "~/in-home"),
        vec![UNDERLINE, CYAN]
    );
    assert_eq!(s.codes_of("cat ~/nope", "~/nope"), vec![CYAN]);
    assert_eq!(
        s.codes_of("cat in-home", "in-home"),
        vec![CYAN],
        "not in the cwd"
    );
    let absolute = dir.0.join("in-home");
    let line = format!("cat {}", absolute.display());
    assert_eq!(
        s.codes_of(&line, absolute.to_str().unwrap()),
        vec![UNDERLINE, CYAN]
    );
    // `~user` cannot be checked here.
    assert_eq!(s.codes_of("cat ~root", "~root"), vec![CYAN]);
}

#[test]
fn an_option_is_never_underlined_and_neither_is_a_word_with_an_expansion() {
    let dir = TempDir::new();
    dir.file("-l");
    dir.file("x");
    let s = Setup::new(Theme::default(), &dir.0, None);
    assert_eq!(s.codes_of("ls -l", "-l"), vec![CYAN]);
    assert!(!s.codes_of("ls $PWD/x", "$PWD").contains(&UNDERLINE));
    assert_eq!(
        s.codes_of("ls x*", "x*"),
        vec![CYAN],
        "a pattern is not a name"
    );
}

#[test]
fn a_command_that_is_a_path_is_checked_by_the_lookup() {
    let s = setup();
    // The fake knows no path, so a command with a slash is "not found".
    assert_eq!(s.codes_of("./nosuch", "./nosuch"), vec![RED]);
}

#[test]
fn answers_are_remembered_until_the_next_prompt() {
    let dir = TempDir::new();
    let s = Setup::new(Theme::default(), &dir.0, None);
    s.pieces("nosuch");
    s.pieces("nosuch again");
    s.pieces("nosuch");
    assert_eq!(
        s.asked.load(Ordering::Relaxed),
        1,
        "asked once for `nosuch`"
    );
    s.pieces("nosuc");
    assert_eq!(
        s.asked.load(Ordering::Relaxed),
        2,
        "a different word is a new question"
    );

    // A file made while the line is being typed is not seen until the next prompt ...
    s.pieces("cat late");
    dir.file("late");
    assert_eq!(s.codes_of("cat late", "late"), vec![CYAN]);
    // ... and then it is, and the command questions are asked again.
    s.new_prompt(&dir.0, None);
    assert_eq!(s.codes_of("cat late", "late"), vec![UNDERLINE, CYAN]);
    let before = s.asked.load(Ordering::Relaxed);
    s.pieces("nosuch");
    assert_eq!(s.asked.load(Ordering::Relaxed), before + 1, "asked again");
}

#[test]
fn before_the_first_prompt_nothing_is_called_missing() {
    let context = Arc::new(Mutex::new(Context::new()));
    let highlighter = ShellHighlighter::new(Theme::default(), context);
    let text = highlighter.highlight("nosuch x", 0);
    assert!(
        text.buffer
            .iter()
            .all(|(style, _)| !codes(*style).contains(&RED))
    );
}

#[test]
fn a_quote_that_is_still_open_is_not_an_error() {
    let s = setup();
    for line in [
        "echo \"abc",
        "echo 'abc",
        "echo $(ls",
        "echo ${x",
        "echo `ls",
        "ls \\",
    ] {
        for (text, codes) in s.pieces(line) {
            assert!(!codes.contains(&RED), "{line}: {text:?} is red");
        }
    }
}

#[test]
fn a_real_lexer_error_is_red() {
    let s = setup();
    let line = format!("echo {}", "$(".repeat(200));
    assert!(
        s.pieces(&line)
            .iter()
            .any(|(_, codes)| codes.contains(&RED)),
        "too deep nesting is shown as an error"
    );
}

#[test]
fn no_color_removes_every_color_and_keeps_the_attributes() {
    let dir = TempDir::new();
    dir.file("f");
    let s = Setup::new(Theme::default().without_color(), &dir.0, None);
    let line = "nosuch -x 'q' f ; ls | cat > f # note";
    for (text, codes) in s.pieces(line) {
        assert!(
            codes
                .iter()
                .all(|c| !(30..=37).contains(c) && !(90..=97).contains(c)),
            "{text:?} has a color: {codes:?}"
        );
    }
    assert_eq!(s.codes_of(line, "# note"), vec![DIM]);
    assert_eq!(s.codes_of("cat f", "f"), vec![UNDERLINE]);
}

const GALLERY: [&str; 12] = [
    "ls -la /tmp | grep 'x y' > out.txt 2>&1 ; echo \"done: $?\" &",
    "if [ -f a ]; then echo yes; else echo no; fi # why",
    "for i in 1 2 3; do echo $i; done",
    "cat <<EOF\nbody $x\nEOF\nls",
    "A=1 B=\"two words\" cmd --opt=val arg",
    "echo $((1 + 2)) $(ls | wc -l) `date`",
    "case $x in a|b) echo ab;; *) echo other;; esac",
    "echo \\\n  continued",
    "ls \"unclosed",
    "echo ক খ গ | grep 'বাংলা'",
    "~/bin/tool ~ ~root",
    "  \t  ",
];

#[test]
fn the_text_is_never_changed() {
    let s = setup();
    for line in GALLERY {
        // Every prefix: text being typed is always a prefix of a finished line.
        for end in (0..=line.len()).filter(|e| line.is_char_boundary(*e)) {
            let prefix = &line[..end];
            assert_eq!(s.highlighter.highlight(prefix, end).raw_string(), prefix);
        }
    }
}

#[test]
fn nothing_is_bold_and_only_comments_are_dim() {
    let s = setup();
    for line in GALLERY {
        for (text, codes) in s.pieces(line) {
            assert!(!codes.contains(&1), "{text:?} in {line:?} is bold");
            assert!(
                !codes.contains(&DIM) || text.starts_with('#'),
                "{text:?} in {line:?} is dim but is no comment"
            );
        }
    }
}

#[test]
fn only_ansi_colors_are_ever_used() {
    let s = setup();
    for line in GALLERY {
        for (_, codes) in s.pieces(line) {
            for code in codes {
                assert!(
                    matches!(code, 2 | 4 | 7 | 30..=37 | 90..=97),
                    "{code} in {line:?} is not a plain ANSI code"
                );
            }
        }
    }
}

#[test]
fn every_color_name_is_drawn_with_its_own_code() {
    for color in AnsiColor::ALL {
        let style = to_style(RoleStyle {
            color: Some(color),
            ..RoleStyle::PLAIN
        });
        assert_eq!(codes(style), vec![color.sgr_foreground()], "{color:?}");
    }
    let all = to_style(RoleStyle {
        color: None,
        dim: true,
        underline: true,
        reverse: true,
    });
    assert_eq!(codes(all), vec![DIM, UNDERLINE, 7]);
    assert_eq!(codes(to_style(RoleStyle::PLAIN)), Vec::<u8>::new());
}

#[test]
fn literal_values_take_quotes_and_escapes_away() {
    let value = |text: &str| literal_value(text);
    assert_eq!(value("plain"), Some("plain".to_owned()));
    assert_eq!(value("'a b'c"), Some("a bc".to_owned()));
    assert_eq!(value("\"a b\"c"), Some("a bc".to_owned()));
    assert_eq!(value("a\\ b"), Some("a b".to_owned()));
    assert_eq!(
        value("\"a\\$b\\\"c\\\\d\\e\""),
        Some("a$b\"c\\d\\e".to_owned())
    );
    assert_eq!(value("a\\\nb"), Some("ab".to_owned()), "line continuation");
    assert_eq!(
        value("'$x'"),
        Some("$x".to_owned()),
        "no expansion in single quotes"
    );
    // An unfinished quote runs to the end; a trailing backslash is nothing.
    assert_eq!(value("\"abc"), Some("abc".to_owned()));
    assert_eq!(value("'abc"), Some("abc".to_owned()));
    assert_eq!(value("abc\\"), Some("abc".to_owned()));
    // Anything that needs the shell to run.
    assert_eq!(value("$x"), None);
    assert_eq!(value("a${x}b"), None);
    assert_eq!(value("\"$x\""), None);
    assert_eq!(value("`x`"), None);
    assert_eq!(value("$'a'"), None);
    assert_eq!(value("$(x)"), None);
}

#[test]
fn a_poisoned_context_does_not_stop_highlighting() {
    let s = setup();
    let context = Arc::clone(&s.context);
    let _ = std::thread::spawn(move || {
        let _guard = context.lock().unwrap();
        panic!("poison the lock");
    })
    .join();
    assert_eq!(s.codes_of("nosuch", "nosuch"), vec![RED]);
}

#[test]
fn a_request_to_the_ai_is_plain_text() {
    let s = setup();
    // No red `?`, no yellow quote, no lookup of a command.
    for line in ["? what's in *.txt | here", "?? nosuch 'x", "  ? list files"] {
        let pieces = s.pieces(line);
        assert_eq!(pieces.len(), 1, "{pieces:?}");
        assert_eq!(pieces[0].1, Vec::<u8>::new(), "{line:?}");
        assert_eq!(pieces[0].0, line);
    }
    assert_eq!(s.asked.load(Ordering::Relaxed), 0);
    // `?x` is still a glob word and `nosuch ?` is still an unknown command.
    assert_eq!(s.codes_of("nosuch ? x", "nosuch"), vec![RED]);
}
