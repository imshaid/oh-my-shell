//! Parameter expansion: `$x`, `${x}`, `${x:-word}`, `${#x}`, `${x%pattern}`, `$@` and so on.

use omsh_syntax::ast::{ParamOp, Parameter};

use super::pattern::{Pattern, boundaries, unit_at};
use super::split::DEFAULT_IFS;
use super::{Builder, Ctx, ExpandError, Expander, Kind, Result};

/// What a name stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Value {
    Unset,
    Scalar(Vec<u8>),
    /// `$@` or `$*`: the positional parameters. `joined` is true for `$*`.
    List {
        items: Vec<Vec<u8>>,
        joined: bool,
    },
}

impl Value {
    /// True if the value is not set, or is empty and `colon` asks to count that too.
    fn is_missing(&self, colon: bool) -> bool {
        match self {
            Self::Unset => true,
            Self::Scalar(v) => colon && v.is_empty(),
            Self::List { items, .. } => items.is_empty(),
        }
    }
}

/// A name that can be assigned to.
fn is_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Expander<'_> {
    fn value_of(&self, name: &str) -> Value {
        let shell = &*self.shell;
        match name {
            "@" | "*" => Value::List {
                items: shell.positional.clone(),
                joined: name == "*",
            },
            "#" => Value::Scalar(shell.positional.len().to_string().into_bytes()),
            "?" => Value::Scalar(shell.status.code().to_string().into_bytes()),
            "$" => Value::Scalar(std::process::id().to_string().into_bytes()),
            // There are no background jobs until S09.
            "!" => Value::Unset,
            "-" => Value::Scalar(shell.options.dollar_dash(shell.interactive).into_bytes()),
            "0" => Value::Scalar(shell.name.clone()),
            digits if digits.bytes().all(|b| b.is_ascii_digit()) && !digits.is_empty() => digits
                .parse::<usize>()
                .ok()
                .and_then(|n| shell.positional.get(n.checked_sub(1)?))
                .map_or(Value::Unset, |v| Value::Scalar(v.clone())),
            other => shell
                .vars
                .get(other)
                .map_or(Value::Unset, |v| Value::Scalar(v.to_vec())),
        }
    }

    pub(super) fn parameter(
        &mut self,
        parameter: &Parameter,
        builder: &mut Builder,
        ctx: Ctx,
    ) -> Result<()> {
        let name = parameter.name.as_str();
        let value = self.value_of(name);

        // Under `-u`, a variable or positional parameter that is not set is an error, except
        // where the operator says what to do about it (`-`, `=`, `?`, `+`).
        let handles_unset = matches!(
            parameter.op.map(|(op, _)| op),
            Some(
                ParamOp::ColonDash
                    | ParamOp::Dash
                    | ParamOp::ColonEquals
                    | ParamOp::Equals
                    | ParamOp::ColonQuestion
                    | ParamOp::Question
                    | ParamOp::ColonPlus
                    | ParamOp::Plus
            )
        );
        if self.shell.options.nounset
            && value == Value::Unset
            && !handles_unset
            && (is_variable_name(name) || name.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(ExpandError(format!("{name}: parameter not set")));
        }

        if parameter.length {
            let length = match &value {
                Value::Unset => 0,
                Value::Scalar(v) => boundaries(v).len() - 1,
                Value::List { items, .. } => items.len(),
            };
            builder.push(length.to_string().as_bytes(), ctx.result);
            return Ok(());
        }
        let Some((op, _)) = parameter.op else {
            self.push_value(value, builder, ctx);
            return Ok(());
        };
        let colon = matches!(
            op,
            ParamOp::ColonDash | ParamOp::ColonEquals | ParamOp::ColonQuestion | ParamOp::ColonPlus
        );
        let missing = value.is_missing(colon);
        match op {
            ParamOp::ColonDash | ParamOp::Dash => {
                if missing {
                    self.sequence(&parameter.arg, builder, ctx.argument())?;
                } else {
                    self.push_value(value, builder, ctx);
                }
            }
            ParamOp::ColonPlus | ParamOp::Plus => {
                if !missing {
                    self.sequence(&parameter.arg, builder, ctx.argument())?;
                } else if ctx.quoted {
                    // `"${x:+word}"` is still one (empty) word.
                    builder.push(&[], Kind::Quoted);
                }
            }
            ParamOp::ColonEquals | ParamOp::Equals => {
                if missing {
                    if !is_variable_name(name) {
                        return Err(ExpandError(format!("{name}: cannot assign in this way")));
                    }
                    let assigned = self.string(&parameter.arg, Ctx::ASSIGNMENT)?;
                    self.shell
                        .vars
                        .set(name, assigned.clone())
                        .map_err(|_| ExpandError(format!("{name}: is read only")))?;
                    builder.push(&assigned, ctx.result);
                } else {
                    self.push_value(value, builder, ctx);
                }
            }
            ParamOp::ColonQuestion | ParamOp::Question => {
                if missing {
                    let message = self.string(&parameter.arg, Ctx::QUOTED)?;
                    let message = if message.is_empty() {
                        let what = if colon && matches!(value, Value::Scalar(_)) {
                            "parameter null or not set"
                        } else {
                            "parameter not set"
                        };
                        what.to_owned()
                    } else {
                        String::from_utf8_lossy(&message).into_owned()
                    };
                    return Err(ExpandError(format!("{name}: {message}")));
                }
                self.push_value(value, builder, ctx);
            }
            ParamOp::Hash | ParamOp::DoubleHash | ParamOp::Percent | ParamOp::DoublePercent => {
                let pattern = self.pattern(&parameter.arg)?;
                let trimmed = match value {
                    Value::Unset => Value::Unset,
                    Value::Scalar(v) => Value::Scalar(trim(&pattern, op, &v)),
                    Value::List { items, joined } => Value::List {
                        items: items.iter().map(|v| trim(&pattern, op, v)).collect(),
                        joined,
                    },
                };
                self.push_value(trimmed, builder, ctx);
            }
            ParamOp::Unsupported => {
                return Err(ExpandError(format!("${{{name}...}}: not supported")));
            }
        }
        Ok(())
    }

    /// Adds a value to the word being built.
    fn push_value(&self, value: Value, builder: &mut Builder, ctx: Ctx) {
        match value {
            // Quoted, an unset parameter is still an empty word: `"$x"`.
            Value::Unset => builder.push(&[], ctx.result),
            Value::Scalar(v) => builder.push(&v, ctx.result),
            Value::List {
                items,
                joined: false,
            } => {
                // `$@` keeps its parameters apart, quoted or not. Unquoted, they are split
                // further afterwards, and empty ones vanish.
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        builder.break_field();
                    }
                    builder.push(item, ctx.result);
                }
            }
            Value::List {
                items,
                joined: true,
            } => {
                let separator = self.first_ifs_character();
                builder.push(&items.join(separator.as_slice()), ctx.result);
            }
        }
    }

    /// What goes between the parameters in `"$*"`: the first character of `$IFS`, a space if
    /// it is unset, nothing if it is empty.
    fn first_ifs_character(&self) -> Vec<u8> {
        let ifs = self.shell.vars.get("IFS").unwrap_or(DEFAULT_IFS);
        if ifs.is_empty() {
            return Vec::new();
        }
        ifs[..unit_at(ifs).1].to_vec()
    }
}

/// Cuts the part of `value` that the pattern matches off the start (`#`, `##`) or the end
/// (`%`, `%%`).
fn trim(pattern: &Pattern, op: ParamOp, value: &[u8]) -> Vec<u8> {
    match op {
        ParamOp::Hash => pattern.shortest_prefix(value).map(|n| value[n..].to_vec()),
        ParamOp::DoubleHash => pattern.longest_prefix(value).map(|n| value[n..].to_vec()),
        ParamOp::Percent => pattern.shortest_suffix(value).map(|n| value[..n].to_vec()),
        ParamOp::DoublePercent => pattern.longest_suffix(value).map(|n| value[..n].to_vec()),
        _ => None,
    }
    .unwrap_or_else(|| value.to_vec())
}
