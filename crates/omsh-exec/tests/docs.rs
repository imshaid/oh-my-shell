//! `docs/commands.md` lists every builtin a person can type, and only builtins that exist.

use std::fs;
use std::path::Path;

use omsh_exec::{builtin_names, is_builtin};

fn commands_page() -> String {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/commands.md");
    fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", file.display()))
}

/// The text between the heading `## Builtins` and the next `## ` heading.
fn builtins_section(page: &str) -> String {
    let mut inside = false;
    let mut section = String::new();
    for line in page.lines() {
        if line.starts_with("## ") {
            inside = line == "## Builtins";
            continue;
        }
        if inside {
            section.push_str(line);
            section.push('\n');
        }
    }
    section
}

#[test]
fn every_builtin_has_an_entry_in_the_command_reference() {
    let page = commands_page();
    let section = builtins_section(&page);
    let missing: Vec<&str> = builtin_names()
        .into_iter()
        .filter(|name| {
            !section.contains(&format!("### `{name}`"))
                && !section.contains(&format!("`{name}` and "))
                && !section.contains(&format!(" and `{name}`"))
        })
        .collect();
    assert!(
        missing.is_empty(),
        "docs/commands.md has no entry under 'Builtins' for: {missing:?}"
    );
}

#[test]
fn every_builtin_the_reference_describes_exists() {
    let page = commands_page();
    let section = builtins_section(&page);
    assert!(
        !section.is_empty(),
        "docs/commands.md has no '## Builtins' section"
    );
    let stale: Vec<&str> = section
        .lines()
        .filter_map(|line| line.strip_prefix("### "))
        .flat_map(|heading| heading.split('`').skip(1).step_by(2))
        .filter(|name| !is_builtin(name))
        .collect();
    assert!(
        stale.is_empty(),
        "docs/commands.md describes builtins that do not exist: {stale:?}"
    );
}
