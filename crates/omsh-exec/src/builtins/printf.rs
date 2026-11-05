//! `printf format [argument...]`.
//!
//! The format is a text with escapes (`\n`, `\101`) and conversions (`%s`, `%5.2f`). It is used
//! again and again while arguments are left over. Missing arguments count as an empty string or
//! zero. This follows dash: `\c` only means something inside `%b`, `%c` of nothing writes a
//! NUL byte, a number that is not completely read is used as far as it goes and gives status 1,
//! and a bad directive stops with status 2.
//!
//! Numbers are formatted here and not with the C library's `printf`, which would need `unsafe`.

use crate::exec::Flow;
use crate::io::Io;
use crate::shell::Shell;
use crate::status::ExitStatus;

/// The flags, width and precision of one conversion.
#[derive(Clone, Copy, Debug, Default)]
struct Spec {
    minus: bool,
    plus: bool,
    space: bool,
    alt: bool,
    zero: bool,
    width: usize,
    precision: Option<usize>,
}

/// What stopped the format early.
enum Stop {
    /// `\c` in `%b`: nothing more is written, and that is not an error.
    Quiet,
    /// A directive that is not valid.
    Directive(String),
}

struct Printer<'a> {
    out: Vec<u8>,
    args: &'a [Vec<u8>],
    next: usize,
    /// 1 if a number could not be read in full.
    status: u8,
    messages: Vec<String>,
}

/// `printf`.
pub(super) fn printf(shell: &Shell, args: &[Vec<u8>], io: &Io) -> Flow {
    let args = match args.first().map(Vec::as_slice) {
        Some(b"--") => &args[1..],
        _ => args,
    };
    let Some((format, operands)) = args.split_first() else {
        io.say("oh-my-shell: printf: usage: printf format [arg ...]");
        return Ok(ExitStatus::USAGE);
    };
    let mut printer = Printer {
        out: Vec::new(),
        args: operands,
        next: 0,
        status: 0,
        messages: Vec::new(),
    };
    let stop = printer.run(format);
    let flow = shell.emit(io, 1, &printer.out);
    for message in &printer.messages {
        io.say(format!("oh-my-shell: printf: {message}"));
    }
    flow?;
    match stop {
        Some(Stop::Directive(text)) => {
            io.say(format!("oh-my-shell: printf: {text}: invalid directive"));
            Ok(ExitStatus::USAGE)
        }
        _ => Ok(ExitStatus(printer.status)),
    }
}

