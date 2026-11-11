//! A word of the syntax tree, reduced to what the rules need to know.

use omsh_syntax::Span;
use omsh_syntax::ast::{List, PartKind, Word, WordPart};

/// One word of a command, with quotes removed.
#[derive(Clone, Debug)]
pub struct Arg<'a> {
    /// The text that is known before the command runs. Anything that is only known at run
    /// time (`$x`, `$(...)`, arithmetic) contributes nothing, so `"$d"/` is `/`: the
    /// *empty case*, which is what a rule must worry about.
    pub text: String,
    /// One flag for each byte of `text`: true if it was written unquoted, so a pattern
    /// character there is special. Text put in for `~` or `$HOME` is never special.
    pub active: Vec<bool>,
    /// The word as the person wrote it. Shown as the target of a finding.
    pub src: String,
    /// Where the word is.
    pub span: Span,
    /// True if some part is only known at run time.
    pub unknown: bool,
    /// True if a command substitution is part of it.
    pub cmdsub: bool,
    /// True if it has an unquoted `*`, `?` or `[`.
    pub glob: bool,
    /// The command substitutions inside it, outermost only.
    pub subs: Vec<&'a List>,
}

impl<'a> Arg<'a> {
    /// Reads `word`. `src` is the text the tree came from. `home` is what `~` and `$HOME`
    /// mean; without it they stay unknown.
    #[must_use]
    pub fn from_word(word: &'a Word, src: &str, home: Option<&str>) -> Self {
        let mut arg = Self {
            text: String::new(),
            active: Vec::new(),
            src: word.span.text(src).to_owned(),
            span: word.span,
            unknown: false,
            cmdsub: false,
            glob: false,
            subs: Vec::new(),
        };
        arg.parts(&word.parts, false, home);
        let tilde = matches!(
            word.parts.first().map(|part| &part.kind),
            Some(PartKind::Literal(text)) if text.starts_with('~')
        );
        if tilde
            && (arg.text == "~" || arg.text.starts_with("~/"))
            && let Some(home) = home
        {
            let mut active = vec![false; home.len()];
            active.extend_from_slice(&arg.active[1..]);
            arg.text = format!("{home}{}", &arg.text[1..]);
            arg.active = active;
        }
        arg
    }

    /// The word a program gets from `xargs`: something read at run time.
    #[must_use]
    pub fn from_input() -> Self {
        Self {
            text: String::new(),
            active: Vec::new(),
            src: "(read by xargs)".to_owned(),
            span: Span::new(0, 0),
            unknown: true,
            cmdsub: true,
            glob: false,
            subs: Vec::new(),
        }
    }

    /// Adds known text; `active` says whether pattern characters in it are special.
    fn push(&mut self, text: &str, active: bool) {
        self.text.push_str(text);
        self.active.extend(std::iter::repeat_n(active, text.len()));
    }

    fn parts(&mut self, parts: &'a [WordPart], quoted: bool, home: Option<&str>) {
        for part in parts {
            match &part.kind {
                PartKind::Literal(text) => {
                    if !quoted && text.contains(['*', '?', '[']) {
                        self.glob = true;
                    }
                    self.push(text, !quoted);
                }
                PartKind::Escaped(c) => self.push(c.encode_utf8(&mut [0; 4]), false),
                PartKind::SingleQuoted(text) | PartKind::AnsiCQuoted(text) => {
                    self.push(text, false);
                }
                PartKind::DoubleQuoted(inner) => self.parts(inner, true, home),
                PartKind::Parameter(parameter) => {
                    if parameter.name == "HOME"
                        && parameter.op.is_none()
                        && !parameter.length
                        && let Some(home) = home
                    {
                        self.push(home, false);
                    } else {
                        self.unknown = true;
                        self.nested(&parameter.arg);
                    }
                }
                PartKind::CommandSub(list) => {
                    self.unknown = true;
                    self.cmdsub = true;
                    self.subs.push(list);
                }
                PartKind::Arithmetic(inner) => {
                    self.unknown = true;
                    self.nested(inner);
                }
            }
        }
    }

    /// Collects command substitutions hidden in the argument of a parameter or in arithmetic.
    fn nested(&mut self, parts: &'a [WordPart]) {
        let before = self.subs.len();
        collect_subs(parts, &mut self.subs);
        if self.subs.len() > before {
            self.cmdsub = true;
        }
    }
}

