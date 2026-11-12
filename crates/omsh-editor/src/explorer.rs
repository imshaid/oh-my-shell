//! The bridge to the history explorer (`Ctrl+R`): the list of commands comes from the history
//! store, the view itself is in `omsh-ui`.

use std::time::Duration;

use omsh_history::{Distinct, EXPLORER_WINDOW, NotReady, Store};
use omsh_ui::explorer::Item;

/// How long to wait for the history writer before the list is read, so that the command that
/// just ran is in it. The same short wait the hinter makes for its first question.
const SETTLE: Duration = Duration::from_millis(50);

/// What the view is opened with.
pub(crate) struct Listing {
    pub items: Vec<Item>,
    /// The history could not be read (the database is not open, or the question failed).
    pub unavailable: bool,
    pub window: usize,
}

/// The newest [`EXPLORER_WINDOW`] runs of every shell, one item for each distinct command, the
/// newest first.
pub(crate) fn listing(store: &Store) -> Listing {
    let _ = store.settle(SETTLE);
    from_rows(store.distinct_recent(EXPLORER_WINDOW))
}

/// The list for what the store answered; "not ready" is the one case that says the history
/// cannot be read, so that the empty list does not claim there is nothing in it.
fn from_rows(rows: Result<Vec<Distinct>, NotReady>) -> Listing {
    match rows {
        Ok(rows) => Listing {
            items: rows.into_iter().map(item).collect(),
            unavailable: false,
            window: EXPLORER_WINDOW,
        },
        Err(NotReady) => Listing {
            items: Vec::new(),
            unavailable: true,
            window: EXPLORER_WINDOW,
        },
    }
}

fn item(row: Distinct) -> Item {
    Item::new(
        row.command,
        row.cwd,
        row.exit_status,
        row.started_ms,
        row.runs,
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn record(store: &Store, command: &str, cwd: &str, status: i32) {
        let token = store.start(command, cwd);
        store.finish(token, status, Duration::from_millis(3));
    }

    #[test]
    fn the_list_has_one_item_for_each_command_newest_first() {
        let store = Store::in_memory();
        record(&store, "ls", "/a", 0);
        record(&store, "make", "/a", 2);
        record(&store, "ls", "/b", 0);
        // Not settled by the test: the listing waits for the writer itself.
        let listing = listing(&store);
        assert!(!listing.unavailable);
        assert_eq!(listing.window, EXPLORER_WINDOW);
        let commands: Vec<_> = listing.items.iter().map(|i| i.command.as_str()).collect();
        assert_eq!(commands, ["ls", "make"]);
        // The newest run of `ls` is the one in /b; `make` failed.
        assert_eq!(listing.items[0].cwd, "/b");
        assert_eq!(listing.items[0].runs, 2);
        assert!(listing.items[1].failed());
    }

    #[test]
    fn a_history_that_cannot_be_read_is_said_to_be_unavailable() {
        let listing = from_rows(Err(NotReady));
        assert!(listing.unavailable);
        assert!(listing.items.is_empty());
        assert_eq!(listing.window, EXPLORER_WINDOW);
        // An empty history that can be read is not "unavailable".
        let empty = from_rows(Ok(Vec::new()));
        assert!(!empty.unavailable && empty.items.is_empty());
    }
}
