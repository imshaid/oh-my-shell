//! The last few commands of this shell, for the AI context (blueprint §13.5).
//!
//! Kept in memory for the life of the shell, not read back from the history database: it is the
//! same with `--no-config`, it needs no wait for the history writer, and it is only what *this*
//! shell ran.
//!
//! A line the person asked not to remember (a `[history] ignore` pattern, for example one that
//! starts with a space) is left out. A line with a secret is kept here and **masked when the
//! context is built** (`omsh_ai::PlanContext::build`), so the model still sees that a command was
//! run. Output of commands is never kept.

use std::collections::VecDeque;

use omsh_ai::{RECENT_COMMANDS, RecentCommand};
use omsh_history::Filter;

pub struct Recent {
    items: VecDeque<RecentCommand>,
    filter: Filter,
    last: Option<Last>,
}

/// The last line that ended, for `??` and `Alt+?` (S24). Unlike the list of recent commands it
/// also remembers a line that the history ignores, so that "why did this fail" never explains
/// the command before it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Last {
    Ran {
        command: String,
        status: i32,
    },
    /// A line matching `[history] ignore`: it is not sent anywhere.
    Ignored,
}

impl Recent {
    /// `filter` is the history filter: its ignore patterns decide which lines are left out.
    #[must_use]
    pub fn new(filter: Filter) -> Self {
        Self {
            items: VecDeque::new(),
            filter,
            last: None,
        }
    }

    /// Notes that `command` ran and ended with `status`.
    pub fn record(&mut self, command: &str, status: i32) {
        let command = command.strip_suffix('\n').unwrap_or(command);
        if command.trim().is_empty() {
            return;
        }
        if self.filter.ignores(command) {
            self.last = Some(Last::Ignored);
            return;
        }
        self.last = Some(Last::Ran {
            command: command.to_owned(),
            status,
        });
        if self.items.len() == RECENT_COMMANDS {
            self.items.pop_front();
        }
        self.items.push_back(RecentCommand {
            command: command.to_owned(),
            status,
        });
    }

    /// The last line that ended (blank lines do not count).
    #[must_use]
    pub fn last(&self) -> Option<&Last> {
        self.last.as_ref()
    }

    /// The commands noted, oldest first.
    #[must_use]
    pub fn items(&self) -> Vec<RecentCommand> {
        self.items.iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(recent: &Recent) -> Vec<(String, i32)> {
        recent
            .items()
            .into_iter()
            .map(|item| (item.command, item.status))
            .collect()
    }

    #[test]
    fn keeps_the_last_three_in_order_with_their_status() {
        let mut recent = Recent::new(Filter::default());
        for (n, command) in ["a", "b", "c", "d"].into_iter().enumerate() {
            recent.record(command, n as i32);
        }
        assert_eq!(
            commands(&recent),
            [("b".into(), 1), ("c".into(), 2), ("d".into(), 3)]
        );
    }

    #[test]
    fn blank_lines_and_ignored_lines_are_left_out() {
        let mut recent = Recent::new(Filter::new(&[" *".to_owned(), "history".to_owned()]));
        recent.record("", 0);
        recent.record("   ", 0);
        recent.record(" secret thing", 0);
        recent.record("history", 0);
        recent.record("ls -l\n", 0);
        assert_eq!(commands(&recent), [("ls -l".into(), 0)]);
    }

    #[test]
    fn the_last_line_is_remembered_even_when_the_history_ignores_it() {
        let mut recent = Recent::new(Filter::new(&[" *".to_owned()]));
        assert_eq!(recent.last(), None);
        recent.record("make", 2);
        assert_eq!(
            recent.last(),
            Some(&Last::Ran {
                command: "make".into(),
                status: 2
            })
        );
        recent.record("", 0);
        recent.record("  \n", 0);
        assert!(matches!(recent.last(), Some(Last::Ran { command, .. }) if command == "make"));
        recent.record(" false", 1);
        assert_eq!(recent.last(), Some(&Last::Ignored));
        // The list of recent commands still has only what was not ignored.
        assert_eq!(commands(&recent), [("make".into(), 2)]);
        recent.record("ls\n", 0);
        assert_eq!(
            recent.last(),
            Some(&Last::Ran {
                command: "ls".into(),
                status: 0
            })
        );
    }

    #[test]
    fn a_line_with_a_secret_is_kept_to_be_masked_later() {
        let mut recent = Recent::new(Filter::default());
        recent.record("export API_KEY=abc123", 0);
        assert_eq!(commands(&recent), [("export API_KEY=abc123".into(), 0)]);
    }
}
