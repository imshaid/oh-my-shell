//! Reading `/proc`: which processes a test left behind, which process group owns the terminal,
//! which signals a program ignores. Linux only, like the shell.

use std::fs;

use nix::unistd::Pid;

/// One process, as `/proc/<pid>/stat` describes it.
#[derive(Debug, Clone)]
pub struct Member {
    pub pid: Pid,
    /// The program name, cut to 15 bytes by the kernel.
    pub name: String,
    /// `R`, `S`, `T` (stopped), `Z` (zombie) ...
    pub state: char,
}

/// The pids and names of the processes whose session is `session`.
pub fn session_members(session: u32) -> Vec<Member> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return found;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // "pid (name) state ppid pgrp session ...": the name may hold spaces and parentheses,
        // so the fields are counted from the last closing parenthesis.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let name = stat[open + 1..close].to_owned();
        let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        if fields.get(3).and_then(|s| s.parse::<u32>().ok()) == Some(session) {
            found.push(Member {
                pid: Pid::from_raw(pid),
                name,
                state: fields.first().and_then(|s| s.chars().next()).unwrap_or('?'),
            });
        }
    }
    found
}

/// Like [`session_members`], without the zombies: a zombie holds no resources, and one whose
/// parent is a thread of the test process that is stuck cannot be collected.
pub fn live_members(session: u32) -> Vec<Member> {
    session_members(session)
        .into_iter()
        .filter(|m| m.state != 'Z' && m.state != 'X')
        .collect()
}

/// Fields of `/proc/<pid>/stat` after the name: state, parent, process group, session, tty,
/// foreground group of the tty.
fn stat_fields(pid: u32) -> Vec<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
    stat.rfind(')')
        .map(|close| {
            stat[close + 1..]
                .split_whitespace()
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn number(pid: u32, index: usize) -> i32 {
    stat_fields(pid)
        .get(index)
        .and_then(|field| field.parse().ok())
        .unwrap_or(-1)
}

/// The process group that owns the terminal of the process `pid`. `-1` if it is gone.
pub fn foreground_group(pid: u32) -> i32 {
    number(pid, 5)
}

/// The process group of `pid`. `-1` if it is gone.
pub fn process_group(pid: u32) -> i32 {
    number(pid, 2)
}

/// The parent of `pid`. `-1` if it is gone.
pub fn parent(pid: u32) -> i32 {
    number(pid, 1)
}

/// `T` for a stopped process, `S` or `R` for one that runs, empty if it is gone.
pub fn state(pid: Pid) -> String {
    stat_fields(pid.as_raw() as u32)
        .first()
        .cloned()
        .unwrap_or_default()
}

/// The set of signals the process ignores, as the bit mask of `SigIgn` in `/proc/<pid>/status`
/// (signal `n` is bit `n - 1`). Zero if the process is gone.
pub fn ignored_signals(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("SigIgn:"))
                .and_then(|mask| u64::from_str_radix(mask.trim(), 16).ok())
        })
        .unwrap_or(0)
}

/// True if this test process ignores `SIGHUP` (it was started under `nohup`). The shell and its
/// programs inherit that, and an ignored hangup stays ignored, so the tests that send one cannot
/// work there.
pub fn hup_is_ignored() -> bool {
    ignored_signals(std::process::id()) & 1 != 0
}
