//! The first start (UI upgrade, phase 5): the welcome, "Stay up to date?" and, when `eza` is
//! on `PATH`, "Use eza for ls?".
//!
//! It runs once: on the start that wrote `config.toml`. A person at a terminal with keys gets
//! three panels; everyone else gets the three lines of text of version 1.0 and is asked nothing.
//! Every question starts on the answer that changes nothing, and nothing is written until an
//! answer is given: the settings are saved with [`omsh_config::set_setting`], one key each, and a
//! write that fails is said out loud instead of being forgotten.
//!
//! `OMSH_FIRST_RUN=plain` asks for the text welcome even on a terminal (for provisioning scripts
//! and for tests that are not about the panels).

use std::path::Path;

use omsh_config::{Change, Config, LsProgram, set_setting};
use omsh_exec::{Io, OmshCommand, Shell};
use omsh_ui::inline::Ended;
use omsh_ui::panels::MenuUi;
use omsh_ui::panels::first_run::{StayUpdated, UseEza, Welcome, WelcomeChoice};
use omsh_ui::panels::menu::run_on_terminal;
use omsh_ui::widgets::Widget;

use crate::ai::AiSession;
use crate::ai::plan::{RealTerminal, TerminalSource};

/// The environment variable that asks for the text welcome. Its value is `plain`.
pub const VARIABLE: &str = "OMSH_FIRST_RUN";

/// What the first start left changed, for the rest of the start.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Answers {
    /// The person said yes to the daily check (the file now says `update.check = true`).
    pub check_daily: bool,
}

/// What the first start needs.
pub struct FirstRun<'a> {
    pub shell: &'a mut Shell,
    pub files: &'a mut Io,
    /// `None` under `--no-config`, which has no first run anyway.
    pub ai: Option<&'a AiSession>,
    /// The panels' look; `None` when this terminal is not for panels.
    pub ui: Option<&'a MenuUi>,
    pub config: &'a Config,
    pub config_file: Option<&'a Path>,
    /// The home folder, to write the settings path as `~/...`.
    pub home: Option<&'a Path>,
    /// `--no-ai`: the welcome does not offer the AI setup.
    pub no_ai: bool,
}

/// Runs the first-start questions. `say` prints one line on standard error.
pub fn run(first: FirstRun<'_>, say: &dyn Fn(&str)) -> Answers {
    let FirstRun {
        shell,
        files,
        ai,
        ui,
        config,
        config_file,
        home,
        no_ai,
    } = first;
    let plain = shell
        .var(VARIABLE)
        .is_some_and(|value| value == b"plain".as_slice());
    let panels = ui.filter(|ui| !plain && !ui.caps.ci).filter(|_| {
        let mut source = RealTerminal;
        TerminalSource::open(&mut source, shell, files).menu_keys()
    });
    let Some(ui) = panels else {
        for line in crate::tour::welcome(config_file, home) {
            say(&line);
        }
        return Answers::default();
    };
    let mut answers = Answers::default();
    let offer_ai = !no_ai && config.ai.enabled && ai.is_some();
    let settings = config_file.map(|file| shown_path(file, home));

    // 1. The welcome.
    let Some(choice) = ask(&mut Welcome::new(offer_ai, settings), ui) else {
        return answers;
    };
    match choice {
        WelcomeChoice::Skip => {}
        WelcomeChoice::Tour => {
            let mut source = RealTerminal;
            let mut terminal = TerminalSource::open(&mut source, shell, files);
            crate::tour::run(&mut *terminal);
        }
        WelcomeChoice::SetUpAi => {
            if let Some(ai) = ai {
                let mut source = RealTerminal;
                let mut terminal = TerminalSource::open(&mut source, shell, files);
                ai.run(&[b"ai".to_vec(), b"setup".to_vec()], &mut *terminal);
            }
        }
    }

    // 2. The daily check. The question is the only place the network is asked about.
    let Some(yes) = ask(&mut StayUpdated::new(), ui) else {
        return answers;
    };
    if save(config_file, "update.check", &Change::Bool(yes), say) {
        answers.check_daily = yes;
    }

    // 3. `ls`. Only when there is something to choose.
    if config.ui.ls == LsProgram::Ls && shell.program_on_path("eza") {
        let Some(yes) = ask(&mut UseEza::new(), ui) else {
            return answers;
        };
        if yes && save(config_file, "ui.ls", &Change::Text("eza".to_owned()), say) {
            // At once, so that the answer can be seen: the next start does the same from the file.
            define_ls_alias(shell);
        }
    }
    answers
}

/// Defines `ls` as an alias of `eza`, if `eza` is on `PATH`. Used at the start of every
/// interactive shell with `ui.ls = "eza"`, and right after the first-run answer.
pub fn define_ls_alias(shell: &mut Shell) -> bool {
    if shell.program_on_path("eza") {
        shell.define_alias("ls", "eza");
        true
    } else {
        false
    }
}

/// Shows one panel. `None` when it could not be shown or was given up with Ctrl+C or a hang-up:
/// the rest of the questions are not asked then (the settings view has all of them).
fn ask<W: Widget>(widget: &mut W, ui: &MenuUi) -> Option<W::Output> {
    match run_on_terminal(widget, ui, Box::new(omsh_exec::hung_up)) {
        Ok(Ended::Chosen(answer)) => Some(answer),
        Ok(Ended::Cancelled | Ended::Interrupted | Ended::Closed) | Err(_) => None,
    }
}

/// Writes one key of `config.toml`. False, with a line saying why, when it could not be done.
fn save(file: Option<&Path>, key: &str, change: &Change, say: &dyn Fn(&str)) -> bool {
    let Some(file) = file else {
        return false;
    };
    match set_setting(file, key, change) {
        Ok(()) => true,
        Err(error) => {
            say(&format!(
                "oh-my-shell: could not save {key} in {}: {error}",
                file.display()
            ));
            false
        }
    }
}

/// The settings file as a person writes it: `~/...` inside the home folder.
fn shown_path(file: &Path, home: Option<&Path>) -> String {
    home.and_then(|home| file.strip_prefix(home).ok())
        .map_or_else(
            || file.display().to_string(),
            |rest| format!("~/{}", rest.display()),
        )
}

#[cfg(test)]
mod tests;
