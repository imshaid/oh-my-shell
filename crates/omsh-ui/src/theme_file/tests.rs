use super::*;
use std::ffi::OsString;
use std::fs;

fn nord() -> Theme {
    Theme::default()
}

fn at(problem: &Problem) -> (usize, usize) {
    (problem.line.unwrap(), problem.column.unwrap())
}

#[test]
fn an_empty_file_is_nord() {
    let (theme, problems) = parse_theme("");
    assert_eq!(theme, nord());
    assert!(problems.is_empty());
}

#[test]
fn a_color_name_changes_the_color_and_keeps_the_attributes() {
    let (theme, problems) = parse_theme(
        "[tokens]\ndim = \"cyan\"\n[roles]\ncomment = \"blue\"\nvalid_path = \"green\"\n",
    );
    assert!(problems.is_empty(), "{problems:?}");
    let dim = theme.token(Token::Dim);
    assert_eq!(dim.color, Some(AnsiColor::Cyan));
    assert!(dim.dim, "the attribute of nord stays");
    assert_eq!(theme.get(Role::Comment).color, Some(AnsiColor::Blue));
    assert!(theme.get(Role::Comment).dim);
    let path = theme.get(Role::ValidPath);
    assert_eq!(path.color, Some(AnsiColor::Green));
    assert!(path.underline);
}

#[test]
fn default_means_the_terminals_own_color() {
    let (theme, problems) = parse_theme("[tokens]\ndanger = \"default\"\n");
    assert!(problems.is_empty());
    assert_eq!(theme.token(Token::Danger).color, None);
}

#[test]
fn a_table_gives_the_whole_look() {
    let (theme, problems) =
        parse_theme("[roles]\nquote = { color = \"magenta\", underline = true }\nparam = {}\n");
    assert!(problems.is_empty(), "{problems:?}");
    let quote = theme.get(Role::Quote);
    assert_eq!(quote.color, Some(AnsiColor::Magenta));
    assert!(quote.underline && !quote.dim && !quote.reverse);
    assert_eq!(
        theme.get(Role::Param),
        RoleStyle::PLAIN,
        "an empty table is plain"
    );
}

#[test]
fn the_rest_of_nord_is_untouched() {
    let (theme, _) = parse_theme("[tokens]\ndanger = \"magenta\"\n");
    let mut expected = nord();
    expected.set_token(Token::Danger, RoleStyle::color(AnsiColor::Magenta));
    assert_eq!(theme, expected);
}

#[test]
fn hex_and_numbers_are_refused_with_their_place() {
    let (theme, problems) =
        parse_theme("[tokens]\naccent = \"#5E81AC\"\npath = \"color208\"\nai = 5\n");
    assert_eq!(theme, nord(), "every wrong value keeps nord's look");
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert_eq!(at(&problems[0]), (2, 10));
    assert!(problems[0].message.contains("#5E81AC"));
    assert!(problems[0].message.contains("16 ANSI names"));
    assert_eq!(at(&problems[1]), (3, 8));
    assert_eq!(at(&problems[2]), (4, 6));
    assert!(problems[2].message.contains("an integer"));
    assert!(problems.iter().all(|p| p.severity == Severity::Error));
}

#[test]
fn bold_is_refused() {
    let (theme, problems) = parse_theme("[roles]\ncommand = { color = \"red\", bold = true }\n");
    assert_eq!(
        theme,
        nord(),
        "a table with a mistake is not applied at all"
    );
    assert_eq!(problems.len(), 1);
    assert_eq!(at(&problems[0]), (2, 28));
    assert!(problems[0].message.contains("bold is not available"));
}

#[test]
fn a_wrong_type_inside_a_table_is_refused() {
    let (theme, problems) = parse_theme("[roles]\nquote = { dim = \"yes\", color = 3 }\n");
    assert_eq!(theme, nord());
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems[0].message.contains("must be true or false"));
    assert!(problems[1].message.contains("must be a color name"));
}

#[test]
fn good_keys_next_to_bad_ones_still_apply() {
    let (theme, problems) = parse_theme("[tokens]\nsuccess = \"#00ff00\"\nwarning = \"red\"\n");
    assert_eq!(problems.len(), 1);
    assert_eq!(theme.token(Token::Success).color, Some(AnsiColor::Green));
    assert_eq!(theme.token(Token::Warning).color, Some(AnsiColor::Red));
}

#[test]
fn unknown_names_are_warnings() {
    let (theme, problems) =
        parse_theme("[tokens]\nhotpink = \"red\"\n[roles]\nnope = \"red\"\n[extra]\nx = 1\n");
    assert_eq!(theme, nord());
    assert_eq!(problems.len(), 3, "{problems:?}");
    assert!(problems.iter().all(|p| p.severity == Severity::Warning));
    assert!(problems[0].message.contains("tokens.hotpink"));
    assert!(problems[1].message.contains("roles.nope"));
    assert!(problems[2].message.contains("[extra]"));
}

