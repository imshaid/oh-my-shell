//! A simple command, with the programs that only run another program peeled off.

use omsh_syntax::Span;

use crate::word::Arg;

/// Options of `sudo` and `doas` that take a value in the next word.
const SUDO_VALUE_OPTIONS: &[&str] = &[
    "-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-T", "-U", "-R",
];

/// A command as it will run: the program, its words, and how many `sudo`s were in front.
#[derive(Clone, Debug)]
pub struct Invocation<'a> {
    /// The program's name without its folder (`/bin/rm` is `rm`).
    pub name: String,
    /// The words after the name.
    pub args: Vec<Arg<'a>>,
    /// How many of `sudo` and `doas` were peeled off.
    pub sudo: u8,
    /// Where the whole command is.
    pub span: Span,
}

/// The words of an invocation, sorted into options and operands.
#[derive(Debug)]
pub struct Options<'i, 'a> {
    /// The letters of all short options together (`-aG -n5` gives `aGn5`).
    pub short: String,
    /// The long options without their value (`--hard`, `--force=yes` give `hard`, `force`).
    pub long: Vec<&'i str>,
    /// The other words, in order. The value of an option that takes one is among them.
    pub operands: Vec<&'i Arg<'a>>,
}

impl Options<'_, '_> {
    /// True if the short option `letter` is among them.
    #[must_use]
    pub fn has_short(&self, letter: char) -> bool {
        self.short.contains(letter)
    }

    /// True if the long option `name` (without the dashes) is among them.
    #[must_use]
    pub fn has_long(&self, name: &str) -> bool {
        self.long.contains(&name)
    }
}

impl<'a> Invocation<'a> {
    /// Sorts the words. A word with a part only known at run time is an operand: it may
    /// become anything, but a rule cannot tell which.
    #[must_use]
    pub fn options(&self) -> Options<'_, 'a> {
        let mut options = Options {
            short: String::new(),
            long: Vec::new(),
            operands: Vec::new(),
        };
        let mut rest_are_operands = false;
        for arg in &self.args {
            let text = arg.text.as_str();
            if rest_are_operands || arg.unknown || text.len() < 2 || !text.starts_with('-') {
                options.operands.push(arg);
            } else if text == "--" {
                rest_are_operands = true;
            } else if let Some(long) = text.strip_prefix("--") {
                options.long.push(long.split('=').next().unwrap_or(long));
            } else {
                options.short.push_str(&text[1..]);
            }
        }
        options
    }

    /// The value of an option, in any of the usual spellings: `-G x`, `-aG x`, `-Gx`,
    /// `--groups x` and `--groups=x`.
    #[must_use]
    pub fn option_value(&self, short: char, long: &str) -> Option<&str> {
        let mut args = self.args.iter();
        while let Some(arg) = args.next() {
            let text = arg.text.as_str();
            if let Some(rest) = text.strip_prefix("--") {
                if let Some(value) = rest.strip_prefix(long) {
                    if let Some(value) = value.strip_prefix('=') {
                        return Some(value);
                    }
                    if value.is_empty() {
                        return args.next().map(|next| next.text.as_str());
                    }
                }
            } else if let Some(letters) = text.strip_prefix('-')
                && let Some(at) = letters.find(short)
            {
                let attached = &letters[at + short.len_utf8()..];
                return if attached.is_empty() {
                    args.next().map(|next| next.text.as_str())
                } else {
                    Some(attached)
                };
            }
        }
        None
    }

    /// True if the only word is `--help` or `--version`: the program prints and stops.
    #[must_use]
    pub fn only_asks_for_help(&self) -> bool {
        matches!(
            self.args.as_slice(),
            [only] if !only.unknown && (only.text == "--help" || only.text == "--version")
        )
    }
}

