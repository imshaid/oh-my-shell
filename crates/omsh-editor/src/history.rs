//! The editor's view of the history database.
//!
//! `reedline` asks a [`History`] for what the Up arrow and the history search show. [`DbHistory`]
//! answers from the store of `omsh-history`. It does not write: the shell records each command
//! itself, with the directory, the status and the duration that the editor does not know.
//!
//! **A line that is not stored can be recalled once.** For a line the filter refuses (it
//! matches an ignore pattern, or it looks like a secret), [`History::save`] answers with an
//! error. `reedline` then keeps that line by itself, in memory, as "ran but not stored": the
//! next Up brings it back, and the next command replaces it. It is never written anywhere.

use std::time::Duration;

use chrono::DateTime;
use omsh_history::{Entry, Filter, Query, Store};
use reedline::{
    CommandLineSearch, History, HistoryItem, HistoryItemId, HistorySessionId, ReedlineError,
    ReedlineErrorVariants, Result, SearchDirection, SearchQuery,
};

/// The history as the editor sees it.
pub(crate) struct DbHistory {
    store: Store,
    filter: Filter,
}

impl DbHistory {
    pub(crate) fn new(store: Store, filter: Filter) -> Self {
        Self { store, filter }
    }

    /// The number of this shell, in the editor's type.
    pub(crate) fn session_id(&self) -> HistorySessionId {
        HistorySessionId::new(self.store.session())
    }
}

fn unsupported(feature: &'static str) -> ReedlineError {
    ReedlineError(ReedlineErrorVariants::HistoryFeatureUnsupported {
        history: "the history database",
        feature,
    })
}

fn to_item(entry: Entry) -> HistoryItem {
    HistoryItem {
        id: Some(HistoryItemId::new(entry.id)),
        start_timestamp: DateTime::from_timestamp_millis(entry.started_ms),
        command_line: entry.command,
        session_id: Some(HistorySessionId::new(entry.session)),
        hostname: Some(entry.host),
        cwd: Some(entry.cwd),
        duration: entry
            .duration_ms
            .and_then(|ms| u64::try_from(ms).ok())
            .map(Duration::from_millis),
        exit_status: entry.exit_status.map(i64::from),
        more_info: None,
    }
}

/// The editor's question in the store's terms.
///
/// The editor walks through history one entry at a time: "the newest entry below this id".
/// While it does, it wants to skip entries with the same text as the one it shows, so that
/// `ls`, `ls`, `ls` is one step and not three. It keeps that text in a field this crate cannot
/// read, so the same is done from the id: skip what reads like the entry the walk starts from.
fn to_query(asked: &SearchQuery) -> Result<Query> {
    let filter = &asked.filter;
    if filter.hostname.is_some() {
        return Err(unsupported("search by host"));
    }
    if filter.cwd_prefix.is_some() {
        return Err(unsupported("search by directory prefix"));
    }
    if filter.exit_successful == Some(true) {
        return Err(unsupported("search for successful commands"));
    }
    if filter.more_info_json.is_some() {
        return Err(unsupported("search by extra information"));
    }
    if asked.start_time.is_some() || asked.end_time.is_some() {
        return Err(unsupported("search by time"));
    }
    let backward = asked.direction == SearchDirection::Backward;
    let start = asked.start_id.map(|id| id.0);
    // The end is included, the start is not.
    let end = asked.end_id.map(|id| id.0);
    let (below_id, above_id) = if backward {
        (start, end.map(|id| id.saturating_sub(1)))
    } else {
        (end.map(|id| id.saturating_add(1)), start)
    };
    let mut query = Query {
        cwd: filter.cwd_exact.clone(),
        failed: filter.exit_successful == Some(false),
        unlike: start,
        own_or_older: filter.session.is_some(),
        below_id,
        above_id,
        limit: asked
            .limit
            .map(|limit| u32::try_from(limit.max(0)).unwrap_or(u32::MAX)),
        newest_first: backward,
        ..Query::default()
    };
    match &filter.command_line {
        Some(CommandLineSearch::Prefix(text)) => query.prefix = Some(text.clone()),
        Some(CommandLineSearch::Substring(text)) => query.contains = Some(text.clone()),
        Some(CommandLineSearch::Exact(text)) => query.exact = Some(text.clone()),
        None => {}
    }
    Ok(query)
}

impl History for DbHistory {
    fn save(&mut self, item: HistoryItem) -> Result<HistoryItem> {
        if self.filter.stores(&item.command_line) {
            // The shell writes it; the editor finds it in the store at the next prompt.
            Ok(item)
        } else {
            Err(ReedlineError(ReedlineErrorVariants::OtherHistoryError(
                "the line is not stored",
            )))
        }
    }

    fn load(&self, id: HistoryItemId) -> Result<HistoryItem> {
        self.store.get(id.0).map(to_item).ok_or(ReedlineError(
            ReedlineErrorVariants::OtherHistoryError("no such entry"),
        ))
    }

    fn count(&self, query: SearchQuery) -> Result<i64> {
        let count = self.store.count(&to_query(&query)?);
        Ok(i64::try_from(count).unwrap_or(i64::MAX))
    }

