//! What a command does to the file system, read from the syntax alone, for the undo journal
//! (§12.5): `rm` that can go through the trash, and `mkdir`, `mv` and `>` that can be undone.
//!
//! Like [`crate::blast`], this module only reads words. Patterns and the real state of the
//! disk are looked at by whoever runs the step ([`Change`] is the question, not the answer).

use omsh_syntax::Span;
use omsh_syntax::ast::RedirectOp;

use crate::blast::Target;
use crate::command::Invocation;
use crate::word::Arg;

/// Why an `rm` cannot be sent to the trash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotTrashable {
    /// A wrapper stands in front of it (`sudo rm`, `xargs rm`, `env rm`, `command rm`).
    Wrapped,
    /// It is in the text of an `eval` or `sh -c`.
    InText,
    /// It is in the body of a function that is only being defined.
    InFunction,
    /// An option the trash does not do (`-i`, `-I`, `--one-file-system`, ...).
    Option(String),
}

impl NotTrashable {
    /// The reason in plain words.
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Wrapped => "it runs through sudo or another program".to_owned(),
            Self::InText => "it is inside the text of eval or sh -c".to_owned(),
            Self::InFunction => "it is inside a function that is being defined".to_owned(),
            Self::Option(option) => format!("the trash does not do the option {option}"),
        }
    }
}

/// An `rm` command in a line.
#[derive(Clone, Debug)]
pub struct RmSite {
    /// The word with the name `rm`. The trash replaces exactly this.
    pub name: Span,
    /// The whole command.
    pub command: Span,
    /// `Ok` if the command can be run as a move to the trash.
    pub trashable: Result<(), NotTrashable>,
}

/// The options of `rm` that the trash does.
const SHORT: &str = "frRdv";
const LONG: &[&str] = &["force", "recursive", "dir", "verbose", "preserve-root"];

/// Judges the options of an `rm` command.
pub fn rm_site_options(inv: &Invocation<'_>) -> Result<(), NotTrashable> {
    if inv.wrapped {
        return Err(NotTrashable::Wrapped);
    }
    let options = inv.options();
    if let Some(letter) = options.short.chars().find(|c| !SHORT.contains(*c)) {
        return Err(NotTrashable::Option(format!("-{letter}")));
    }
    if let Some(long) = options.long.iter().find(|name| !LONG.contains(name)) {
        return Err(NotTrashable::Option(format!("--{long}")));
    }
    Ok(())
}

/// What a change does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// `mkdir`; with `-p`, the missing folders above too.
    Mkdir { parents: bool },
    /// `mv SOURCE... DEST`; `-T` treats `DEST` as a name, never as a folder to go into.
    Move { no_target_dir: bool },
    /// A redirection that writes a file (`>`, `>|`, `>>`, `&>`, `&>>`).
    Write,
}

/// A change to the file system, for the journal.
#[derive(Clone, Debug)]
pub struct Change {
    pub kind: ChangeKind,
    /// The path words: the folders to make, the sources and the destination (last), or the
    /// file written.
    pub targets: Vec<Target>,
    /// The working directory where it runs, if known.
    pub cwd: Option<String>,
    /// What `~` means there.
    pub home: Option<String>,
    /// The command, or for a redirection the command it belongs to.
    pub span: Span,
    /// Set if this kind of command cannot be journaled, and why.
    pub skip: Option<String>,
}

impl Change {
    /// A short name for panel lines.
    #[must_use]
    pub const fn what(&self) -> &'static str {
        match self.kind {
            ChangeKind::Mkdir { .. } => "mkdir",
            ChangeKind::Move { .. } => "mv",
            ChangeKind::Write => "a redirection",
        }
    }
}

fn target(arg: &Arg<'_>) -> Target {
    Target {
        src: arg.src.clone(),
        text: arg.text.clone(),
        active: arg.active.clone(),
        glob: arg.glob,
        unknown: arg.unknown,
    }
}

/// The change `mkdir` or `mv` makes, if `inv` is one.
#[must_use]
pub fn change_of(inv: &Invocation<'_>, cwd: Option<&str>, home: Option<&str>) -> Option<Change> {
    let options = inv.options();
    let make = |kind, targets, skip: Option<String>| Change {
        kind,
        targets,
        cwd: cwd.map(str::to_owned),
        home: home.map(str::to_owned),
        span: inv.span,
        skip,
    };
    let wrapped = inv
        .wrapped
        .then(|| "it runs through sudo or another program".to_owned());
    match inv.name.as_str() {
        "mkdir" => {
            let mut skip = wrapped;
            if let Some(letter) = options.short.chars().find(|c| !"pvm".contains(*c)) {
                skip.get_or_insert(format!("the option -{letter} is not followed"));
            }
            if let Some(long) = options
                .long
                .iter()
                .find(|name| !matches!(**name, "parents" | "verbose" | "mode"))
            {
                skip.get_or_insert(format!("the option --{long} is not followed"));
            }
            // The value of `-m MODE` is among the operands.
            let mode = inv.option_value('m', "mode");
            let mut skipped_mode = mode.is_none();
            let targets = options
                .operands
                .iter()
                .filter(|arg| {
                    if !skipped_mode && Some(arg.text.as_str()) == mode {
                        skipped_mode = true;
                        false
                    } else {
                        true
                    }
                })
                .map(|arg| target(arg))
                .collect();
            let parents = options.has_short('p') || options.has_long("parents");
            Some(make(ChangeKind::Mkdir { parents }, targets, skip))
        }
        "mv" => {
            let mut skip = wrapped;
            if let Some(letter) = options.short.chars().find(|c| !"fnvTu".contains(*c)) {
                skip.get_or_insert(format!("the option -{letter} is not followed"));
            }
            if let Some(long) = options.long.iter().find(|name| {
                !matches!(
                    **name,
                    "force" | "no-clobber" | "verbose" | "no-target-directory" | "update"
                )
            }) {
                skip.get_or_insert(format!("the option --{long} is not followed"));
            }
            let targets: Vec<Target> = options.operands.iter().map(|arg| target(arg)).collect();
            if targets.len() < 2 {
                skip.get_or_insert("it needs a source and a destination".to_owned());
            }
            let no_target_dir = options.has_short('T') || options.has_long("no-target-directory");
            Some(make(ChangeKind::Move { no_target_dir }, targets, skip))
        }
        _ => None,
    }
}

