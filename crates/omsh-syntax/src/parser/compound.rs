//! Compound commands: `( )`, `{ }`, `if`, `while`, `until`, `for` and `case`.
//!
//! Each function starts with the cursor on the opening word. Once it has got past the first
//! words of its construct, it returns what it has even if something later is wrong. The
//! caller then meets the same bad token and deals with it.

use super::{Expected, ListPlace, ParseErrorKind, Parser, is_name};
use crate::ast::{
    CaseArm, CaseCommand, Command, CommandKind, ForCommand, IfBranch, IfCommand, List, LoopCommand,
    Word,
};
use crate::span::{Span, strip_line_continuations};
use crate::token::{Op, ReservedWord, Token, TokenKind};

impl Parser<'_> {
    /// Parses a compound command and the redirections after it.
    pub(super) fn parse_compound(&mut self) -> Option<Command> {
        let token = self.peek()?;
        if !self.enter(token.span) {
            return None;
        }
        let kind = match self.reserved() {
            Some(ReservedWord::If) => Some(self.parse_if()),
            Some(ReservedWord::While) => Some(CommandKind::While(self.parse_loop())),
            Some(ReservedWord::Until) => Some(CommandKind::Until(self.parse_loop())),
            Some(ReservedWord::For) => self.parse_for(),
            Some(ReservedWord::Case) => self.parse_case(),
            Some(ReservedWord::LeftBrace) => Some(self.parse_group()),
            _ => Some(self.parse_subshell()),
        };
        self.depth -= 1;
        let kind = kind?;
        let redirects = self.parse_trailing_redirects();
        Some(Command {
            kind,
            redirects,
            span: Span::new(token.span.start, self.last_end),
        })
    }

    /// Parses a list that must hold at least one command, as `dash` and `bash` both require
    /// in a compound command: `{ }` and `if a; then fi` are errors.
    fn parse_body(&mut self) -> List {
        let list = self.parse_list(ListPlace::Nested);
        if list.items.is_empty() {
            self.unexpected(Expected::Command);
        }
        list
    }

    /// Parses a list after `opener` if `opener` is at the cursor.
    fn parse_body_after(&mut self, opener: ReservedWord) -> List {
        if self.expect_reserved(opener) {
            self.parse_body()
        } else {
            List::empty(self.last_end)
        }
    }

    fn parse_subshell(&mut self) -> CommandKind {
        let open = self.peek().map(|token| token.span);
        self.advance();
        let list = self.parse_body();
        if self.expect_right_paren()
            && let Some(open) = open
        {
            self.check_arithmetic_command(open, &list);
        }
        CommandKind::Subshell(list)
    }

    fn parse_group(&mut self) -> CommandKind {
        self.advance();
        let list = self.parse_body();
        self.expect_reserved(ReservedWord::RightBrace);
        CommandKind::Group(list)
    }

    fn parse_if(&mut self) -> CommandKind {
        let mut branches = Vec::new();
        let mut else_body = None;
        loop {
            // This is a loop and not a recursive call, so a long `elif` chain uses no stack.
            let start = self.peek_start();
            self.advance();
            let condition = self.parse_body();
            let body = self.parse_body_after(ReservedWord::Then);
            let span = Span::new(start, body.span.end);
            branches.push(IfBranch {
                condition,
                body,
                span,
            });
            match self.reserved() {
                Some(ReservedWord::Elif) => {}
                Some(ReservedWord::Else) => {
                    self.advance();
                    else_body = Some(self.parse_body());
                    break;
                }
                _ => break,
            }
        }
        self.expect_reserved(ReservedWord::Fi);
        CommandKind::If(IfCommand {
            branches,
            else_body,
        })
    }

    /// Parses `while` or `until`, which differ only in the word they start with.
    fn parse_loop(&mut self) -> LoopCommand {
        self.advance();
        let condition = self.parse_body();
        let body = self.parse_do_group();
        LoopCommand { condition, body }
    }

    fn parse_do_group(&mut self) -> List {
        let at_do = self.reserved() == Some(ReservedWord::Do);
        let body = self.parse_body_after(ReservedWord::Do);
        if at_do {
            self.expect_reserved(ReservedWord::Done);
        }
        body
    }

    fn parse_for(&mut self) -> Option<CommandKind> {
        self.advance();
        let Some((_, name_span)) = self.peek_word() else {
            let span = self
                .peek()
                .map_or(Span::new(self.end, self.end), |token| token.span);
            self.report(ParseErrorKind::BadForName, span);
            return None;
        };
        self.advance();
        let name = strip_line_continuations(name_span.text(self.src)).into_owned();
        if !is_name(&name) {
            self.report(ParseErrorKind::BadForName, name_span);
        }

        self.skip_newlines();
        let words = if self.reserved() == Some(ReservedWord::In) {
            self.advance();
            Some(self.parse_for_words())
        } else {
            // `for i; do` is allowed too.
            if self.peek_op() == Some(Op::Semi) {
                self.advance();
            }
            None
        };
        self.skip_newlines();
        let body = self.parse_do_group();
        Some(CommandKind::For(ForCommand {
            name,
            name_span,
            words,
            body,
        }))
    }

    /// Parses the words after `in` and the `;` or line ending that ends them.
    fn parse_for_words(&mut self) -> Vec<Word> {
        let mut words = Vec::new();
        while let Some((word, span)) = self.peek_word() {
            self.advance();
            let word = self.word(word, span);
            self.check_braces(&word);
            words.push(word);
        }
        match self.peek() {
            Some(Token {
                kind: TokenKind::Newline | TokenKind::Op(Op::Semi),
                ..
            }) => self.advance(),
            Some(_) => self.unexpected(Expected::CommandEnd),
            None => {}
        }
        words
    }

    fn parse_case(&mut self) -> Option<CommandKind> {
        self.advance();
        let Some((word, span)) = self.peek_word() else {
            self.unexpected(Expected::CaseWord);
            return None;
        };
        self.advance();
        let word = self.word(word, span);
        self.skip_newlines();
        let mut arms = Vec::new();
        if self.expect_reserved(ReservedWord::In) {
            self.parse_case_arms(&mut arms);
        }
        Some(CommandKind::Case(CaseCommand { word, arms }))
    }

    /// Parses the arms and the closing `esac`.
    fn parse_case_arms(&mut self, arms: &mut Vec<CaseArm>) {
        loop {
            self.skip_newlines();
            // Only here does `esac` close the command. In `(esac)` and `a|esac`, it is a
            // pattern, because the parsing of an arm has already begun.
            if self.reserved() == Some(ReservedWord::Esac) {
                self.advance();
                return;
            }
            let Some(arm) = self.parse_case_arm() else {
                return;
            };
            arms.push(arm);
            if self.peek_op() == Some(Op::DoubleSemi) {
                self.advance();
            } else {
                self.expect_reserved(ReservedWord::Esac);
                return;
            }
        }
    }

    fn parse_case_arm(&mut self) -> Option<CaseArm> {
        let start = self.peek_start();
        if self.peek_op() == Some(Op::LeftParen) {
            self.advance();
        }
        let mut patterns = Vec::new();
        loop {
            let Some((word, span)) = self.peek_word() else {
                self.unexpected(Expected::CasePattern);
                return None;
            };
            self.advance();
            patterns.push(self.word(word, span));
            if self.peek_op() != Some(Op::Pipe) {
                break;
            }
            self.advance();
        }
        if !self.expect_right_paren() {
            return None;
        }
        let body = self.parse_list(ListPlace::Nested);
        let span = Span::new(start, body.span.end);
        Some(CaseArm {
            patterns,
            body,
            span,
        })
    }
}
