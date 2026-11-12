//! Flags of commands, read from fish's completion files (`complete -c ls -s a -l all -d "…"`).
//!
//! fish ships a completion file for each of hundreds of commands, and other packages add more
//! under `vendor_completions.d`. If they are on the machine, their plain option lines are a
//! free list of flags with a one-line description. Only the facts needed to offer a flag are
//! taken: the command (`-c`), the short (`-s`), long (`-l`) and old-style (`-o`) names, and the
//! description (`-d`). The files are never run; they are read as text.
//!
//! Lines that depend on a condition (`-n …`, mostly the sub-commands of git, docker, …) are left
//! out in this version: the condition is fish code. A line that this parser does not understand
//! is skipped, and nothing here can fail.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

/// A file larger than this is not read.
const FILE_LIMIT: u64 = 4 * 1024 * 1024;
/// Descriptions are cut to this many characters.
const DESCRIPTION_LIMIT: usize = 80;

/// One option of a command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Flag {
    /// `-a`
    pub short: Option<char>,
    /// `--all`
    pub long: Option<String>,
    /// `-help` (fish's "old style": a single dash and a longer name).
    pub old: Option<String>,
    pub description: String,
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// The directories to look in, first match wins: the user's own, the vendor directories of
/// installed packages, then fish's own. `config_home`, `data_home` and `data_dirs` are the values
/// of `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_DATA_DIRS`. Relative entries are ignored.
pub fn directories(
    home: Option<&Path>,
    config_home: Option<&str>,
    data_home: Option<&str>,
    data_dirs: Option<&str>,
) -> Vec<PathBuf> {
    let config = non_empty(config_home)
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".config")));
    let data = non_empty(data_home)
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".local/share")));
    let shared: Vec<PathBuf> = non_empty(data_dirs)
        .unwrap_or("/usr/local/share:/usr/share")
        .split(':')
        .map(PathBuf::from)
        .collect();
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |dir: PathBuf| {
        if dir.is_absolute() && !out.contains(&dir) {
            out.push(dir);
        }
    };
    if let Some(config) = config {
        add(config.join("fish/completions"));
    }
    for base in data.iter().chain(&shared) {
        add(base.join("fish/vendor_completions.d"));
    }
    for base in &shared {
        add(base.join("fish/completions"));
    }
    out
}

