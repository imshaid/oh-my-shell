//! `omsh tour`: a one-minute walk through what the shell does (§15.5).
//!
//! Six short pages of plain text. On a terminal each page waits for a key (Enter or `n` for the
//! next page, `b` for the one before, `q` or Esc to leave), so a person reads at their own speed;
//! anywhere else (a pipe, a file) all pages are printed one after the other. The tour only
//! prints: it runs no command, reads no file, and makes no network request, so it also works
//! with `--no-ai` and `--no-config`.

use omsh_exec::{Key, ReadError, Terminal};

/// One page: a title and the lines under it.
struct Page {
    title: &'static str,
    lines: &'static [&'static str],
}

const PAGES: &[Page] = &[
    Page {
        title: "Typing",
        lines: &[
            "Commands are colored as you type: a command that is not found turns red.",
            "Gray text after the cursor is a hint from your history. Right or End takes",
            "all of it, Alt+F takes one word.",
            "Tab completes commands, paths, variables and git names, and lists choices.",
            "An abbreviation from [editor.abbreviations] expands when you press space.",
        ],
    },
    Page {
        title: "History and folders",
        lines: &[
            "Ctrl+R opens the history explorer: type to search, Enter puts the command in",
            "the line (it does not run it). Ctrl+R again switches between all commands,",
            "this folder and failed ones.",
            "`history search WORDS` lists older commands. `j proj` jumps to the folder",
            "that best matches the words, and `j -` goes back.",
        ],
    },
    Page {
        title: "Palette and settings",
        lines: &[
            "Ctrl+P opens the command palette: actions, aliases, abbreviations, functions",
            "and builtins with a line of help. Picking one puts text in the line.",
            "The Settings entry shows every setting and changes switches and choices;",
            "a change is written to config.toml and applies at the next start.",
            "Colors live in themes/NAME.toml next to config.toml: [ui] theme = \"NAME\".",
        ],
    },
    Page {
        title: "Safety",
        lines: &[
            "A risky line (rm -rf, chmod -R, dd, ...) shows what it would touch and asks",
            "before it runs. Answer t to move the files to the trash instead of deleting.",
            "`undo` puts back what the trash holds; `undo N` picks a specific deletion.",
            "Lines that look like they hold a secret are never stored in the history.",
        ],
    },
    Page {
        title: "The AI layer (off until you set it up)",
        lines: &[
            "Nothing is sent anywhere until you choose a provider: `omsh ai setup`.",
            "`? request` plans commands and asks before each one runs.",
            "`?? command` explains a command; `??` alone and Alt+? explain a failure.",
            "`cmd | ? question` asks about a command's output. `? history ...` searches.",
            "`omsh ai status` shows what is on. `--no-ai` turns the layer off.",
        ],
    },
    Page {
        title: "Notifications and where things are",
        lines: &[
            "A command that ran 10 seconds or more ends with a desktop notification (it",
            "needs notify-send and a desktop session). [notify] in config.toml sets it.",
            "config.toml and init.omsh (your aliases, exports and functions) are in your",
            "config folder, usually ~/.config/oh-my-shell.",
            "`omsh tour` shows this tour again.",
        ],
    },
];

/// The three lines shown once, when the first-run `config.toml` has just been written (§15.5).
/// `home` shortens the path to `~/...`.
#[must_use]
pub fn welcome(
    config_file: Option<&std::path::Path>,
    home: Option<&std::path::Path>,
) -> [String; 3] {
    let settings = match config_file {
        Some(file) => {
            let shown = home
                .and_then(|home| file.strip_prefix(home).ok())
                .map_or_else(
                    || file.display().to_string(),
                    |rest| format!("~/{}", rest.display()),
                );
            format!("Welcome to Oh My Shell. Settings: {shown}")
        }
        None => "Welcome to Oh My Shell.".to_owned(),
    };
    [
        settings,
        "Ctrl+R searches history, Ctrl+P opens the palette, `undo` restores deletions.".to_owned(),
        "Type `omsh tour` for a one-minute tour. This message is shown once.".to_owned(),
    ]
}

/// What a key does in the tour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Move {
    Next,
    Back,
    Quit,
    /// Not a key the tour uses: ask again.
    Stay,
}

fn move_for(key: Key) -> Move {
    match key {
        Key::Enter | Key::Char('n' | 'N' | ' ') => Move::Next,
        Key::Char('b' | 'B') => Move::Back,
        Key::Escape | Key::Char('q' | 'Q') => Move::Quit,
        Key::Char(_) => Move::Stay,
    }
}

