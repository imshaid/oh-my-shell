//! The jobs panel of `jobs -i` (U6): the table of `omsh-ui` shown on the terminal, over the job
//! table of the executor.
//!
//! The executor owns the jobs and does what the panel asks (see `omsh_exec::JobsPanel`); this
//! file only turns the executor's rows into rows of the panel and the panel's answer into the
//! executor's. It exists only with a `MenuUi` (line editor on, a real terminal), so a script, a
//! pipe and `TERM=dumb` get the plain list.

use std::time::Duration;

use omsh_exec::{JobAction, JobRow, JobState, JobWatch, JobsPanel, Picked, Terminal};
use omsh_ui::inline::Ended;
use omsh_ui::panels::MenuUi;
use omsh_ui::panels::jobs_table::{JobsTable, RowKind, TableAction, TableRow};
use omsh_ui::panels::menu::run_on_terminal;

/// The jobs panel of the shell.
pub(crate) struct Panel {
    ui: MenuUi,
}

impl Panel {
    pub(crate) fn new(ui: MenuUi) -> Self {
        Self { ui }
    }
}

/// A row of the executor as a row of the table.
pub(crate) fn table_row(row: &JobRow) -> TableRow {
    TableRow {
        number: row.number,
        mark: row.mark,
        kind: match row.state {
            JobState::Running => RowKind::Running,
            JobState::Stopped(_) => RowKind::Stopped,
            JobState::Done => RowKind::Done,
            JobState::Exit(_) | JobState::Signaled(_) => RowKind::Failed,
        },
        word: row.state.word(),
        text: row.text.clone(),
        // Whole seconds are shown, so a sub-second difference never reads as a change.
        elapsed: Duration::from_secs(row.elapsed.as_secs()),
    }
}

pub(crate) fn action(action: TableAction) -> JobAction {
    match action {
        TableAction::Foreground => JobAction::Foreground,
        TableAction::Background => JobAction::Background,
        TableAction::Stop => JobAction::Stop,
        TableAction::Kill => JobAction::Kill,
    }
}

impl JobsPanel for Panel {
    fn pick(&self, watch: &JobWatch, terminal: &mut dyn Terminal) -> Picked {
        if !terminal.menu_keys() {
            return Picked::Unavailable;
        }
        let watch = watch.clone();
        let mut table = JobsTable::new(Box::new(move || {
            watch.rows().iter().map(table_row).collect()
        }));
        match run_on_terminal(&mut table, &self.ui, Box::new(omsh_exec::hung_up)) {
            Ok(Ended::Chosen((number, chosen))) => Picked::Act(number, action(chosen)),
            Ok(Ended::Interrupted) => Picked::Interrupted,
            Ok(Ended::Cancelled | Ended::Closed) => Picked::Closed,
            Err(_) => Picked::Unavailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: JobState) -> JobRow {
        JobRow {
            number: 3,
            mark: '+',
            state,
            text: "sleep 30".to_owned(),
            elapsed: Duration::from_millis(5900),
        }
    }

    #[test]
    fn the_states_map_to_the_kinds_with_their_real_words() {
        let cases = [
            (JobState::Running, RowKind::Running, "Running"),
            (JobState::Stopped(None), RowKind::Stopped, "Stopped"),
            (
                JobState::Stopped(Some("tty input")),
                RowKind::Stopped,
                "Stopped (tty input)",
            ),
            (JobState::Done, RowKind::Done, "Done"),
            (JobState::Exit(3), RowKind::Failed, "Exit 3"),
            (
                JobState::Signaled("Terminated".to_owned()),
                RowKind::Failed,
                "Terminated",
            ),
        ];
        for (state, kind, word) in cases {
            let mapped = table_row(&row(state));
            assert_eq!((mapped.kind, mapped.word.as_str()), (kind, word));
            assert_eq!((mapped.number, mapped.mark), (3, '+'));
            assert_eq!(mapped.text, "sleep 30");
        }
    }

    #[test]
    fn the_time_is_whole_seconds_so_that_no_fraction_reads_as_a_change() {
        assert_eq!(
            table_row(&row(JobState::Running)).elapsed,
            Duration::from_secs(5)
        );
    }

    #[test]
    fn every_action_maps_to_its_own() {
        use TableAction::{Background, Foreground, Kill, Stop};
        assert_eq!(action(Foreground), JobAction::Foreground);
        assert_eq!(action(Background), JobAction::Background);
        assert_eq!(action(Stop), JobAction::Stop);
        assert_eq!(action(Kill), JobAction::Kill);
    }
}
