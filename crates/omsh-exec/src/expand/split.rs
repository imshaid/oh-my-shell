//! Field splitting: cutting the result of an expansion into words at `$IFS` characters.

use super::pattern::unit_at;
use super::{Field, Kind};

/// What `$IFS` means when it is not set.
pub(crate) const DEFAULT_IFS: &[u8] = b" \t\n";

/// One character of `$IFS`.
struct Delimiter<'a> {
    bytes: &'a [u8],
    /// True for space, tab and line ending, which are skipped at both ends and merge.
    whitespace: bool,
}

fn delimiters(ifs: &[u8]) -> Vec<Delimiter<'_>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < ifs.len() {
        let len = unit_at(&ifs[at..]).1;
        let bytes = &ifs[at..at + len];
        out.push(Delimiter {
            bytes,
            whitespace: matches!(bytes, b" " | b"\t" | b"\n"),
        });
        at += len;
    }
    out
}

/// Splits one field. Only characters that came from an expansion and were not quoted can
/// separate fields. An empty result is dropped unless a quoted part made it count (`""`).
pub(crate) fn split(field: Field, ifs: &[u8]) -> Vec<Field> {
    if ifs.is_empty() || !field.kinds.contains(&Kind::Expanded) {
        return if field.bytes.is_empty() && !field.forced {
            Vec::new()
        } else {
            vec![field]
        };
    }
    let delims = delimiters(ifs);
    let mut out = Vec::new();
    let mut current: Option<Field> = None;
    // True right after whitespace ended a field: a following non-whitespace delimiter then
    // belongs to the same separator (`a : b` with `IFS=' :'` is two fields, not three).
    let mut closed_by_space = false;
    let mut i = 0;
    while i < field.bytes.len() {
        if field.kinds[i] == Kind::Expanded
            && let Some(delim) = delims.iter().find(|d| {
                let end = i + d.bytes.len();
                end <= field.bytes.len()
                    && &field.bytes[i..end] == d.bytes
                    && field.kinds[i..end].iter().all(|k| *k == Kind::Expanded)
            })
        {
            i += delim.bytes.len();
            if delim.whitespace {
                if let Some(done) = current.take() {
                    out.push(done);
                    closed_by_space = true;
                }
            } else if closed_by_space {
                closed_by_space = false;
            } else {
                out.push(current.take().unwrap_or_else(empty_field));
            }
            continue;
        }
        let f = current.get_or_insert_with(Field::default);
        f.bytes.push(field.bytes[i]);
        f.kinds.push(field.kinds[i]);
        f.forced |= field.kinds[i] == Kind::Quoted;
        closed_by_space = false;
        i += 1;
    }
    out.extend(current);
    if out.is_empty() && field.forced {
        out.push(empty_field());
    }
    out
}

/// An empty field that is kept, because a delimiter or a quote made it.
fn empty_field() -> Field {
    Field {
        forced: true,
        ..Field::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expanded(text: &str) -> Field {
        Field {
            bytes: text.as_bytes().to_vec(),
            kinds: vec![Kind::Expanded; text.len()],
            forced: false,
        }
    }

    fn texts(fields: Vec<Field>) -> Vec<String> {
        fields
            .into_iter()
            .map(|f| String::from_utf8(f.bytes).unwrap())
            .collect()
    }

    fn s(text: &str, ifs: &str) -> Vec<String> {
        texts(split(expanded(text), ifs.as_bytes()))
    }

    #[test]
    fn whitespace_runs_make_one_separator() {
        assert_eq!(s("a b", " \t\n"), ["a", "b"]);
        assert_eq!(s("  a   b  ", " \t\n"), ["a", "b"]);
        assert_eq!(s("a\t\nb", " \t\n"), ["a", "b"]);
    }

    #[test]
    fn only_whitespace_gives_no_field() {
        assert!(s("   ", " \t\n").is_empty());
        assert!(s("", " \t\n").is_empty());
    }

    #[test]
    fn each_non_whitespace_delimiter_counts() {
        assert_eq!(s("a:b", ":"), ["a", "b"]);
        assert_eq!(s("a::b", ":"), ["a", "", "b"]);
        assert_eq!(s("a::b:", ":"), ["a", "", "b"]);
        assert_eq!(s(":a", ":"), ["", "a"]);
        assert_eq!(s(":", ":"), [""]);
    }

    #[test]
    fn whitespace_next_to_another_delimiter_merges_with_it() {
        assert_eq!(s("a : b", " :"), ["a", "b"]);
        assert_eq!(s("a :: b", " :"), ["a", "", "b"]);
        assert_eq!(s(" a:b ", " :"), ["a", "b"]);
    }

    #[test]
    fn an_empty_ifs_does_not_split() {
        assert_eq!(s("a b", ""), ["a b"]);
    }

    #[test]
    fn a_delimiter_can_be_more_than_one_byte() {
        assert_eq!(s("aংb", "ং"), ["a", "b"]);
    }

    #[test]
    fn quoted_and_literal_characters_never_split() {
        let mut field = expanded("a b");
        field.kinds[1] = Kind::Quoted;
        assert_eq!(texts(split(field, b" ")), ["a b"]);
        let literal = Field {
            bytes: b"a b".to_vec(),
            kinds: vec![Kind::Literal; 3],
            forced: false,
        };
        assert_eq!(texts(split(literal, b" ")), ["a b"]);
    }

    #[test]
    fn a_quoted_empty_string_keeps_the_field() {
        let field = Field {
            bytes: Vec::new(),
            kinds: Vec::new(),
            forced: true,
        };
        assert_eq!(texts(split(field, b" ")), [""]);
        let mut field = expanded("  ");
        field.forced = true;
        assert_eq!(texts(split(field, b" ")), [""]);
        // Without the quotes, nothing is left.
        assert!(texts(split(Field::default(), b" ")).is_empty());
    }

    #[test]
    fn text_around_a_quote_joins_the_first_and_last_pieces() {
        // `"x"$v"y"` with v=" a b ": the quoted text sticks to its neighbours.
        let mut field = Field::default();
        field.bytes.extend_from_slice(b"x a b y");
        field.kinds = vec![
            Kind::Quoted,
            Kind::Expanded,
            Kind::Expanded,
            Kind::Expanded,
            Kind::Expanded,
            Kind::Expanded,
            Kind::Quoted,
        ];
        field.forced = true;
        assert_eq!(texts(split(field, b" ")), ["x", "a", "b", "y"]);
    }
}
