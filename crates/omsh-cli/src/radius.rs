//! The blast-radius lines of the confirmation panels (§12.3).
//!
//! The counting is `omsh_safety::blast`; this module gives it its limits, shares them among the
//! commands of one line, and turns a result into the lines a person reads. Every number shown
//! was counted; when a limit stopped the count, the line says "at least" and says why.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use omsh_config::Safety;
use omsh_safety::Finding;
use omsh_safety::blast::{self, Fs, Limits, Op, Radius, Stop};

use crate::probe::ShellFs;

/// The longest example path or word shown, in characters.
const SHOWN_WIDTH: usize = 70;

/// `[safety] blast_radius_max_entries` and `blast_radius_max_ms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlastLimits {
    /// Entries counted before the walk stops.
    pub max_entries: u32,
    /// Milliseconds before the walk stops.
    pub max_ms: u32,
}

impl BlastLimits {
    /// The limits the configuration asks for.
    #[must_use]
    pub fn from_config(safety: &Safety) -> Self {
        Self {
            max_entries: safety.blast_radius_max_entries,
            max_ms: safety.blast_radius_max_ms,
        }
    }

    fn start(self) -> Limits {
        Limits::starting_now(
            u64::from(self.max_entries),
            Duration::from_millis(u64::from(self.max_ms)),
        )
    }
}

impl Default for BlastLimits {
    fn default() -> Self {
        Self::from_config(&Safety::default())
    }
}