    fn search(&self, query: SearchQuery) -> Result<Vec<HistoryItem>> {
        Ok(self
            .store
            .query(&to_query(&query)?)
            .into_iter()
            .map(to_item)
            .collect())
    }

    fn update(
        &mut self,
        _id: HistoryItemId,
        _updater: &dyn Fn(HistoryItem) -> HistoryItem,
    ) -> Result<()> {
        Err(unsupported("changing an entry"))
    }

    fn clear(&mut self) -> Result<()> {
        Err(unsupported("removing everything"))
    }

    fn delete(&mut self, id: HistoryItemId) -> Result<()> {
        match self.store.delete(&[id.0]) {
            Some(_) => Ok(()),
            None => Err(ReedlineError(ReedlineErrorVariants::OtherHistoryError(
                "the history did not answer",
            ))),
        }
    }

    fn sync(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn session(&self) -> Option<HistorySessionId> {
        Some(self.session_id())
    }
}

#[cfg(test)]
mod tests {
    use reedline::SearchFilter;

    use super::*;

    const PATIENT: Duration = Duration::from_secs(10);

    fn history_of(commands: &[&str]) -> DbHistory {
        let store = Store::in_memory();
        for command in commands {
            let token = store.start(command, "/work");
            store.finish(
                token,
                i32::from(command.starts_with("false")),
                Duration::ZERO,
            );
        }
        assert!(store.settle(PATIENT));
        DbHistory::new(store, Filter::new(&[" *".to_owned()]))
    }

    /// What the editor's own cursor does for Up and Down (it is private to `reedline`): ask
    /// for one entry beyond the one shown, in the session, with the typed text as a prefix.
    struct Walk<'a> {
        history: &'a DbHistory,
        typed: &'a str,
        current: Option<HistoryItem>,
    }

    impl<'a> Walk<'a> {
        fn new(history: &'a DbHistory, typed: &'a str) -> Self {
            Self {
                history,
                typed,
                current: None,
            }
        }

        fn step(&mut self, direction: SearchDirection) -> Option<&str> {
            let forward = direction == SearchDirection::Forward;
            if forward && self.current.is_none() {
                return None;
            }
            let filter = if self.typed.is_empty() {
                SearchFilter::anything(self.history.session())
            } else {
                SearchFilter::from_text_search(
                    CommandLineSearch::Prefix(self.typed.to_owned()),
                    self.history.session(),
                )
            };
            let mut next = self
                .history
                .search(SearchQuery {
                    start_id: self.current.as_ref().and_then(|item| item.id),
                    end_id: None,
                    start_time: None,
                    end_time: None,
                    direction,
                    limit: Some(1),
                    filter,
                })
                .unwrap();
            if next.len() == 1 {
                self.current = next.pop();
            } else if forward {
                self.current = None;
            }
            self.current.as_ref().map(|item| item.command_line.as_str())
        }

        fn back(&mut self) -> Option<&str> {
            self.step(SearchDirection::Backward)
        }
    }

    /// Presses Up `times` times with `typed` in the line, and returns what the line shows
    /// after each.
    fn walk_back(history: &DbHistory, typed: &str, times: usize) -> Vec<Option<String>> {
        let mut walk = Walk::new(history, typed);
        (0..times).map(|_| walk.back().map(str::to_owned)).collect()
    }

    fn shown(steps: &[Option<String>]) -> Vec<&str> {
        steps.iter().map(|step| step.as_deref().unwrap()).collect()
    }

    #[test]
    fn up_walks_from_the_newest_command_to_the_oldest() {
        let history = history_of(&["one", "two", "three"]);
        assert_eq!(shown(&walk_back(&history, "", 3)), ["three", "two", "one"]);
        // At the oldest entry, Up stays there.
        assert_eq!(walk_back(&history, "", 5)[4].as_deref(), Some("one"));
    }

    #[test]
    fn up_passes_over_repeats_of_the_command_it_shows() {
        let history = history_of(&["make", "ls", "ls", "ls", "make", "make"]);
        assert_eq!(shown(&walk_back(&history, "", 3)), ["make", "ls", "make"]);
    }

    #[test]
    fn up_with_typed_text_finds_commands_that_start_with_it() {
        let history = history_of(&["git status", "ls", "git push", "echo git"]);
        assert_eq!(
            shown(&walk_back(&history, "git", 2)),
            ["git push", "git status"]
        );
        assert_eq!(walk_back(&history, "nothing", 1), [None]);
    }

    #[test]
    fn down_walks_forward_again() {
        let history = history_of(&["one", "two", "three"]);
        let mut walk = Walk::new(&history, "");
        for _ in 0..3 {
            walk.back();
        }
        assert_eq!(walk.step(SearchDirection::Forward), Some("two"));
        walk.step(SearchDirection::Forward);
        assert_eq!(
            walk.step(SearchDirection::Forward),
            None,
            "past the newest: the typed line"
        );
    }

