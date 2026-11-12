//! The entries of the command palette (`Ctrl+P`): what the shell can do, and the names a person
//! can type that the shell knows right now.

use std::collections::BTreeMap;
use std::path::Path;

use omsh_ui::palette::{Effect, Entry, Kind};

use crate::complete::Facts;

/// One line that says what a builtin does. The executor owns the texts (this crate does not
/// depend on it), so the shell hands the lookup in.
pub type BuiltinHelp = fn(&str) -> Option<&'static str>;

/// Everything the palette lists. `config_file` is `config.toml`, or `None` when this shell reads
/// none (`--no-config`): then there is nothing to edit.
pub(crate) fn entries(
    facts: &Facts,
    abbreviations: &BTreeMap<String, String>,
    config_file: Option<&Path>,
    help: BuiltinHelp,
) -> Vec<Entry> {
    let mut out = actions(config_file);
    // A name is put in the line with a blank after it: the next thing typed is its argument.
    let insert = |text: &str| Effect::Insert(format!("{text} "));
    for (name, text) in &facts.alias_text {
        out.push(Entry::new(Kind::Alias, name, text, None, insert(name)));
    }
    for (name, text) in abbreviations {
        out.push(Entry::new(
            Kind::Abbreviation,
            name,
            format!("expands to {text}"),
            None,
            insert(text),
        ));
    }
    for name in &facts.functions {
        out.push(Entry::new(
            Kind::Function,
            name,
            "a function of this shell",
            None,
            insert(name),
        ));
    }
    for name in &facts.builtins {
        out.push(Entry::new(
            Kind::Builtin,
            name,
            help(name).unwrap_or_default(),
            None,
            insert(name),
        ));
    }
    out
}

/// What the shell can do, in the order the palette lists it.
fn actions(config_file: Option<&Path>) -> Vec<Entry> {
    let command = |name: &str, detail: &str, line: &str| {
        Entry::new(
            Kind::Action,
            name,
            detail,
            None,
            Effect::Insert(line.to_owned()),
        )
    };
    let mut out = vec![
        Entry::new(
            Kind::Action,
            "Settings",
            "Show the settings and change switches.",
            None,
            Effect::OpenSettings,
        ),
        Entry::new(
            Kind::Action,
            "History explorer",
            "Search every command that was run.",
            Some("Ctrl+R"),
            Effect::OpenHistory,
        ),
        Entry::new(
            Kind::Action,
            "Explain the last failure",
            "Ask the AI why the last command failed.",
            Some("Alt+?"),
            Effect::ExplainLast,
        ),
        command(
            "Undo the last deletion",
            "Put back files that were moved to the trash.",
            "undo",
        ),
        command("Jobs", "List background and stopped jobs.", "jobs"),
        command(
            "AI status",
            "Show how the AI layer is set up.",
            "omsh ai status",
        ),
        command(
            "AI setup",
            "Choose a provider, a model and a key.",
            "omsh ai setup",
        ),
    ];
    if let Some(file) = config_file {
        out.push(command(
            "Edit config.toml",
            "Open the configuration file in your editor.",
            &format!(
                "\"${{VISUAL:-${{EDITOR:-vi}}}}\" {}",
                single_quoted(&file.to_string_lossy())
            ),
        ));
    }
    out
}

