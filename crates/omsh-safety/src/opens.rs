//! The paths a command opens, for the plan panel's "no such path" note.
//!
//! Only a short list of commands is looked at: the ones that open something that must already
//! exist (`cd`, `ls`, `cat`, `less`, `head`, `tail`, `code`, `xdg-open`). A command where a
//! missing path is normal (`mkdir`, `touch`, `cp`, `mv`, `tee`, an editor, a redirection) is not
//! in the list. A command is also left out whole when it has an option this module does not know
//! to take no value, so that the value of `head -n 5` is never taken for a path.
//!
//! Nothing here touches the file system: the words are only sorted, and the paths are resolved
//! as text from the working directory the walk knows at that point. The caller checks them.

use omsh_syntax::Span;

use crate::command::Invocation;
use crate::paths;
use crate::word::Arg;

/// A path word of a command that opens it.
#[derive(Clone, Debug)]
pub struct Opened {
    /// The program (`code`, `cd`, ...).
    pub command: String,
    /// The word as the person or the model wrote it, quotes and all.
    pub src: String,
    /// The same path without quotes. A leading `~` is kept, so a suggestion reads like the word.
    pub shown: String,
    /// The path made absolute and cleaned of `.`, `..` and `//`.
    pub path: String,
    /// Where the word is in the text that was analyzed.
    pub span: Span,
    /// True if the path must be a folder (`cd`).
    pub dir_only: bool,
    /// True for `cd` with a relative name that does not start with `./` or `../`: `CDPATH` may
    /// find it somewhere else.
    pub cdpath: bool,
    /// True if the word has a `..` part.
    pub dotdot: bool,
    /// The directory a relative word was resolved from (`None` for an absolute word).
    pub cwd: Option<String>,
}

/// Which options a command may have without this module losing track of its operands.
struct Spec {
    /// Only the first operand is looked at.
    first_only: bool,
    dir_only: bool,
    /// Short option letters that take no value; `None` means any letter.
    short: Option<&'static str>,
    /// Short option letters whose value is attached or in the next word.
    short_value: &'static str,
    /// Digits as options (`head -5`).
    digits: bool,
    /// Long options that take no value.
    long: &'static [&'static str],
    /// Any long option, and the `--name=value` form of every long option.
    long_any: bool,
    long_eq: bool,
    /// A word starting with `+` is an option (`less +F`).
    plus_is_option: bool,
    /// An unknown word before the operands could be an option's value.
    unknown_is_risky: bool,
}

const NONE: Spec = Spec {
    first_only: false,
    dir_only: false,
    short: Some(""),
    short_value: "",
    digits: false,
    long: &[],
    long_any: false,
    long_eq: false,
    plus_is_option: true,
    unknown_is_risky: false,
};

/// `ls` long options that take no value; the `--name=value` form of any is also fine.
const LS_LONG: &[&str] = &[
    "all",
    "almost-all",
    "author",
    "classify",
    "context",
    "dereference",
    "directory",
    "dired",
    "escape",
    "file-type",
    "full-time",
    "group-directories-first",
    "human-readable",
    "inode",
    "kibibytes",
    "literal",
    "no-group",
    "numeric-uid-gid",
    "recursive",
    "reverse",
    "show-control-chars",
    "hide-control-chars",
    "si",
    "size",
    "zero",
];

fn spec_of(name: &str) -> Option<Spec> {
    Some(match name {
        "cd" => Spec {
            first_only: true,
            dir_only: true,
            short: Some("LPe@"),
            ..NONE
        },
        "ls" => Spec {
            short: None,
            short_value: "IwT",
            long: LS_LONG,
            long_eq: true,
            plus_is_option: false,
            unknown_is_risky: true,
            ..NONE
        },
        "cat" => Spec {
            short: None,
            long_any: true,
            plus_is_option: false,
            ..NONE
        },
        "less" => NONE,
        "head" | "tail" => Spec {
            short: Some("fFqvz"),
            short_value: "nc",
            digits: true,
            long: &[
                "quiet",
                "silent",
                "verbose",
                "follow",
                "retry",
                "zero-terminated",
            ],
            long_eq: true,
            unknown_is_risky: true,
            ..NONE
        },
        "code" => Spec {
            short: Some("nra"),
            long: &["new-window", "reuse-window", "add"],
            plus_is_option: false,
            ..NONE
        },
        "xdg-open" => Spec {
            first_only: true,
            plus_is_option: false,
            ..NONE
        },
        _ => return None,
    })
}

