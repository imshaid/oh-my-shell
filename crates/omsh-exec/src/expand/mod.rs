//! Word expansion, in the order POSIX gives (§6.4): tilde, parameters, command substitution
//! and arithmetic, then field splitting, then pathname expansion, then quote removal.
//!
//! # How the pieces fit
//!
//! A word is first turned into one or more [`Field`]s: bytes, each tagged with a [`Kind`] that
//! remembers where it came from. Splitting looks only at bytes that came from an expansion
//! and were not quoted; pathname expansion looks only at bytes that were not quoted. Quote
//! removal costs nothing: the quote characters are never copied into the field.

mod ansi_c;
mod arith;
mod param;
mod pathname;
mod pattern;
mod split;
mod tilde;

use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::thread;

use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::unistd::{pipe2, read};
use omsh_syntax::ast::{Assignment, List, PartKind, Word, WordPart};

use crate::io::Io;
use crate::shell::Shell;

use self::pattern::Pattern;
use self::split::DEFAULT_IFS;

/// Why a piece of text may or may not be special to later steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Written in the word, not quoted. Pattern characters in it are special.
    Literal,
    /// Quoted, or the result of a tilde expansion: never special.
    Quoted,
    /// The result of an expansion, not quoted. Pattern characters and `$IFS` characters in
    /// it are special.
    Expanded,
}

/// A word being built: its bytes and what kind each one is.
#[derive(Clone, Debug, Default)]
pub(crate) struct Field {
    pub(crate) bytes: Vec<u8>,
    pub(crate) kinds: Vec<Kind>,
    /// True if a quote was involved, so that the field counts even when it is empty (`""`).
    pub(crate) forced: bool,
}

impl Field {
    fn push(&mut self, bytes: &[u8], kind: Kind) {
        self.bytes.extend_from_slice(bytes);
        self.kinds.extend(std::iter::repeat_n(kind, bytes.len()));
        self.forced |= kind == Kind::Quoted;
    }
}

/// The fields of one word. A word has more than one only when `"$@"` or a similar form puts
/// in a break.
#[derive(Debug, Default)]
struct Builder {
    done: Vec<Field>,
    current: Field,
}

impl Builder {
    fn push(&mut self, bytes: &[u8], kind: Kind) {
        self.current.push(bytes, kind);
    }

    /// Adds text that later steps must leave alone, like quoted text, but that does not make
    /// a field of its own when it is empty: the home directory of `~`.
    fn push_protected(&mut self, bytes: &[u8]) {
        self.current.bytes.extend_from_slice(bytes);
        self.current
            .kinds
            .extend(std::iter::repeat_n(Kind::Quoted, bytes.len()));
    }

    /// Ends the field being built and starts another.
    fn break_field(&mut self) {
        self.done.push(std::mem::take(&mut self.current));
    }

    fn finish(mut self) -> Vec<Field> {
        self.done.push(self.current);
        self.done
    }
}

/// Whether a `~` can start a tilde prefix here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tilde {
    Off,
    /// At the start of a word.
    Word,
    /// At the start of an assignment value, and after each `:` in it.
    Assignment,
}

/// What a piece of text means where it is found.
#[derive(Clone, Copy, Debug)]
struct Ctx {
    /// The kind of text written in the word.
    literal: Kind,
    /// The kind of the result of an expansion.
    result: Kind,
    /// True inside double quotes, where `$@` keeps its fields apart.
    quoted: bool,
    tilde: Tilde,
}

impl Ctx {
    /// An ordinary word.
    const WORD: Self = Self {
        literal: Kind::Literal,
        result: Kind::Expanded,
        quoted: false,
        tilde: Tilde::Word,
    };
    /// Inside double quotes.
    const QUOTED: Self = Self {
        literal: Kind::Quoted,
        result: Kind::Quoted,
        quoted: true,
        tilde: Tilde::Off,
    };
    /// The value of an assignment: no splitting and no pattern characters.
    const ASSIGNMENT: Self = Self {
        tilde: Tilde::Assignment,
        ..Self::QUOTED
    };
    /// The target of a redirection and the word of a `case`: like an assignment value, but
    /// only a tilde at the start counts.
    const TARGET: Self = Self {
        tilde: Tilde::Word,
        ..Self::QUOTED
    };
    /// The word after `#` or `%`, which is a pattern.
    const PATTERN: Self = Self {
        tilde: Tilde::Off,
        ..Self::WORD
    };

