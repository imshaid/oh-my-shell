//! The Markdown files of the repository: every relative link goes to a file that exists, every
//! `#anchor` goes to a heading that exists, and nothing public points at private files.
//!
//! The pages link to source files by relative path (`docs/report.md` has a table of them), so
//! this test also keeps those links from going stale when a file is moved or renamed.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// `README.md`, `CONTRIBUTING.md` and every `docs/*.md`.
fn pages() -> Vec<PathBuf> {
    let root = root();
    let mut pages = vec![root.join("README.md"), root.join("CONTRIBUTING.md")];
    let mut docs: Vec<PathBuf> = fs::read_dir(root.join("docs"))
        .unwrap_or_else(|error| panic!("cannot read docs/: {error}"))
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    docs.sort();
    pages.extend(docs);
    pages
}

fn read(path: &Path) -> String {
    fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// The text without fenced code blocks, which may hold anything.
fn prose(text: &str) -> String {
    let mut fenced = false;
    let mut out = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if !fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The anchor GitHub makes for a heading: lower case, letters, digits, `-` and `_` kept, spaces
/// turned into `-`, everything else dropped.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            '-' | '_' => Some(c),
            c if c.is_alphanumeric() => Some(c),
            _ => None,
        })
        .collect()
}

fn anchors(text: &str) -> BTreeSet<String> {
    prose(text)
        .lines()
        .filter(|line| line.starts_with('#'))
        .map(|line| slug(line.trim_start_matches('#')))
        .collect()
}

/// The targets of the `[text](target)` links in `text`.
fn links(text: &str) -> Vec<String> {
    let prose = prose(text);
    let mut targets = Vec::new();
    let mut rest = prose.as_str();
    while let Some(at) = rest.find("](") {
        rest = &rest[at + 2..];
        if let Some(end) = rest.find(')') {
            targets.push(rest[..end].to_owned());
            rest = &rest[end..];
        }
    }
    targets
}

#[test]
fn there_are_pages_to_check() {
    let pages = pages();
    let names: Vec<String> = pages
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    for wanted in [
        "README.md",
        "CONTRIBUTING.md",
        "user-guide.md",
        "commands.md",
        "design.md",
        "report.md",
    ] {
        assert!(names.iter().any(|n| n == wanted), "{wanted} is missing");
    }
}

#[test]
fn every_relative_link_and_anchor_goes_somewhere() {
    let mut broken = Vec::new();
    let mut checked = 0;
    for page in pages() {
        let text = read(&page);
        let folder = page.parent().map_or_else(root, Path::to_path_buf);
        for target in links(&text) {
            if target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            checked += 1;
            let (path_part, anchor) = match target.split_once('#') {
                Some((path, anchor)) => (path, Some(anchor)),
                None => (target.as_str(), None),
            };
            let file = if path_part.is_empty() {
                page.clone()
            } else {
                folder.join(path_part)
            };
            if !file.exists() {
                broken.push(format!("{}: {target} (no such file)", page.display()));
                continue;
            }
            if let Some(anchor) = anchor
                && file.extension().is_some_and(|ext| ext == "md")
                && !anchors(&read(&file)).contains(anchor)
            {
                broken.push(format!("{}: {target} (no such heading)", page.display()));
            }
        }
    }
    assert!(
        checked > 20,
        "only {checked} relative links found; is the parser broken?"
    );
    assert!(broken.is_empty(), "broken links:\n{}", broken.join("\n"));
}

#[test]
fn public_pages_do_not_mention_private_files() {
    let mut found = Vec::new();
    for page in pages() {
        let text = read(&page);
        for word in [".private", "CLAUDE.local", "BLUEPRINT", "PROGRESS.md"] {
            if text.contains(word) {
                found.push(format!("{} mentions {word}", page.display()));
            }
        }
    }
    assert!(found.is_empty(), "{}", found.join("\n"));
}
