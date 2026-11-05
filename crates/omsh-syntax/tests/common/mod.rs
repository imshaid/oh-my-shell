//! Generators of shell-like text for the property tests.
//!
//! Random bytes almost never look like shell, so most of what is generated here is built from
//! pieces of shell: fragments that are glued together, test lines that are damaged a little,
//! and text from the characters that mean something to a shell. Plain arbitrary text comes
//! along too, for the rest.

use proptest::prelude::*;

/// How many cases each property runs. §17 and S04 ask for 10,000.
pub const CASES: u32 = 10_000;

/// Pieces of shell: operators, quotes, expansions, reserved words, every bash-only form from
/// §6.2, and text that is not ASCII.
const FRAGMENTS: &[&str] = &[
    "a",
    "b",
    "echo",
    "x=1",
    "A=",
    " ",
    "  ",
    "\t",
    "\n",
    "\r\n",
    ";",
    ";;",
    "&",
    "&&",
    "|",
    "||",
    "!",
    "(",
    ")",
    "{",
    "}",
    "{ ",
    " }",
    "[[",
    "]]",
    "<",
    ">",
    ">>",
    "<<",
    "<<-",
    "<<<",
    "<&",
    ">&",
    "&>",
    "&>>",
    "<>",
    ">|",
    "2>",
    "2>&1",
    "10<",
    "99999999999>",
    "<<EOF\n",
    "EOF\n",
    "<<'E'\n",
    "\nE\n",
    "<<-\tT\n",
    "'",
    "\"",
    "`",
    "\\",
    "\\\n",
    "\\\\",
    "$",
    "${",
    "$(",
    "$((",
    "))",
    "${x",
    "${x:-",
    "${x//a/b}",
    "${x:1}",
    "${!x}",
    "${#x}",
    "${#",
    "${}",
    "$x",
    "$1",
    "$@",
    "$?",
    "$'",
    "$'a\\n'",
    "'a b'",
    "\"a $x\"",
    "`a`",
    "$(a)",
    "$((1+2))",
    "#",
    "# c\n",
    "if ",
    "then ",
    "elif ",
    "else ",
    "fi",
    "while ",
    "until ",
    "do ",
    "done",
    "for ",
    "in ",
    "case ",
    "esac",
    "function ",
    "select ",
    "coproc ",
    "time ",
    "shopt ",
    "((",
    "<(",
    ">(",
    "|&",
    ";&",
    ";;&",
    "+=",
    "=(",
    "[0]=",
    "{a,b}",
    "{1..3}",
    "{,}",
    "$(<f)",
    "~",
    "~/x",
    "*",
    "?",
    "[a-z]",
    "f() ",
    "é",
    "বাংলা",
    "😀",
    "\u{0}",
];

/// Glued-together fragments.
fn soup() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(FRAGMENTS), 0..24).prop_map(|pieces| pieces.concat())
}

/// A line from `corpus` with up to four edits: something inserted, a character removed, a chunk
/// repeated, or the end cut off. Edits fall on character boundaries.
fn damaged(corpus: Vec<&'static str>) -> impl Strategy<Value = String> {
    let edit = (any::<u16>(), 0..4_u8, prop::sample::select(FRAGMENTS));
    (
        prop::sample::select(corpus),
        prop::collection::vec(edit, 0..5),
    )
        .prop_map(|(line, edits)| {
            let mut text = line.to_owned();
            for (place, kind, fragment) in edits {
                let mut at = usize::from(place) % (text.len() + 1);
                while !text.is_char_boundary(at) {
                    at -= 1;
                }
                match kind {
                    0 => text.insert_str(at, fragment),
                    1 => {
                        if at < text.len() {
                            text.remove(at);
                        }
                    }
                    2 => {
                        let mut end = (at + 8).min(text.len());
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        let chunk = text.get(at..end).unwrap_or("").to_owned();
                        text.insert_str(at, &chunk);
                    }
                    _ => text.truncate(at),
                }
            }
            text
        })
}

/// Shell-like text of every kind. `corpus` is a list of test lines to damage.
pub fn shell_text(corpus: Vec<&'static str>) -> impl Strategy<Value = String> {
    prop_oneof![
        4 => soup(),
        4 => damaged(corpus),
        3 => "[-a-z0-9 \t\n$(){}\\[\\]<>|&;'\"`\\\\#!=*?~,.:+@%^/]{0,48}",
        1 => any::<String>(),
    ]
}
