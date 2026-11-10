//! The `history` builtin, and the shell's record of what it runs.
//!
//! ```text
//! history [--here] [--failed] [--since AGE] [-n COUNT | --all] [search TEXT...]
//! history delete ID...
//! ```
//!
//! The executor knows the builtin only as a hook (`omsh_exec::HistoryCommand`); this module is
//! what the hook runs, on the store of `omsh-history`.

use std::fmt::Write as _;
use std::time::Duration;

use chrono::{Local, TimeZone};
use omsh_exec::{BuiltinOutput, HistoryCommand};
use omsh_history::{Entry, Filter, Query, SETTLE, Store, Token, now_ms};

/// How many commands plain `history` shows on a terminal.
const ON_A_TERMINAL: u32 = 100;

/// How long the shell waits for the last writes when it ends.
const CLOSE: Duration = Duration::from_secs(1);

const USAGE: &str = "\
usage: history [--here] [--failed] [--since AGE] [-n COUNT | --all] [search TEXT...]
       history delete ID...
  --here       only commands run in this directory
  --failed     only commands that ended with an error
  --since AGE  only commands newer than AGE: a number and s, m, h, d or w (2d = two days)
  -n COUNT     the newest COUNT commands
  --all        every command (the default when the output is not a terminal;
               on a terminal the default is the newest 100)
  search TEXT  only commands that contain TEXT
  delete ID    remove the entries with these numbers
";

/// The history of one shell: where it is stored and which lines are.
#[derive(Clone, Debug)]
pub struct History {
    pub store: Store,
    pub filter: Filter,
}

impl History {
    /// Notes that `command` starts in `cwd`, unless it is a line that is not stored. A line
    /// ending at the end of `command` is not part of it.
    pub fn begin(&self, command: &str, cwd: &str) -> Option<Token> {
        let command = command.strip_suffix('\n').unwrap_or(command);
        self.filter
            .stores(command)
            .then(|| self.store.start(command, cwd))
    }

    /// What there is to tell the user about the history (it is kept in memory only, a write
    /// was lost), once. This does not wait for the writer: the prompt must not be late.
    pub fn problem(&self) -> Option<String> {
        self.store.take_problem()
    }

    /// The shell ends: give the last writes time to reach the file, and close it.
    pub fn close(&self) {
        self.store.close(CLOSE);
    }
}

/// What a `history` command line asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Request {
    List(Listing),
    Delete(Vec<i64>),
    Help,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Listing {
    here: bool,
    failed: bool,
    /// How far back, in milliseconds.
    since: Option<i64>,
    search: Option<String>,
    limit: Limit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Limit {
    /// 100 on a terminal, everything elsewhere.
    #[default]
    Unset,
    Newest(u32),
    All,
}

/// Reads an age such as `90s`, `15m`, `3h`, `2d` or `1w` as milliseconds.
fn parse_age(text: &str) -> Option<i64> {
    let unit = text.chars().last()?;
    let seconds: i64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        'w' => 604_800,
        _ => return None,
    };
    let digits = &text[..text.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits
        .parse::<i64>()
        .ok()?
        .checked_mul(seconds)?
        .checked_mul(1000)
}

fn parse_count(text: &str) -> Option<u32> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|count| *count > 0)
}