/// The flags of `command`, from the first `<command>.fish` found in `dirs`. Nothing if there is
/// no such file. `None` if `cancelled` said stop.
pub(crate) fn load(
    command: &str,
    dirs: &[PathBuf],
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<Flag>> {
    // The name becomes part of a path: only a plain file name is allowed.
    let safe = !command.is_empty()
        && !command.starts_with('.')
        && !command.contains(['/', '\0'])
        && command.len() < 200;
    if !safe {
        return Some(Vec::new());
    }
    for dir in dirs {
        if cancelled() {
            return None;
        }
        let Ok(file) = File::open(dir.join(format!("{command}.fish"))) else {
            continue;
        };
        let mut bytes = Vec::new();
        if file.take(FILE_LIMIT).read_to_end(&mut bytes).is_err() {
            continue;
        }
        return Some(parse(command, &String::from_utf8_lossy(&bytes)));
    }
    Some(Vec::new())
}

/// The options of `command` in the text of a completion file.
pub(crate) fn parse(command: &str, text: &str) -> Vec<Flag> {
    let mut flags = Vec::new();
    let mut joined = String::new();
    for line in text.lines() {
        // A backslash at the end of a line continues it.
        if let Some(stem) = line.strip_suffix('\\') {
            joined.push_str(stem);
            joined.push(' ');
            continue;
        }
        joined.push_str(line);
        if let Some(flag) = parse_line(command, &joined) {
            flags.push(flag);
        }
        joined.clear();
    }
    flags
}

/// What one `complete` line says, if it is a plain option line of `command`.
fn parse_line(command: &str, line: &str) -> Option<Flag> {
    let words = tokens(line);
    let mut words = words.iter().map(String::as_str).peekable();
    // `and complete …` and `or complete …` are how fish writes "if the line before worked".
    if matches!(words.peek(), Some(&("and" | "or"))) {
        words.next();
    }
    if words.next() != Some("complete") {
        return None;
    }
    let mut seen = Seen::default();
    while let Some(word) = words.next() {
        if let Some(long) = word.strip_prefix("--") {
            seen.apply(long_option(long)?, &mut words);
        } else if let Some(letters) = word.strip_prefix('-') {
            // `-xa 'x y'` is `-x -a 'x y'`: clustered letters, values in order.
            for letter in letters.chars() {
                seen.apply(short_option(letter)?, &mut words);
            }
        }
    }
    let for_this_command = seen.commands.iter().any(|name| name == command);
    if seen.skip || !for_this_command {
        return None;
    }
    let short = seen.shorts.first().and_then(|name| name.chars().next());
    let long = seen.longs.into_iter().next();
    let old = seen.olds.into_iter().next();
    if short.is_none() && long.is_none() && old.is_none() {
        return None;
    }
    Some(Flag {
        short,
        long,
        old,
        description: describe(&seen.description),
    })
}

/// What an option of `complete` means for this parser.
#[derive(Clone, Copy, Debug)]
enum Opt {
    Command,
    Short,
    Long,
    Old,
    Description,
    /// The line depends on fish code, or is not about options at all: leave it out.
    Skip {
        takes_value: bool,
    },
    /// Understood, and nothing to remember.
    Ignore {
        takes_value: bool,
    },
}

fn long_option(name: &str) -> Option<Opt> {
    Some(match name {
        "command" => Opt::Command,
        "short-option" => Opt::Short,
        "long-option" => Opt::Long,
        "old-option" => Opt::Old,
        "description" => Opt::Description,
        "condition" | "wraps" => Opt::Skip { takes_value: true },
        "erase" => Opt::Skip { takes_value: false },
        "arguments" | "path" | "do-complete" => Opt::Ignore { takes_value: true },
        "require-parameter" | "exclusive" | "no-files" | "keep-order" | "force-files" | "help" => {
            Opt::Ignore { takes_value: false }
        }
        _ => return None,
    })
}

fn short_option(letter: char) -> Option<Opt> {
    Some(match letter {
        'c' => Opt::Command,
        's' => Opt::Short,
        'l' => Opt::Long,
        'o' => Opt::Old,
        'd' => Opt::Description,
        'n' | 'w' => Opt::Skip { takes_value: true },
        'e' => Opt::Skip { takes_value: false },
        'a' | 'p' | 'C' => Opt::Ignore { takes_value: true },
        'r' | 'x' | 'f' | 'k' | 'F' | 'h' => Opt::Ignore { takes_value: false },
        _ => return None,
    })
}

#[derive(Default)]
struct Seen {
    commands: Vec<String>,
    shorts: Vec<String>,
    longs: Vec<String>,
    olds: Vec<String>,
    description: String,
    skip: bool,
}

impl Seen {
    fn apply<'a>(&mut self, option: Opt, words: &mut impl Iterator<Item = &'a str>) {
        let mut value = |takes: bool| -> Option<String> {
            takes.then(|| words.next().map(str::to_owned)).flatten()
        };
        match option {
            Opt::Command => {
                if let Some(name) = value(true) {
                    // A name made at run time (`$cmd`) cannot be known.
                    if !name.contains(['$', '(']) {
                        self.commands.push(name);
                    }
                }
            }
            Opt::Short => self.shorts.extend(value(true)),
            Opt::Long => self.longs.extend(value(true)),
            Opt::Old => self.olds.extend(value(true)),
            Opt::Description => self.description = value(true).unwrap_or_default(),
            Opt::Skip { takes_value } => {
                value(takes_value);
                self.skip = true;
            }
            Opt::Ignore { takes_value } => {
                value(takes_value);
            }
        }
    }
}