/// What one option word means: it stands alone, it takes the next word, or it is not known.
enum OptionKind {
    Plain,
    TakesNext,
    Unknown,
}

fn classify(spec: &Spec, text: &str) -> OptionKind {
    if let Some(long) = text.strip_prefix("--") {
        let name = long.split('=').next().unwrap_or(long);
        let has_value = long.contains('=');
        return if spec.long_any || spec.long.contains(&name) || (spec.long_eq && has_value) {
            OptionKind::Plain
        } else {
            OptionKind::Unknown
        };
    }
    let letters: Vec<char> = text[1..].chars().collect();
    for (at, letter) in letters.iter().enumerate() {
        if spec.short_value.contains(*letter) {
            // The value is the rest of the word, or the next word if nothing is left.
            return if at + 1 == letters.len() {
                OptionKind::TakesNext
            } else {
                OptionKind::Plain
            };
        }
        let known = match spec.short {
            None => true,
            Some(only) => only.contains(*letter),
        };
        if !(known || (spec.digits && letter.is_ascii_digit())) {
            return OptionKind::Unknown;
        }
    }
    OptionKind::Plain
}

/// The operands of `inv`, or `None` if an option is not one this module understands.
fn operands<'i, 'a>(inv: &'i Invocation<'a>, spec: &Spec) -> Option<Vec<&'i Arg<'a>>> {
    let mut out = Vec::new();
    let mut after_dashes = false;
    let mut skip_value = false;
    for arg in &inv.args {
        if skip_value {
            skip_value = false;
            continue;
        }
        if after_dashes {
            out.push(arg);
            continue;
        }
        if arg.unknown {
            if spec.unknown_is_risky {
                return None;
            }
            out.push(arg);
            continue;
        }
        let text = arg.text.as_str();
        if text == "--" {
            after_dashes = true;
        } else if text.len() > 1 && text.starts_with('-') {
            match classify(spec, text) {
                OptionKind::Plain => {}
                OptionKind::TakesNext => skip_value = true,
                OptionKind::Unknown => return None,
            }
        } else if spec.plus_is_option && text.starts_with('+') {
            return None;
        } else {
            out.push(arg);
        }
    }
    Some(out)
}

/// True for a URL (`https://...`) or one of the other schemes people give to `xdg-open`. A name
/// with a colon in it (`a.rs:12`) is not one.
fn looks_like_url(text: &str) -> bool {
    text.contains("://")
        || ["mailto:", "tel:", "data:", "javascript:"]
            .iter()
            .any(|scheme| text.starts_with(scheme))
}

/// `file:12` and `file:12:3` (what `code` opens at a line) without the position.
fn without_position(text: &str) -> &str {
    let mut rest = text;
    for _ in 0..2 {
        match rest.rsplit_once(':') {
            Some((head, tail))
                if !head.is_empty()
                    && !tail.is_empty()
                    && tail.bytes().all(|b| b.is_ascii_digit()) =>
            {
                rest = head;
            }
            _ => break,
        }
    }
    rest
}

fn one(
    inv: &Invocation<'_>,
    arg: &Arg<'_>,
    spec: &Spec,
    cwd: Option<&str>,
    home: Option<&str>,
) -> Option<Opened> {
    if arg.unknown || arg.glob || arg.cmdsub {
        return None;
    }
    let mut text = arg.text.as_str();
    if text.is_empty() || text == "-" || text.chars().any(char::is_control) {
        return None;
    }
    // A `~` that was not resolved is `~user`, or the home folder is not known.
    if text.starts_with('~') {
        return None;
    }
    if inv.name == "code" {
        text = without_position(text);
    }
    if looks_like_url(text) {
        return None;
    }
    let absolute = text.starts_with('/');
    let base = if absolute { None } else { Some(cwd?) };
    let path = paths::resolve(base.unwrap_or("/"), text);
    // A word that began with `~` (unquoted) keeps it for the suggestion.
    let tilde = home.filter(|home| {
        (arg.src == "~" || arg.src.starts_with("~/"))
            && text.starts_with(home)
            && (text.len() == home.len() || text[home.len()..].starts_with('/'))
    });
    let shown = match tilde {
        Some(home) => format!("~{}", &text[home.len()..]),
        None => text.to_owned(),
    };
    Some(Opened {
        command: inv.name.clone(),
        src: arg.src.clone(),
        shown,
        path,
        span: arg.span,
        dir_only: spec.dir_only,
        cdpath: inv.name == "cd"
            && !absolute
            && !text.starts_with("./")
            && !text.starts_with("../")
            && text != "."
            && text != "..",
        dotdot: text.split('/').any(|part| part == ".."),
        cwd: base.map(str::to_owned),
    })
}