impl Printer<'_> {
    /// Runs the format over and over until the arguments are used up.
    fn run(&mut self, format: &[u8]) -> Option<Stop> {
        loop {
            let before = self.next;
            if let Some(stop) = self.pass(format) {
                return match stop {
                    Stop::Quiet => None,
                    other => Some(other),
                };
            }
            if self.next >= self.args.len() || self.next == before {
                return None;
            }
        }
    }

    fn argument(&mut self) -> Option<&[u8]> {
        let arg = self.args.get(self.next)?;
        self.next += 1;
        Some(arg)
    }

    /// One walk through the format.
    fn pass(&mut self, format: &[u8]) -> Option<Stop> {
        let mut i = 0;
        while i < format.len() {
            match format[i] {
                b'\\' => {
                    i += self.format_escape(&format[i..]);
                }
                b'%' => match self.conversion(format, i + 1) {
                    Ok(end) => i = end,
                    Err(stop) => return Some(stop),
                },
                byte => {
                    self.out.push(byte);
                    i += 1;
                }
            }
        }
        None
    }

    /// An escape in the format. `text` starts at the backslash. Returns how many bytes it used.
    fn format_escape(&mut self, text: &[u8]) -> usize {
        match escape(text, false) {
            Escape::Byte(byte, used) => {
                self.out.push(byte);
                used
            }
            // Not an escape: the backslash is an ordinary character.
            Escape::Literal | Escape::Stop => {
                self.out.push(b'\\');
                1
            }
        }
    }

    /// A conversion; `at` is the position after the `%`. Returns the position after it.
    fn conversion(&mut self, format: &[u8], mut at: usize) -> Result<usize, Stop> {
        let start = at;
        let mut spec = Spec::default();
        while let Some(&flag) = format.get(at) {
            match flag {
                b'-' => spec.minus = true,
                b'+' => spec.plus = true,
                b' ' => spec.space = true,
                b'#' => spec.alt = true,
                b'0' => spec.zero = true,
                _ => break,
            }
            at += 1;
        }
        if format.get(at) == Some(&b'*') {
            let width = self.number_argument();
            if width < 0 {
                spec.minus = true;
            }
            spec.width = usize::try_from(width.unsigned_abs()).unwrap_or(usize::MAX);
            at += 1;
        } else {
            let (value, end) = digits(format, at);
            spec.width = value.unwrap_or(0);
            at = end;
        }
        if format.get(at) == Some(&b'.') {
            at += 1;
            if format.get(at) == Some(&b'*') {
                let precision = self.number_argument();
                // A negative precision is the same as none.
                spec.precision = usize::try_from(precision).ok();
                at += 1;
            } else {
                let (value, end) = digits(format, at);
                spec.precision = Some(value.unwrap_or(0));
                at = end;
            }
        }
        let Some(&kind) = format.get(at) else {
            return Err(Stop::Directive(describe(&format[start - 1..])));
        };
        at += 1;
        match kind {
            b'%' if at == start + 1 => self.out.push(b'%'),
            b's' => {
                let arg = self.argument().unwrap_or_default().to_vec();
                self.text(&spec, &arg);
            }
            b'b' => {
                let arg = self.argument().unwrap_or_default().to_vec();
                let (text, stopped) = expand_b(&arg);
                self.text(&spec, &text);
                if stopped {
                    return Err(Stop::Quiet);
                }
            }
            b'c' => {
                let arg = self.argument().unwrap_or_default().to_vec();
                let first = first_character(&arg);
                self.text(
                    &Spec {
                        precision: None,
                        ..spec
                    },
                    first,
                );
            }
            b'd' | b'i' => {
                let value = self.number_argument();
                self.signed(&spec, value);
            }
            b'o' | b'u' | b'x' | b'X' => {
                // A negative number is shown as the 64-bit number with the same bits.
                let value = self.number_argument().cast_unsigned();
                self.unsigned(&spec, kind, value);
            }
            b'e' | b'E' | b'f' | b'F' | b'g' | b'G' => {
                let value = self.float_argument();
                self.float(&spec, kind, value);
            }
            _ => {
                return Err(Stop::Directive(describe(&format[start - 1..at])));
            }
        }
        Ok(at)
    }

    /// The next argument as an integer. A missing one is 0.
    fn number_argument(&mut self) -> i64 {
        let Some(arg) = self.argument().map(<[u8]>::to_vec) else {
            return 0;
        };
        let (value, problem) = parse_integer(&arg);
        if let Some(problem) = problem {
            self.problem(&arg, problem);
        }
        value
    }

    fn float_argument(&mut self) -> f64 {
        let Some(arg) = self.argument().map(<[u8]>::to_vec) else {
            return 0.0;
        };
        let (value, problem) = parse_float(&arg);
        if let Some(problem) = problem {
            self.problem(&arg, problem);
        }
        value
    }

    fn problem(&mut self, arg: &[u8], problem: Problem) {
        self.status = 1;
        let what = match problem {
            Problem::NotANumber => "expected numeric value",
            Problem::Partial => "not completely converted",
            Problem::Range => "Numerical result out of range",
        };
        self.messages
            .push(format!("{}: {what}", String::from_utf8_lossy(arg)));
    }

    /// Writes `body` padded to the width. Used for text, where `0` does not mean zero fill.
    fn text(&mut self, spec: &Spec, body: &[u8]) {
        let body = match spec.precision {
            Some(limit) if limit < body.len() => &body[..char_boundary(body, limit)],
            _ => body,
        };
        let fill = spec.width.saturating_sub(body.len());
        if spec.minus {
            self.out.extend_from_slice(body);
            self.out.extend(std::iter::repeat_n(b' ', fill));
        } else {
            self.out.extend(std::iter::repeat_n(b' ', fill));
            self.out.extend_from_slice(body);
        }
    }

    /// Writes a number: a sign or prefix, then digits, padded to the width. `zero_ok` says
    /// whether the `0` flag may fill with zeros.
    fn number(&mut self, spec: &Spec, prefix: &[u8], digits: &[u8], zero_ok: bool) {
        let length = prefix.len() + digits.len();
        let fill = spec.width.saturating_sub(length);
        if spec.minus {
            self.out.extend_from_slice(prefix);
            self.out.extend_from_slice(digits);
            self.out.extend(std::iter::repeat_n(b' ', fill));
        } else if spec.zero && zero_ok {
            self.out.extend_from_slice(prefix);
            self.out.extend(std::iter::repeat_n(b'0', fill));
            self.out.extend_from_slice(digits);
        } else {
            self.out.extend(std::iter::repeat_n(b' ', fill));
            self.out.extend_from_slice(prefix);
            self.out.extend_from_slice(digits);
        }
    }

    fn signed(&mut self, spec: &Spec, value: i64) {
        let prefix: &[u8] = if value < 0 {
            b"-"
        } else if spec.plus {
            b"+"
        } else if spec.space {
            b" "
        } else {
            b""
        };
        let digits = with_precision(value.unsigned_abs().to_string(), spec.precision, value == 0);
        self.number(spec, prefix, digits.as_bytes(), spec.precision.is_none());
    }

    fn unsigned(&mut self, spec: &Spec, kind: u8, value: u64) {
        let text = match kind {
            b'o' => format!("{value:o}"),
            b'x' => format!("{value:x}"),
            b'X' => format!("{value:X}"),
            _ => value.to_string(),
        };
        let mut digits = with_precision(text, spec.precision, value == 0);
        let mut prefix: &[u8] = b"";
        if spec.alt {
            match kind {
                b'o' if !digits.starts_with('0') => digits.insert(0, '0'),
                b'x' if value != 0 => prefix = b"0x",
                b'X' if value != 0 => prefix = b"0X",
                _ => {}
            }
        }
        self.number(spec, prefix, digits.as_bytes(), spec.precision.is_none());
    }

    fn float(&mut self, spec: &Spec, kind: u8, value: f64) {
        let prefix: &[u8] = if value.is_sign_negative() && !value.is_nan() {
            b"-"
        } else if spec.plus {
            b"+"
        } else if spec.space {
            b" "
        } else {
            b""
        };
        let magnitude = value.abs();
        let finite = magnitude.is_finite();
        let mut text = if !finite {
            if magnitude.is_nan() { "nan" } else { "inf" }.to_owned()
        } else {
            format_float(kind.to_ascii_lowercase(), magnitude, spec)
        };
        if kind.is_ascii_uppercase() {
            text.make_ascii_uppercase();
        }
        self.number(spec, prefix, text.as_bytes(), finite);
    }
}