/// A description on one line, without control characters, and not too long.
fn describe(text: &str) -> String {
    let first = text.lines().next().unwrap_or("");
    let clean: String = first.chars().filter(|c| !c.is_control()).collect();
    let clean = clean.trim();
    if clean.chars().count() > DESCRIPTION_LIMIT {
        let cut: String = clean.chars().take(DESCRIPTION_LIMIT - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        clean.to_owned()
    }
}

/// Splits a fish line into words: blanks separate, `'…'` and `"…"` group, a backslash escapes.
/// A `#` that starts a word begins a comment, and an unquoted `;`, `|`, `&` or `>` ends the
/// command, so only the first command of the line is read.
fn tokens(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    out.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            '#' if !in_word => break,
            ';' | '|' | '&' | '>' | '<' => break,
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            '\'' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '\'' => break,
                        '\\' if matches!(chars.peek(), Some('\\' | '\'')) => {
                            current.extend(chars.next());
                        }
                        other => current.push(other),
                    }
                }
            }
            '"' => {
                in_word = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' if matches!(chars.peek(), Some('\\' | '"' | '$')) => {
                            current.extend(chars.next());
                        }
                        other => current.push(other),
                    }
                }
            }
            other => {
                in_word = true;
                current.push(other);
            }
        }
    }
    if in_word {
        out.push(current);
    }
    out
}

