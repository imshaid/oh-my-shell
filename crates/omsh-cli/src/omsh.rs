//! The words after `omsh`: which command runs.
//!
//! `omsh ai ...`, `omsh tour` and `omsh help` are answered by [`crate::ai::AiSession`], as
//! before. `omsh update` and `omsh uninstall` (S29b) are handled here, first, and everything
//! else goes on to the AI session. The executor sees one [`OmshCommand`].

use std::path::PathBuf;
use std::sync::Arc;

use omsh_ai::Keys;
use omsh_ai::download::{SERVER_VARIABLE, release_origin};
use omsh_config::{Config, Paths};
use omsh_exec::{OmshCommand, Shell, Terminal};

use crate::ai::AiSession;
use crate::uninstall::{self, UninstallEnv};
use crate::update::{self, Network, UpdateEnv, place};

/// The `omsh` command of one shell or of the `omsh` program.
pub struct Tools {
    ai: Arc<AiSession>,
    update: UpdateEnv,
    uninstall: UninstallEnv,
}

impl Tools {
    #[must_use]
    pub fn new(ai: Arc<AiSession>, update: UpdateEnv, uninstall: UninstallEnv) -> Self {
        Self {
            ai,
            update,
            uninstall,
        }
    }
}

impl OmshCommand for Tools {
    fn run(&self, args: &[Vec<u8>], terminal: &mut dyn Terminal) -> u8 {
        let first = args.first().map(Vec::as_slice);
        let rest = || -> Vec<String> {
            args.iter()
                .skip(1)
                .map(|word| String::from_utf8_lossy(word).into_owned())
                .collect()
        };
        match first {
            Some(b"update") => {
                let rest = rest();
                let words: Vec<&str> = rest.iter().map(String::as_str).collect();
                update::run(
                    &words,
                    terminal,
                    &self.update,
                    place::running_program(),
                    &Network,
                )
            }
            Some(b"uninstall") => {
                let rest = rest();
                let words: Vec<&str> = rest.iter().map(String::as_str).collect();
                uninstall::run(&words, terminal, &self.uninstall, place::running_program())
            }
            _ => self.ai.run(args, terminal),
        }
    }
}

/// What `omsh update` and the daily check need, read from the shell's variables and the paths.
/// Nothing is opened or asked here.
#[must_use]
pub fn update_env(shell: &Shell, paths: &Paths, no_config: bool) -> UpdateEnv {
    let var = |name: &str| {
        shell
            .var(name)
            .map(|value| String::from_utf8_lossy(value).into_owned())
    };
    let folder = |name: &str| var(name).filter(|v| !v.is_empty()).map(PathBuf::from);
    UpdateEnv {
        origin: release_origin(var(SERVER_VARIABLE).as_deref()),
        // `--no-config` writes nothing, not even the daily stamp.
        state_dir: if no_config {
            None
        } else {
            paths.state_dir().map(PathBuf::from)
        },
        home: folder("HOME"),
        cargo_home: folder("CARGO_HOME"),
    }
}

/// What `omsh uninstall` needs. The account database (the login shell) is read only when the
/// command runs, never at start: a name service can be slow.
///
/// `as_tool` is true for the `omsh` program and false inside the interactive shell, where
/// `omsh uninstall` is refused.
#[must_use]
pub fn uninstall_env(
    update: &UpdateEnv,
    config: &Config,
    paths: &Paths,
    as_tool: bool,
) -> UninstallEnv {
    UninstallEnv {
        paths: paths.clone(),
        home: update.home.clone(),
        cargo_home: update.cargo_home.clone(),
        login_shell: Box::new(|| {
            nix::unistd::User::from_uid(nix::unistd::geteuid())
                .ok()
                .flatten()
                .map(|user| user.shell)
        }),
        etc_shells: PathBuf::from("/etc/shells"),
        keys: Keys::system(paths.config_dir().map(|dir| dir.join("credentials"))),
        ai_set_up: config.ai.provider.is_some(),
        as_tool,
    }
}
