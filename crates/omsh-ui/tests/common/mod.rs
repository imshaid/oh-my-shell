//! Helpers shared by the integration tests of `omsh-ui`.
#![allow(dead_code, reason = "each test file uses some of the helpers")]

/// Every SGR parameter written in `text`: the numbers between `ESC [` and `m`, split at `;`.
/// A sequence that is not an SGR sequence (does not end in `m`) is returned as an error, because
/// the panels must not write anything else.
pub fn sgr_parameters(text: &str) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            continue;
        }
        if chars.next() != Some('[') {
            return Err(format!(
                "an escape that does not start a CSI sequence in {text:?}"
            ));
        }
        let mut params = String::new();
        loop {
            match chars.next() {
                Some('m') => break,
                Some(c) if c.is_ascii_digit() || c == ';' => params.push(c),
                other => {
                    return Err(format!(
                        "a sequence that is not SGR ({other:?}) in {text:?}"
                    ));
                }
            }
        }
        if params.is_empty() {
            found.push("0".to_owned());
        } else {
            found.extend(params.split(';').map(str::to_owned));
        }
    }
    Ok(found)
}

/// Fails unless every SGR parameter in `text` is one the 16-color palette needs: reset (0), dim
/// (2), underline (4), reverse (7), and the colors 30 to 37 and 90 to 97. No 38 or 48 (extended
/// colors), no 58, no bold, no background.
pub fn assert_palette_only(text: &str) {
    let parameters = sgr_parameters(text).unwrap_or_else(|why| panic!("{why}"));
    for parameter in parameters {
        let number: u32 = parameter
            .parse()
            .unwrap_or_else(|_| panic!("SGR parameter {parameter:?} in {text:?}"));
        let allowed = matches!(number, 0 | 2 | 4 | 7 | 30..=37 | 90..=97);
        assert!(
            allowed,
            "SGR parameter {number} is not from the 16-color palette: {text:?}"
        );
    }
}

/// Fails if text written while the faint attribute (SGR 2) is on holds a letter or a digit.
/// Faint is for decoration (a rule, a separator, the empty part of a bar). Text that carries
/// information is drawn at full weight, because faint text is hard to read on some terminal
/// themes. Faint is on from a sequence with parameter 2 until a reset (0 or an empty list).
pub fn assert_faint_is_decoration(text: &str) {
    let mut faint = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut params = String::new();
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
                params.push(next);
            }
            let list: Vec<&str> = params.split(';').collect();
            if params.is_empty() || list.contains(&"0") {
                faint = false;
            }
            if list.contains(&"2") {
                faint = true;
            }
        } else if faint && c.is_alphanumeric() {
            panic!("faint text holds {c:?} in {:?}", visible_inner(text));
        }
    }
}

/// `text` with every SGR sequence shown as `⟨parameters⟩`, so a snapshot says what was written.
/// It also fails on faint letters and digits (see [`assert_faint_is_decoration`]), so every
/// panel snapshot checks the rule.
pub fn visible(text: &str) -> String {
    assert_faint_is_decoration(text);
    visible_inner(text)
}

fn visible_inner(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut params = String::new();
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
                params.push(next);
            }
            out.push('⟨');
            out.push_str(&params);
            out.push('⟩');
        } else if c == '\x1b' {
            out.push_str("⟨ESC⟩");
        } else {
            out.push(c);
        }
    }
    out
}