/// `%` and the characters of a directive, as an error message shows them.
fn describe(text: &[u8]) -> String {
    String::from_utf8_lossy(text).into_owned()
}

/// Reads decimal digits at `at`. Returns the number, if there were digits, and the end.
fn digits(text: &[u8], at: usize) -> (Option<usize>, usize) {
    let mut end = at;
    let mut value: usize = 0;
    while let Some(d) = text.get(end).filter(|b| b.is_ascii_digit()) {
        value = value
            .saturating_mul(10)
            .saturating_add(usize::from(d - b'0'));
        end += 1;
    }
    ((end > at).then_some(value), end)
}

/// Pads the digits of an integer with zeros to the precision. A precision of 0 shows nothing
/// for the value 0.
fn with_precision(digits: String, precision: Option<usize>, is_zero: bool) -> String {
    match precision {
        Some(0) if is_zero => String::new(),
        Some(minimum) if digits.len() < minimum => {
            format!("{}{digits}", "0".repeat(minimum - digits.len()))
        }
        _ => digits,
    }
}

/// The largest prefix of `body` that is at most `limit` bytes and does not cut a UTF-8
/// character in two.
fn char_boundary(body: &[u8], limit: usize) -> usize {
    let mut end = limit.min(body.len());
    // A continuation byte is 10xxxxxx.
    while end > 0 && end < body.len() && body[end] & 0xC0 == 0x80 {
        end -= 1;
    }
    end
}

