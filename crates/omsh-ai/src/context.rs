//! The context that goes to the model with a request (blueprint §13.5), and the one place
//! where it is made safe to send.
//!
//! The caller collects the raw facts ([`Gathered`]); [`PlanContext::build`] decides what may go
//! out. For every item: the `[ai.context]` switch is looked at again (the caller also does not
//! collect what is off, this is the second guard), every text is masked with the rules of
//! `omsh-secrets` (§14.4), control characters are replaced (a name with an escape code must not
//! reach the model's prompt or the terminal that shows it), and sizes are limited.
//!
//! Masking comes **before** shortening: cutting a key in half could leave a prefix too short for
//! the rules to see.

use omsh_config::AiContext;
use omsh_secrets::mask_counting;

/// How many of the last commands are sent (§13.5).
pub const RECENT_COMMANDS: usize = 3;
/// A command is shown with at most this many lines of at most this many characters.
const COMMAND_LINES: usize = 3;
const LINE_CHARS: usize = 200;
/// Paths, names and branches are cut at this length.
const NAME_CHARS: usize = 300;

/// Where the working directory is with respect to git.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitState {
    NotARepository,
    /// On a branch.
    Branch(String),
    /// Detached: the first characters of the commit.
    Detached(String),
}

/// A command that ran in this shell, and how it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecentCommand {
    pub command: String,
    pub status: i32,
}

/// What the caller knows, before anything is checked. `None` and an empty list mean "not
/// collected".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Gathered {
    pub os: Option<String>,
    pub cwd: Option<String>,
    pub git: Option<GitState>,
    /// Oldest first.
    pub recent: Vec<RecentCommand>,
}

/// What the caller lets the model know besides the request. Built by [`PlanContext::build`];
/// every text in it is already masked and clean. `None` means "not sent".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanContext {
    /// `PRETTY_NAME` of the OS, e.g. `Ubuntu 26.04 LTS`.
    pub os: Option<String>,
    /// The shell's working directory.
    pub cwd: Option<String>,
    pub git: Option<GitState>,
    /// The last commands of this shell, oldest first.
    pub recent: Vec<RecentCommand>,
    /// True if masking replaced something in the context.
    pub masked: bool,
}

impl PlanContext {
    /// The context that may be sent: only what `allowed` lets through, masked, clean and short.
    #[must_use]
    pub fn build(gathered: Gathered, allowed: &AiContext) -> Self {
        let mut masked = false;
        let mut name = |text: &str| -> String {
            let (text, hid) = mask_counting(text);
            masked |= hid;
            single_line(&text, NAME_CHARS)
        };
        let os = gathered.os.filter(|_| allowed.os).map(|os| name(&os));
        let cwd = gathered.cwd.filter(|_| allowed.cwd).map(|cwd| name(&cwd));
        let git = gathered.git.filter(|_| allowed.git).map(|git| match git {
            GitState::NotARepository => GitState::NotARepository,
            GitState::Branch(branch) => GitState::Branch(name(&branch)),
            GitState::Detached(commit) => GitState::Detached(name(&commit)),
        });
        let mut recent = Vec::new();
        if allowed.recent_commands {
            let skip = gathered.recent.len().saturating_sub(RECENT_COMMANDS);
            for item in gathered.recent.into_iter().skip(skip) {
                let (command, hid) = mask_counting(&item.command);
                masked |= hid;
                recent.push(RecentCommand {
                    command: shorten_command(&command),
                    status: item.status,
                });
            }
        }
        Self {
            os,
            cwd,
            git,
            recent,
            masked,
        }
    }

    /// The context lines of the system prompt, ready to append.
    #[must_use]
    pub fn lines(&self) -> String {
        let mut out = String::new();
        if let Some(os) = &self.os {
            out.push_str(&format!("- OS: {os}\n"));
        }
        if let Some(cwd) = &self.cwd {
            out.push_str(&format!("- Working directory: {cwd}\n"));
        }
        match &self.git {
            None => {}
            Some(GitState::NotARepository) => out.push_str("- Git: not inside a repository\n"),
            Some(GitState::Branch(branch)) => {
                out.push_str(&format!("- Git: inside a repository, on branch {branch}\n"));
            }
            Some(GitState::Detached(commit)) => {
                out.push_str(&format!(
                    "- Git: inside a repository, detached at {commit}\n"
                ));
            }
        }
        if !self.recent.is_empty() {
            out.push_str(
                "- Recent commands in this shell, oldest first (their output is never sent):\n",
            );
            for item in &self.recent {
                let mut lines = item.command.lines();
                out.push_str("  - ");
                out.push_str(lines.next().unwrap_or_default());
                for more in lines {
                    out.push_str("\n    ");
                    out.push_str(more);
                }
                out.push_str(&format!("  (exit {})\n", item.status));
            }
        }
        out
    }
}