/// The path words `inv` opens, for a command in the list; nothing for any other command.
#[must_use]
pub fn opened(inv: &Invocation<'_>, cwd: Option<&str>, home: Option<&str>) -> Vec<Opened> {
    let Some(spec) = spec_of(&inv.name) else {
        return Vec::new();
    };
    let Some(operands) = operands(inv, &spec) else {
        return Vec::new();
    };
    let take = if spec.first_only { 1 } else { operands.len() };
    operands
        .into_iter()
        .take(take)
        .filter_map(|arg| one(inv, arg, &spec, cwd, home))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use omsh_syntax::parse;

    use crate::{Context, FsProbe, analyze};

    const CWD: &str = "/home/me/work";
    const HOME: &str = "/home/me";

    /// (command, resolved path) of every opened path of `line`.
    fn opens(line: &str) -> Vec<(String, String)> {
        let list = parse(line).unwrap();
        let ctx = Context {
            cwd: Some(Path::new(CWD)),
            home: Some(Path::new(HOME)),
            probe: &FsProbe,
        };
        analyze(&list, line, &ctx)
            .opens
            .into_iter()
            .map(|o| (o.command, o.path))
            .collect()
    }

    fn paths_of(line: &str) -> Vec<String> {
        opens(line).into_iter().map(|(_, path)| path).collect()
    }

    #[test]
    fn the_real_case_is_resolved_from_the_working_directory() {
        assert_eq!(
            opens("code ../night-before-codebase"),
            [(
                "code".to_owned(),
                "/home/me/night-before-codebase".to_owned()
            )]
        );
    }

    #[test]
    fn each_listed_command_opens_its_operands() {
        assert_eq!(
            paths_of("cd a b"),
            ["/home/me/work/a"],
            "cd: the first only"
        );
        assert_eq!(paths_of("ls a b"), ["/home/me/work/a", "/home/me/work/b"]);
        assert_eq!(paths_of("cat a b"), ["/home/me/work/a", "/home/me/work/b"]);
        assert_eq!(paths_of("less a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("head a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("tail a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("code a b"), ["/home/me/work/a", "/home/me/work/b"]);
        assert_eq!(
            paths_of("xdg-open a b"),
            ["/home/me/work/a"],
            "the first only"
        );
    }

    #[test]
    fn commands_where_a_missing_path_is_normal_are_not_looked_at() {
        for line in [
            "mkdir a",
            "touch a",
            "cp a b",
            "mv a b",
            "tee a",
            "ln -s a b",
            "vim a",
            "nano a",
            "echo a > b",
            "cat < b",
            "rm a",
            "git clone x y",
        ] {
            let found = opens(line);
            // `cat < b` has no operand; the others are not in the list.
            assert!(found.is_empty(), "{line}: {found:?}");
        }
    }

    #[test]
    fn a_redirection_target_is_never_a_path_to_open() {
        assert_eq!(paths_of("cat a > out"), ["/home/me/work/a"]);
        assert_eq!(paths_of("ls 2> err"), Vec::<String>::new());
    }

    #[test]
    fn an_option_that_may_take_a_value_leaves_the_command_out() {
        // `-g file:12` and `--diff a b` are not plain opens.
        assert!(opens("code -g a:12").is_empty());
        assert!(opens("code --diff a b").is_empty());
        assert!(opens("code --install-extension x").is_empty());
        assert!(opens("less -N a").is_empty());
        assert!(opens("less +F a").is_empty());
        assert!(opens("tail -s 5 a").is_empty());
        assert!(opens("xdg-open --foo a").is_empty());
        assert!(opens("cd -x a").is_empty());
        assert!(opens("ls --sort size a").is_empty());
    }

    #[test]
    fn the_value_of_a_known_option_is_not_a_path() {
        assert_eq!(paths_of("head -n 5 a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("head -n5 a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("tail -c 10 -f a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("head -5 a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("head --lines=5 a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("ls -I '*.o' a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("ls -w 80 a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("ls -la --color=auto a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("ls --all a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("code -n a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("code --reuse-window a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("cd -P a"), ["/home/me/work/a"]);
    }

    #[test]
    fn words_after_two_dashes_are_operands() {
        assert_eq!(paths_of("ls -- -a"), ["/home/me/work/-a"]);
        assert_eq!(paths_of("cat -- --weird"), ["/home/me/work/--weird"]);
    }

    #[test]
    fn words_that_are_known_only_at_run_time_or_are_not_paths_are_skipped() {
        assert!(opens("cd $x").is_empty());
        assert!(opens("cd \"$x/y\"").is_empty());
        assert!(opens("ls $(pwd)/x").is_empty());
        assert!(opens("ls *.c").is_empty());
        assert!(opens("ls a?").is_empty());
        assert!(opens("cd ~other/x").is_empty());
        assert!(opens("cd -").is_empty());
        assert!(opens("xdg-open https://example.org").is_empty());
        assert!(opens("xdg-open mailto:a@b.c").is_empty());
        assert!(opens("code ''").is_empty());
        assert!(
            opens("cat $f a")
                .iter()
                .all(|(_, p)| p == "/home/me/work/a")
        );
        // An unknown word before the operands of a command with value options: it may be one.
        assert!(opens("head $flags a").is_empty());
        assert!(opens("ls $flags a").is_empty());
    }

    #[test]
    fn a_position_after_a_name_is_not_part_of_the_path_for_code() {
        assert_eq!(paths_of("code a.rs:12"), ["/home/me/work/a.rs"]);
        assert_eq!(paths_of("code a.rs:12:3"), ["/home/me/work/a.rs"]);
        assert_eq!(paths_of("cat a.rs:12"), ["/home/me/work/a.rs:12"]);
    }

    #[test]
    fn tilde_and_home_are_resolved_and_the_tilde_is_kept_for_display() {
        let list = parse("code ~/x \"$HOME/y\" '~/z'").unwrap();
        let ctx = Context {
            cwd: Some(Path::new(CWD)),
            home: Some(Path::new(HOME)),
            probe: &FsProbe,
        };
        let found = analyze(&list, "code ~/x \"$HOME/y\" '~/z'", &ctx).opens;
        let got: Vec<(&str, &str)> = found
            .iter()
            .map(|o| (o.path.as_str(), o.shown.as_str()))
            .collect();
        assert_eq!(
            got,
            [("/home/me/x", "~/x"), ("/home/me/y", "/home/me/y")],
            "a quoted `~` is a name, not the home folder: it is left alone"
        );
    }

    #[test]
    fn a_tilde_with_no_known_home_is_skipped() {
        assert!(opens("HOME=/x; cd ~/a").is_empty());
    }

    #[test]
    fn the_working_directory_follows_cd_in_the_line() {
        assert_eq!(paths_of("cd /srv && ls a"), ["/srv", "/srv/a"]);
        assert_eq!(
            paths_of("cd /srv; cd b; cat c"),
            ["/srv", "/srv/b", "/srv/b/c"]
        );
        // A subshell and a pipeline stage do not move the line.
        assert_eq!(paths_of("(cd /srv); ls a"), ["/srv", "/home/me/work/a"]);
        assert_eq!(paths_of("cd /srv | cat; ls a"), ["/srv", "/home/me/work/a"]);
    }

    #[test]
    fn a_directory_that_is_not_known_gives_no_relative_path() {
        // After `cd "$x"` nothing relative is known; an absolute word still is.
        assert_eq!(paths_of("cd \"$x\"; ls a /etc/b"), ["/etc/b"]);
    }

    #[test]
    fn function_bodies_and_eval_text_are_not_looked_at() {
        assert!(opens("f() { cd /a; ls b; }").is_empty());
        assert!(opens("eval 'cd /a'").is_empty());
        assert!(opens("sh -c 'cat /a'").is_empty());
    }

    #[test]
    fn wrappers_are_peeled_and_xargs_input_is_unknown() {
        assert_eq!(paths_of("sudo cat a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("env X=1 code a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("command cd a"), ["/home/me/work/a"]);
        assert_eq!(paths_of("echo x | xargs ls a"), Vec::<String>::new());
    }

    #[test]
    fn the_flags_for_the_panel_are_set() {
        let line = "cd src; cd ./x; cd ../y; cd /z; ls ../w";
        let list = parse(line).unwrap();
        let ctx = Context {
            cwd: Some(Path::new(CWD)),
            home: Some(Path::new(HOME)),
            probe: &FsProbe,
        };
        let found = analyze(&list, line, &ctx).opens;
        let flags: Vec<(bool, bool, bool)> = found
            .iter()
            .map(|o| (o.dir_only, o.cdpath, o.dotdot))
            .collect();
        assert_eq!(
            flags,
            [
                (true, true, false),
                (true, false, false),
                (true, false, true),
                (true, false, false),
                (false, false, true)
            ]
        );
        assert_eq!(found[3].cwd, None, "an absolute word has no base");
        assert!(found[0].cwd.is_some());
    }
}