#[test]
fn a_section_of_the_wrong_type_is_an_error() {
    let (theme, problems) = parse_theme("tokens = 3\n");
    assert_eq!(theme, nord());
    assert_eq!(problems.len(), 1);
    assert_eq!(at(&problems[0]), (1, 10));
    assert!(problems[0].message.contains("[tokens] must be a table"));
}

#[test]
fn a_syntax_error_ignores_the_whole_file() {
    let (theme, problems) = parse_theme("[tokens]\ndanger = \"magenta\"\naccent = \n");
    assert_eq!(
        theme,
        nord(),
        "even the good key before the error is dropped"
    );
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].severity, Severity::Error);
    assert_eq!(problems[0].line, Some(3));
    assert!(problems[0].message.contains("the whole file is ignored"));
}

#[test]
fn the_column_counts_characters() {
    let (_, problems) = parse_theme("[tokens]\n\"é\" = 1\naccent = \"nope\"\n");
    let bad = problems.iter().find(|p| p.line == Some(3)).unwrap();
    assert_eq!(bad.column, Some(10));
}

fn scratch(name: &str) -> (std::path::PathBuf, Paths) {
    let dir = std::env::temp_dir().join(format!("omsh-theme-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("oh-my-shell/themes")).unwrap();
    let base = dir.clone();
    let paths = Paths::from_lookup(move |var| {
        (var == "XDG_CONFIG_HOME").then(|| OsString::from(base.clone()))
    });
    (dir, paths)
}

#[test]
fn a_built_in_name_reads_no_file() {
    let none = Paths::from_lookup(|_| None);
    let loaded = load_theme("nord", &none);
    assert_eq!(loaded.theme, nord());
    assert!(loaded.problems.is_empty());
    assert_eq!(loaded.file, None);
}

#[test]
fn a_theme_file_is_read_from_the_themes_folder() {
    let (dir, paths) = scratch("read");
    fs::write(
        dir.join("oh-my-shell/themes/mine.toml"),
        "[tokens]\ndanger = \"magenta\"\n",
    )
    .unwrap();
    let loaded = load_theme("mine", &paths);
    assert!(loaded.problems.is_empty());
    assert_eq!(
        loaded.theme.token(Token::Danger).color,
        Some(AnsiColor::Magenta)
    );
    assert_eq!(loaded.file, Some(dir.join("oh-my-shell/themes/mine.toml")));
}

#[test]
fn a_missing_theme_falls_back_to_nord_with_one_line() {
    let (_, paths) = scratch("missing");
    let loaded = load_theme("ghost", &paths);
    assert_eq!(loaded.theme, nord());
    assert_eq!(loaded.problems.len(), 1);
    assert!(
        loaded.problems[0]
            .message
            .contains("no theme called \"ghost\"")
    );
}

#[test]
fn a_name_that_could_climb_out_is_not_a_file() {
    let (_, paths) = scratch("climb");
    let loaded = load_theme("../config", &paths);
    assert_eq!(loaded.theme, nord());
    assert_eq!(loaded.file, None, "no file is even looked for");
    assert_eq!(loaded.problems.len(), 1);
}

#[test]
fn an_unreadable_theme_falls_back_to_nord() {
    let (dir, paths) = scratch("dir");
    fs::create_dir_all(dir.join("oh-my-shell/themes/folder.toml")).unwrap();
    let loaded = load_theme("folder", &paths);
    assert_eq!(loaded.theme, nord());
    assert_eq!(loaded.problems.len(), 1);
}

#[test]
fn the_report_names_the_file_line_and_column() {
    let (dir, paths) = scratch("report");
    fs::write(
        dir.join("oh-my-shell/themes/x.toml"),
        "[tokens]\naccent = \"#fff\"\n",
    )
    .unwrap();
    let loaded = load_theme("x", &paths);
    let lines = loaded.report(10);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("themes/x.toml:2:10: "), "{lines:?}");
}

#[test]
fn a_long_report_is_cut_with_a_count() {
    let text: String = std::iter::once("[tokens]\n".to_owned())
        .chain((0..5).map(|n| format!("k{n} = 1\n")))
        .collect();
    let (theme, problems) = parse_theme(&text);
    assert_eq!(theme, nord());
    let loaded = LoadedTheme {
        theme,
        problems,
        file: None,
    };
    let lines = loaded.report(2);
    assert_eq!(lines.len(), 3);
    assert!(lines[2].ends_with("... and 3 more problems"), "{lines:?}");
}