/// The change a redirection makes, if it writes a file. `command` is the span of the command
/// it belongs to.
#[must_use]
pub fn write_of(
    op: RedirectOp,
    word: &Arg<'_>,
    command: Span,
    cwd: Option<&str>,
    home: Option<&str>,
) -> Option<Change> {
    if !matches!(
        op,
        RedirectOp::Output
            | RedirectOp::Clobber
            | RedirectOp::Append
            | RedirectOp::OutputAndError
            | RedirectOp::AppendAndError
    ) {
        return None;
    }
    // Devices, the terminal and `/dev/null` are not files to bring back.
    if !word.unknown && word.text.starts_with("/dev/") {
        return None;
    }
    Some(Change {
        kind: ChangeKind::Write,
        targets: vec![target(word)],
        cwd: cwd.map(str::to_owned),
        home: home.map(str::to_owned),
        span: command,
        skip: None,
    })
}

#[cfg(test)]
mod tests {
    use omsh_syntax::ast::CommandKind;
    use omsh_syntax::parse;

    use super::{ChangeKind, NotTrashable, change_of, rm_site_options};
    use crate::command::{Invocation, invocation};
    use crate::word::Arg;

    fn inv(line: &'static str) -> Invocation<'static> {
        let list = Box::leak(Box::new(parse(line).unwrap()));
        let command = &list.items[0].and_or.first.commands[0];
        let CommandKind::Simple(simple) = &command.kind else {
            panic!("not a simple command");
        };
        let words = simple
            .words
            .iter()
            .map(|word| Arg::from_word(word, line, None))
            .collect();
        invocation(command.span, words).unwrap()
    }

    #[test]
    fn rm_with_the_options_the_trash_knows_is_trashable() {
        for line in [
            "rm a",
            "rm -rf a b",
            "rm -r -f -v a",
            "rm -Rd a",
            "rm --recursive --force a",
            "rm -- -weird",
            "/bin/rm a",
            "rm --preserve-root a",
        ] {
            assert_eq!(rm_site_options(&inv(line)), Ok(()), "{line}");
        }
    }

    #[test]
    fn rm_with_other_options_or_a_wrapper_is_not() {
        assert_eq!(
            rm_site_options(&inv("rm -i a")),
            Err(NotTrashable::Option("-i".to_owned()))
        );
        assert_eq!(
            rm_site_options(&inv("rm -rI a")),
            Err(NotTrashable::Option("-I".to_owned()))
        );
        assert_eq!(
            rm_site_options(&inv("rm --one-file-system -r a")),
            Err(NotTrashable::Option("--one-file-system".to_owned()))
        );
        assert_eq!(
            rm_site_options(&inv("rm --no-preserve-root -r a")),
            Err(NotTrashable::Option("--no-preserve-root".to_owned()))
        );
        for line in [
            "sudo rm a",
            "env rm a",
            "command rm a",
            "xargs rm",
            "nohup rm a",
        ] {
            assert_eq!(
                rm_site_options(&inv(line)),
                Err(NotTrashable::Wrapped),
                "{line}"
            );
        }
    }

    #[test]
    fn mkdir_and_mv_are_read() {
        let c = change_of(&inv("mkdir -p a/b c"), Some("/w"), None).unwrap();
        assert_eq!(c.kind, ChangeKind::Mkdir { parents: true });
        assert_eq!(c.targets.len(), 2);
        assert!(c.skip.is_none());
        let c = change_of(&inv("mkdir -m 700 a"), Some("/w"), None).unwrap();
        assert_eq!(c.targets.len(), 1, "the mode is not a folder");
        assert_eq!(c.targets[0].text, "a");
        let c = change_of(&inv("mv -f a b"), None, None).unwrap();
        assert_eq!(
            c.kind,
            ChangeKind::Move {
                no_target_dir: false
            }
        );
        assert_eq!(c.targets.len(), 2);
        let c = change_of(&inv("mv -T a b"), None, None).unwrap();
        assert_eq!(
            c.kind,
            ChangeKind::Move {
                no_target_dir: true
            }
        );
    }

    #[test]
    fn what_cannot_be_followed_says_so() {
        assert!(
            change_of(&inv("sudo mkdir a"), None, None)
                .unwrap()
                .skip
                .is_some()
        );
        assert!(
            change_of(&inv("mv -t dir a"), None, None)
                .unwrap()
                .skip
                .is_some()
        );
        assert!(
            change_of(&inv("mv --backup a b"), None, None)
                .unwrap()
                .skip
                .is_some()
        );
        assert!(change_of(&inv("mv a"), None, None).unwrap().skip.is_some());
        assert!(
            change_of(&inv("mkdir -Z a"), None, None)
                .unwrap()
                .skip
                .is_some()
        );
        assert!(change_of(&inv("cp a b"), None, None).is_none());
    }
}
