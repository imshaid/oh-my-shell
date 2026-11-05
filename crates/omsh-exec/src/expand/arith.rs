//! Arithmetic expansion: `$(( ... ))`.
//!
//! Integers only, as 64-bit signed numbers that wrap around on overflow. The operators are
//! those of C without `++`, `--` and `,`, which is what dash supports, and a variable that is
//! not set counts as 0.
//!
//! The expression is read by "precedence climbing": a function per level of priority, each
//! calling the one above it for its operands.

use crate::vars::Variables;

/// Evaluates `expr`, which has already been through parameter expansion. Assignments inside
/// it (`$((x = 3))`) change `vars`.
///
/// # Errors
///
/// Returns a message for a bad expression, a bad number or a division by zero.
pub(crate) fn evaluate(expr: &[u8], vars: &mut Variables) -> Result<i64, String> {
    let tokens = tokenize(expr)?;
    let mut parser = Parser {
        tokens,
        pos: 0,
        vars,
    };
    let value = parser.assignment(true)?;
    match parser.peek(0) {
        Token::End => Ok(value),
        other => Err(format!("{}: unexpected {}", show(expr), other.describe())),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Number(i64),
    Name(String),
    Op(&'static str),
    End,
}

impl Token {
    fn describe(&self) -> String {
        match self {
            Self::Number(n) => format!("number {n}"),
            Self::Name(name) => format!("name `{name}`"),
            Self::Op(op) => format!("`{op}`"),
            Self::End => "end of the expression".to_owned(),
        }
    }
}

/// The operators, longest first so that `<<=` is not read as `<<` and `=`.
const OPERATORS: &[&str] = &[
    "<<=", ">>=", "&&", "||", "==", "!=", "<=", ">=", "<<", ">>", "+=", "-=", "*=", "/=", "%=",
    "&=", "^=", "|=", "+", "-", "*", "/", "%", "<", ">", "&", "^", "|", "!", "~", "?", ":", "(",
    ")", "=",
];

fn show(expr: &[u8]) -> String {
    String::from_utf8_lossy(expr).trim().to_owned()
}

fn tokenize(expr: &[u8]) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < expr.len() {
        let c = expr[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c.is_ascii_digit() {
            let end = run_end(expr, i);
            tokens.push(Token::Number(parse_number(&expr[i..end])?));
            i = end;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let end = run_end(expr, i);
            tokens.push(Token::Name(
                String::from_utf8_lossy(&expr[i..end]).into_owned(),
            ));
            i = end;
        } else if let Some(op) = OPERATORS
            .iter()
            .find(|op| expr[i..].starts_with(op.as_bytes()))
        {
            tokens.push(Token::Op(op));
            i += op.len();
        } else {
            return Err(format!(
                "{}: unexpected character `{}`",
                show(expr),
                String::from_utf8_lossy(&expr[i..=i])
            ));
        }
    }
    tokens.push(Token::End);
    Ok(tokens)
}

/// The end of the run of letters, digits and underscores that starts at `from`.
fn run_end(expr: &[u8], from: usize) -> usize {
    expr[from..]
        .iter()
        .position(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
        .map_or(expr.len(), |n| from + n)
}

/// Reads `123`, `0x1F` or `017`. A number that is too big becomes the largest one, as in dash.
fn parse_number(text: &[u8]) -> Result<i64, String> {
    let bad = || format!("Illegal number: {}", String::from_utf8_lossy(text));
    let (digits, radix) = match text {
        [b'0', b'x' | b'X', rest @ ..] => (rest, 16),
        [b'0', rest @ ..] if !rest.is_empty() => (rest, 8),
        _ => (text, 10),
    };
    if digits.is_empty() {
        return Err(bad());
    }
    let mut value: i64 = 0;
    for &b in digits {
        let digit = char::from(b).to_digit(radix).ok_or_else(bad)?;
        value = value
            .checked_mul(i64::from(radix))
            .and_then(|v| v.checked_add(i64::from(digit)))
            .unwrap_or(i64::MAX);
    }
    Ok(value)
}

/// The value of a variable used in an expression. Unset and empty are 0; anything else must
/// be a number, with an optional sign and surrounding blanks.
fn variable_value(vars: &Variables, name: &str) -> Result<i64, String> {
    let Some(raw) = vars.get(name) else {
        return Ok(0);
    };
    let text = String::from_utf8_lossy(raw);
    let text = text.trim();
    if text.is_empty() {
        return Ok(0);
    }
    let (negative, digits) = match text.as_bytes() {
        [b'-', rest @ ..] => (true, rest),
        [b'+', rest @ ..] => (false, rest),
        all => (false, all),
    };
    let value = parse_number(digits).map_err(|_| format!("Illegal number: {text}"))?;
    Ok(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    vars: &'a mut Variables,
}

/// Binary operators from the loosest to the tightest binding.
const LEVELS: &[&[&str]] = &[
    &["||"],
    &["&&"],
    &["|"],
    &["^"],
    &["&"],
    &["==", "!="],
    &["<", "<=", ">", ">="],
    &["<<", ">>"],
    &["+", "-"],
    &["*", "/", "%"],
];

impl Parser<'_> {
    fn peek(&self, ahead: usize) -> &Token {
        self.tokens.get(self.pos + ahead).unwrap_or(&Token::End)
    }

    fn eat(&mut self, op: &str) -> bool {
        if matches!(self.peek(0), Token::Op(o) if *o == op) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// `eval` is false in a branch that is not taken (`0 && (1/0)`): it is read, but it must
    /// not assign or fail.
    fn assignment(&mut self, eval: bool) -> Result<i64, String> {
        if let (Token::Name(name), Token::Op(op)) = (self.peek(0), self.peek(1))
            && op.ends_with('=')
            && !matches!(*op, "==" | "!=" | "<=" | ">=")
        {
            let (name, op) = (name.clone(), *op);
            self.pos += 2;
            let right = self.assignment(eval)?;
            if !eval {
                return Ok(0);
            }
            let value = if op == "=" {
                right
            } else {
                let left = variable_value(self.vars, &name)?;
                apply(&op[..op.len() - 1], left, right)?
            };
            self.vars.set(&name, value.to_string().into_bytes());
            return Ok(value);
        }
        self.conditional(eval)
    }

    fn conditional(&mut self, eval: bool) -> Result<i64, String> {
        let condition = self.binary(0, eval)?;
        if !self.eat("?") {
            return Ok(condition);
        }
        let then = self.assignment(eval && condition != 0)?;
        if !self.eat(":") {
            return Err(format!("expected `:`, found {}", self.peek(0).describe()));
        }
        let otherwise = self.assignment(eval && condition == 0)?;
        Ok(if condition != 0 { then } else { otherwise })
    }

    fn binary(&mut self, level: usize, eval: bool) -> Result<i64, String> {
        if level == LEVELS.len() {
            return self.unary(eval);
        }
        let mut left = self.binary(level + 1, eval)?;
        loop {
            let Token::Op(op) = *self.peek(0) else {
                return Ok(left);
            };
            if !LEVELS[level].contains(&op) {
                return Ok(left);
            }
            self.pos += 1;
            left = match op {
                "&&" => {
                    let right = self.binary(level + 1, eval && left != 0)?;
                    i64::from(left != 0 && right != 0)
                }
                "||" => {
                    let right = self.binary(level + 1, eval && left == 0)?;
                    i64::from(left != 0 || right != 0)
                }
                _ => {
                    let right = self.binary(level + 1, eval)?;
                    if eval { apply(op, left, right)? } else { 0 }
                }
            };
        }
    }

    fn unary(&mut self, eval: bool) -> Result<i64, String> {
        for op in ["+", "-", "~", "!"] {
            if self.eat(op) {
                let value = self.unary(eval)?;
                return Ok(match op {
                    "-" => value.wrapping_neg(),
                    "~" => !value,
                    "!" => i64::from(value == 0),
                    _ => value,
                });
            }
        }
        self.primary(eval)
    }

    fn primary(&mut self, eval: bool) -> Result<i64, String> {
        match self.peek(0).clone() {
            Token::Number(n) => {
                self.pos += 1;
                Ok(n)
            }
            Token::Name(name) => {
                self.pos += 1;
                if eval {
                    variable_value(self.vars, &name)
                } else {
                    Ok(0)
                }
            }
            Token::Op("(") => {
                self.pos += 1;
                let value = self.assignment(eval)?;
                if self.eat(")") {
                    Ok(value)
                } else {
                    Err(format!("expected `)`, found {}", self.peek(0).describe()))
                }
            }
            other => Err(format!(
                "expected a number or a name, found {}",
                other.describe()
            )),
        }
    }
}

/// Applies a binary operator.
fn apply(op: &str, left: i64, right: i64) -> Result<i64, String> {
    Ok(match op {
        "+" => left.wrapping_add(right),
        "-" => left.wrapping_sub(right),
        "*" => left.wrapping_mul(right),
        "/" | "%" if right == 0 => return Err("division by zero".to_owned()),
        "/" => left.wrapping_div(right),
        "%" => left.wrapping_rem(right),
        // The shift count is taken modulo 64, as the processor does.
        "<<" => left.wrapping_shl(right as u32),
        ">>" => left.wrapping_shr(right as u32),
        "<" => i64::from(left < right),
        "<=" => i64::from(left <= right),
        ">" => i64::from(left > right),
        ">=" => i64::from(left >= right),
        "==" => i64::from(left == right),
        "!=" => i64::from(left != right),
        "&" => left & right,
        "^" => left ^ right,
        "|" => left | right,
        _ => return Err(format!("unknown operator `{op}`")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(expr: &str) -> Result<i64, String> {
        evaluate(expr.as_bytes(), &mut Variables::default())
    }

    fn ok(expr: &str) -> i64 {
        eval(expr).unwrap()
    }

    #[test]
    fn numbers_in_three_bases() {
        assert_eq!(ok("42"), 42);
        assert_eq!(ok("0x1F"), 31);
        assert_eq!(ok("0X10"), 16);
        assert_eq!(ok("010"), 8);
        assert_eq!(ok("0"), 0);
        assert!(eval("08").is_err());
        assert!(eval("0x").is_err());
        assert!(eval("12abc").is_err());
    }

    #[test]
    fn a_number_that_is_too_big_becomes_the_largest() {
        assert_eq!(ok("9223372036854775808"), i64::MAX);
    }

    #[test]
    fn operators_follow_c_priorities() {
        assert_eq!(ok("1 + 2 * 3"), 7);
        assert_eq!(ok("(1 + 2) * 3"), 9);
        assert_eq!(ok("10 - 2 - 3"), 5);
        assert_eq!(ok("100 / 10 / 5"), 2);
        assert_eq!(ok("7 % 4"), 3);
        assert_eq!(ok("1 << 3 + 1"), 16);
        assert_eq!(ok("6 & 3 | 8"), 10);
        assert_eq!(ok("6 ^ 3"), 5);
        assert_eq!(ok("1 < 2 == 1"), 1);
        assert_eq!(ok("3 >= 4"), 0);
        assert_eq!(ok("2 != 3"), 1);
    }

    #[test]
    fn unary_operators() {
        assert_eq!(ok("-5"), -5);
        assert_eq!(ok("- -5"), 5);
        assert_eq!(ok("+5"), 5);
        assert_eq!(ok("~0"), -1);
        assert_eq!(ok("!0"), 1);
        assert_eq!(ok("!7"), 0);
        // dash reads `--` as two minus signs.
        assert_eq!(ok("3--1"), 4);
    }

    #[test]
    fn logic_and_the_conditional_operator() {
        assert_eq!(ok("1 && 0"), 0);
        assert_eq!(ok("1 && 5"), 1);
        assert_eq!(ok("0 || 5"), 1);
        assert_eq!(ok("1 ? 2 : 3"), 2);
        assert_eq!(ok("0 ? 2 : 3"), 3);
        assert_eq!(ok("1 ? 0 ? 5 : 6 : 7"), 6);
        assert!(eval("1 ? 2").is_err());
    }

    #[test]
    fn a_branch_that_is_not_taken_has_no_effect() {
        assert_eq!(ok("0 && 1 / 0"), 0);
        assert_eq!(ok("1 || 1 / 0"), 1);
        assert_eq!(ok("1 ? 5 : 1 / 0"), 5);
        let mut vars = Variables::default();
        assert_eq!(evaluate(b"0 && (x = 5)", &mut vars), Ok(0));
        assert_eq!(vars.get("x"), None);
        assert_eq!(evaluate(b"1 ? 1 : (y = 5)", &mut vars), Ok(1));
        assert_eq!(vars.get("y"), None);
    }

    #[test]
    fn division_by_zero_is_an_error() {
        assert_eq!(eval("1 / 0"), Err("division by zero".to_owned()));
        assert_eq!(eval("1 % 0"), Err("division by zero".to_owned()));
    }

    #[test]
    fn arithmetic_wraps_around() {
        assert_eq!(ok("9223372036854775807 + 1"), i64::MIN);
        assert_eq!(ok("-9223372036854775807 - 1 - 1"), i64::MAX);
        assert_eq!(ok("(-9223372036854775807 - 1) / -1"), i64::MIN);
        assert_eq!(ok("(-9223372036854775807 - 1) % -1"), 0);
        assert_eq!(ok("1 << 65"), 2);
    }

    #[test]
    fn variables_and_assignment() {
        let mut vars = Variables::from_pairs([("a", "4"), ("hex", "0x10"), ("sp", " 7 ")]);
        assert_eq!(evaluate(b"a * 2", &mut vars), Ok(8));
        assert_eq!(evaluate(b"unset_one + 1", &mut vars), Ok(1));
        assert_eq!(evaluate(b"hex + 1", &mut vars), Ok(17));
        assert_eq!(evaluate(b"sp + 1", &mut vars), Ok(8));
        assert_eq!(evaluate(b"-hex", &mut vars), Ok(-16));
        assert_eq!(evaluate(b"x = 2", &mut vars), Ok(2));
        assert_eq!(vars.get("x"), Some(&b"2"[..]));
        assert_eq!(evaluate(b"x += 3", &mut vars), Ok(5));
        assert_eq!(evaluate(b"x <<= 2", &mut vars), Ok(20));
        assert_eq!(evaluate(b"x %= 7", &mut vars), Ok(6));
        assert_eq!(evaluate(b"x = y = 9", &mut vars), Ok(9));
        assert_eq!(vars.get("y"), Some(&b"9"[..]));
        // `==` is a comparison, not an assignment.
        assert_eq!(evaluate(b"x == 9", &mut vars), Ok(1));
    }

    #[test]
    fn a_variable_that_is_not_a_number_is_an_error() {
        let mut vars = Variables::from_pairs([("s", "abc"), ("e", "")]);
        assert_eq!(
            evaluate(b"s + 1", &mut vars),
            Err("Illegal number: abc".to_owned())
        );
        assert_eq!(evaluate(b"e + 1", &mut vars), Ok(1));
    }

    #[test]
    fn what_dash_does_not_support_is_an_error() {
        assert!(eval("").is_err());
        assert!(eval("1 +").is_err());
        assert!(eval("x++").is_err());
        assert!(eval("2 ** 3").is_err());
        assert!(eval("1, 2").is_err());
        assert!(eval("(1").is_err());
        assert!(eval("1 2").is_err());
        assert!(eval("1 $ 2").is_err());
        assert!(eval("3 = 4").is_err());
    }
}