    #[test]
    fn up_does_not_show_what_another_shell_ran_meanwhile() {
        /// Removes the directory when the test ends, also when it fails.
        struct Scratch(std::path::PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let dir = std::env::temp_dir().join(format!("omsh-editor-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let scratch = Scratch(dir);
        let path = scratch.0.join("history.db");
        let other = Store::open(&path, 1000);
        let token = other.start("older", "/");
        other.finish(token, 0, Duration::ZERO);
        assert!(other.settle(PATIENT));
        std::thread::sleep(Duration::from_millis(20));
        let mine = Store::open(&path, 1000);
        std::thread::sleep(Duration::from_millis(20));
        let token = other.start("elsewhere", "/");
        other.finish(token, 0, Duration::ZERO);
        let token = mine.start("mine", "/");
        mine.finish(token, 0, Duration::ZERO);
        assert!(other.settle(PATIENT) && mine.settle(PATIENT));
        let history = DbHistory::new(mine, Filter::default());
        assert_eq!(shown(&walk_back(&history, "", 2)), ["mine", "older"]);
        drop(history);
        drop(other);
    }

    #[test]
    fn the_search_finds_text_anywhere_in_a_command() {
        let history = history_of(&["git status", "ls", "echo git"]);
        let found = history
            .search(SearchQuery::all_that_contain_rev("git".to_owned()))
            .unwrap();
        let commands: Vec<&str> = found
            .iter()
            .map(|item| item.command_line.as_str())
            .collect();
        assert_eq!(commands, ["echo git", "git status"]);
        let item = &found[1];
        assert_eq!(item.cwd.as_deref(), Some("/work"));
        assert_eq!(item.exit_status, Some(0));
        assert_eq!(item.session_id, history.session());
        assert_eq!(
            history.load(item.id.unwrap()).unwrap().command_line,
            "git status"
        );
        assert!(history.load(HistoryItemId::new(999)).is_err());
        assert_eq!(history.count_all().unwrap(), 3);
    }

    #[test]
    fn the_end_of_a_range_is_included_and_the_start_is_not() {
        let history = history_of(&["a", "b", "c", "d"]);
        let ids: Vec<i64> = history
            .search(SearchQuery::everything(SearchDirection::Forward, None))
            .unwrap()
            .iter()
            .map(|item| item.id.unwrap().0)
            .collect();
        let range = |direction, start: usize, end: usize| {
            let found = history
                .search(SearchQuery {
                    direction,
                    start_time: None,
                    end_time: None,
                    start_id: Some(HistoryItemId::new(ids[start])),
                    end_id: Some(HistoryItemId::new(ids[end])),
                    limit: None,
                    filter: SearchFilter::anything(None),
                })
                .unwrap();
            found
                .into_iter()
                .map(|item| item.command_line)
                .collect::<Vec<_>>()
        };
        assert_eq!(range(SearchDirection::Forward, 0, 2), ["b", "c"]);
        assert_eq!(range(SearchDirection::Backward, 3, 1), ["c", "b"]);
    }

    #[test]
    fn saving_writes_nothing_and_refuses_a_line_that_is_not_stored() {
        let mut history = history_of(&["kept"]);
        let saved = history
            .save(HistoryItem::from_command_line("echo new"))
            .unwrap();
        assert_eq!(saved.id, None);
        // The refusal is what makes the editor keep the line for one recall.
        assert!(
            history
                .save(HistoryItem::from_command_line(" hidden"))
                .is_err()
        );
        assert!(
            history
                .save(HistoryItem::from_command_line("export API_KEY=abc"))
                .is_err()
        );
        assert_eq!(history.count_all().unwrap(), 1);
    }

    #[test]
    fn questions_the_store_cannot_answer_are_refused() {
        let history = history_of(&["a"]);
        let mut by_host = SearchFilter::anything(None);
        by_host.hostname = Some("h".to_owned());
        let mut successful = SearchFilter::anything(None);
        successful.exit_successful = Some(true);
        for filter in [by_host, successful] {
            assert!(
                history
                    .search(SearchQuery::last_with_search(filter))
                    .is_err()
            );
        }
        let mut history = history;
        assert!(history.clear().is_err());
        assert!(history.update(HistoryItemId::new(1), &|item| item).is_err());
        assert!(history.sync().is_ok());
    }

    #[test]
    fn failed_commands_and_a_directory_can_be_asked_for() {
        let history = history_of(&["true", "false x", "ls"]);
        let mut failed = SearchFilter::anything(None);
        failed.exit_successful = Some(false);
        let found = history
            .search(SearchQuery::last_with_search(failed))
            .unwrap();
        assert_eq!(found[0].command_line, "false x");
        let elsewhere = SearchFilter::from_text_search_cwd(
            "/elsewhere".to_owned(),
            CommandLineSearch::Prefix(String::new()),
            None,
        );
        assert!(
            history
                .search(SearchQuery::last_with_search(elsewhere))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn an_entry_can_be_removed() {
        let mut history = history_of(&["a", "b"]);
        let first = history
            .search(SearchQuery::everything(SearchDirection::Forward, None))
            .unwrap()[0]
            .id
            .unwrap();
        history.delete(first).unwrap();
        assert_eq!(history.count_all().unwrap(), 1);
    }
}