/// The text of page `index` (counting from 0).
fn render(index: usize) -> String {
    let page = &PAGES[index];
    let mut text = format!(
        "Oh My Shell tour ({} of {}): {}\n\n",
        index + 1,
        PAGES.len(),
        page.title
    );
    for line in page.lines {
        text.push_str("  ");
        text.push_str(line);
        text.push('\n');
    }
    text
}

/// The exit status of Ctrl+C: 128 + `SIGINT`.
const CANCELLED: u8 = 130;

/// Runs the tour. Returns the exit status of `omsh tour`.
pub fn run(terminal: &mut dyn Terminal) -> u8 {
    if !terminal.input_is_terminal() {
        for index in 0..PAGES.len() {
            if index > 0 {
                terminal.out("\n");
            }
            terminal.out(&render(index));
        }
        return 0;
    }
    let mut index = 0;
    loop {
        terminal.out(&render(index));
        let last = index + 1 == PAGES.len();
        let prompt = format!(
            "\n[Enter] {}   [b] back   [q] quit ",
            if last { "finish" } else { "next" }
        );
        loop {
            match terminal.read_key(&prompt) {
                Ok(key) => match move_for(key) {
                    Move::Next if last => {
                        terminal.err("\n");
                        return 0;
                    }
                    Move::Next => index += 1,
                    Move::Back => index = index.saturating_sub(1),
                    Move::Quit => {
                        terminal.err("\n");
                        return 0;
                    }
                    Move::Stay => continue,
                },
                Err(ReadError::Interrupted) => {
                    terminal.err("\n");
                    return CANCELLED;
                }
                Err(ReadError::EndOfInput) => {
                    terminal.err("\n");
                    return 0;
                }
                Err(ReadError::Failed) => return 1,
            }
            terminal.err("\n");
            terminal.out("\n");
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// The widest line of a page, in characters: with the two-blank indent it fits an 80-column
    /// window.
    const WIDTH: usize = 76;
    /// The most lines under a title.
    const HEIGHT: usize = 8;

    /// Scripted keys; remembers what was printed.
    struct Script {
        terminal: bool,
        keys: VecDeque<Result<Key, ReadError>>,
        out: String,
        err: String,
        asked: usize,
    }

    impl Script {
        fn new(terminal: bool, keys: Vec<Result<Key, ReadError>>) -> Self {
            Self {
                terminal,
                keys: keys.into(),
                out: String::new(),
                err: String::new(),
                asked: 0,
            }
        }
    }

    impl Terminal for Script {
        fn input_is_terminal(&self) -> bool {
            self.terminal
        }
        fn out(&mut self, text: &str) {
            self.out.push_str(text);
        }
        fn err(&mut self, text: &str) {
            self.err.push_str(text);
        }
        fn read_line(
            &mut self,
            _prompt: &str,
            _hidden: bool,
            _line: &mut Vec<u8>,
        ) -> Result<(), ReadError> {
            Err(ReadError::EndOfInput)
        }
        fn read_key(&mut self, _prompt: &str) -> Result<Key, ReadError> {
            self.asked += 1;
            self.keys.pop_front().unwrap_or(Err(ReadError::EndOfInput))
        }
        fn interrupted(&self) -> bool {
            false
        }
    }

    fn enters(n: usize) -> Vec<Result<Key, ReadError>> {
        vec![Ok(Key::Enter); n]
    }

    fn titles_seen(out: &str) -> Vec<usize> {
        (1..=PAGES.len())
            .filter(|n| out.contains(&format!("({n} of {})", PAGES.len())))
            .collect()
    }

    #[test]
    fn the_welcome_is_three_lines_and_names_the_config_file_with_a_short_home() {
        let lines = welcome(
            Some(std::path::Path::new(
                "/home/me/.config/oh-my-shell/config.toml",
            )),
            Some(std::path::Path::new("/home/me")),
        );
        assert_eq!(
            lines[0],
            "Welcome to Oh My Shell. Settings: ~/.config/oh-my-shell/config.toml"
        );
        assert!(
            lines[1].contains("Ctrl+R") && lines[1].contains("Ctrl+P") && lines[1].contains("undo")
        );
        assert!(lines[2].contains("omsh tour") && lines[2].contains("once"));
        for line in &lines {
            assert!(line.chars().count() <= 80, "{line}");
            assert!(!line.contains('\n'));
        }
    }

    #[test]
    fn a_config_file_outside_home_is_shown_in_full_and_none_is_left_out() {
        let lines = welcome(
            Some(std::path::Path::new("/etc/x/config.toml")),
            Some(std::path::Path::new("/home/me")),
        );
        assert!(lines[0].contains("/etc/x/config.toml"), "{}", lines[0]);
        assert_eq!(welcome(None, None)[0], "Welcome to Oh My Shell.");
    }

    #[test]
    fn there_are_six_pages_that_fit_a_small_window() {
        assert_eq!(PAGES.len(), 6);
        for (i, page) in PAGES.iter().enumerate() {
            assert!(
                page.lines.len() <= HEIGHT,
                "page {i} has {} lines",
                page.lines.len()
            );
            for line in page.lines {
                assert!(
                    line.chars().count() <= WIDTH,
                    "page {i}: {} characters: {line}",
                    line.chars().count()
                );
            }
        }
    }

    #[test]
    fn the_pages_say_only_what_the_shell_can_do() {
        let all: String = PAGES
            .iter()
            .flat_map(|p| p.lines.iter())
            .copied()
            .collect::<Vec<_>>()
            .join("\n");
        for needle in [
            "Ctrl+R",
            "Ctrl+P",
            "omsh ai setup",
            "undo",
            "notify-send",
            "omsh tour",
        ] {
            assert!(all.contains(needle), "{needle}");
        }
        // The tour never promises to run anything for the person.
        assert!(!all.to_lowercase().contains("sudo"));
    }

    #[test]
    fn enter_walks_through_every_page_in_order_and_ends_with_status_0() {
        let mut t = Script::new(true, enters(6));
        assert_eq!(run(&mut t), 0);
        assert_eq!(titles_seen(&t.out), [1, 2, 3, 4, 5, 6]);
        assert_eq!(t.asked, 6);
    }

    #[test]
    fn b_goes_back_one_page_and_stays_on_the_first() {
        let mut keys = vec![
            Ok(Key::Char('b')),
            Ok(Key::Enter),
            Ok(Key::Enter),
            Ok(Key::Char('b')),
        ];
        keys.push(Ok(Key::Char('q')));
        let mut t = Script::new(true, keys);
        assert_eq!(run(&mut t), 0);
        // 1, (b stays on 1), 2, 3, (b back to 2), then quit.
        let order: Vec<usize> = t
            .out
            .match_indices("tour (")
            .map(|(at, _)| {
                t.out[at + 6..]
                    .chars()
                    .next()
                    .unwrap()
                    .to_digit(10)
                    .unwrap() as usize
            })
            .collect();
        assert_eq!(order, [1, 1, 2, 3, 2]);
    }

    #[test]
    fn q_and_escape_leave_at_once() {
        for key in [Key::Char('q'), Key::Escape] {
            let mut t = Script::new(true, vec![Ok(key)]);
            assert_eq!(run(&mut t), 0);
            assert_eq!(titles_seen(&t.out), [1]);
        }
    }

    #[test]
    fn a_key_the_tour_does_not_use_asks_again_without_moving() {
        let mut t = Script::new(
            true,
            vec![Ok(Key::Char('x')), Ok(Key::Char('7')), Ok(Key::Char('q'))],
        );
        assert_eq!(run(&mut t), 0);
        assert_eq!(titles_seen(&t.out), [1]);
        assert_eq!(t.asked, 3);
        assert_eq!(
            t.out.matches("tour (1 of 6)").count(),
            1,
            "the page is not printed again"
        );
    }

    #[test]
    fn ctrl_c_gives_130_and_end_of_input_gives_0() {
        let mut t = Script::new(true, vec![Err(ReadError::Interrupted)]);
        assert_eq!(run(&mut t), 130);
        let mut t = Script::new(true, vec![Ok(Key::Enter), Err(ReadError::EndOfInput)]);
        assert_eq!(run(&mut t), 0);
        let mut t = Script::new(true, vec![Err(ReadError::Failed)]);
        assert_eq!(run(&mut t), 1);
    }

    #[test]
    fn without_a_terminal_every_page_is_printed_and_no_key_is_read() {
        let mut t = Script::new(false, Vec::new());
        assert_eq!(run(&mut t), 0);
        assert_eq!(titles_seen(&t.out), [1, 2, 3, 4, 5, 6]);
        assert_eq!(t.asked, 0);
        assert!(t.err.is_empty());
    }

    #[test]
    fn keys_are_mapped_as_the_prompt_says() {
        assert_eq!(move_for(Key::Enter), Move::Next);
        assert_eq!(move_for(Key::Char('n')), Move::Next);
        assert_eq!(move_for(Key::Char('b')), Move::Back);
        assert_eq!(move_for(Key::Char('q')), Move::Quit);
        assert_eq!(move_for(Key::Escape), Move::Quit);
        assert_eq!(move_for(Key::Char('z')), Move::Stay);
    }
}
