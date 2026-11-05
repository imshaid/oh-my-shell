//! Decoding the text of `$'...'`.

/// Turns the text between the quotes of `$'...'` into the bytes it stands for.
///
/// Follows bash: `\n`, `\t` and the other single-letter escapes, `\nnn` (octal), `\xHH`,
/// `\uHHHH`, `\UHHHHHHHH` and `\cX`. A byte of value 0 ends the string, because a C string
/// cannot hold it. A backslash before any other character stays as it is.
pub(crate) fn decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 == bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        let escape = bytes[i + 1];
        i += 2;
        let single = match escape {
            b'a' => Some(0x07),
            b'b' => Some(0x08),
            b'e' | b'E' => Some(0x1b),
            b'f' => Some(0x0c),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            b'v' => Some(0x0b),
            b'\\' | b'\'' | b'"' | b'?' => Some(escape),
            _ => None,
        };
        if let Some(byte) = single {
            out.push(byte);
            continue;
        }
        match escape {
            b'0'..=b'7' => {
                let (value, used) = number(&bytes[i - 1..], 8, 3);
                i += used - 1;
                // Only the low 8 bits count, as in bash.
                let byte = (value & 0xff) as u8;
                if byte == 0 {
                    return out;
                }
                out.push(byte);
            }
            b'x' => {
                let (value, used) = number(&bytes[i..], 16, 2);
                if used == 0 {
                    out.extend_from_slice(b"\\x");
                    continue;
                }
                i += used;
                let byte = (value & 0xff) as u8;
                if byte == 0 {
                    return out;
                }
                out.push(byte);
            }
            b'u' | b'U' => {
                let max = if escape == b'u' { 4 } else { 8 };
                let (value, used) = number(&bytes[i..], 16, max);
                if used == 0 {
                    out.push(b'\\');
                    out.push(escape);
                    continue;
                }
                i += used;
                if value == 0 {
                    return out;
                }
                match char::from_u32(value) {
                    Some(c) => {
                        let mut buf = [0_u8; 4];
                        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    }
                    // A surrogate or a number past U+10FFFF is not a character.
                    None => out.extend_from_slice(&bytes[i - used - 2..i]),
                }
            }
            b'c' if i < bytes.len() => {
                let target = bytes[i];
                i += 1;
                let byte = if target == b'?' { 0x7f } else { target & 0x1f };
                if byte == 0 {
                    return out;
                }
                out.push(byte);
            }
            other => {
                out.push(b'\\');
                out.push(other);
            }
        }
    }
    out
}

/// Reads up to `max` digits of `radix` from the start of `bytes`. Returns the value and how
/// many digits were read.
fn number(bytes: &[u8], radix: u32, max: usize) -> (u32, usize) {
    let mut value = 0_u32;
    let mut used = 0;
    for &b in bytes.iter().take(max) {
        let Some(digit) = char::from(b).to_digit(radix) else {
            break;
        };
        value = value.wrapping_mul(radix).wrapping_add(digit);
        used += 1;
    }
    (value, used)
}

#[cfg(test)]
mod tests {
    use super::decode;

    fn d(text: &str) -> Vec<u8> {
        decode(text)
    }

    #[test]
    fn plain_text_is_unchanged() {
        assert_eq!(d("abc বাংলা"), "abc বাংলা".as_bytes());
        assert_eq!(d(""), b"");
    }

    #[test]
    fn single_letter_escapes() {
        assert_eq!(d(r"a\nb\tc"), b"a\nb\tc");
        assert_eq!(d(r"\a\b\e\E\f\r\v"), b"\x07\x08\x1b\x1b\x0c\r\x0b");
        assert_eq!(d(r#"\\ \' \" \?"#), br#"\ ' " ?"#);
    }

    #[test]
    fn octal_escapes_take_up_to_three_digits() {
        assert_eq!(d(r"\101"), b"A");
        assert_eq!(d(r"\1011"), b"A1");
        assert_eq!(d(r"\7"), b"\x07");
        assert_eq!(d(r"\377"), b"\xff");
        // Only the low 8 bits count.
        assert_eq!(d(r"\777"), b"\xff");
    }

    #[test]
    fn hex_escapes_take_up_to_two_digits() {
        assert_eq!(d(r"\x41"), b"A");
        assert_eq!(d(r"\x414"), b"A4");
        assert_eq!(d(r"\x4"), b"\x04");
        assert_eq!(d(r"\xff"), b"\xff");
        assert_eq!(d(r"\xg"), b"\\xg");
    }

    #[test]
    fn unicode_escapes_make_utf8() {
        assert_eq!(d(r"অ"), "অ".as_bytes());
        assert_eq!(d(r"\U0001F600"), "😀".as_bytes());
        assert_eq!(d(r"\u41"), b"A");
        assert_eq!(d(r"\ug"), b"\\ug");
        // A surrogate is not a character: it stays as written.
        assert_eq!(d(r"\ud800"), br"\ud800");
    }

    #[test]
    fn control_escapes() {
        assert_eq!(d(r"\cA"), b"\x01");
        assert_eq!(d(r"\ca"), b"\x01");
        assert_eq!(d(r"\c?"), b"\x7f");
    }

    #[test]
    fn a_zero_byte_ends_the_string() {
        assert_eq!(d(r"ab\0cd"), b"ab");
        assert_eq!(d(r"ab\x00cd"), b"ab");
        assert_eq!(d(r"ab\u0000cd"), b"ab");
    }

    #[test]
    fn an_unknown_escape_and_a_final_backslash_stay() {
        assert_eq!(d(r"\q"), b"\\q");
        assert_eq!(d("a\\"), b"a\\");
    }
}