/// The flags that start with `typed` (a word that starts with a dash), each once, as `(text,
/// description)`, sorted. `-` offers every form; `--` only the long ones.
pub(crate) fn candidates(flags: &[Flag], typed: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for flag in flags {
        let forms = flag
            .short
            .map(|letter| format!("-{letter}"))
            .into_iter()
            .chain(flag.long.as_ref().map(|name| format!("--{name}")))
            .chain(flag.old.as_ref().map(|name| format!("-{name}")));
        for form in forms {
            if form.starts_with(typed) && seen.insert(form.clone()) {
                out.push((form, flag.description.clone()));
            }
        }
    }
    out.sort_by_cached_key(|(form, _)| (form.trim_start_matches('-').to_lowercase(), form.clone()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(text: &str) -> Vec<Flag> {
        parse("ls", text)
    }

    fn one(text: &str) -> Flag {
        let mut found = flags(text);
        assert_eq!(found.len(), 1, "{text:?} -> {found:?}");
        found.remove(0)
    }

    #[test]
    fn short_long_and_old_names_with_a_description() {
        let flag = one("complete -c ls -s a -l all -d 'Show hidden'");
        assert_eq!(flag.short, Some('a'));
        assert_eq!(flag.long.as_deref(), Some("all"));
        assert_eq!(flag.old, None);
        assert_eq!(flag.description, "Show hidden");
        let flag = one("complete -c ls -o help -d \"Print help\"");
        assert_eq!(flag.old.as_deref(), Some("help"));
        assert_eq!(flag.short, None);
        let flag = one("complete -c ls -l color");
        assert_eq!(flag.long.as_deref(), Some("color"));
        assert_eq!(flag.description, "");
    }

    #[test]
    fn the_long_spellings_of_the_options_work_too() {
        let flag = one(
            "complete --command ls --short-option b --long-option block --description 'Blocks'",
        );
        assert_eq!(
            (flag.short, flag.long.as_deref()),
            (Some('b'), Some("block"))
        );
        assert_eq!(flag.description, "Blocks");
    }

    #[test]
    fn clustered_letters_and_values_that_are_not_needed_are_passed_over() {
        let flag = one("complete -c ls -xa 'one two' -s q -d 'Quiet'");
        assert_eq!(flag.short, Some('q'));
        assert_eq!(flag.description, "Quiet");
        let flag = one("complete -c ls -f -r -l size -a '(seq 3)' -d 'S'");
        assert_eq!(flag.long.as_deref(), Some("size"));
    }

    #[test]
    fn a_line_with_a_condition_is_left_out() {
        assert!(flags("complete -c ls -n '__fish_use_subcommand' -l x").is_empty());
        assert!(flags("complete -c ls --condition 'test -f x' -l x").is_empty());
        assert!(
            flags("complete -c ls -n'x' -l x").is_empty(),
            "joined to its value is not an option"
        );
    }

    #[test]
    fn lines_about_other_commands_or_no_option_are_left_out() {
        assert!(flags("complete -c cat -s n").is_empty());
        assert!(
            flags("complete -c ls -a 'x y z'").is_empty(),
            "arguments only"
        );
        assert!(flags("complete -c ls -e").is_empty(), "erase");
        assert!(flags("complete -c ls -w grep").is_empty(), "wraps");
        assert!(
            flags("complete -c $cmd -s n").is_empty(),
            "a name known only at run time"
        );
        assert!(flags("complete -c (basename x) -s n").is_empty());
    }

    #[test]
    fn several_commands_on_one_line_share_it() {
        assert_eq!(parse("a", "complete -c a -c b -s x").len(), 1);
        assert_eq!(parse("b", "complete -c a -c b -s x").len(), 1);
        assert_eq!(parse("c", "complete -c a -c b -s x").len(), 0);
    }

    #[test]
    fn and_or_in_front_and_blanks_are_fine() {
        assert_eq!(
            one("    complete -c ls -s S -d 'Sort by size'").short,
            Some('S')
        );
        assert_eq!(one("and complete -c ls -s S").short, Some('S'));
        assert_eq!(one("or complete -c ls -s S").short, Some('S'));
        assert!(flags("not complete -c ls -s S").is_empty());
    }

    #[test]
    fn comments_other_code_and_garbage_are_skipped() {
        let text = "# complete -c ls -s z\n\
                    set -l uname (uname -s)\n\
                    test \"$uname\" != SunOS\n\
                    if ls --version >/dev/null\n\
                    complete -c ls -s a # a trailing comment -s b\n\
                    end\n\
                    complete\n\
                    complete -c\n\
                    complete -c ls --no-such-option -s c\n\
                    complete -c ls -s\n";
        let found = flags(text);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].short, Some('a'));
    }

    #[test]
    fn a_command_ends_at_a_semicolon_or_a_pipe() {
        let found = flags("complete -c ls -s a; complete -c ls -s b");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].short, Some('a'));
    }

    #[test]
    fn a_backslash_continues_a_line() {
        let found = flags("complete -c ls \\\n    -s a \\\n    -d 'Joined'\ncomplete -c ls -s b\n");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].description, "Joined");
        assert_eq!(found[1].short, Some('b'));
    }

    #[test]
    fn quotes_and_escapes_follow_fish() {
        assert_eq!(
            tokens(r#"a 'b c' "d e" f\ g 'it\'s' "q\"x" 'a\\b' 'a\nb'"#),
            ["a", "b c", "d e", "f g", "it's", "q\"x", "a\\b", "a\\nb"]
        );
        assert_eq!(tokens("a '' b"), ["a", "", "b"]);
        assert_eq!(tokens("a 'unfinished"), ["a", "unfinished"]);
        assert_eq!(tokens("# only a comment"), Vec::<String>::new());
        assert_eq!(tokens("a#b"), ["a#b"], "a hash inside a word is a letter");
    }

    #[test]
    fn descriptions_are_one_clean_short_line() {
        assert_eq!(describe("  spaced  "), "spaced");
        assert_eq!(describe("one\ntwo"), "one");
        assert_eq!(describe("a\x1b[31mb"), "a[31mb");
        let long = "x".repeat(200);
        let cut = describe(&long);
        assert_eq!(cut.chars().count(), DESCRIPTION_LIMIT);
        assert!(cut.ends_with('…'));
        assert_eq!(
            describe(&"y".repeat(DESCRIPTION_LIMIT)).len(),
            DESCRIPTION_LIMIT
        );
    }

    #[test]
    fn nothing_here_panics_on_strange_text() {
        for text in [
            "",
            "\\",
            "'",
            "\"",
            "complete -c",
            "complete -",
            "complete --",
            "complete -- -c ls",
            "complete -c ls -s \u{0}",
            "complete -c ls -s é -l ü",
            "\u{feff}complete -c ls -s a",
            "complete -c ls -d",
            "\\\n\\\n\\",
        ] {
            let _ = flags(text);
        }
        let big = "complete -c ls -s a\n".repeat(50_000);
        assert_eq!(flags(&big).len(), 50_000);
    }

    #[test]
    fn candidates_are_filtered_by_what_was_typed() {
        let found = flags(
            "complete -c ls -s a -l all -d 'All'\n\
             complete -c ls -s A -l almost-all -d 'Almost'\n\
             complete -c ls -o help -d 'Help'\n\
             complete -c ls -s l -d 'Long'\n",
        );
        let texts = |typed: &str| -> Vec<String> {
            candidates(&found, typed)
                .into_iter()
                .map(|(text, _)| text)
                .collect()
        };
        assert_eq!(texts("--al"), ["--all", "--almost-all"]);
        assert_eq!(texts("--"), ["--all", "--almost-all"]);
        assert_eq!(texts("-a"), ["-a"]);
        assert_eq!(texts("-h"), ["-help"]);
        assert_eq!(
            texts("-"),
            ["-A", "-a", "--all", "--almost-all", "-help", "-l"]
        );
        assert!(texts("--zzz").is_empty());
        assert_eq!(candidates(&found, "--all")[0].1, "All");
    }

    #[test]
    fn a_form_that_two_lines_declare_is_offered_once() {
        let found = flags("complete -c ls -s a -d 'one'\ncomplete -c ls -s a -d 'two'\n");
        let all = candidates(&found, "-a");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].1, "one");
    }

    #[test]
    fn the_directories_follow_fish_order_and_the_environment() {
        let home = Path::new("/home/me");
        let dirs = directories(Some(home), None, None, None);
        assert_eq!(
            dirs.iter()
                .map(|dir| dir.to_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "/home/me/.config/fish/completions",
                "/home/me/.local/share/fish/vendor_completions.d",
                "/usr/local/share/fish/vendor_completions.d",
                "/usr/share/fish/vendor_completions.d",
                "/usr/local/share/fish/completions",
                "/usr/share/fish/completions",
            ]
        );
        let dirs = directories(Some(home), Some("/cfg"), Some("/data"), Some("/a:/b:/a"));
        assert_eq!(
            dirs.iter()
                .map(|dir| dir.to_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "/cfg/fish/completions",
                "/data/fish/vendor_completions.d",
                "/a/fish/vendor_completions.d",
                "/b/fish/vendor_completions.d",
                "/a/fish/completions",
                "/b/fish/completions",
            ]
        );
    }

    #[test]
    fn relative_directories_and_empty_values_are_ignored() {
        let dirs = directories(None, Some(""), Some("rel"), Some("x:/ok"));
        assert_eq!(
            dirs.iter()
                .map(|dir| dir.to_str().unwrap())
                .collect::<Vec<_>>(),
            ["/ok/fish/vendor_completions.d", "/ok/fish/completions"]
        );
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omsh-fish-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn never() -> bool {
        false
    }

    #[test]
    fn the_first_directory_with_the_file_wins() {
        let (a, b) = (scratch("first-a"), scratch("first-b"));
        std::fs::write(b.join("tool.fish"), "complete -c tool -s b\n").unwrap();
        let dirs = vec![a.clone(), b.clone()];
        assert_eq!(load("tool", &dirs, &never).unwrap()[0].short, Some('b'));
        std::fs::write(a.join("tool.fish"), "complete -c tool -s a\n").unwrap();
        assert_eq!(load("tool", &dirs, &never).unwrap()[0].short, Some('a'));
        assert_eq!(load("other", &dirs, &never), Some(Vec::new()));
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn a_command_name_cannot_leave_the_directory() {
        let dir = scratch("escape");
        std::fs::write(
            dir.join("secret.fish"),
            "complete -c ../secret -s x\ncomplete -c sub/../../secret -s x\n",
        )
        .unwrap();
        let inner = dir.join("inner");
        // `sub` exists, so `inner/sub/../../secret.fish` would be read if the name were let through.
        std::fs::create_dir_all(inner.join("sub")).unwrap();
        for name in [
            "../secret",
            "sub/../../secret",
            "..",
            ".hidden",
            "",
            "a/b",
            "a\0b",
        ] {
            assert_eq!(
                load(name, std::slice::from_ref(&inner), &never),
                Some(Vec::new()),
                "{name:?}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_that_cannot_be_read_is_passed_over_and_cancel_stops() {
        let (a, b) = (scratch("bad-a"), scratch("bad-b"));
        // A folder named like the file cannot be read as one.
        std::fs::create_dir_all(a.join("tool.fish")).unwrap();
        std::fs::write(b.join("tool.fish"), "complete -c tool -s b\n").unwrap();
        let dirs = vec![a.clone(), b.clone()];
        assert_eq!(load("tool", &dirs, &never).unwrap().len(), 1);
        assert_eq!(load("tool", &dirs, &|| true), None);
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn a_file_that_is_not_utf8_is_read_with_replacements() {
        let dir = scratch("latin");
        std::fs::write(
            dir.join("tool.fish"),
            b"complete -c tool -s a -d 'caf\xe9'\n",
        )
        .unwrap();
        let found = load("tool", std::slice::from_ref(&dir), &never).unwrap();
        assert_eq!(found[0].description, "caf\u{fffd}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