    /// The context for the word after an operator such as `:-` in `${x:-word}`. Unquoted, the
    /// text of the word is split like the rest of the expansion.
    const fn argument(self) -> Self {
        if self.quoted {
            Self {
                tilde: Tilde::Off,
                ..self
            }
        } else {
            Self {
                literal: Kind::Expanded,
                result: Kind::Expanded,
                quoted: false,
                tilde: Tilde::Word,
            }
        }
    }
}

/// A problem found while expanding, such as `${x?message}` or a bad arithmetic expression.
/// The line stops and the status becomes 2.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ExpandError(pub(crate) String);

type Result<T> = std::result::Result<T, ExpandError>;

/// Expands words in the state of a shell.
pub(crate) struct Expander<'a> {
    pub(crate) shell: &'a mut Shell,
    /// The files of the command being expanded. The shell's own messages go to its
    /// descriptor 2, and a command inside `$(...)` starts with the same table.
    pub(crate) io: &'a Io,
}

impl Expander<'_> {
    /// Expands the words of a command: every step, including splitting and pathname
    /// expansion. One word can give any number of results.
    pub(crate) fn expand_words(&mut self, words: &[Word]) -> Result<Vec<Vec<u8>>> {
        let mut out = Vec::with_capacity(words.len());
        for word in words {
            let mut builder = Builder::default();
            self.sequence(&word.parts, &mut builder, Ctx::WORD)?;
            let fields = builder.finish();
            let ifs = self.shell.vars.get("IFS").unwrap_or(DEFAULT_IFS).to_vec();
            for field in fields {
                for field in split::split(field, &ifs) {
                    let matches = if self.shell.options.noglob {
                        Vec::new()
                    } else {
                        pathname::expand(&field, self.shell.cwd_override.as_deref())
                    };
                    if matches.is_empty() {
                        out.push(field.bytes);
                    } else {
                        out.extend(matches);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Expands the values of `NAME=value` words, with no splitting and no pathname expansion,
    /// and sets each variable before the next value is expanded: in `x=a y=$x`, `y` gets `a`.
    ///
    /// Returns the names and values, which are what a command receives in its environment
    /// when they stand in front of it.
    pub(crate) fn apply_assignments(
        &mut self,
        assignments: &[Assignment],
    ) -> Result<Vec<(String, Vec<u8>)>> {
        let mut out = Vec::with_capacity(assignments.len());
        for assignment in assignments {
            let value = self.string(&assignment.value.parts, Ctx::ASSIGNMENT)?;
            self.shell
                .vars
                .set(&assignment.name, value.clone())
                .map_err(|_| ExpandError(format!("{}: is read only", assignment.name)))?;
            out.push((assignment.name.clone(), value));
        }
        Ok(out)
    }

    /// Expands a word that stays one piece of text: the target of a redirection and the word
    /// of a `case`. Quote removal applies; splitting and pathname expansion do not.
    pub(crate) fn target(&mut self, word: &Word) -> Result<Vec<u8>> {
        self.string(&word.parts, Ctx::TARGET)
    }

    /// Expands the body of a here-document that is not quoted.
    pub(crate) fn heredoc_body(&mut self, parts: &[WordPart]) -> Result<Vec<u8>> {
        self.string(parts, Ctx::QUOTED)
    }

    /// Expands parts into one piece of text: for the word of `${x:=word}`, an arithmetic
    /// expression, and the like.
    fn string(&mut self, parts: &[WordPart], ctx: Ctx) -> Result<Vec<u8>> {
        let mut builder = Builder::default();
        self.sequence(parts, &mut builder, ctx)?;
        let fields = builder.finish();
        let mut out = Vec::new();
        for (i, field) in fields.iter().enumerate() {
            if i > 0 {
                out.push(b' ');
            }
            out.extend_from_slice(&field.bytes);
        }
        Ok(out)
    }

    /// Expands parts as a pattern, in which quoted characters are plain.
    pub(crate) fn pattern(&mut self, parts: &[WordPart]) -> Result<Pattern> {
        let mut builder = Builder::default();
        self.sequence(parts, &mut builder, Ctx::PATTERN)?;
        let mut bytes = Vec::new();
        let mut active = Vec::new();
        for (i, field) in builder.finish().iter().enumerate() {
            if i > 0 {
                bytes.push(b' ');
                active.push(false);
            }
            bytes.extend_from_slice(&field.bytes);
            active.extend(field.kinds.iter().map(|k| *k != Kind::Quoted));
        }
        Ok(Pattern::new(&bytes, &active))
    }

    /// Expands the pieces of a word, one after another, into `builder`.
    fn sequence(&mut self, parts: &[WordPart], builder: &mut Builder, ctx: Ctx) -> Result<()> {
        for (i, part) in parts.iter().enumerate() {
            if let PartKind::Literal(text) = &part.kind
                && ctx.tilde != Tilde::Off
                && (i == 0 || ctx.tilde == Tilde::Assignment)
            {
                let found = tilde::find(
                    text,
                    i == 0,
                    ctx.tilde == Tilde::Assignment,
                    i + 1 == parts.len(),
                    &self.shell.vars,
                );
                if !found.is_empty() {
                    let mut at = 0;
                    for r in &found {
                        builder.push(&text.as_bytes()[at..r.start], ctx.literal);
                        // A home directory is not split or matched against files.
                        builder.push_protected(&r.home);
                        at = r.end;
                    }
                    builder.push(&text.as_bytes()[at..], ctx.literal);
                    continue;
                }
            }
            self.part(part, builder, ctx)?;
        }
        Ok(())
    }

    fn part(&mut self, part: &WordPart, builder: &mut Builder, ctx: Ctx) -> Result<()> {
        match &part.kind {
            PartKind::Literal(text) => builder.push(text.as_bytes(), ctx.literal),
            PartKind::Escaped(c) => {
                let mut buf = [0_u8; 4];
                builder.push(c.encode_utf8(&mut buf).as_bytes(), Kind::Quoted);
            }
            PartKind::SingleQuoted(text) => builder.push(text.as_bytes(), Kind::Quoted),
            PartKind::AnsiCQuoted(text) => builder.push(&ansi_c::decode(text), Kind::Quoted),
            PartKind::DoubleQuoted(inner) => {
                if inner.is_empty() {
                    // `""` has no parts, but it still makes a word. (`"$@"` with no
                    // parameters is different: it makes none.)
                    builder.push(&[], Kind::Quoted);
                } else {
                    self.sequence(inner, builder, Ctx::QUOTED)?;
                }
            }
            PartKind::Parameter(parameter) => self.parameter(parameter, builder, ctx)?,
            PartKind::CommandSub(list) => {
                let output = self.command_substitution(list);
                builder.push(&output, ctx.result);
            }
            PartKind::Arithmetic(inner) => {
                let expression = self.string(inner, Ctx::QUOTED)?;
                let value = arith::evaluate(&expression, &mut self.shell.vars)
                    .map_err(|e| ExpandError(format!("arithmetic expression: {e}")))?;
                builder.push(value.to_string().as_bytes(), ctx.result);
            }
        }
        Ok(())
    }

    /// Runs `$(...)` and returns what it printed, without the final line endings.
    ///
    /// The commands run in this process, on a copy of the shell, so that an assignment or an
    /// `exit` inside does not reach the outer shell. Their standard output is a pipe. A second
    /// thread reads the pipe while the commands run, because a program that prints more than a
    /// pipe holds (64 KiB) would otherwise wait for a reader that is waiting for it.
    fn command_substitution(&mut self, list: &List) -> Vec<u8> {
        let Ok((reader, writer)) = pipe2(OFlag::O_CLOEXEC) else {
            self.io.say("oh-my-shell: cannot make a pipe");
            return Vec::new();
        };
        let mut table = self.io.clone();
        table.set(1, Arc::new(writer));
        let mut copy = self.shell.subshell();
        // Outside a job, the substitution's programs stay in the shell's own group.
        copy.no_new_jobs = copy.job.is_none();
        let mut output = Vec::new();
        let status = thread::scope(|scope| {
            let drain = thread::Builder::new().spawn_scoped(scope, || {
                drain(&reader, &mut output);
            });
            if let Err(error) = drain {
                self.io
                    .say(format!("oh-my-shell: cannot read a substitution: {error}"));
                return crate::status::ExitStatus(1);
            }
            let flow = copy.run_list(list, &table);
            // The `EXIT` trap of the substitution writes to the same pipe.
            let status = copy.finish_copy(flow, &table);
            // Let go of the writing end, or the reader never sees the end of the output.
            drop(table);
            status
        });
        self.shell.cmdsub_status = Some(status);
        while output.last() == Some(&b'\n') {
            output.pop();
        }
        // A shell word cannot hold a NUL byte. dash drops it quietly.
        output.retain(|b| *b != 0);
        output
    }
}

/// Reads `pipe` until its writers are gone, adding everything to `buffer`.
fn drain(pipe: &OwnedFd, buffer: &mut Vec<u8>) {
    let mut chunk = [0_u8; 8192];
    loop {
        match read(pipe, &mut chunk) {
            Ok(0) => return,
            Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            Err(Errno::EINTR) => {}
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests;
