//! Everything that starts with `$`: parameters, arithmetic and command substitution.

use super::events::{Close, NullSink, Open, ParamInfo, Sink};
use super::{Ctx, MAX_NESTING, Quote, Scanner, is_name_char, is_name_start, is_special_param};
use crate::span::Span;
use crate::token::{LexErrorKind, ParamOp};

impl<S: Sink> Scanner<'_, S> {
    /// Whether the `$` at `pos` starts an expansion, as opposed to being a plain `$`.
    pub(super) fn dollar_starts_expansion(&self, ctx: Ctx) -> bool {
        match self.at(self.skip_continuations(self.pos + 1)) {
            Some(b'{' | b'(') => true,
            Some(b'\'') => ctx.ansi_c_quotes(),
            Some(b) => is_name_char(b) || is_special_param(b),
            None => false,
        }
    }

    pub(super) fn scan_dollar(&mut self, ctx: Ctx) {
        let start = self.pos;
        let after = self.skip_continuations(start + 1);
        match self.at(after) {
            Some(b'\'') => self.scan_single_quoted(start, after + 1, Quote::AnsiC),
            Some(b'(') => {
                let arithmetic = self
                    .follows(after + 1, b'(')
                    .filter(|&inner| self.looks_like_arithmetic(inner));
                match arithmetic {
                    Some(inner) => self.scan_arithmetic(start, inner),
                    None => self.scan_command_sub(start, after + 1),
                }
            }
            Some(b'{') => self.scan_braced_param(start, after + 1, ctx),
            _ => self.scan_short_param(start, after),
        }
    }

    /// Scans `$NAME`, `$1` or `$?`. `$10` is `$1` followed by `0`, as POSIX says.
    fn scan_short_param(&mut self, start: usize, name_start: usize) {
        self.sink.open(Open::Parameter, start);
        let mut end = name_start + 1;
        if self.at(name_start).is_some_and(is_name_start) {
            loop {
                let next = self.skip_continuations(end);
                if !self.at(next).is_some_and(is_name_char) {
                    break;
                }
                end = next + 1;
            }
        }
        self.pos = end;
        self.sink.close(
            Close::Parameter(ParamInfo {
                name: Span::new(name_start, end),
                braced: false,
                length: false,
                op: None,
            }),
            end,
        );
    }

    /// Scans `${...}`. `inner` is the first byte after the brace.
    fn scan_braced_param(&mut self, start: usize, inner: usize, ctx: Ctx) {
        if !self.enter(start) {
            return;
        }
        self.sink.open(Open::Parameter, start);
        let (name, length) = self.scan_param_name(inner);
        let (op, op_span) = self.scan_param_op();
        let op = match op {
            Some(_) if length => Some(ParamOp::Unsupported),
            other => other,
        };

        if op.is_some() {
            let single_quotes = !ctx.in_dq()
                || matches!(
                    op,
                    Some(
                        ParamOp::Hash
                            | ParamOp::DoubleHash
                            | ParamOp::Percent
                            | ParamOp::DoublePercent
                    )
                );
            let closed = self.scan_parts(Ctx::ParamArg {
                in_dq: ctx.in_dq(),
                single_quotes,
            });
            if closed {
                self.pos += 1;
            } else {
                self.error(LexErrorKind::UnterminatedParameter, start, inner);
            }
        } else if let Some(end) = self.follows(self.pos, b'}') {
            self.pos = end;
        } else {
            self.error(LexErrorKind::UnterminatedParameter, start, inner);
        }

        self.sink.close(
            Close::Parameter(ParamInfo {
                name,
                braced: true,
                length,
                op: op.map(|op| (op, op_span)),
            }),
            self.pos,
        );
        self.depth -= 1;
    }

    /// Reads the name in `${...}` and whether it is a `${#...}` length. A `#` directly
    /// after the brace is the length operator only if something that can be measured
    /// follows it. `${#}`, `${#-x}` and `${#:-x}` all use the parameter `$#`.
    fn scan_param_name(&mut self, inner: usize) -> (Span, bool) {
        let first = self.skip_continuations(inner);
        let mut length = false;
        let mut name_start = first;
        if self.at(first) == Some(b'#') {
            let second = self.skip_continuations(first + 1);
            let measures = match self.at(second) {
                None | Some(b'}' | b':' | b'=' | b'+' | b'%') => false,
                Some(b'-' | b'?' | b'#') => self.follows(second + 1, b'}').is_some(),
                Some(_) => true,
            };
            if measures {
                length = true;
                name_start = second;
            }
        }
        let mut end = name_start;
        match self.at(name_start) {
            Some(b) if is_name_start(b) => {
                end = self.consume_while(name_start, is_name_char);
            }
            Some(b) if b.is_ascii_digit() => {
                end = self.consume_while(name_start, |b| b.is_ascii_digit());
            }
            Some(b) if is_special_param(b) => end = name_start + 1,
            _ => {}
        }
        self.pos = end;
        (Span::new(name_start, end), length)
    }

    /// Returns the end of the run of bytes satisfying `pred`, ignoring continuations.
    fn consume_while(&self, start: usize, pred: fn(u8) -> bool) -> usize {
        let mut end = start;
        loop {
            let next = self.skip_continuations(end);
            if !self.at(next).is_some_and(pred) {
                return end;
            }
            end = next + 1;
        }
    }

    /// Reads the operator after the name in `${...}`, if there is one.
    fn scan_param_op(&mut self) -> (Option<ParamOp>, Span) {
        let start = self.skip_continuations(self.pos);
        let two = |this: &Self, second: u8, double: ParamOp, single: ParamOp| match this
            .follows(start + 1, second)
        {
            Some(end) => (double, end),
            None => (single, start + 1),
        };
        let (op, end) = match self.at(start) {
            None | Some(b'}') => return (None, Span::new(start, start)),
            Some(b':') => {
                let after = self.skip_continuations(start + 1);
                let op = match self.at(after) {
                    Some(b'-') => Some(ParamOp::ColonDash),
                    Some(b'=') => Some(ParamOp::ColonEquals),
                    Some(b'?') => Some(ParamOp::ColonQuestion),
                    Some(b'+') => Some(ParamOp::ColonPlus),
                    _ => None,
                };
                match op {
                    Some(op) => (op, after + 1),
                    None => (ParamOp::Unsupported, start),
                }
            }
            Some(b'-') => (ParamOp::Dash, start + 1),
            Some(b'=') => (ParamOp::Equals, start + 1),
            Some(b'?') => (ParamOp::Question, start + 1),
            Some(b'+') => (ParamOp::Plus, start + 1),
            Some(b'#') => two(self, b'#', ParamOp::DoubleHash, ParamOp::Hash),
            Some(b'%') => two(self, b'%', ParamOp::DoublePercent, ParamOp::Percent),
            Some(_) => (ParamOp::Unsupported, start),
        };
        self.pos = end;
        (Some(op), Span::new(start, end))
    }

    /// Decides whether `$((` opens arithmetic or a substitution holding a subshell, as in
    /// `$((echo a); (echo b))`. It is arithmetic unless the `)` matching the second `(`
    /// has something other than `)` after it.
    ///
    /// The `)` is found by the arithmetic scan itself, run on a sink that drops everything
    /// (`Ctx::Arithmetic { probe: true }`). So a parenthesis inside a backtick body, a
    /// `$(...)` with its `case` patterns and here-documents, or a double quote does not
    /// count, exactly as in the real scan; two hand-written scans of the same text can
    /// disagree, and a flat one did. The lookahead differs from the real scan in one rule:
    /// single quotes hide a parenthesis (`$((echo ')'))` is arithmetic).
    ///
    /// Every nested `$((` is met by the lookahead and by the real scan, so an answer is
    /// kept by position; otherwise the work would double at each level.
    fn looks_like_arithmetic(&mut self, inner: usize) -> bool {
        // Past the limit both paths end in the nesting error, whatever is decided here.
        if self.depth >= MAX_NESTING {
            return true;
        }
        let key = (inner, self.limit, self.depth);
        if let Some(&known) = self.memo.get(&key) {
            return known;
        }
        let mut sink = NullSink;
        let mut probe = Scanner {
            src: self.src,
            bytes: self.bytes,
            pos: inner,
            limit: self.limit,
            end: self.end,
            // The level `enter` gives to the arithmetic, if it is one.
            depth: self.depth + 1,
            aborted: false,
            sink: &mut sink,
            memo: &mut *self.memo,
            probe_answer: None,
        };
        probe.scan_parts(Ctx::Arithmetic { probe: true });
        // The input ran out before the `)`: the real scan reports the unfinished expansion.
        let answer = probe.probe_answer.unwrap_or(true);
        self.memo.insert(key, answer);
        answer
    }

    fn scan_arithmetic(&mut self, start: usize, inner: usize) {
        if !self.enter(start) {
            return;
        }
        self.sink.open(Open::Arithmetic, start);
        self.pos = inner;
        let closed = self.scan_parts(Ctx::Arithmetic { probe: false });
        let body = Span::new(inner, self.pos);
        if closed {
            self.pos = self.follows(self.pos + 1, b')').unwrap_or(self.pos + 1);
        } else {
            self.error(LexErrorKind::UnterminatedArithmetic, start, inner);
        }
        self.sink.close(Close::Arithmetic { body }, self.pos);
        self.depth -= 1;
    }

    fn scan_command_sub(&mut self, start: usize, inner: usize) {
        if !self.enter(start) {
            return;
        }
        self.sink.open(Open::CommandSub, start);
        self.pos = inner;
        let closed = self.scan_list(true);
        let body = Span::new(inner, self.pos);
        if closed {
            self.pos += 1;
        } else {
            self.error(LexErrorKind::UnterminatedCommandSub, start, inner);
        }
        self.sink.close(Close::CommandSub { body }, self.pos);
        self.depth -= 1;
    }
}