fn parse(args: &[String]) -> Result<Request, String> {
    let mut listing = Listing::default();
    let mut words = args.iter().map(String::as_str);
    let set_limit = |listing: &mut Listing, limit: Limit| {
        if listing.limit == Limit::Unset {
            listing.limit = limit;
            Ok(())
        } else {
            Err("-n and --all cannot be used together or twice".to_owned())
        }
    };
    while let Some(word) = words.next() {
        match word {
            "--here" => listing.here = true,
            "--failed" => listing.failed = true,
            "--all" => set_limit(&mut listing, Limit::All)?,
            "--help" | "-h" => return Ok(Request::Help),
            "--since" => {
                let age = words.next().ok_or("--since needs an age, such as 2d")?;
                listing.since = Some(parse_age(age).ok_or_else(|| bad_age(age))?);
            }
            "-n" => {
                let count = words.next().ok_or("-n needs a count")?;
                let count = parse_count(count).ok_or_else(|| bad_count(count))?;
                set_limit(&mut listing, Limit::Newest(count))?;
            }
            "search" => {
                let text = words.by_ref().collect::<Vec<_>>().join(" ");
                if text.is_empty() {
                    return Err("search needs a text".to_owned());
                }
                listing.search = Some(text);
            }
            "delete" => {
                if listing != Listing::default() {
                    return Err("delete takes only entry numbers".to_owned());
                }
                let ids = words
                    .by_ref()
                    .map(|id| {
                        parse_count(id)
                            .map(i64::from)
                            .ok_or_else(|| format!("delete: {id}: not an entry number"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                if ids.is_empty() {
                    return Err("delete needs an entry number".to_owned());
                }
                return Ok(Request::Delete(ids));
            }
            _ => {
                if let Some(age) = word.strip_prefix("--since=") {
                    listing.since = Some(parse_age(age).ok_or_else(|| bad_age(age))?);
                } else if let Some(count) = word
                    .strip_prefix("-n")
                    .filter(|rest| rest.bytes().next().is_some_and(|b| b.is_ascii_digit()))
                {
                    let count = parse_count(count).ok_or_else(|| bad_count(count))?;
                    set_limit(&mut listing, Limit::Newest(count))?;
                } else if word.starts_with('-') {
                    return Err(format!("{word}: unknown option"));
                } else {
                    return Err(format!(
                        "{word}: unknown word (did you mean `search {word}`?)"
                    ));
                }
            }
        }
    }
    Ok(Request::List(listing))
}

fn bad_age(age: &str) -> String {
    format!("{age}: not an age (a number and s, m, h, d or w, such as 2d)")
}

fn bad_count(count: &str) -> String {
    format!("{count}: not a count (a number above 0)")
}

/// The query for a listing. It asks for the newest first, so that a limit keeps the newest;
/// the answer is turned around for showing.
fn query_for(listing: &Listing, cwd: &str, to_terminal: bool, now: i64) -> Query {
    Query {
        cwd: listing.here.then(|| cwd.to_owned()),
        failed: listing.failed,
        since_ms: listing.since.map(|age| now.saturating_sub(age)),
        contains: listing.search.clone(),
        limit: match listing.limit {
            Limit::Newest(count) => Some(count),
            Limit::All => None,
            Limit::Unset => to_terminal.then_some(ON_A_TERMINAL),
        },
        newest_first: true,
        ..Query::default()
    }
}

/// The local date and time of a moment given in milliseconds since 1970.
fn local_time(ms: i64) -> String {
    Local.timestamp_millis_opt(ms).single().map_or_else(
        || "????-??-?? ??:??".to_owned(),
        |time| time.format("%Y-%m-%d %H:%M").to_string(),
    )
}

/// The listing: one entry after the other, oldest first, as `number  date time  command`. The
/// numbers are lined up on the right. No colors.
fn render(newest_first: &[Entry], time: impl Fn(i64) -> String) -> String {
    let width = newest_first
        .iter()
        .map(|entry| entry.id.to_string().len())
        .max()
        .unwrap_or(1);
    let mut out = String::new();
    for entry in newest_first.iter().rev() {
        let _ = writeln!(
            out,
            "{:>width$}  {}  {}",
            entry.id,
            time(entry.started_ms),
            entry.command
        );
    }
    out
}

fn failure(message: &str, status: u8) -> BuiltinOutput {
    BuiltinOutput {
        out: Vec::new(),
        err: format!("oh-my-shell: history: {message}\n").into_bytes(),
        status,
    }
}

impl HistoryCommand for History {
    fn run(&self, args: &[Vec<u8>], cwd: &[u8], to_terminal: bool) -> BuiltinOutput {
        let args: Vec<String> = args
            .iter()
            .map(|arg| String::from_utf8_lossy(arg).into_owned())
            .collect();
        let request = match parse(&args) {
            Ok(request) => request,
            Err(message) => return failure(&format!("{message}\n{}", USAGE.trim_end()), 2),
        };
        match request {
            Request::Help => BuiltinOutput {
                out: USAGE.as_bytes().to_vec(),
                ..BuiltinOutput::default()
            },
            Request::List(listing) => {
                // So that the listing has the commands that just ran, this one included.
                self.store.settle(SETTLE);
                let cwd = String::from_utf8_lossy(cwd);
                let query = query_for(&listing, &cwd, to_terminal, now_ms());
                BuiltinOutput {
                    out: render(&self.store.query(&query), local_time).into_bytes(),
                    ..BuiltinOutput::default()
                }
            }
            Request::Delete(ids) => {
                let Some(removed) = self.store.delete(&ids) else {
                    return failure("delete: the history did not answer; nothing was removed", 1);
                };
                let missing: Vec<String> = ids
                    .iter()
                    .zip(&removed)
                    .filter(|(_, removed)| !**removed)
                    .map(|(id, _)| format!("delete: {id}: no such entry"))
                    .collect();
                if missing.is_empty() {
                    BuiltinOutput::default()
                } else {
                    failure(&missing.join("\noh-my-shell: history: "), 1)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATIENT: Duration = Duration::from_secs(10);

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    fn listing(line: &str) -> Listing {
        match parse(&words(line)) {
            Ok(Request::List(listing)) => listing,
            other => panic!("{line}: {other:?}"),
        }
    }

    #[test]
    fn no_words_ask_for_everything_the_default_way() {
        assert_eq!(listing(""), Listing::default());
        assert_eq!(listing("").limit, Limit::Unset);
    }

    #[test]
    fn options_and_search_combine() {
        let asked = listing("--here --failed --since 2d -n 5 search git  push");
        assert_eq!(
            asked,
            Listing {
                here: true,
                failed: true,
                since: Some(2 * 86_400_000),
                search: Some("git push".to_owned()),
                limit: Limit::Newest(5),
            }
        );
        assert_eq!(listing("--since=90s").since, Some(90_000));
        assert_eq!(listing("-n12").limit, Limit::Newest(12));
        assert_eq!(listing("--all").limit, Limit::All);
        // Everything after `search` is the text, also what looks like an option.
        assert_eq!(
            listing("search --here -n").search.as_deref(),
            Some("--here -n")
        );
    }

    #[test]
    fn ages_have_a_number_and_a_unit() {
        assert_eq!(parse_age("1s"), Some(1000));
        assert_eq!(parse_age("15m"), Some(900_000));
        assert_eq!(parse_age("3h"), Some(10_800_000));
        assert_eq!(parse_age("1w"), Some(604_800_000));
        assert_eq!(parse_age("0d"), Some(0));
        for bad in [
            "",
            "d",
            "2",
            "2x",
            "-2d",
            "2.5h",
            "2 d",
            "99999999999999999999d",
            "9223372036854775w",
            "২d",
        ] {
            assert_eq!(parse_age(bad), None, "{bad}");
        }
    }

    #[test]
    fn wrong_words_are_refused_with_a_reason() {
        for (line, reason) in [
            ("--nope", "--nope: unknown option"),
            ("git", "git: unknown word (did you mean `search git`?)"),
            ("--since", "--since needs an age, such as 2d"),
            (
                "--since soon",
                "soon: not an age (a number and s, m, h, d or w, such as 2d)",
            ),
            ("-n", "-n needs a count"),
            ("-n 0", "0: not a count (a number above 0)"),
            ("-n x", "x: not a count (a number above 0)"),
            (
                "-n 5 --all",
                "-n and --all cannot be used together or twice",
            ),
            (
                "--all --all",
                "-n and --all cannot be used together or twice",
            ),
            ("search", "search needs a text"),
            ("delete", "delete needs an entry number"),
            ("delete 3 x", "delete: x: not an entry number"),
            ("delete -1", "delete: -1: not an entry number"),
            ("delete 0", "delete: 0: not an entry number"),
            ("--here delete 3", "delete takes only entry numbers"),
        ] {
            assert_eq!(parse(&words(line)), Err(reason.to_owned()), "{line}");
        }
    }

    #[test]
    fn delete_and_help_are_requests_of_their_own() {
        assert_eq!(
            parse(&words("delete 3 14")),
            Ok(Request::Delete(vec![3, 14]))
        );
        assert_eq!(parse(&words("--help")), Ok(Request::Help));
        assert_eq!(parse(&words("--here -h")), Ok(Request::Help));
    }

    #[test]
    fn a_terminal_gets_the_newest_hundred_and_a_pipe_everything() {
        let plain = Listing::default();
        assert_eq!(query_for(&plain, "/", true, 0).limit, Some(100));
        assert_eq!(query_for(&plain, "/", false, 0).limit, None);
        // `-n` and `--all` mean the same in both cases.
        for to_terminal in [true, false] {
            let newest = listing("-n 7");
            assert_eq!(query_for(&newest, "/", to_terminal, 0).limit, Some(7));
            let all = listing("--all");
            assert_eq!(query_for(&all, "/", to_terminal, 0).limit, None);
        }
    }

    #[test]
    fn the_query_carries_the_filters() {
        let query = query_for(
            &listing("--here --failed --since 1h search x"),
            "/w",
            false,
            5_000_000,
        );
        assert_eq!(query.cwd.as_deref(), Some("/w"));
        assert!(query.failed);
        assert_eq!(query.since_ms, Some(1_400_000));
        assert_eq!(query.contains.as_deref(), Some("x"));
        assert!(query.newest_first);
        assert_eq!(query_for(&Listing::default(), "/w", false, 0).cwd, None);
    }

    fn entry(id: i64, command: &str) -> Entry {
        Entry {
            id,
            command: command.to_owned(),
            cwd: "/".to_owned(),
            exit_status: Some(0),
            started_ms: id * 1000,
            duration_ms: Some(1),
            session: 1,
            host: "h".to_owned(),
        }
    }

    #[test]
    fn the_listing_is_oldest_first_with_numbers_lined_up_and_no_colors() {
        let newest_first = [
            entry(100, "third"),
            entry(99, "second\nline two"),
            entry(7, "first"),
        ];
        let shown = render(&newest_first, |ms| format!("T{}", ms / 1000));
        assert_eq!(
            shown,
            "  7  T7  first\n 99  T99  second\nline two\n100  T100  third\n"
        );
        assert!(!shown.contains('\x1b'));
        assert_eq!(render(&[], |_| String::new()), "");
    }

    #[test]
    fn local_time_has_date_hours_and_minutes() {
        let shown = local_time(1_700_000_000_000);
        let shape: String = shown
            .chars()
            .map(|c| if c.is_ascii_digit() { '9' } else { c })
            .collect();
        assert_eq!(shape, "9999-99-99 99:99");
    }

    fn history() -> History {
        History {
            store: Store::in_memory(),
            filter: Filter::new(&[" *".to_owned()]),
        }
    }

    fn run(history: &History, line: &str, cwd: &str, to_terminal: bool) -> (String, String, u8) {
        let args: Vec<Vec<u8>> = words(line).into_iter().map(String::into_bytes).collect();
        let output = history.run(&args, cwd.as_bytes(), to_terminal);
        (
            String::from_utf8(output.out).unwrap(),
            String::from_utf8(output.err).unwrap(),
            output.status,
        )
    }

    /// The commands of a listing, without numbers and times.
    fn listed(out: &str) -> Vec<&str> {
        out.lines()
            .map(|line| line.trim_start().splitn(3, "  ").last().unwrap())
            .collect()
    }

    #[test]
    fn lines_that_are_not_stored_are_not_begun() {
        let history = history();
        assert!(history.begin("ls\n", "/").is_some());
        assert!(history.begin(" ls\n", "/").is_none());
        assert!(history.begin("\n", "/").is_none());
        assert!(history.begin("export API_KEY=abc\n", "/").is_none());
        let (out, _, _) = run(&history, "", "/", false);
        // The line ending is not part of the command.
        assert_eq!(listed(&out), ["ls"]);
    }

    #[test]
    fn the_builtin_lists_filters_and_deletes() {
        let history = history();
        for (command, cwd, status) in [
            ("make", "/repo", 2),
            ("ls", "/home", 0),
            ("git push", "/repo", 1),
            ("git status", "/repo", 0),
        ] {
            let token = history.begin(command, cwd).unwrap();
            history
                .store
                .finish(token, status, Duration::from_millis(3));
        }
        assert!(history.store.settle(PATIENT));
        let commands = |line: &str| {
            let (out, err, status) = run(&history, line, "/repo", false);
            assert_eq!((err.as_str(), status), ("", 0), "{line}");
            listed(&out)
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        };
        assert_eq!(commands(""), ["make", "ls", "git push", "git status"]);
        assert_eq!(commands("--here"), ["make", "git push", "git status"]);
        assert_eq!(commands("--failed"), ["make", "git push"]);
        assert_eq!(commands("--here --failed search git"), ["git push"]);
        assert_eq!(commands("-n 2"), ["git push", "git status"]);
        assert_eq!(commands("--since 1h").len(), 4);
        assert_eq!(commands("search nothing-like-this"), Vec::<String>::new());

        let (out, _, _) = run(&history, "", "/", false);
        let first_id: i64 = out.split_whitespace().next().unwrap().parse().unwrap();
        let (out, err, status) = run(&history, &format!("delete {first_id} 4242"), "/", false);
        assert_eq!(out, "");
        assert_eq!(err, "oh-my-shell: history: delete: 4242: no such entry\n");
        assert_eq!(status, 1);
        assert_eq!(commands(""), ["ls", "git push", "git status"]);
        let (_, err, status) = run(&history, &format!("delete {}", first_id + 1), "/", false);
        assert_eq!((err.as_str(), status), ("", 0));
        assert_eq!(commands(""), ["git push", "git status"]);
    }

    #[test]
    fn a_terminal_shows_the_newest_hundred() {
        let history = history();
        for n in 1..=130 {
            let token = history.begin(&format!("echo {n}"), "/").unwrap();
            history.store.finish(token, 0, Duration::ZERO);
        }
        let (out, _, _) = run(&history, "", "/", true);
        let shown = listed(&out);
        assert_eq!(shown.len(), 100);
        assert_eq!((shown[0], shown[99]), ("echo 31", "echo 130"));
        assert_eq!(listed(&run(&history, "", "/", false).0).len(), 130);
        assert_eq!(listed(&run(&history, "--all", "/", true).0).len(), 130);
        assert_eq!(
            listed(&run(&history, "-n 3", "/", true).0),
            ["echo 128", "echo 129", "echo 130"]
        );
    }

    #[test]
    fn a_mistake_shows_the_reason_and_the_usage() {
        let (out, err, status) = run(&history(), "--nope", "/", false);
        assert_eq!(out, "");
        assert_eq!(status, 2);
        assert!(
            err.starts_with("oh-my-shell: history: --nope: unknown option\nusage: history"),
            "{err}"
        );
        assert!(err.ends_with('\n') && !err.ends_with("\n\n"));
        let (out, err, status) = run(&history(), "--help", "/", false);
        assert!(out.starts_with("usage: history"));
        assert_eq!((err.as_str(), status), ("", 0));
    }
}
