//! Where the running program lives, and so whether `omsh update` and `omsh uninstall` may
//! change it.
//!
//! A file that a package manager or `cargo` put there is theirs: replacing it behind their
//! back would make `apt` or `cargo` believe a version that is not on the disk. Both commands
//! refuse and say what to do instead. A build in a `target/` folder is refused too, which also
//! keeps a test from ever replacing the binary it is running from.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// Who owns the program file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    /// Put there by `install.sh` (or by hand): `omsh update` may replace it.
    Own,
    /// Under `/usr` (not `/usr/local`), `/bin` or `/sbin`: the `.deb` package, or another
    /// package manager.
    Package,
    /// In `$CARGO_HOME/bin` or `~/.cargo/bin`: `cargo install`.
    Cargo,
    /// A `target/debug` or `target/release` folder of a source tree.
    Build,
}

/// Decides from the path of the running program. `home` and `cargo_home` are the user's home
/// and `$CARGO_HOME`, when known.
#[must_use]
pub fn classify(exe: &Path, home: Option<&Path>, cargo_home: Option<&Path>) -> Place {
    if in_build_folder(exe) {
        return Place::Build;
    }
    let parent = exe.parent();
    let cargo_bins = cargo_home
        .map(|dir| dir.join("bin"))
        .into_iter()
        .chain(home.map(|dir| dir.join(".cargo").join("bin")));
    for bin in cargo_bins {
        if parent == Some(bin.as_path()) {
            return Place::Cargo;
        }
    }
    if exe.starts_with("/usr/local") {
        return Place::Own;
    }
    if ["/usr", "/bin", "/sbin"]
        .iter()
        .any(|root| exe.starts_with(root))
    {
        return Place::Package;
    }
    Place::Own
}

/// A path with a `target` folder followed, directly or after a target triple, by `debug` or
/// `release`.
fn in_build_folder(path: &Path) -> bool {
    let names: Vec<&OsStr> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    names.iter().enumerate().any(|(i, name)| {
        *name == "target"
            && names
                .iter()
                .skip(i + 1)
                .take(2)
                .any(|next| *next == "debug" || *next == "release")
    })
}

/// The path of the running program, or a sentence saying why it is not usable. Linux marks a
/// program file that was replaced or removed while it runs with ` (deleted)`.
///
/// # Errors
///
/// The sentence to show.
pub fn running_program() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot find the running program ({error})"))?;
    if exe.to_string_lossy().ends_with(" (deleted)") || !exe.is_file() {
        return Err(
            "this program was replaced or removed while it was running; start the new one"
                .to_owned(),
        );
    }
    Ok(exe)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(exe: &str) -> Place {
        classify(
            Path::new(exe),
            Some(Path::new("/home/u")),
            Some(Path::new("/opt/cargo-home")),
        )
    }

    #[test]
    fn the_installer_places_are_ours() {
        assert_eq!(place("/home/u/.local/bin/oh-my-shell"), Place::Own);
        assert_eq!(place("/usr/local/bin/oh-my-shell"), Place::Own);
        assert_eq!(place("/home/u/bin/oh-my-shell"), Place::Own);
        assert_eq!(place("/opt/tools/oh-my-shell"), Place::Own);
    }

    #[test]
    fn the_package_places_are_refused() {
        assert_eq!(place("/usr/bin/oh-my-shell"), Place::Package);
        assert_eq!(place("/bin/oh-my-shell"), Place::Package);
        assert_eq!(place("/usr/sbin/oh-my-shell"), Place::Package);
        assert_eq!(place("/usr/lib/oh-my-shell/oh-my-shell"), Place::Package);
        assert_eq!(place("/sbin/oh-my-shell"), Place::Package);
    }

    #[test]
    fn a_name_that_only_starts_like_usr_is_not_a_package() {
        assert_eq!(place("/usrx/bin/oh-my-shell"), Place::Own);
        assert_eq!(place("/home/u/usr/bin/oh-my-shell"), Place::Own);
    }

    #[test]
    fn cargo_install_places_are_refused() {
        assert_eq!(place("/home/u/.cargo/bin/oh-my-shell"), Place::Cargo);
        assert_eq!(place("/opt/cargo-home/bin/oh-my-shell"), Place::Cargo);
    }

    #[test]
    fn cargo_home_without_bin_is_not_cargo() {
        assert_eq!(place("/home/u/.cargo/oh-my-shell"), Place::Own);
        assert_eq!(place("/opt/cargo-home/bin/deep/oh-my-shell"), Place::Own);
    }

    #[test]
    fn a_build_folder_is_refused() {
        assert_eq!(
            place("/home/u/code/x/target/debug/oh-my-shell"),
            Place::Build
        );
        assert_eq!(
            place("/home/u/code/x/target/release/oh-my-shell"),
            Place::Build
        );
        assert_eq!(
            place("/home/u/code/x/target/x86_64-unknown-linux-gnu/release/oh-my-shell"),
            Place::Build
        );
        assert_eq!(
            place("/home/u/code/x/target/debug/deps/omsh_cli-123"),
            Place::Build
        );
    }

    #[test]
    fn target_alone_is_not_a_build_folder() {
        assert_eq!(place("/home/u/target/oh-my-shell"), Place::Own);
        assert_eq!(place("/home/u/debug/oh-my-shell"), Place::Own);
    }

    #[test]
    fn a_build_folder_wins_over_the_other_rules() {
        assert_eq!(place("/usr/src/x/target/debug/oh-my-shell"), Place::Build);
    }

    #[test]
    fn without_a_home_only_the_paths_decide() {
        assert_eq!(
            classify(Path::new("/home/u/.cargo/bin/oh-my-shell"), None, None),
            Place::Own
        );
        assert_eq!(
            classify(Path::new("/usr/bin/oh-my-shell"), None, None),
            Place::Package
        );
    }
}