/// The first character of `arg`, or one NUL byte for an empty argument, like dash.
fn first_character(arg: &[u8]) -> &[u8] {
    const NUL: &[u8] = &[0];
    if arg.is_empty() {
        return NUL;
    }
    let len = match arg[0] {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    };
    let end = len.min(arg.len());
    if std::str::from_utf8(&arg[..end]).is_ok() {
        &arg[..end]
    } else {
        &arg[..1]
    }
}

/// What an escape turned out to be.
enum Escape {
    /// A byte, and how many bytes of the text it used (the backslash included).
    Byte(u8, usize),
    /// `\c` in `%b`.
    Stop,
    /// Not an escape: the backslash stands for itself.
    Literal,
}

/// Reads the escape at the start of `text` (which begins with a backslash). `in_b` is true for
/// the argument of `%b`, where `\c` stops the output and `\0` takes up to three more digits.
fn escape(text: &[u8], in_b: bool) -> Escape {
    let Some(&c) = text.get(1) else {
        return Escape::Literal;
    };
    let simple = match c {
        b'a' => Some(7),
        b'b' => Some(8),
        b'f' => Some(12),
        b'n' => Some(b'\n'),
        b'r' => Some(b'\r'),
        b't' => Some(b'\t'),
        b'v' => Some(11),
        b'e' => Some(27),
        b'\\' => Some(b'\\'),
        _ => None,
    };
    if let Some(byte) = simple {
        return Escape::Byte(byte, 2);
    }
    match c {
        b'c' if in_b => Escape::Stop,
        b'0'..=b'7' => {
            // `\0NNN` in `%b` has the zero in front of up to three digits.
            let first = if in_b && c == b'0' { 2 } else { 1 };
            let mut value: u32 = 0;
            let mut end = first;
            while end < first + 3
                && let Some(d) = text.get(end).filter(|b| (b'0'..=b'7').contains(b))
            {
                value = value * 8 + u32::from(d - b'0');
                end += 1;
            }
            // Only the low eight bits fit in a byte.
            Escape::Byte((value & 0xff) as u8, end)
        }
        _ => Escape::Literal,
    }
}

/// The text of a `%b` argument with its escapes replaced. The flag is true if `\c` was found,
/// which ends all output.
fn expand_b(arg: &[u8]) -> (Vec<u8>, bool) {
    let mut out = Vec::with_capacity(arg.len());
    let mut i = 0;
    while i < arg.len() {
        if arg[i] == b'\\' {
            match escape(&arg[i..], true) {
                Escape::Byte(byte, used) => {
                    out.push(byte);
                    i += used;
                }
                Escape::Stop => return (out, true),
                Escape::Literal => {
                    out.push(b'\\');
                    i += 1;
                }
            }
        } else {
            out.push(arg[i]);
            i += 1;
        }
    }
    (out, false)
}

/// What was wrong with a number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Problem {
    NotANumber,
    Partial,
    Range,
}

