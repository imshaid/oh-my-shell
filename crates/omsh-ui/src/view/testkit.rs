//! Test helpers for the full-screen views: a buffer written out with its styles, and the rule
//! that faint (dim) text is decoration only.

use ratatui::buffer::Buffer;
use ratatui::style::Modifier;

/// One line for each row of `buffer`, with the style of each run of cells written next to it:
/// `[dim]`, `[rev]`, `[dim+rev]` and a color name where there is one, closed by `[/]`. A run of
/// plain cells has no marker. Trailing blanks are cut, so a snapshot shows what a person sees.
pub fn styled_rows(buffer: &Buffer) -> Vec<String> {
    let area = buffer.area;
    (0..area.height)
        .map(|y| {
            let mut out = String::new();
            let mut open = String::new();
            for x in 0..area.width {
                let cell = &buffer[(x, y)];
                let mut marks: Vec<String> = Vec::new();
                if cell.modifier.contains(Modifier::DIM) {
                    marks.push("dim".to_owned());
                }
                if cell.modifier.contains(Modifier::REVERSED) {
                    marks.push("rev".to_owned());
                }
                if cell.modifier.contains(Modifier::UNDERLINED) {
                    marks.push("under".to_owned());
                }
                if cell.fg != ratatui::style::Color::Reset {
                    marks.push(format!("{:?}", cell.fg).to_lowercase());
                }
                let marker = marks.join("+");
                if marker != open {
                    if !open.is_empty() {
                        out.push_str("[/]");
                    }
                    if !marker.is_empty() {
                        out.push('[');
                        out.push_str(&marker);
                        out.push(']');
                    }
                    open = marker;
                }
                out.push_str(cell.symbol());
            }
            if !open.is_empty() {
                out.push_str("[/]");
            }
            out.trim_end().to_owned()
        })
        .collect()
}

/// Fails if a faint cell holds a letter or a digit. Faint is for decoration (rules, separators,
/// glue); text that carries information is drawn at full weight, because faint text is hard to
/// read on some terminal themes.
pub fn assert_faint_is_decoration(buffer: &Buffer, what: &str) {
    let area = buffer.area;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buffer[(x, y)];
            if cell.modifier.contains(Modifier::DIM)
                && cell.symbol().chars().any(char::is_alphanumeric)
            {
                let row: String = (0..area.width).map(|x| buffer[(x, y)].symbol()).collect();
                panic!(
                    "{what}: faint {:?} at column {x} of row {y}: {:?}",
                    cell.symbol(),
                    row.trim_end()
                );
            }
        }
    }
}