/// Counts what each finding that works on files would touch. One budget covers the whole
/// call, so a line of many commands cannot take many times the time. A command that shows up
/// in two findings is counted once, under the first. The answer has one entry for each finding.
#[must_use]
pub fn measure(findings: &[&Finding], limits: BlastLimits) -> Vec<Option<Radius>> {
    let mut budget = limits.start();
    let fs: Arc<dyn Fs> = Arc::new(ShellFs);
    let uid = omsh_exec::effective_uid();
    let mut done: Vec<(omsh_exec::Span, Vec<String>)> = Vec::new();
    findings
        .iter()
        .map(|finding| {
            let files = finding.files.as_ref()?;
            let key = (
                finding.span,
                files.targets.iter().map(|t| t.src.clone()).collect(),
            );
            if done.contains(&key) {
                return None;
            }
            done.push(key);
            Some(blast::measure(files, &mut budget, uid, &fs))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------
// Words.

/// `n` with a comma between groups of three digits.
fn group(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A size in bytes, as people read it.
fn size(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    #[expect(clippy::cast_precision_loss, reason = "a size shown with one decimal")]
    let mut value = bytes as f64;
    let mut unit = "B";
    for next in UNITS {
        if value < 1024.0 {
            break;
        }
        value /= 1024.0;
        unit = next;
    }
    format!("{value:.1} {unit}")
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", group(n), if n == 1 { one } else { many })
}

/// Text from the file system, safe to print: no control characters, not too long.
fn shown(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    if clean.chars().count() <= SHOWN_WIDTH {
        return clean;
    }
    let mut cut: String = clean.chars().take(SHOWN_WIDTH).collect();
    cut.push_str("...");
    cut
}

/// A path, relative to `cwd` when it is inside it.
fn path_text(path: &Path, cwd: Option<&str>) -> String {
    if let Some(cwd) = cwd
        && let Ok(rest) = path.strip_prefix(cwd)
    {
        return if rest.as_os_str().is_empty() {
            ".".to_owned()
        } else {
            shown(&rest.to_string_lossy())
        };
    }
    shown(&path.to_string_lossy())
}

/// What was counted, in words: `1,204 files and 37 folders`.
fn counted(radius: &Radius) -> String {
    let mut parts = Vec::new();
    if radius.files > 0 {
        parts.push(plural(radius.files, "file", "files"));
    }
    if radius.dirs > 0 {
        parts.push(plural(radius.dirs, "folder", "folders"));
    }
    if radius.others > 0 {
        parts.push(plural(
            radius.others,
            "link or special file",
            "links or special files",
        ));
    }
    let mut text = match parts.len() {
        0 => return String::new(),
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().unwrap_or_default();
            format!("{} and {last}", parts.join(", "))
        }
    };
    if radius.files > 0 {
        text.push_str(&format!(" ({} of file data)", size(radius.bytes)));
    }
    text
}

/// The lines under a finding: what the command would touch, with the limits that were hit and
/// everything that could not be counted. `cwd` is where relative paths are shown from.
#[must_use]
pub fn lines(radius: &Radius, cwd: Option<&str>, limits: BlastLimits) -> Vec<String> {
    let mut out = Vec::new();
    let least = if radius.is_exact() { "" } else { "at least " };
    let searching = matches!(radius.op, Op::Search { .. });
    if radius.entries() > 0 {
        let verb = if searching { "searches" } else { "touches" };
        let mut line = format!("{verb} {least}{}", counted(radius));
        if searching {
            line.push_str("; it deletes only the ones its tests match");
        }
        out.push(line);
        if !searching {
            out.push(owners(radius, least));
        }
        if !radius.examples.is_empty() {
            let names: Vec<String> = radius
                .examples
                .iter()
                .map(|path| path_text(path, cwd))
                .collect();
            out.push(format!("for example: {}", names.join(", ")));
        }
    }
    match radius.stopped {
        Some(Stop::Entries) if radius.entries() == 0 => {
            out.push("not counted: the limit for this line was used up by earlier commands".into());
        }
        Some(Stop::Entries) => out.push(format!(
            "counting stopped at the limit of {} entries; there is more",
            group(u64::from(limits.max_entries))
        )),
        Some(Stop::Time) if radius.entries() == 0 => {
            out.push("not counted: the time limit ran out before anything was read".into());
        }
        Some(Stop::Time) => out.push(format!(
            "counting stopped after {} ms; there is more",
            limits.max_ms
        )),
        None => {}
    }
    if radius.unreadable > 0 {
        out.push(format!(
            "{} could not be read; what is in them is not counted",
            plural(radius.unreadable, "place", "places")
        ));
    }
    if !radius.missing.is_empty() {
        let names: Vec<String> = radius.missing.iter().map(|word| shown(word)).collect();
        out.push(format!("does not exist: {}", names.join(", ")));
    }
    for (word, why) in &radius.not_counted {
        out.push(format!("not counted: {} ({})", shown(word), why.text()));
    }
    out
}

/// The line about folders outside the working directory and files of other users.
fn owners(radius: &Radius, least: &str) -> String {
    let so_far = if least.is_empty() { "" } else { " so far" };
    let outside = match radius.outside_cwd {
        Some(0) => format!("none outside the current folder{so_far}"),
        Some(n) => format!("{least}{n} outside the current folder"),
        None => "whether any are outside the current folder is not known".to_owned(),
    };
    let owned = if radius.not_owned == 0 {
        format!("none owned by someone else{so_far}")
    } else {
        format!("{least}{} owned by someone else", group(radius.not_owned))
    };
    format!("{outside}; {owned}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use omsh_safety::blast::{Op, Radius, Stop, Why};

    use super::{BlastLimits, group, lines, size};

    fn radius(op: Op) -> Radius {
        Radius {
            op,
            files: 0,
            dirs: 0,
            others: 0,
            bytes: 0,
            outside_cwd: Some(0),
            not_owned: 0,
            examples: Vec::new(),
            stopped: None,
            unreadable: 0,
            missing: Vec::new(),
            not_counted: Vec::new(),
        }
    }

    const DELETE: Op = Op::Delete { recursive: true };

    fn limits() -> BlastLimits {
        BlastLimits {
            max_entries: 50_000,
            max_ms: 300,
        }
    }

    #[test]
    fn numbers_are_grouped_and_sizes_are_readable() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_000), "1,000");
        assert_eq!(group(50_000), "50,000");
        assert_eq!(group(1_234_567), "1,234,567");
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1024), "1.0 KiB");
        assert_eq!(size(1536), "1.5 KiB");
        assert_eq!(size(50 * 1024 * 1024 + 200_000), "50.2 MiB");
        assert_eq!(size(u64::MAX), "16.0 EiB");
    }

    #[test]
    fn an_exact_count_has_no_hedging() {
        let mut r = radius(DELETE);
        r.files = 1_204;
        r.dirs = 37;
        r.bytes = 48 * 1024 * 1024 + 200_000;
        r.not_owned = 3;
        r.examples = vec![
            PathBuf::from("/w/build"),
            PathBuf::from("/w/build/a.o"),
            PathBuf::from("/etc/hosts"),
        ];
        let text = lines(&r, Some("/w"), limits());
        assert_eq!(
            text,
            [
                "touches 1,204 files and 37 folders (48.2 MiB of file data)",
                "none outside the current folder; 3 owned by someone else",
                "for example: build, build/a.o, /etc/hosts",
            ]
        );
    }

    #[test]
    fn one_thing_is_singular_and_the_working_folder_is_a_dot() {
        let mut r = radius(DELETE);
        r.dirs = 1;
        r.examples = vec![PathBuf::from("/w")];
        let text = lines(&r, Some("/w"), limits());
        assert_eq!(text[0], "touches 1 folder");
        assert_eq!(text[2], "for example: .");
    }

    #[test]
    fn a_stopped_count_says_at_least_and_why() {
        let mut r = radius(DELETE);
        r.files = 49_899;
        r.dirs = 101;
        r.stopped = Some(Stop::Entries);
        r.not_owned = 2;
        r.outside_cwd = Some(7);
        let text = lines(&r, None, limits());
        assert_eq!(
            text[0],
            "touches at least 49,899 files and 101 folders (0 B of file data)"
        );
        assert_eq!(
            text[1],
            "at least 7 outside the current folder; at least 2 owned by someone else"
        );
        assert_eq!(
            text.last().unwrap(),
            "counting stopped at the limit of 50,000 entries; there is more"
        );
        r.stopped = Some(Stop::Time);
        let text = lines(&r, None, limits());
        assert_eq!(
            text.last().unwrap(),
            "counting stopped after 300 ms; there is more"
        );
        // A zero is only "so far" when the count is not finished.
        r.not_owned = 0;
        r.outside_cwd = Some(0);
        let text = lines(&r, None, limits());
        assert_eq!(
            text[1],
            "none outside the current folder so far; none owned by someone else so far"
        );
    }

    #[test]
    fn nothing_counted_because_the_limit_was_used_up_is_said_plainly() {
        let mut r = radius(DELETE);
        r.stopped = Some(Stop::Entries);
        assert_eq!(
            lines(&r, None, limits()),
            ["not counted: the limit for this line was used up by earlier commands"]
        );
        r.stopped = Some(Stop::Time);
        assert_eq!(
            lines(&r, None, limits()),
            ["not counted: the time limit ran out before anything was read"]
        );
    }

    #[test]
    fn find_says_it_searches_and_does_not_claim_a_deletion_count() {
        let mut r = radius(Op::Search { max_depth: None });
        r.files = 11_000;
        r.dirs = 1_000;
        r.stopped = Some(Stop::Entries);
        let text = lines(&r, None, limits());
        assert!(text[0].starts_with("searches at least 11,000 files and 1,000 folders"));
        assert!(text[0].ends_with("; it deletes only the ones its tests match"));
        assert!(
            text.iter().all(|line| !line.contains("owned")),
            "no ownership line for a search: {text:?}"
        );
    }

    #[test]
    fn what_could_not_be_counted_is_listed() {
        let mut r = radius(DELETE);
        r.files = 1;
        r.outside_cwd = None;
        r.unreadable = 2;
        r.missing = vec!["gone.txt".into()];
        r.not_counted = vec![("\"$d\"".into(), Why::OnlyKnownWhenRun)];
        let text = lines(&r, None, limits());
        assert!(text[1].starts_with("whether any are outside the current folder is not known"));
        assert!(
            text.contains(&"2 places could not be read; what is in them is not counted".to_owned())
        );
        assert!(text.contains(&"does not exist: gone.txt".to_owned()));
        assert!(
            text.contains(&"not counted: \"$d\" (only known when the command runs)".to_owned())
        );
    }

    #[test]
    fn a_file_name_cannot_inject_terminal_codes() {
        let mut r = radius(DELETE);
        r.files = 1;
        r.examples = vec![PathBuf::from("/w/\x1b[2Jevil\nname")];
        r.missing = vec!["a\x07b".into()];
        let text = lines(&r, Some("/w"), limits()).join("\n");
        assert!(
            !text.chars().any(|c| c.is_control() && c != '\n'),
            "{text:?}"
        );
        assert!(text.contains("?[2Jevil?name"));
    }

    #[test]
    fn a_long_name_is_cut() {
        let mut r = radius(DELETE);
        r.files = 1;
        r.examples = vec![PathBuf::from(format!("/w/{}", "x".repeat(200)))];
        let text = lines(&r, Some("/w"), limits());
        assert!(text[2].len() < 100, "{}", text[2]);
        assert!(text[2].ends_with("..."));
    }
}