/// Builds the invocation of a simple command from its words (name first). Wrappers such as
/// `sudo`, `env` and `xargs` are peeled off; `None` if there is no program to know (an empty
/// command, a name only known at run time, a wrapper with nothing after it).
#[must_use]
pub fn invocation(span: Span, mut words: Vec<Arg<'_>>) -> Option<Invocation<'_>> {
    let mut sudo = 0_u8;
    let mut xargs = false;
    let mut at = 0;
    loop {
        let first = words.get(at)?;
        if first.unknown {
            return None;
        }
        match base_name(&first.text) {
            "sudo" | "doas" => {
                sudo = sudo.saturating_add(1);
                at = skip_options(&words, at + 1, SUDO_VALUE_OPTIONS, true);
            }
            "env" => at = skip_options(&words, at + 1, &["-u", "-C", "-S"], true),
            "command" | "exec" | "nohup" | "time" => {
                at = skip_options(&words, at + 1, &[], false);
            }
            "nice" => at = skip_options(&words, at + 1, &["-n", "--adjustment"], false),
            "ionice" => at = skip_options(&words, at + 1, &["-c", "-n", "-p", "-P", "-u"], false),
            "timeout" => {
                at = skip_options(
                    &words,
                    at + 1,
                    &["-s", "-k", "--signal", "--kill-after"],
                    false,
                );
                // The duration.
                at += 1;
            }
            "xargs" => {
                xargs = true;
                at = skip_options(
                    &words,
                    at + 1,
                    &["-n", "-I", "-L", "-P", "-s", "-d", "-E", "-a"],
                    false,
                );
            }
            _ => break,
        }
    }
    let name = base_name(&words.get(at)?.text).to_owned();
    let mut args = words.split_off(at + 1);
    if xargs {
        args.push(Arg::from_input());
    }
    Some(Invocation {
        name,
        args,
        sudo,
        span,
    })
}

/// The part of a command name after its last `/`.
pub(crate) fn base_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// The index of the first word from `start` that is not an option of a wrapper (or, with
/// `assignments`, a `NAME=value`). `value_options` take the next word as their value.
fn skip_options(
    words: &[Arg<'_>],
    start: usize,
    value_options: &[&str],
    assignments: bool,
) -> usize {
    let mut at = start;
    while let Some(word) = words.get(at) {
        if word.unknown {
            break;
        }
        let text = word.text.as_str();
        if text == "--" {
            at += 1;
            break;
        }
        if assignments && is_assignment(text) {
            at += 1;
        } else if text.len() > 1 && text.starts_with('-') {
            at += if value_options.contains(&text) { 2 } else { 1 };
        } else {
            break;
        }
    }
    at
}

fn is_assignment(text: &str) -> bool {
    let Some((name, _)) = text.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use omsh_syntax::ast::CommandKind;
    use omsh_syntax::parse;

    use super::{Invocation, invocation};
    use crate::word::Arg;

    fn parse_invocation(line: &str) -> Option<Invocation<'static>> {
        // The tree must outlive the borrow of the words: leak it, this is test code.
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
        invocation(command.span, words)
    }

    fn summary(line: &str) -> (String, Vec<String>, u8) {
        let inv = parse_invocation(line).unwrap();
        (
            inv.name.clone(),
            inv.args.iter().map(|a| a.text.clone()).collect(),
            inv.sudo,
        )
    }

    #[test]
    fn a_plain_command_keeps_its_words() {
        assert_eq!(
            summary("rm -rf x"),
            ("rm".into(), vec!["-rf".into(), "x".into()], 0)
        );
    }

    #[test]
    fn the_name_loses_its_folder() {
        assert_eq!(summary("/bin/rm x").0, "rm");
        assert_eq!(summary("/sbin/mkfs.ext4 x").0, "mkfs.ext4");
    }

    #[test]
    fn sudo_and_doas_are_peeled_off_and_counted() {
        assert_eq!(summary("sudo rm x"), ("rm".into(), vec!["x".into()], 1));
        assert_eq!(summary("doas rm x").2, 1);
        assert_eq!(summary("sudo sudo rm x").2, 2);
        assert_eq!(summary("sudo -u root -- rm x").0, "rm");
        assert_eq!(summary("sudo -E -n FOO=1 rm x").0, "rm");
        assert_eq!(summary("sudo -u root rm -rf /").1, vec!["-rf", "/"]);
    }

    #[test]
    fn other_wrappers_are_peeled_off() {
        assert_eq!(summary("env A=1 -i rm x").0, "rm");
        assert_eq!(summary("env -u A rm x").0, "rm");
        assert_eq!(summary("command rm x").0, "rm");
        assert_eq!(summary("exec rm x").0, "rm");
        assert_eq!(summary("nohup rm x").0, "rm");
        assert_eq!(summary("time -p rm x").0, "rm");
        assert_eq!(summary("nice -n 10 rm x").0, "rm");
        assert_eq!(summary("nice -5 rm x").0, "rm");
        assert_eq!(summary("timeout -s KILL 5 rm x").0, "rm");
        assert_eq!(
            summary("sudo env A=1 nice rm x"),
            ("rm".into(), vec!["x".into()], 1)
        );
    }

    #[test]
    fn xargs_gives_the_program_a_word_read_at_run_time() {
        let inv = parse_invocation("xargs -n 1 rm -f").unwrap();
        assert_eq!(inv.name, "rm");
        let last = inv.args.last().unwrap();
        assert!(last.unknown && last.cmdsub);
    }

    #[test]
    fn a_wrapper_with_nothing_after_it_has_no_program() {
        assert!(parse_invocation("sudo").is_none());
        assert!(parse_invocation("sudo -u root").is_none());
        assert!(parse_invocation("env A=1").is_none());
        assert!(parse_invocation("timeout 5").is_none());
    }

    #[test]
    fn a_name_only_known_at_run_time_has_no_program() {
        assert!(parse_invocation("$x -rf /").is_none());
        assert!(parse_invocation("sudo $x -rf /").is_none());
    }

    #[test]
    fn options_are_sorted_from_operands() {
        let inv = parse_invocation("rm -rf --no-preserve-root -- -x a $b").unwrap();
        let options = inv.options();
        assert!(options.has_short('r') && options.has_short('f'));
        assert!(options.has_long("no-preserve-root"));
        let operands: Vec<_> = options.operands.iter().map(|a| a.src.as_str()).collect();
        assert_eq!(operands, ["-x", "a", "$b"]);
    }

    #[test]
    fn a_long_option_loses_its_value() {
        let inv = parse_invocation("x --force=yes --groups a").unwrap();
        let options = inv.options();
        assert!(options.has_long("force") && options.has_long("groups"));
    }

    #[test]
    fn an_option_value_is_found_in_every_spelling() {
        for line in [
            "usermod -G sudo bob",
            "usermod -aG sudo bob",
            "usermod -Gsudo bob",
            "usermod -a -G sudo bob",
            "usermod --groups sudo bob",
            "usermod --groups=sudo bob",
        ] {
            let inv = parse_invocation(line).unwrap();
            assert_eq!(inv.option_value('G', "groups"), Some("sudo"), "{line}");
        }
        let inv = parse_invocation("usermod -s /bin/sh bob").unwrap();
        assert_eq!(inv.option_value('G', "groups"), None);
    }

    #[test]
    fn help_and_version_alone_are_recognised() {
        assert!(parse_invocation("rm --help").unwrap().only_asks_for_help());
        assert!(
            parse_invocation("mkfs --version")
                .unwrap()
                .only_asks_for_help()
        );
        assert!(
            !parse_invocation("rm --help x")
                .unwrap()
                .only_asks_for_help()
        );
        assert!(!parse_invocation("rm").unwrap().only_asks_for_help());
    }
}
