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

/// `text` with every SGR sequence shown as `⟨parameters⟩`, so a snapshot says what was written.
pub fn visible(text: &str) -> String {
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