/// Reads an integer the way `strtoimax` does with base 0: a sign, then decimal, `0x` hex or
/// `0` octal digits. An argument that starts with `'` or `"` is the code of the character
/// after it. Returns the value that is used even when there is a problem.
fn parse_integer(arg: &[u8]) -> (i64, Option<Problem>) {
    if let [b'\'' | b'"', rest @ ..] = arg
        && !rest.is_empty()
    {
        let first = first_character(rest);
        let code = std::str::from_utf8(first)
            .ok()
            .and_then(|s| s.chars().next())
            .map_or_else(|| i64::from(first[0]), |c| i64::from(u32::from(c)));
        return (code, None);
    }
    let text = arg.trim_ascii_start();
    if text.is_empty() {
        return (0, None);
    }
    let (negative, body) = match text[0] {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let (radix, digits_at) = match body {
        [b'0', b'x' | b'X', d, ..] if d.is_ascii_hexdigit() => (16, 2),
        [b'0', ..] => (8, 0),
        _ => (10, 0),
    };
    let digits: Vec<u8> = body[digits_at..]
        .iter()
        .copied()
        .take_while(|b| char::from(*b).is_digit(radix))
        .collect();
    if digits.is_empty() {
        return (0, Some(Problem::NotANumber));
    }
    let rest = &body[digits_at + digits.len()..];
    let magnitude = std::str::from_utf8(&digits)
        .ok()
        .and_then(|d| u128::from_str_radix(d, radix).ok());
    let limit = if negative {
        u128::from(i64::MIN.unsigned_abs())
    } else {
        u128::from(i64::MAX.unsigned_abs())
    };
    let (value, mut problem) = match magnitude {
        Some(m) if m <= limit => {
            let m = i128::try_from(m).unwrap_or(0);
            (
                i64::try_from(if negative { -m } else { m }).unwrap_or(0),
                None,
            )
        }
        _ => (
            if negative { i64::MIN } else { i64::MAX },
            Some(Problem::Range),
        ),
    };
    if problem.is_none() && !rest.is_empty() {
        problem = Some(Problem::Partial);
    }
    (value, problem)
}

/// Reads a floating-point number the way `strtod` does: decimal with an optional exponent,
/// `0x` hexadecimal integers, `inf` and `nan`.
fn parse_float(arg: &[u8]) -> (f64, Option<Problem>) {
    let text = arg.trim_ascii_start();
    if text.is_empty() {
        return (0.0, None);
    }
    let (negative, body) = match text[0] {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let apply = |v: f64| if negative { -v } else { v };
    let lower = body.to_ascii_lowercase();
    for (word, value) in [
        (&b"infinity"[..], f64::INFINITY),
        (b"inf", f64::INFINITY),
        (b"nan", f64::NAN),
    ] {
        if lower.starts_with(word) {
            let rest = &body[word.len()..];
            return (apply(value), (!rest.is_empty()).then_some(Problem::Partial));
        }
    }
    if let [b'0', b'x' | b'X', d, ..] = body
        && d.is_ascii_hexdigit()
    {
        let digits: Vec<u8> = body[2..]
            .iter()
            .copied()
            .take_while(u8::is_ascii_hexdigit)
            .collect();
        let value = std::str::from_utf8(&digits)
            .ok()
            .and_then(|d| u128::from_str_radix(d, 16).ok())
            .map_or(f64::INFINITY, |v| v as f64);
        let rest = &body[2 + digits.len()..];
        return (apply(value), (!rest.is_empty()).then_some(Problem::Partial));
    }
    let end = decimal_end(body);
    let Some(number) = std::str::from_utf8(&body[..end])
        .ok()
        .and_then(|s| s.parse::<f64>().ok())
    else {
        return (0.0, Some(Problem::NotANumber));
    };
    let problem = if end < body.len() {
        Some(Problem::Partial)
    } else if number.is_infinite() {
        Some(Problem::Range)
    } else {
        None
    };
    (apply(number), problem)
}

/// The length of the decimal number at the start of `text`: digits, a point, digits and an
/// exponent. Zero if there is none.
fn decimal_end(text: &[u8]) -> usize {
    let mut i = 0;
    let mut seen_digit = false;
    while text.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
        seen_digit = true;
    }
    if text.get(i) == Some(&b'.') {
        i += 1;
        while text.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
            seen_digit = true;
        }
    }
    if !seen_digit {
        return 0;
    }
    if matches!(text.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        if matches!(text.get(j), Some(b'+' | b'-')) {
            j += 1;
        }
        if text.get(j).is_some_and(u8::is_ascii_digit) {
            while text.get(j).is_some_and(u8::is_ascii_digit) {
                j += 1;
            }
            i = j;
        }
    }
    i
}

/// A finite, non-negative number in the style of `%f`, `%e` or `%g` (`kind` is lower case).
fn format_float(kind: u8, value: f64, spec: &Spec) -> String {
    let precision = spec.precision.unwrap_or(6);
    match kind {
        b'f' => fixed(value, precision, spec.alt),
        b'e' => exponent(value, precision, spec.alt),
        _ => general(value, precision, spec.alt),
    }
}

