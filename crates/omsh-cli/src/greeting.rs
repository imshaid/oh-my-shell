//! The hint bar (UI upgrade, phase 2): one dim line above the first prompt of a session that
//! names the keys worth knowing. `ui.greeting` decides: `auto` shows it for the first five
//! sessions after the welcome and then never, `always` every time, `never` not at all.
//!
//! The count is one small file in the state folder (`greeting`, the text `shown=N`). It is
//! written before the line is shown, and the line is not shown when it cannot be written, so a
//! read-only state folder never turns "five times" into "every time".

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

use omsh_config::{AiState, Config, Greeting};
use omsh_ui::Line;
use omsh_ui::shell_look::{AiHint, greeting_due, hint_bar};

/// The count's file in the state folder.
const FILE: &str = "greeting";
/// The most the file may be.
const LIMIT: u64 = 64;

/// How many sessions showed the bar. 0 when there is no file or it is not understood.
fn read(dir: &Path) -> u32 {
    let Ok(file) = fs::File::open(dir.join(FILE)) else {
        return 0;
    };
    let mut text = String::new();
    if file.take(LIMIT).read_to_string(&mut text).is_err() {
        return 0;
    }
    text.lines()
        .find_map(|line| line.strip_prefix("shown="))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Writes the count to a temporary name and renames it, in a folder only the user can enter.
/// No `fsync`: a count lost in a crash only shows the bar once more.
fn write(dir: &Path, shown: u32) -> io::Result<()> {
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let tmp = dir.join(format!("{FILE}.tmp-{}", std::process::id()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(format!("shown={shown}\n").as_bytes())?;
        drop(file);
        fs::rename(&tmp, dir.join(FILE))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// What the bar says about the AI, from the configuration alone: no keyring, no network.
pub fn ai_hint(config: &Config, no_ai: bool) -> AiHint {
    match AiState::new(&config.ai, no_ai) {
        AiState::OffByFlag | AiState::OffByConfig => AiHint::Off,
        AiState::NotSetUp => AiHint::NotSetUp,
        AiState::Ready { .. } => AiHint::Ready,
    }
}

/// The bar to show before the first prompt, if one is due; counts the session when it is.
/// `state_dir` is `None` under `--no-config` (nothing is written there, so nothing is shown in
/// `auto`), and `welcome_now` is true in the session that printed the welcome.
pub fn take(
    config: &Config,
    no_ai: bool,
    state_dir: Option<&Path>,
    welcome_now: bool,
    unicode: bool,
) -> Option<Line> {
    let mode = config.ui.greeting;
    let shown = state_dir.map_or(0, read);
    if !greeting_due(mode, shown, welcome_now, state_dir.is_some()) {
        return None;
    }
    if mode == Greeting::Auto {
        // `greeting_due` is true for `auto` only with a state folder.
        write(state_dir?, shown.saturating_add(1)).ok()?;
    }
    Some(hint_bar(ai_hint(config, no_ai), unicode))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    /// A folder of its own, removed afterwards.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("omsh-greeting-{}-{n}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn state(&self) -> PathBuf {
            self.0.join("state")
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn config(greeting: Greeting) -> Config {
        let mut config = Config::default();
        config.ui.greeting = greeting;
        config
    }

    fn text(line: &Line) -> String {
        line.plain_text()
    }

    #[test]
    fn no_file_is_a_count_of_zero_and_so_is_a_file_that_is_not_understood() {
        let dir = Dir::new();
        assert_eq!(read(&dir.0), 0);
        fs::write(dir.0.join(FILE), "lots\n").unwrap();
        assert_eq!(read(&dir.0), 0);
        fs::write(dir.0.join(FILE), "shown=-3\n").unwrap();
        assert_eq!(read(&dir.0), 0);
        fs::write(dir.0.join(FILE), "shown=3\n").unwrap();
        assert_eq!(read(&dir.0), 3);
    }

    #[test]
    fn the_count_is_written_for_the_user_alone() {
        let dir = Dir::new();
        let state = dir.state();
        write(&state, 4).unwrap();
        assert_eq!(read(&state), 4);
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&state), 0o700);
        assert_eq!(mode(&state.join(FILE)), 0o600);
        let leftovers: Vec<_> = fs::read_dir(&state).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no temporary file is left");
    }

    #[test]
    fn auto_shows_the_bar_five_times_after_the_welcome_and_then_never() {
        let dir = Dir::new();
        let state = dir.state();
        let config = config(Greeting::Auto);
        let session = |welcome| take(&config, false, Some(&state), welcome, true);
        // The welcome session shows nothing and does not count.
        assert!(session(true).is_none());
        assert_eq!(read(&state), 0);
        for n in 1..=5 {
            let bar = session(false).unwrap_or_else(|| panic!("session {n} shows the bar"));
            assert!(text(&bar).contains("Ctrl+P palette"));
            assert_eq!(read(&state), n);
        }
        assert!(session(false).is_none(), "the sixth is quiet");
        assert_eq!(read(&state), 5, "and is not counted");
    }

    #[test]
    fn never_and_always_leave_the_count_alone() {
        let dir = Dir::new();
        let state = dir.state();
        assert!(take(&config(Greeting::Never), false, Some(&state), false, true).is_none());
        for _ in 0..8 {
            assert!(take(&config(Greeting::Always), false, Some(&state), false, true).is_some());
        }
        assert!(!state.exists(), "nothing was written");
    }

    #[test]
    fn without_a_state_folder_auto_shows_nothing() {
        // `--no-config`: there is nowhere to count, so `auto` cannot fade out; it stays quiet.
        assert!(take(&config(Greeting::Auto), false, None, false, true).is_none());
        // `always` does not need the count.
        assert!(take(&config(Greeting::Always), false, None, false, true).is_some());
    }

    #[test]
    fn a_count_that_cannot_be_written_shows_nothing_instead_of_for_ever() {
        let dir = Dir::new();
        // A file where the folder should be.
        let state = dir.state();
        fs::write(&state, "in the way").unwrap();
        assert!(take(&config(Greeting::Auto), false, Some(&state), false, true).is_none());
    }

    #[test]
    fn the_bar_names_ai_setup_only_while_the_ai_is_not_set_up() {
        let mut config = config(Greeting::Always);
        let bar = |config: &Config, no_ai| text(&take(config, no_ai, None, false, true).unwrap());
        assert!(bar(&config, false).contains("omsh ai setup"));
        config.ai.provider = Some(omsh_config::Provider::Ollama);
        config.ai.model = Some("m".to_owned());
        let ready = bar(&config, false);
        assert!(!ready.contains("omsh ai setup"));
        assert!(ready.starts_with("? ask AI"));
        // Off (the flag or the config): `?` is not offered at all.
        let off = bar(&config, true);
        assert!(
            !off.contains("ask AI") && !off.contains("omsh ai setup"),
            "{off}"
        );
        config.ai.enabled = false;
        assert!(!bar(&config, false).contains("ask AI"));
    }

    #[test]
    fn a_huge_count_does_not_overflow() {
        let dir = Dir::new();
        fs::write(dir.0.join(FILE), format!("shown={}\n", u32::MAX)).unwrap();
        assert_eq!(read(&dir.0), u32::MAX);
        // `auto` is long past five: nothing happens, in particular no wrap-around to 0.
        assert!(take(&config(Greeting::Auto), false, Some(&dir.0), false, true).is_none());
    }
}