/// Adds the command substitutions in `parts`, at any depth of quoting, but not the ones
/// inside other substitutions.
pub fn collect_subs<'a>(parts: &'a [WordPart], out: &mut Vec<&'a List>) {
    for part in parts {
        match &part.kind {
            PartKind::CommandSub(list) => out.push(list),
            PartKind::DoubleQuoted(inner) | PartKind::Arithmetic(inner) => {
                collect_subs(inner, out);
            }
            PartKind::Parameter(parameter) => collect_subs(&parameter.arg, out),
            PartKind::Literal(_)
            | PartKind::Escaped(_)
            | PartKind::SingleQuoted(_)
            | PartKind::AnsiCQuoted(_) => {}
        }
    }
}

/// The text of a word that is the same on every run, or `None`.
#[must_use]
pub fn static_text(word: &Word) -> Option<String> {
    let arg = Arg::from_word(word, "", None);
    (!arg.unknown).then_some(arg.text)
}

#[cfg(test)]
mod tests {
    use omsh_syntax::ast::CommandKind;
    use omsh_syntax::parse;

    use super::{Arg, static_text};

    /// The words of the first command of `line`.
    fn args(line: &str, home: Option<&str>) -> Vec<(String, bool, bool, bool)> {
        let list = parse(line).unwrap();
        let CommandKind::Simple(simple) = &list.items[0].and_or.first.commands[0].kind else {
            panic!("not a simple command");
        };
        simple
            .words
            .iter()
            .map(|word| {
                let arg = Arg::from_word(word, line, home);
                (arg.text, arg.unknown, arg.cmdsub, arg.glob)
            })
            .collect()
    }

    #[test]
    fn quotes_are_removed_and_unknown_parts_are_empty() {
        let words = args(r#"x 'a b' "c$d" e\ f"#, None);
        assert_eq!(words[1].0, "a b");
        assert_eq!(words[2], ("c".to_owned(), true, false, false));
        assert_eq!(words[3].0, "e f");
    }

    #[test]
    fn a_command_substitution_is_unknown_and_remembered() {
        let words = args("x $(ls) `ls` \"$(ls)\" ${y:-$(ls)}", None);
        for word in &words[1..] {
            assert!(word.1 && word.2, "{word:?}");
        }
    }

    #[test]
    fn globs_count_only_when_unquoted() {
        let words = args(r#"x *.c "*.c" '?' a\* [ab] "#, None);
        assert!(words[1].3);
        assert!(!words[2].3);
        assert!(!words[3].3);
        assert!(!words[4].3);
        assert!(words[5].3);
    }

    #[test]
    fn home_and_tilde_are_resolved_only_when_known() {
        let known = args("x ~ ~/a $HOME \"$HOME/b\" ${HOME:-z}", Some("/h"));
        assert_eq!(known[1].0, "/h");
        assert_eq!(known[2].0, "/h/a");
        assert_eq!((known[3].0.as_str(), known[3].1), ("/h", false));
        assert_eq!(known[4].0, "/h/b");
        assert!(known[5].1, "an operator makes it unknown");
        let unknown = args("x ~ $HOME", None);
        assert_eq!(unknown[1].0, "~");
        assert_eq!((unknown[2].0.as_str(), unknown[2].1), ("", true));
    }

    #[test]
    fn active_marks_the_bytes_a_pattern_character_may_be_special_in() {
        let list = parse(r#"x a*"b*"\*c ~/*"#).unwrap();
        let CommandKind::Simple(simple) = &list.items[0].and_or.first.commands[0].kind else {
            panic!();
        };
        let word = |i: usize| Arg::from_word(&simple.words[i], r#"x a*"b*"\*c ~/*"#, Some("/h"));
        let mixed = word(1);
        assert_eq!(mixed.text, "a*b**c");
        assert_eq!(mixed.active, [true, true, false, false, false, true]);
        let tilde = word(2);
        assert_eq!(tilde.text, "/h/*");
        assert_eq!(tilde.active, [false, false, true, true]);
        for i in 0..3 {
            let arg = word(i);
            assert_eq!(arg.active.len(), arg.text.len());
        }
    }

    #[test]
    fn a_tilde_inside_quotes_is_not_a_home_directory() {
        let words = args(r#"x '~' "~/a""#, Some("/h"));
        assert_eq!(words[1].0, "~");
        assert_eq!(words[2].0, "~/a");
    }

    #[test]
    fn static_text_is_none_when_anything_is_unknown() {
        let list = parse("x a$b c").unwrap();
        let CommandKind::Simple(simple) = &list.items[0].and_or.first.commands[0].kind else {
            panic!();
        };
        assert_eq!(static_text(&simple.words[0]).as_deref(), Some("x"));
        assert_eq!(static_text(&simple.words[1]), None);
    }
}