/// `text` on one line, control characters replaced, at most `max` characters.
fn single_line(text: &str, max: usize) -> String {
    let clean: String = text
        .chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect();
    cut(&clean, max)
}

fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str("...");
    out
}

/// At most [`COMMAND_LINES`] lines of [`LINE_CHARS`] characters; only line feeds and tabs
/// survive among the control characters.
fn shorten_command(text: &str) -> String {
    let mut lines = text.trim_end().lines();
    let mut out: Vec<String> = Vec::new();
    for line in lines.by_ref().take(COMMAND_LINES) {
        let clean: String = line
            .chars()
            .map(|c| {
                if c == '\t' {
                    ' '
                } else if c.is_control() {
                    '\u{fffd}'
                } else {
                    c
                }
            })
            .collect();
        out.push(cut(&clean, LINE_CHARS));
    }
    if lines.next().is_some() {
        out.push("...".to_owned());
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_on() -> AiContext {
        AiContext::default()
    }

    fn off() -> AiContext {
        AiContext {
            os: false,
            cwd: false,
            git: false,
            recent_commands: false,
        }
    }

    fn recent(command: &str, status: i32) -> RecentCommand {
        RecentCommand {
            command: command.to_owned(),
            status,
        }
    }

    fn everything() -> Gathered {
        Gathered {
            os: Some("Ubuntu 26.04 LTS".into()),
            cwd: Some("/home/me/work".into()),
            git: Some(GitState::Branch("main".into())),
            recent: vec![recent("ls", 0), recent("false", 1)],
        }
    }

    #[test]
    fn everything_allowed_is_everything_sent_in_order() {
        let context = PlanContext::build(everything(), &all_on());
        assert!(!context.masked);
        assert_eq!(
            context.lines(),
            "- OS: Ubuntu 26.04 LTS\n\
             - Working directory: /home/me/work\n\
             - Git: inside a repository, on branch main\n\
             - Recent commands in this shell, oldest first (their output is never sent):\n  \
             - ls  (exit 0)\n  - false  (exit 1)\n"
        );
    }

    #[test]
    fn each_switch_removes_exactly_its_own_line() {
        type Switch = fn(&mut AiContext);
        let cases: [(Switch, &str); 4] = [
            (|c| c.os = false, "OS:"),
            (|c| c.cwd = false, "Working directory"),
            (|c| c.git = false, "Git:"),
            (|c| c.recent_commands = false, "Recent commands"),
        ];
        for (turn_off, gone) in cases {
            let mut allowed = all_on();
            turn_off(&mut allowed);
            let lines = PlanContext::build(everything(), &allowed).lines();
            assert!(!lines.contains(gone), "{gone} stayed: {lines}");
            for other in ["OS:", "Working directory", "Git:", "Recent commands"] {
                if other != gone {
                    assert!(
                        lines.contains(other),
                        "{other} vanished with {gone}: {lines}"
                    );
                }
            }
        }
        assert_eq!(PlanContext::build(everything(), &off()).lines(), "");
    }

    #[test]
    fn nothing_gathered_sends_nothing_even_when_allowed() {
        assert_eq!(
            PlanContext::build(Gathered::default(), &all_on()),
            PlanContext::default()
        );
    }

    #[test]
    fn git_lines() {
        let state = |git| {
            PlanContext::build(
                Gathered {
                    git: Some(git),
                    ..Gathered::default()
                },
                &all_on(),
            )
            .lines()
        };
        assert_eq!(
            state(GitState::NotARepository),
            "- Git: not inside a repository\n"
        );
        assert_eq!(
            state(GitState::Detached("abc1234".into())),
            "- Git: inside a repository, detached at abc1234\n"
        );
    }

    #[test]
    fn every_field_is_masked() {
        let key = format!("ghp_{}", "a1B2".repeat(9));
        let gathered = Gathered {
            os: Some(format!("OS API_KEY={key}")),
            cwd: Some(format!("/tmp/{key}")),
            git: Some(GitState::Branch(format!("fix/{key}"))),
            recent: vec![recent(&format!("export GITHUB_TOKEN={key}"), 0)],
        };
        let context = PlanContext::build(gathered, &all_on());
        assert!(context.masked);
        let lines = context.lines();
        assert!(!lines.contains(&key), "{lines}");
        assert!(lines.contains("[MASKED]"), "{lines}");
        assert!(
            lines.contains("export GITHUB_TOKEN=[MASKED]  (exit 0)"),
            "{lines}"
        );
        assert!(lines.contains("/tmp/[MASKED]"), "{lines}");
    }

    #[test]
    fn a_switch_that_is_off_keeps_a_secret_out_and_does_not_count_as_masked() {
        let gathered = Gathered {
            recent: vec![recent("API_KEY=abc123 run", 0)],
            ..Gathered::default()
        };
        let allowed = AiContext {
            recent_commands: false,
            ..all_on()
        };
        let context = PlanContext::build(gathered, &allowed);
        assert!(!context.masked);
        assert_eq!(context.lines(), "");
    }

    #[test]
    fn a_key_is_masked_before_a_long_command_is_cut() {
        // The key starts before the cut and ends after it: cut first and 30 characters of it
        // would be left too short for the rules to see.
        let key = format!("ghp_{}", "x".repeat(40));
        let command = format!("curl -H '{}' {key} more", "a".repeat(LINE_CHARS - 12));
        let context = PlanContext::build(
            Gathered {
                recent: vec![recent(&command, 0)],
                ..Gathered::default()
            },
            &all_on(),
        );
        assert!(!context.lines().contains("ghp_"), "{}", context.lines());
    }

    #[test]
    fn control_characters_cannot_reach_the_prompt() {
        let context = PlanContext::build(
            Gathered {
                os: Some("Evil\x1b[2JOS\r\n".into()),
                cwd: Some("/tmp/a\x07b\nc".into()),
                git: Some(GitState::Branch("main\x1b]0;x\x07".into())),
                recent: vec![recent("echo a\x1b[31m\tb\nsecond\x00", 0)],
            },
            &all_on(),
        );
        let lines = context.lines();
        assert!(
            !lines.chars().any(|c| c.is_control() && c != '\n'),
            "{lines:?}"
        );
        assert!(
            lines.contains("- OS: Evil\u{fffd}[2JOS\u{fffd}\u{fffd}\n"),
            "{lines:?}"
        );
        assert!(lines.contains("/tmp/a\u{fffd}b\u{fffd}c\n"), "{lines:?}");
        // A command keeps its own line break, indented under its entry.
        assert!(
            lines.contains("echo a\u{fffd}[31m b\n    second\u{fffd}  (exit 0)\n"),
            "{lines:?}"
        );
    }

    #[test]
    fn only_the_last_three_commands_and_short_ones() {
        let many: Vec<RecentCommand> = (0..6).map(|n| recent(&format!("cmd{n}"), n)).collect();
        let context = PlanContext::build(
            Gathered {
                recent: many,
                ..Gathered::default()
            },
            &all_on(),
        );
        let commands: Vec<&str> = context.recent.iter().map(|r| r.command.as_str()).collect();
        assert_eq!(commands, ["cmd3", "cmd4", "cmd5"]);

        let long_line = "x".repeat(LINE_CHARS * 2);
        let tall = format!("a\nb\nc\nd\ne\n{long_line}");
        assert_eq!(shorten_command(&tall), "a\nb\nc\n...");
        let shown = shorten_command(&long_line);
        assert_eq!(shown.chars().count(), LINE_CHARS + 3);
        assert!(shown.ends_with("..."));
    }

    #[test]
    fn long_names_are_cut() {
        let context = PlanContext::build(
            Gathered {
                cwd: Some("d".repeat(NAME_CHARS * 2)),
                ..Gathered::default()
            },
            &all_on(),
        );
        assert_eq!(context.cwd.unwrap().chars().count(), NAME_CHARS + 3);
    }
}
