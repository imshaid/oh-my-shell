//! `docs/commands.md` lists every setting, and only settings that exist.
//!
//! The page is written by hand. These tests are what keep it true: a key added to the schema
//! without a line on the page fails the first test, and a key that was renamed or removed and
//! is still described fails the second.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use omsh_config::SECTIONS;

fn commands_page() -> String {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/commands.md");
    fs::read_to_string(&file)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", file.display()))
}

/// Every `section.key` the schema knows, such as `ui.theme` or `ai.context.os`.
fn known_settings() -> BTreeSet<String> {
    SECTIONS
        .iter()
        .flat_map(|(section, keys)| keys.iter().map(move |key| format!("{section}.{key}")))
        .collect()
}

#[test]
fn every_setting_is_described_in_the_command_reference() {
    let page = commands_page();
    let missing: Vec<String> = known_settings()
        .into_iter()
        .filter(|name| !page.contains(&format!("`{name}`")))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/commands.md does not describe these settings: {missing:?}"
    );
}

#[test]
fn every_setting_the_reference_describes_exists() {
    let page = commands_page();
    let known = known_settings();
    let sections: Vec<String> = SECTIONS
        .iter()
        .map(|(section, _)| format!("{}.", section.split('.').next().unwrap_or(section)))
        .collect();
    let stale: Vec<&str> = page
        .lines()
        .filter_map(|line| line.strip_prefix("- `"))
        .filter_map(|rest| rest.split('`').next())
        .filter(|name| {
            sections
                .iter()
                .any(|prefix| name.starts_with(prefix.as_str()))
        })
        .filter(|name| !known.contains(*name))
        .collect();
    assert!(
        stale.is_empty(),
        "docs/commands.md describes settings that are not in the schema: {stale:?}"
    );
}