fn fixed(value: f64, precision: usize, alt: bool) -> String {
    let mut text = format!("{value:.precision$}");
    if alt && precision == 0 {
        text.push('.');
    }
    text
}

/// `1.5e2` (Rust) to `1.500000e+02` (C).
fn exponent(value: f64, precision: usize, alt: bool) -> String {
    let text = format!("{value:.precision$e}");
    let (mantissa, power) = text.split_once('e').unwrap_or((&text, "0"));
    let power: i32 = power.parse().unwrap_or(0);
    let mut out = mantissa.to_owned();
    if alt && precision == 0 {
        out.push('.');
    }
    out.push('e');
    out.push(if power < 0 { '-' } else { '+' });
    out.push_str(&format!("{:02}", power.unsigned_abs()));
    out
}

/// `%g`: the shorter of `%e` and `%f` for the given number of significant digits, without
/// trailing zeros unless `alt`.
fn general(value: f64, precision: usize, alt: bool) -> String {
    let significant = precision.max(1);
    // The exponent after rounding to the significant digits decides the style.
    let probe = format!("{value:.p$e}", p = significant - 1);
    let power: i32 = probe
        .split_once('e')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(0);
    let significant_i = i32::try_from(significant).unwrap_or(i32::MAX);
    let text = if (-4..significant_i).contains(&power) {
        let decimals = usize::try_from(significant_i - 1 - power).unwrap_or(0);
        fixed(value, decimals, alt)
    } else {
        exponent(value, significant - 1, alt)
    };
    if alt { text } else { strip_zeros(&text) }
}

