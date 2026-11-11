//! Reading the words of a `find` command: where it starts looking and whether it deletes.

use crate::command::{Invocation, base_name};
use crate::word::Arg;

/// Primaries that take the next word as their value, so that word is never a primary itself
/// (`find . -name -delete` looks for a file called `-delete`).
const VALUE_PRIMARIES: &[&str] = &[
    "-name",
    "-iname",
    "-path",
    "-ipath",
    "-wholename",
    "-iwholename",
    "-regex",
    "-iregex",
    "-lname",
    "-ilname",
    "-user",
    "-group",
    "-uid",
    "-gid",
    "-type",
    "-xtype",
    "-newer",
    "-anewer",
    "-cnewer",
    "-size",
    "-perm",
    "-mtime",
    "-atime",
    "-ctime",
    "-mmin",
    "-amin",
    "-cmin",
    "-mindepth",
    "-fstype",
    "-links",
    "-inum",
    "-samefile",
    "-printf",
    "-fprint",
    "-fprint0",
    "-fprintf",
    "-context",
];

/// Primaries that run a program.
const EXEC_PRIMARIES: &[&str] = &["-exec", "-execdir", "-ok", "-okdir"];

/// Programs that remove what they are given.
const REMOVERS: &[&str] = &["rm", "rmdir", "unlink", "shred"];

/// What a `find` command was asked to do.
#[derive(Debug)]
pub(crate) struct Find<'i, 'a> {
    /// The words that name where to start. Empty means the current directory.
    pub starts: Vec<&'i Arg<'a>>,
    /// `-H`, `-L` or `-follow`: links are followed, which the blast radius does not model.
    pub follows_links: bool,
    /// A `-maxdepth` that is the same on every run.
    pub max_depth: Option<u32>,
    /// `-delete`, or `-exec` of a program that removes files.
    pub deletes: bool,
}

/// Reads the arguments of a `find` invocation.
pub(crate) fn parse<'i, 'a>(inv: &'i Invocation<'a>) -> Find<'i, 'a> {
    let args = &inv.args;
    let mut find = Find {
        starts: Vec::new(),
        follows_links: false,
        max_depth: None,
        deletes: false,
    };
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.text.as_str() {
            "-H" | "-L" if !arg.unknown => find.follows_links = true,
            "-P" => {}
            "-D" => i += 1,
            text if text.starts_with("-O") && !arg.unknown => {}
            _ => break,
        }
        i += 1;
    }
    while let Some(arg) = args.get(i) {
        let text = arg.text.as_str();
        let expression =
            !arg.unknown && (text.starts_with('-') || matches!(text, "(" | "!" | ")" | ","));
        if expression {
            break;
        }
        find.starts.push(arg);
        i += 1;
    }
    while let Some(arg) = args.get(i) {
        let text = arg.text.as_str();
        if arg.unknown {
            i += 1;
            continue;
        }
        if VALUE_PRIMARIES.contains(&text) {
            i += 2;
            continue;
        }
        match text {
            "-delete" => find.deletes = true,
            "-follow" => find.follows_links = true,
            "-maxdepth" => {
                if let Some(next) = args.get(i + 1)
                    && !next.unknown
                    && let Ok(depth) = next.text.parse::<u32>()
                {
                    find.max_depth = Some(find.max_depth.map_or(depth, |old| old.min(depth)));
                }
                i += 2;
                continue;
            }
            text if EXEC_PRIMARIES.contains(&text) => {
                if let Some(program) = args.get(i + 1)
                    && !program.unknown
                    && REMOVERS.contains(&base_name(&program.text))
                {
                    find.deletes = true;
                }
                // The program's own words, up to the `;` or `+` that ends them.
                i += 1;
                while let Some(word) = args.get(i) {
                    i += 1;
                    if !word.unknown && matches!(word.text.as_str(), ";" | "+") {
                        break;
                    }
                }
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    find
}
