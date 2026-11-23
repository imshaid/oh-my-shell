//! The reading of desktop entries. Every fixture is made here, in a folder of its own: no test
//! looks at the real `/usr/share/applications`.

use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::atomic::{AtomicU32, Ordering};

use omsh_exec::{Io, Shell};

use super::*;

/// A folder that is removed when it goes out of scope.
struct Tree(PathBuf);

impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("omsh-desktop-{}-{n}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    fn text(&self, relative: &str) -> String {
        self.path(relative).to_string_lossy().into_owned()
    }

    /// A file with `content`, in folders made as needed.
    fn put(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, content).unwrap();
        path
    }

    /// A program that may be run.
    fn program(&self, relative: &str) -> PathBuf {
        let path = self.put(relative, "#!/bin/sh\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A shell whose `PATH` is `path`.
fn shell_with_path(path: &str) -> Shell {
    let mut shell = Shell::new();
    shell.run_source(&format!("PATH='{path}'"), true, &mut Io::process());
    shell
}

/// A desktop entry of an application.
fn app(name: &str, exec: &str) -> String {
    format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\n")
}

fn entry(id: &str, text: &str) -> Entry {
    parse_entry(id, text).unwrap_or_else(|| panic!("{id} is not an entry:\n{text}"))
}

// -- Exec ---------------------------------------------------------------------------------------

fn words(value: &str) -> Option<Vec<String>> {
    parse_exec(value)
}

fn strings(list: &[&str]) -> Option<Vec<String>> {
    Some(list.iter().map(|word| (*word).to_owned()).collect())
}

#[test]
fn exec_words_are_split_on_blanks() {
    assert_eq!(words("tool  --x\ty"), strings(&["tool", "--x", "y"]));
    assert_eq!(words(""), strings(&[]));
}

#[test]
fn quotes_are_undone_before_anything_else() {
    assert_eq!(
        words(r#""/opt/My App/run" --x"#),
        strings(&["/opt/My App/run", "--x"])
    );
    // Inside quotes: \" \` \$ and \\ are the escapes; the others are an invalid line.
    assert_eq!(
        words(r#"tool "a \"b\" \$c \`d\` \\e""#),
        strings(&["tool", r#"a "b" $c `d` \e"#])
    );
    assert_eq!(words(r#"tool "a \n""#), None);
    // An empty quoted word is a word.
    assert_eq!(words(r#"tool "" x"#), strings(&["tool", "", "x"]));
}

#[test]
fn an_unclosed_quote_is_an_invalid_line() {
    assert_eq!(words(r#"tool "unclosed"#), None);
    assert_eq!(words(r#"tool "a \"#), None);
}

#[test]
fn field_codes_for_files_and_addresses_are_removed() {
    for code in ["%f", "%F", "%u", "%U", "%d", "%D", "%n", "%N", "%v", "%m"] {
        assert_eq!(
            words(&format!("tool {code} -x")),
            strings(&["tool", "-x"]),
            "{code}"
        );
    }
    assert_eq!(words("Telegram -- %U"), strings(&["Telegram", "--"]));
    // Joined to other text, the code goes and the text stays.
    assert_eq!(words("tool --file=%f"), strings(&["tool", "--file="]));
}

#[test]
fn a_percent_percent_is_one_percent() {
    assert_eq!(words("tool 100%%"), strings(&["tool", "100%"]));
    assert_eq!(words(r#"tool "100%%""#), strings(&["tool", "100%"]));
}

#[test]
fn an_unknown_field_code_makes_the_line_invalid() {
    for line in ["tool %z", "tool %", "tool a%", "tool %1", "tool %%%q", "%z"] {
        assert_eq!(words(line), None, "{line}");
    }
    // A code in a quoted word is undefined by the specification: not accepted.
    assert_eq!(words(r#"tool "%f""#), None);
}

#[test]
fn a_field_code_in_front_is_not_taken_for_the_program() {
    assert_eq!(words("%u tool"), None);
    assert_eq!(words("%F"), None);
}

#[test]
fn the_codes_that_name_the_application_are_allowed() {
    assert_eq!(
        words("tool %i %c %k"),
        strings(&["tool"]),
        "only the program is used"
    );
}

#[test]
fn the_string_escapes_come_off_before_the_quotes() {
    // In the file a quoted backslash is four backslashes (the specification says so).
    let text = app("Odd", r#""/opt/a\\\\b" %U"#);
    let found = parse_entry("odd.desktop", &text);
    // The program has a backslash, which is not a plain character: the entry is not used. The
    // point is the order: this must not be read as an invalid quote.
    assert_eq!(found, None);
    assert_eq!(unescape(r"a\sb\\c\x"), "a b\\c\\x");
    assert_eq!(unescape(r"\\\\"), "\\\\");
}

// -- one file -----------------------------------------------------------------------------------

#[test]
fn a_telegram_entry_has_its_program_and_never_the_action() {
    let text = "\
[Desktop Entry]
Version=1.0
Type=Application
Name=Telegram
Name[de]=Telegramm
Comment=New era of messaging
TryExec=Telegram
Exec=Telegram -- %U
Icon=org.telegram.desktop
Actions=Quit;

[Desktop Action Quit]
Name=Quit Telegram
Exec=Telegram -quit
";
    let found = entry("org.telegram.desktop.desktop", text);
    assert_eq!(found.name, "Telegram");
    assert_eq!(found.program, "Telegram");
    assert_eq!(found.try_exec.as_deref(), Some("Telegram"));
}

#[test]
fn a_key_of_an_action_group_is_never_read() {
    // Nothing in the entry's own group: the action's Exec and Name must not fill the gaps.
    let text = "\
[Desktop Entry]
Type=Application
Name=App

[Desktop Action Quit]
Name=Quit
Exec=quitter
";
    assert_eq!(parse_entry("a.desktop", text), None);
    // With both, the entry's own Exec wins and the action's name does not replace its name.
    let text = "\
[Desktop Entry]
Type=Application
Name=App
Exec=real

[Desktop Action Quit]
Name=Quit App
Exec=quitter
";
    let found = entry("a.desktop", text);
    assert_eq!(
        (found.name.as_str(), found.program.as_str()),
        ("App", "real")
    );
}

#[test]
fn a_localized_name_is_not_the_name() {
    let text = "[Desktop Entry]\nType=Application\nName[de]=Nur Deutsch\nExec=tool\n";
    assert_eq!(parse_entry("a.desktop", text), None);
}

#[test]
fn comments_blanks_and_spaces_around_the_equals_sign_are_fine() {
    let text = "# a comment\n\n[Desktop Entry]\n# another\nType = Application\nName = Spaced\nExec = tool %f\n";
    let found = entry("a.desktop", text);
    assert_eq!(
        (found.name.as_str(), found.program.as_str()),
        ("Spaced", "tool")
    );
}

#[test]
fn a_repeated_key_counts_once_with_its_first_value() {
    let text = "[Desktop Entry]\nType=Application\nName=A\nExec=first\nExec=second\n";
    assert_eq!(entry("a.desktop", text).program, "first");
}

#[test]
fn entries_that_must_not_be_used() {
    let cases = [
        (
            "hidden",
            "[Desktop Entry]\nType=Application\nName=A\nExec=tool\nHidden=true\n",
        ),
        (
            "a link, not an application",
            "[Desktop Entry]\nType=Link\nName=A\nURL=https://example.org\nExec=tool\n",
        ),
        (
            "a directory",
            "[Desktop Entry]\nType=Directory\nName=A\nExec=tool\n",
        ),
        ("no Exec", "[Desktop Entry]\nType=Application\nName=A\n"),
        ("no Name", "[Desktop Entry]\nType=Application\nExec=tool\n"),
        ("no Type", "[Desktop Entry]\nName=A\nExec=tool\n"),
        ("no group", "Type=Application\nName=A\nExec=tool\n"),
        (
            "an unknown field code",
            "[Desktop Entry]\nType=Application\nName=A\nExec=tool %z\n",
        ),
        (
            "an @@ marker",
            "[Desktop Entry]\nType=Application\nName=A\nExec=tool @@u %U @@\n",
        ),
        (
            "flatpak",
            "[Desktop Entry]\nType=Application\nName=A\nExec=/usr/bin/flatpak run --command=x org.x.X @@u %U @@\n",
        ),
        (
            "flatpak without markers",
            "[Desktop Entry]\nType=Application\nName=A\nExec=flatpak run org.x.X\n",
        ),
        (
            "env wrapper",
            "[Desktop Entry]\nType=Application\nName=A\nExec=env BAMF=x /snap/bin/firefox %u\n",
        ),
        (
            "a shell",
            "[Desktop Entry]\nType=Application\nName=A\nExec=sh -c \"tool\"\n",
        ),
        (
            "bash by its path",
            "[Desktop Entry]\nType=Application\nName=A\nExec=/bin/bash -c tool\n",
        ),
        (
            "a variable in front",
            "[Desktop Entry]\nType=Application\nName=A\nExec=FOO=1 tool\n",
        ),
        (
            "a relative path",
            "[Desktop Entry]\nType=Application\nName=A\nExec=./tool\n",
        ),
        (
            "a name that starts with a dash",
            "[Desktop Entry]\nType=Application\nName=A\nExec=-tool\n",
        ),
        (
            "a program with a blank in it",
            "[Desktop Entry]\nType=Application\nName=A\nExec=\"/opt/My App/run\" %U\n",
        ),
        (
            "a program with a shell character",
            "[Desktop Entry]\nType=Application\nName=A\nExec=\"a;b\"\n",
        ),
    ];
    for (why, text) in cases {
        assert_eq!(parse_entry("a.desktop", text), None, "{why}");
    }
}

#[test]
fn the_program_may_be_a_bare_name_or_an_absolute_path() {
    for (exec, program) in [
        ("google-chrome-stable %U", "google-chrome-stable"),
        ("/opt/google/chrome/chrome %U", "/opt/google/chrome/chrome"),
        ("\"/opt/app/run\" --x", "/opt/app/run"),
        ("Telegram -- %U", "Telegram"),
        ("code-oss --unity-launch %F", "code-oss"),
    ] {
        assert_eq!(
            entry("a.desktop", &app("A", exec)).program,
            program,
            "{exec}"
        );
    }
}

#[test]
fn nothing_hidden_in_the_name_or_the_exec() {
    // `\s` in the name is a blank, a `\n` would be a newline.
    assert_eq!(
        entry("a.desktop", &app(r"Two\sWords", "tool")).name,
        "Two Words"
    );
}

// -- names --------------------------------------------------------------------------------------

#[test]
fn keys_ignore_case_spaces_and_the_usual_endings() {
    assert_eq!(key("telegram-desktop"), "telegram");
    assert_eq!(key("Google Chrome"), "google-chrome");
    assert_eq!(key("google-chrome-stable"), "google-chrome");
    assert_eq!(key("x-desktop-stable"), "x");
    assert_eq!(key("x-stable-desktop"), "x");
    assert_eq!(key("Telegram"), "telegram");
    // Only an ending: `desktop` and `stable` alone, or inside a name, stay.
    assert_eq!(key("desktop"), "desktop");
    assert_eq!(key("stable-diffusion"), "stable-diffusion");
    assert_eq!(key("org.telegram.desktop"), "org.telegram.desktop");
    assert_eq!(key("-desktop"), "");
}

#[test]
fn an_entry_is_named_by_its_id_or_name_or_only_by_its_program() {
    let chrome = entry(
        "com.google.Chrome.desktop",
        &app("Google Chrome", "google-chrome-stable %U"),
    );
    assert_eq!(chrome.rank("google-chrome"), Some(Rank::Named), "by name");
    assert_eq!(
        chrome.rank("com.google.chrome"),
        Some(Rank::Named),
        "by id, whole"
    );
    assert_eq!(chrome.rank("chrome"), None);
    let old = entry("google-chrome.desktop", &app("Other", "other"));
    assert_eq!(old.rank("google-chrome"), Some(Rank::Named), "by id");
    assert_eq!(old.rank("google"), None);
    // The program alone: a web-app shortcut that runs the browser.
    let shortcut = entry(
        "chrome-abc-Default.desktop",
        &app(
            "WhatsApp Web",
            "/opt/google/chrome/google-chrome --app-id=abc",
        ),
    );
    assert_eq!(shortcut.rank("google-chrome"), Some(Rank::Program));
    assert_eq!(shortcut.rank("whatsapp-web"), Some(Rank::Named));
    assert!(Rank::Named < Rank::Program);
}

// -- where the files are ------------------------------------------------------------------------

fn dirs(home: Option<&str>, data_home: Option<&str>, data_dirs: Option<&str>) -> Vec<String> {
    applications_dirs(home.map(Path::new), data_home, data_dirs)
        .iter()
        .map(|dir| dir.to_string_lossy().into_owned())
        .collect()
}

#[test]
fn the_defaults_are_the_ones_of_the_specification() {
    assert_eq!(
        dirs(Some("/home/u"), None, None),
        [
            "/home/u/.local/share/applications",
            "/usr/local/share/applications",
            "/usr/share/applications"
        ]
    );
}

#[test]
fn the_variables_replace_the_defaults_and_keep_their_order() {
    assert_eq!(
        dirs(Some("/home/u"), Some("/d"), Some("/a:/b")),
        ["/d/applications", "/a/applications", "/b/applications"]
    );
}

#[test]
fn an_empty_variable_is_not_set_and_a_relative_folder_is_ignored() {
    assert_eq!(
        dirs(Some("/home/u"), Some(""), Some("")),
        [
            "/home/u/.local/share/applications",
            "/usr/local/share/applications",
            "/usr/share/applications"
        ]
    );
    assert_eq!(
        dirs(Some("/home/u"), Some("rel"), Some("/a:rel2::/a")),
        ["/a/applications"]
    );
    // No home and no variable: only the shared folders.
    assert_eq!(
        dirs(None, None, None),
        ["/usr/local/share/applications", "/usr/share/applications"]
    );
}

#[test]
fn the_folders_come_from_the_variables_of_the_shell() {
    let mut shell = Shell::new();
    let mut files = Io::process();
    shell.run_source("HOME=/h XDG_DATA_DIRS=/x:/y", true, &mut files);
    // `VAR=value` alone sets a shell variable, which is what the shell's own commands see.
    shell.run_source("XDG_DATA_HOME=/own", true, &mut files);
    assert_eq!(
        dirs_of(&shell),
        [
            PathBuf::from("/own/applications"),
            PathBuf::from("/x/applications"),
            PathBuf::from("/y/applications"),
        ]
    );
}

#[test]
fn no_folders_at_all_give_no_entries_and_no_error() {
    assert!(scan(&[]).is_empty());
    let tree = Tree::new();
    assert!(scan(&[tree.path("missing/applications")]).is_empty());
    // A file where a folder is expected.
    let file = tree.put("applications", "not a folder");
    assert!(scan(&[file]).is_empty());
    // An empty folder.
    fs::create_dir_all(tree.path("share/applications")).unwrap();
    assert!(scan(&[tree.path("share/applications")]).is_empty());
}

#[test]
fn a_subfolder_gives_its_name_to_the_id_with_a_dash() {
    let tree = Tree::new();
    tree.put("apps/kde/org.kde.tool.desktop", &app("Tool", "tool"));
    tree.put("apps/a/b/deep.desktop", &app("Deep", "deep"));
    tree.put("apps/plain.desktop", &app("Plain", "plain"));
    tree.put("apps/readme.txt", "not an entry");
    let ids: Vec<String> = scan(&[tree.path("apps")])
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(
        ids,
        [
            "a-b-deep.desktop",
            "kde-org.kde.tool.desktop",
            "plain.desktop"
        ]
    );
}

#[test]
fn the_first_file_with_an_id_wins() {
    let tree = Tree::new();
    tree.put("own/same.desktop", &app("Own", "own-tool"));
    tree.put("shared/same.desktop", &app("Shared", "shared-tool"));
    tree.put("shared/other.desktop", &app("Other", "other-tool"));
    let entries = scan(&[tree.path("own"), tree.path("shared")]);
    let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
    assert_eq!(names, ["Own", "Other"]);
}

#[test]
fn a_hidden_file_still_hides_the_same_id_further_down() {
    let tree = Tree::new();
    tree.put(
        "own/same.desktop",
        "[Desktop Entry]\nType=Application\nName=Gone\nExec=gone\nHidden=true\n",
    );
    tree.put("shared/same.desktop", &app("Shared", "shared-tool"));
    assert!(scan(&[tree.path("own"), tree.path("shared")]).is_empty());
}

#[test]
fn a_linked_file_is_read_and_a_linked_folder_is_not_followed() {
    let tree = Tree::new();
    let real = tree.put("elsewhere/real.desktop", &app("Real", "real"));
    fs::create_dir_all(tree.path("apps")).unwrap();
    symlink(&real, tree.path("apps/linked.desktop")).unwrap();
    // A loop: a link back to the folder itself.
    symlink(tree.path("apps"), tree.path("apps/loop")).unwrap();
    symlink(tree.path("elsewhere"), tree.path("apps/folder")).unwrap();
    let entries = scan(&[tree.path("apps")]);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "linked.desktop");
}

#[test]
fn a_big_file_and_a_file_that_is_not_text_are_left_out() {
    let tree = Tree::new();
    let big = format!(
        "{}# {}\n",
        app("Big", "big"),
        "x".repeat(usize::try_from(MAX_FILE_BYTES).unwrap())
    );
    tree.put("apps/big.desktop", &big);
    let bytes = tree.path("apps/binary.desktop");
    fs::create_dir_all(tree.path("apps")).unwrap();
    fs::write(bytes, [0xff, 0xfe, 0x00, b'[']).unwrap();
    tree.put("apps/fine.desktop", &app("Fine", "fine"));
    let entries = scan(&[tree.path("apps")]);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "Fine");
}

// -- the fix ------------------------------------------------------------------------------------

/// A machine with `google-chrome-stable` and `Telegram` installed, and a shell that finds them.
struct Machine {
    tree: Tree,
    shell: Shell,
}

impl Machine {
    fn new() -> Self {
        let tree = Tree::new();
        tree.program("bin/google-chrome-stable");
        tree.program("bin/Telegram");
        let shell = shell_with_path(&tree.text("bin"));
        Self { tree, shell }
    }
}

fn chrome_entries() -> Vec<Entry> {
    vec![
        entry(
            "google-chrome.desktop",
            &app("Google Chrome", "/usr/bin/google-chrome-stable %U"),
        ),
        entry(
            "com.google.Chrome.desktop",
            &app("Google Chrome", "google-chrome-stable %U"),
        ),
    ]
}

#[test]
fn two_entries_of_one_program_are_one_program() {
    let machine = Machine::new();
    let entries = vec![
        entry(
            "google-chrome.desktop",
            &app(
                "Google Chrome",
                &format!("{} %U", machine.tree.text("bin/google-chrome-stable")),
            ),
        ),
        entry(
            "com.google.Chrome.desktop",
            &app("Google Chrome", "google-chrome-stable %U"),
        ),
    ];
    let fix = find_fix(&machine.shell, &entries, "google-chrome").expect("one program");
    assert_eq!(fix.from, "google-chrome");
    // The first entry in precedence order says how the program is written.
    assert_eq!(fix.to, machine.tree.text("bin/google-chrome-stable"));
    assert_eq!(fix.app.as_deref(), Some("Google Chrome"));
}

#[test]
fn the_same_file_under_the_same_name_is_one_program() {
    let machine = Machine::new();
    // A second way to the same file: a folder that is a link to `bin`.
    symlink(machine.tree.path("bin"), machine.tree.path("bin2")).unwrap();
    let entries = vec![
        entry("a.desktop", &app("Google Chrome", "google-chrome-stable")),
        entry(
            "b.desktop",
            &app(
                "Google Chrome",
                &machine.tree.text("bin2/google-chrome-stable"),
            ),
        ),
    ];
    let fix = find_fix(&machine.shell, &entries, "google-chrome").expect("one program");
    // The first entry says how the program is written.
    assert_eq!(fix.to, "google-chrome-stable");
}

#[test]
fn the_same_file_under_two_names_is_two_programs() {
    // A multi-call binary does different things under different names, so one file is not enough.
    let machine = Machine::new();
    symlink(
        machine.tree.path("bin/google-chrome-stable"),
        machine.tree.path("bin/chrome-alias"),
    )
    .unwrap();
    let entries = vec![
        entry("a.desktop", &app("Google Chrome", "google-chrome-stable")),
        entry("b.desktop", &app("Google Chrome", "chrome-alias")),
    ];
    assert_eq!(find_fix(&machine.shell, &entries, "google-chrome"), None);
}

#[test]
fn the_one_entry_of_a_telegram_gives_its_program_and_name() {
    let machine = Machine::new();
    let text = "\
[Desktop Entry]
Type=Application
Name=Telegram
TryExec=Telegram
Exec=Telegram -- %U

[Desktop Action Quit]
Name=Quit
Exec=Telegram -quit
";
    let entries = vec![entry("org.telegram.desktop.desktop", text)];
    let fix = find_fix(&machine.shell, &entries, "telegram-desktop").expect("a fix");
    assert_eq!(
        fix,
        Fix {
            from: "telegram-desktop".to_owned(),
            to: "Telegram".to_owned(),
            app: Some("Telegram".to_owned())
        }
    );
}

#[test]
fn only_the_desktop_and_stable_endings_are_ignored_so_beta_and_bin_names_do_not_match() {
    let machine = Machine::new();
    machine.tree.program("bin/google-chrome-beta");
    machine.tree.program("bin/google-chrome-bin");
    let entries = vec![
        entry(
            "a.desktop",
            &app("Google Chrome Beta", "google-chrome-beta"),
        ),
        entry("b.desktop", &app("Google Chrome Bin", "google-chrome-bin")),
    ];
    assert_eq!(find_fix(&machine.shell, &entries, "google-chrome"), None);
    // The two endings that are ignored do match.
    let entries = vec![entry(
        "c.desktop",
        &app("Google Chrome", "google-chrome-stable"),
    )];
    assert!(find_fix(&machine.shell, &entries, "google-chrome-desktop").is_some());
}

#[test]
fn two_different_programs_are_not_a_fix() {
    let machine = Machine::new();
    machine.tree.program("bin/google-chrome-beta");
    let entries = vec![
        entry("a.desktop", &app("Google Chrome", "google-chrome-stable")),
        entry(
            "b.desktop",
            &app("Google Chrome Beta", "google-chrome-beta"),
        ),
        entry(
            "google-chrome.desktop",
            &app("Chrome (beta)", "google-chrome-beta"),
        ),
    ];
    assert_eq!(find_fix(&machine.shell, &entries, "google-chrome"), None);
}

#[test]
fn an_entry_for_a_program_that_is_not_installed_is_no_fix() {
    let machine = Machine::new();
    let entries = vec![entry(
        "gone.desktop",
        &app("Gone Tool", "gone-tool-not-installed"),
    )];
    assert_eq!(find_fix(&machine.shell, &entries, "gone-tool"), None);
    // The other entry of the name decides alone: the missing one does not make it ambiguous.
    let entries = vec![
        entry("a.desktop", &app("Google Chrome", "google-chrome-stable")),
        entry("b.desktop", &app("Google Chrome", "not-installed-chrome")),
    ];
    let fix = find_fix(&machine.shell, &entries, "google-chrome").expect("a fix");
    assert_eq!(fix.to, "google-chrome-stable");
}

#[test]
fn a_try_exec_that_is_not_there_leaves_the_entry_out() {
    let machine = Machine::new();
    let text = format!(
        "{}TryExec=/nonexistent/google-chrome-stable\n",
        app("Google Chrome", "google-chrome-stable")
    );
    let entries = vec![entry("a.desktop", &text)];
    assert_eq!(find_fix(&machine.shell, &entries, "google-chrome"), None);
    // With one that is there (found in PATH), it is used.
    let text = format!(
        "{}TryExec=google-chrome-stable\n",
        app("Google Chrome", "google-chrome-stable")
    );
    let entries = vec![entry("a.desktop", &text)];
    assert!(find_fix(&machine.shell, &entries, "google-chrome").is_some());
}

#[test]
fn a_name_with_nothing_in_it_matches_nothing() {
    let machine = Machine::new();
    assert_eq!(
        find_fix(&machine.shell, &chrome_entries(), "-desktop"),
        None
    );
    assert_eq!(find_fix(&machine.shell, &chrome_entries(), ""), None);
    assert_eq!(find_fix(&machine.shell, &[], "google-chrome"), None);
}

#[test]
fn a_function_of_the_same_name_is_not_a_program() {
    let mut machine = Machine::new();
    machine
        .shell
        .run_source("google-chrome-stable() { :; }", true, &mut Io::process());
    machine.tree.program("bin/other-tool");
    // The entry's program is a function of the shell and also a file; the file is what counts.
    let entries = vec![entry(
        "a.desktop",
        &app("Google Chrome", "google-chrome-stable"),
    )];
    assert!(find_fix(&machine.shell, &entries, "google-chrome").is_some());
    // A name that is only a function has no file: not a fix.
    let entries = vec![entry("b.desktop", &app("Fn Tool", "fn-only"))];
    machine
        .shell
        .run_source("fn-only() { :; }", true, &mut Io::process());
    assert_eq!(find_fix(&machine.shell, &entries, "fn-tool"), None);
}

// -- ranks: an entry that says the name comes before one that only runs a program of that name -

/// The shape of a real machine: Chrome has two entries of one program, and a web-app shortcut
/// (WhatsApp Web) of the user's own runs the browser's real binary, which is not in `PATH`.
fn browser_and_shortcut(machine: &Machine) -> Vec<Entry> {
    let t = &machine.tree;
    t.program("opt/google-chrome");
    let stable = t.text("bin/google-chrome-stable");
    let chrome = |extra: &str| {
        format!(
            "[Desktop Entry]\nType=Application\nName=Google Chrome\nExec={stable} %U\n{extra}\n\
             [Desktop Action new-window]\nName=New Window\nExec={stable}\n"
        )
    };
    t.put("share/google-chrome.desktop", &chrome(""));
    t.put(
        "share/com.google.Chrome.desktop",
        &chrome("NoDisplay=true\n"),
    );
    t.put(
        "own/chrome-hnpfjngllnobngcgfapefoaidbinmjnm-Default.desktop",
        &app(
            "WhatsApp Web",
            &format!(
                "{} --profile-directory=Default --app-id=hnpfjngllnobngcgfapefoaidbinmjnm",
                t.text("opt/google-chrome")
            ),
        ),
    );
    // The user's own folder comes first, as `$XDG_DATA_HOME` does.
    scan(&[t.path("own"), t.path("share")])
}

#[test]
fn a_web_app_shortcut_that_runs_the_browser_does_not_make_the_browser_ambiguous() {
    let machine = Machine::new();
    let entries = browser_and_shortcut(&machine);
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries[0].name, "WhatsApp Web",
        "the shortcut is the first entry"
    );
    let fix = find_fix(&machine.shell, &entries, "google-chrome").expect("a fix");
    assert_eq!(fix.to, machine.tree.text("bin/google-chrome-stable"));
    assert_eq!(fix.app.as_deref(), Some("Google Chrome"));
}

#[test]
fn a_fix_found_only_by_its_program_has_no_application_name() {
    let machine = Machine::new();
    let entries = browser_and_shortcut(&machine);
    // Only the shortcut: it is found by its program alone, and it is another application.
    let fix = find_fix(&machine.shell, &entries[..1], "google-chrome").expect("a fix");
    assert_eq!(fix.to, machine.tree.text("opt/google-chrome"));
    assert_eq!(fix.app, None);
}

#[test]
fn the_same_name_by_two_different_programs_is_ambiguous_whatever_the_order() {
    let machine = Machine::new();
    let entries = browser_and_shortcut(&machine);
    machine.tree.program("bin/google-chrome-beta");
    let other = entry("beta.desktop", &app("Google Chrome", "google-chrome-beta"));
    // Two entries that say the name, with two programs: not a fix, and the shortcut does not
    // break the tie.
    let mut both = entries.clone();
    both.push(other.clone());
    assert_eq!(find_fix(&machine.shell, &both, "google-chrome"), None);
    both.reverse();
    assert_eq!(find_fix(&machine.shell, &both, "google-chrome"), None);
}

#[test]
fn two_different_programs_that_only_run_the_name_are_ambiguous() {
    let machine = Machine::new();
    machine.tree.program("opt/google-chrome");
    machine.tree.program("opt2/google-chrome");
    let entries = vec![
        entry(
            "a.desktop",
            &app("One", &machine.tree.text("opt/google-chrome")),
        ),
        entry(
            "b.desktop",
            &app("Two", &machine.tree.text("opt2/google-chrome")),
        ),
    ];
    assert_eq!(find_fix(&machine.shell, &entries, "google-chrome"), None);
    // The same program under two shortcuts is one.
    let entries = vec![
        entry(
            "a.desktop",
            &app("One", &machine.tree.text("opt/google-chrome")),
        ),
        entry(
            "b.desktop",
            &app("Two", &machine.tree.text("opt/google-chrome")),
        ),
    ];
    assert!(find_fix(&machine.shell, &entries, "google-chrome").is_some());
}

#[test]
fn a_better_rank_after_an_ambiguous_worse_one_starts_again() {
    let machine = Machine::new();
    machine.tree.program("opt/google-chrome");
    machine.tree.program("opt2/google-chrome");
    let entries = vec![
        entry(
            "a.desktop",
            &app("One", &machine.tree.text("opt/google-chrome")),
        ),
        entry(
            "b.desktop",
            &app("Two", &machine.tree.text("opt2/google-chrome")),
        ),
        entry(
            "google-chrome.desktop",
            &app("Google Chrome", "google-chrome-stable"),
        ),
    ];
    let fix = find_fix(&machine.shell, &entries, "google-chrome").expect("a fix");
    assert_eq!(fix.to, "google-chrome-stable");
    assert_eq!(fix.app.as_deref(), Some("Google Chrome"));
}

#[test]
fn an_entry_that_says_the_name_but_cannot_be_used_leaves_the_next_rank_to_decide() {
    let machine = Machine::new();
    machine.tree.program("opt/google-chrome");
    let entries = vec![
        entry(
            "google-chrome.desktop",
            &app("Google Chrome", "not-installed-chrome"),
        ),
        entry(
            "a.desktop",
            &app("One", &machine.tree.text("opt/google-chrome")),
        ),
    ];
    let fix = find_fix(&machine.shell, &entries, "google-chrome").expect("a fix");
    assert_eq!(fix.to, machine.tree.text("opt/google-chrome"));
    assert_eq!(fix.app, None);
}

// -- folders written with a trailing slash, as `XDG_DATA_DIRS` often is --------------------------

#[test]
fn a_trailing_slash_in_a_folder_of_xdg_data_dirs_is_fine() {
    assert_eq!(
        dirs(Some("/home/u"), Some(""), Some("/a/:/b/")),
        [
            "/home/u/.local/share/applications",
            "/a/applications",
            "/b/applications"
        ]
    );
    // The value of a real machine: flatpak folders first, trailing slashes on the last two.
    assert_eq!(
        dirs(
            Some("/home/u"),
            Some(""),
            Some(
                "/home/u/.local/share/flatpak/exports/share:/var/lib/flatpak/exports/share:/usr/local/share/:/usr/share/"
            )
        ),
        [
            "/home/u/.local/share/applications",
            "/home/u/.local/share/flatpak/exports/share/applications",
            "/var/lib/flatpak/exports/share/applications",
            "/usr/local/share/applications",
            "/usr/share/applications"
        ]
    );
}

#[test]
fn entries_are_found_with_an_empty_xdg_data_home_and_slashes_at_the_end_of_xdg_data_dirs() {
    let tree = Tree::new();
    tree.put(
        "home/.local/share/applications/own.desktop",
        &app("Own", "own-tool"),
    );
    tree.put(
        "local/applications/local.desktop",
        &app("Local", "local-tool"),
    );
    tree.put(
        "share/applications/shared.desktop",
        &app("Shared", "shared-tool"),
    );
    let mut shell = Shell::new();
    shell.run_source(
        &format!(
            "HOME='{}' XDG_DATA_HOME= XDG_DATA_DIRS='{}/none1/:{}/none2/:{}/local/:{}/share/'",
            tree.text("home"),
            tree.text(""),
            tree.text(""),
            tree.text(""),
            tree.text("")
        ),
        true,
        &mut Io::process(),
    );
    let names: Vec<String> = scan(&dirs_of(&shell))
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert_eq!(names, ["Own", "Local", "Shared"]);
}