/// Removes trailing zeros after the decimal point, and the point if nothing is left, from the
/// mantissa of `text`.
fn strip_zeros(text: &str) -> String {
    let (mantissa, rest) = match text.split_once('e') {
        Some((m, r)) => (m, format!("e{r}")),
        None => (text, String::new()),
    };
    let mantissa = if mantissa.contains('.') {
        mantissa.trim_end_matches('0').trim_end_matches('.')
    } else {
        mantissa
    };
    format!("{mantissa}{rest}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::capture::Captured;
    use crate::vars::Variables;

    fn run(args: &[&str]) -> (String, String, u8) {
        let shell = Shell::with_variables(Variables::default()).subshell();
        let captured = Captured::new();
        let args: Vec<Vec<u8>> = args.iter().map(|a| a.as_bytes().to_vec()).collect();
        let status = printf(&shell, &args, &captured.io).unwrap().code();
        (captured.stdout(), captured.stderr(), status)
    }

    fn out(args: &[&str]) -> String {
        run(args).0
    }

    #[test]
    fn text_and_padding() {
        assert_eq!(
            out(&[
                "[%s][%5s][%-5s][%.2s][%05s]",
                "abc",
                "abc",
                "abc",
                "abc",
                "abc"
            ]),
            "[abc][  abc][abc  ][ab][  abc]"
        );
        assert_eq!(out(&["%c%c", "xyz", ""]), "x\0");
        assert_eq!(out(&["%5c|", "z"]), "    z|");
    }

    #[test]
    fn precision_never_cuts_a_character_in_two() {
        assert_eq!(out(&["%.4s|", "বাংলা"]), "ব|");
        assert_eq!(out(&["%.6s|", "বাংলা"]), "বা|");
    }

    #[test]
    fn integers() {
        assert_eq!(
            out(&[
                "[%d][%5d][%-5d][%05d][%+d][% d][%.3d][%5.3d]",
                "42",
                "42",
                "42",
                "42",
                "42",
                "42",
                "7",
                "7"
            ]),
            "[42][   42][42   ][00042][+42][ 42][007][  007]"
        );
        assert_eq!(
            out(&["%x %#x %#o %X %u", "255", "255", "8", "255", "-1"]),
            "ff 0xff 010 FF 18446744073709551615"
        );
        assert_eq!(
            out(&["%.0d|%5.0d|%#x|%#o", "0", "0", "0", "0"]),
            "|     |0|0"
        );
        assert_eq!(out(&["%d %d %d", "0x1f", "010", "\"a"]), "31 8 97");
    }

    #[test]
    fn floats() {
        assert_eq!(
            out(&[
                "%f|%.2f|%e|%E|%g|%G|%10.3f|%-10.1f|%+f|%010.2f",
                "3.14159",
                "3.14159",
                "31415.9",
                "0.00031",
                "100000",
                "1e-10",
                "2.5",
                "2.5",
                "1",
                "-2.5"
            ]),
            "3.141590|3.14|3.141590e+04|3.100000E-04|100000|1E-10|     2.500|2.5       |+1.000000|-000002.50"
        );
        assert_eq!(
            out(&[
                "%g %g %g %g %g %#g",
                "0",
                "100000",
                "1000000",
                "0.0001",
                "0.00001",
                "1"
            ]),
            "0 100000 1e+06 0.0001 1e-05 1.00000"
        );
        assert_eq!(
            out(&["%.0f %.0f %.0f %.0f", "0.5", "1.5", "2.5", "3.5"]),
            "0 2 2 4"
        );
        assert_eq!(out(&["%f %f", "inf", "nan"]), "inf nan");
        assert_eq!(out(&["%010f", "inf"]), "       inf");
    }

    #[test]
    fn the_format_is_used_again_while_arguments_are_left() {
        assert_eq!(
            out(&["a%sb%sc\n", "1", "2", "3", "4", "5"]),
            "a1b2c\na3b4c\na5bc\n"
        );
        assert_eq!(out(&["%s\n"]), "\n");
        assert_eq!(out(&["x\n", "a", "b"]), "x\n");
        assert_eq!(
            out(&["%*d|%-*d|%.*f", "5", "1", "5", "2", "2", "3.14159"]),
            "    1|2    |3.14"
        );
    }

    #[test]
    fn escapes_in_the_format() {
        assert_eq!(
            out(&["a\\tb\\n\\101\\0101\\q\\e\\\\"]),
            "a\tb\nA\u{8}1\\q\u{1b}\\"
        );
        assert_eq!(out(&["[\\c]"]), "[\\c]");
        assert_eq!(out(&["\\x41"]), "\\x41");
    }

    #[test]
    fn b_expands_escapes_and_c_stops_everything() {
        assert_eq!(out(&["%b|", "a\\tb", "x\\0101y", "\\101"]), "a\tb|xAy|A|");
        assert_eq!(out(&["%b|%s", "a\\cb", "later"]), "a");
    }

    #[test]
    fn a_number_that_is_not_complete_is_used_as_far_as_it_goes() {
        let (stdout, stderr, status) = run(&["%d\n", "12abc"]);
        assert_eq!(stdout, "12\n");
        assert!(
            stderr.contains("12abc: not completely converted"),
            "{stderr}"
        );
        assert_eq!(status, 1);
        let (stdout, stderr, status) = run(&["%d\n", "abc"]);
        assert_eq!(stdout, "0\n");
        assert!(stderr.contains("expected numeric value"), "{stderr}");
        assert_eq!(status, 1);
        let (stdout, _, status) = run(&["%d\n", "99999999999999999999"]);
        assert_eq!(stdout, "9223372036854775807\n");
        assert_eq!(status, 1);
        assert_eq!(run(&["%d", ""]).2, 0);
        assert_eq!(run(&["%d", " 12"]).0, "12");
    }

    #[test]
    fn a_bad_directive_stops_with_2_after_the_text_before_it() {
        let (stdout, stderr, status) = run(&["ab%z\n", "1"]);
        assert_eq!(stdout, "ab");
        assert!(stderr.contains("%z: invalid directive"), "{stderr}");
        assert_eq!(status, 2);
        assert_eq!(run(&["%5%"]).2, 2);
        assert_eq!(run(&["%ld", "1"]).2, 2);
    }

    #[test]
    fn no_arguments_is_a_usage_error() {
        let (stdout, stderr, status) = run(&[]);
        assert_eq!(stdout, "");
        assert!(stderr.contains("usage"), "{stderr}");
        assert_eq!(status, 2);
        assert_eq!(out(&["--", "-x\n"]), "-x\n");
    }
}
