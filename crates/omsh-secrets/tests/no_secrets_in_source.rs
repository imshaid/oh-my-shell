//! No source file of the workspace holds text that looks like a secret.
//!
//! Tests need fake keys, but a key-shaped string in the source is what the masking rules (and any
//! scanner a person runs on the repository) are made to find. A test builds its fake key from
//! pieces at run time (`["AKIA", "IOSFODNN7EXAMPLE"].concat()`), so this test can insist that the
//! source holds no such text at all.
//!
//! Only the *formats* of known keys are checked (an AWS or GitHub key, a `sk-` key, a Google
//! key). A line such as `API_KEY=hunter2` has no real-key shape and is how the name rules of
//! `omsh-secrets` are tested, so it is allowed. A private-key header
//! (`-----BEGIN ... PRIVATE KEY-----`) is marker text, not key material, so it is allowed too.

use std::fs;
use std::path::{Path, PathBuf};

/// How the text of every key format that has a fixed look begins.
const FORMAT_STARTS: &[&str] = &[
    "AKIA",
    "ASIA",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "AIza",
    "sk-",
];

/// True if `line` holds text that has the look of a real key.
fn holds_key_shape(line: &str) -> bool {
    omsh_secrets::find(line).into_iter().any(|range| {
        let found = &line[range];
        FORMAT_STARTS.iter().any(|start| found.starts_with(start))
    })
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_rust_source_holds_text_that_looks_like_a_secret() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate lives in the crates folder");
    let mut files = Vec::new();
    rust_files(crates, &mut files);
    assert!(
        files.len() > 100,
        "found only {} source files under {}: the walk is broken",
        files.len(),
        crates.display()
    );
    let mut hits = Vec::new();
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for (number, line) in text.lines().enumerate() {
            if holds_key_shape(line) {
                hits.push(format!("{}:{}", file.display(), number + 1));
            }
        }
    }
    assert!(
        hits.is_empty(),
        "text that looks like a secret is in the source (build it at run time instead):\n{}",
        hits.join("\n")
    );
}