/// `text` as one shell word in single quotes.
pub(crate) fn single_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            functions: vec!["mkcd".into()],
            aliases: vec!["ll".into()],
            alias_text: vec![("ll".into(), "ls -l".into())],
            builtins: vec!["cd".into(), "mystery".into()],
            ..Facts::default()
        }
    }

    fn help(name: &str) -> Option<&'static str> {
        (name == "cd").then_some("Change the current folder.")
    }

    fn abbreviations() -> BTreeMap<String, String> {
        BTreeMap::from([("gco".to_owned(), "git checkout".to_owned())])
    }

    fn find<'a>(entries: &'a [Entry], name: &str) -> &'a Entry {
        entries.iter().find(|entry| entry.name == name).unwrap()
    }

    #[test]
    fn every_kind_of_name_the_shell_knows_is_listed() {
        let entries = entries(&facts(), &abbreviations(), None, help);
        let kinds = |name: &str| find(&entries, name).kind;
        assert_eq!(kinds("ll"), Kind::Alias);
        assert_eq!(kinds("gco"), Kind::Abbreviation);
        assert_eq!(kinds("mkcd"), Kind::Function);
        assert_eq!(kinds("cd"), Kind::Builtin);
        assert_eq!(kinds("Settings"), Kind::Action);
    }

    #[test]
    fn a_name_is_put_in_the_line_with_a_blank_after_it_and_nothing_runs() {
        let entries = entries(&facts(), &abbreviations(), None, help);
        assert_eq!(find(&entries, "ll").effect, Effect::Insert("ll ".into()));
        assert_eq!(
            find(&entries, "mkcd").effect,
            Effect::Insert("mkcd ".into())
        );
        assert_eq!(find(&entries, "cd").effect, Effect::Insert("cd ".into()));
        assert!(
            entries
                .iter()
                .all(|entry| !matches!(&entry.effect, Effect::Insert(text) if text.contains('\n')))
        );
    }

    #[test]
    fn an_abbreviation_puts_its_text_in_the_line_not_its_name() {
        let entries = entries(&facts(), &abbreviations(), None, help);
        let gco = find(&entries, "gco");
        assert_eq!(gco.effect, Effect::Insert("git checkout ".into()));
        assert_eq!(gco.detail, "expands to git checkout");
    }

    #[test]
    fn an_alias_shows_its_text_and_a_builtin_its_help() {
        let entries = entries(&facts(), &abbreviations(), None, help);
        assert_eq!(find(&entries, "ll").detail, "ls -l");
        assert_eq!(find(&entries, "cd").detail, "Change the current folder.");
        assert_eq!(find(&entries, "mystery").detail, "");
    }

    #[test]
    fn the_actions_come_first_in_a_fixed_order_and_the_keys_are_named() {
        let entries = entries(&facts(), &abbreviations(), None, help);
        let actions: Vec<_> = entries
            .iter()
            .take_while(|entry| entry.kind == Kind::Action)
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(
            actions,
            [
                "Settings",
                "History explorer",
                "Explain the last failure",
                "Undo the last deletion",
                "Jobs",
                "AI status",
                "AI setup"
            ]
        );
        assert_eq!(find(&entries, "Settings").effect, Effect::OpenSettings);
        assert_eq!(
            find(&entries, "History explorer").key.as_deref(),
            Some("Ctrl+R")
        );
        assert_eq!(
            find(&entries, "Explain the last failure").effect,
            Effect::ExplainLast
        );
        assert_eq!(
            find(&entries, "Explain the last failure").key.as_deref(),
            Some("Alt+?")
        );
    }

    #[test]
    fn editing_the_config_is_offered_only_when_there_is_a_file() {
        let none = entries(&facts(), &abbreviations(), None, help);
        assert!(none.iter().all(|entry| entry.name != "Edit config.toml"));
        let some = entries(
            &facts(),
            &abbreviations(),
            Some(Path::new("/home/me/it's here/config.toml")),
            help,
        );
        assert_eq!(
            find(&some, "Edit config.toml").effect,
            Effect::Insert(
                "\"${VISUAL:-${EDITOR:-vi}}\" '/home/me/it'\\''s here/config.toml'".into()
            )
        );
    }

    #[test]
    fn single_quoting_survives_the_shell() {
        assert_eq!(single_quoted("a b"), "'a b'");
        assert_eq!(single_quoted("it's"), "'it'\\''s'");
        assert_eq!(single_quoted("$x `y` \"z\""), "'$x `y` \"z\"'");
    }
}
