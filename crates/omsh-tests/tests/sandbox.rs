//! The safety rule for the tests of `omsh update` and `omsh uninstall` (S29b): they run only in a
//! sandbox whose folders are checked not to be the real home directory. These tests prove the
//! check: a real `HOME` is refused, so is anything inside it, so is a link that points into it,
//! and a fresh temporary folder is accepted.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use omsh_tests::pty_test;
use omsh_tests::sandbox::{FOLDERS, Sandbox, assert_isolated, problems, protected_homes, resolve};

const BIN: &str = env!("CARGO_BIN_EXE_omsh-pty-shell");

fn real_home() -> PathBuf {
    let homes = protected_homes();
    assert!(!homes.is_empty(), "this user has a home directory");
    assert!(homes[0].is_dir(), "{} exists", homes[0].display());
    homes[0].clone()
}

fn named(name: &str, path: &Path) -> Vec<(String, PathBuf)> {
    vec![(name.to_owned(), path.to_path_buf())]
}

/// A fresh folder outside the real home (the test fails clearly if `TMPDIR` is inside it).
struct Fresh(PathBuf);

impl Fresh {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("omsh-sandbox-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)
            .unwrap_or_else(|error| panic!("cannot make {}: {error}", dir.display()));
        Self(dir)
    }
}

impl Drop for Fresh {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_protected_homes_come_from_the_account_database() {
    let homes = protected_homes();
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::current())
        .unwrap()
        .unwrap();
    assert!(homes.contains(&resolve(&user.dir)), "{homes:?}");
}

#[test]
fn the_real_home_is_refused() {
    let home = real_home();
    let found = problems(&named("HOME", &home), &protected_homes());
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("HOME"), "{found:?}");
}

#[test]
fn a_folder_inside_the_real_home_is_refused_even_when_it_does_not_exist() {
    let home = real_home();
    for inside in [
        home.join(".config"),
        home.join(".local/share/oh-my-shell"),
        home.join("no/such/folder/yet"),
    ] {
        let found = problems(&named("XDG_DATA_HOME", &inside), &protected_homes());
        assert_eq!(found.len(), 1, "{}: {found:?}", inside.display());
    }
}

#[test]
fn one_bad_folder_among_good_ones_is_enough() {
    let fresh = Fresh::new("mixed");
    let home = real_home();
    let mut folders: Vec<(String, PathBuf)> = FOLDERS
        .iter()
        .map(|(name, under)| ((*name).to_owned(), fresh.0.join(under)))
        .collect();
    assert!(problems(&folders, &protected_homes()).is_empty());
    // HOME is fine, XDG_DATA_HOME points into the real home.
    folders[2].1 = home.join(".local/share");
    let found = problems(&folders, &protected_homes());
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("XDG_DATA_HOME"), "{found:?}");
}

#[test]
fn a_link_in_a_fresh_folder_that_points_into_the_real_home_is_refused() {
    let fresh = Fresh::new("link");
    symlink(real_home(), fresh.0.join("sneaky")).unwrap();
    for path in [
        fresh.0.join("sneaky"),
        fresh.0.join("sneaky/.config"),
        fresh.0.join("sneaky/not/there/yet"),
    ] {
        let found = problems(&named("HOME", &path), &protected_homes());
        assert_eq!(found.len(), 1, "{}: {found:?}", path.display());
    }
}

#[test]
fn a_fresh_temporary_folder_is_accepted() {
    let fresh = Fresh::new("fresh");
    let folders: Vec<(String, PathBuf)> = FOLDERS
        .iter()
        .map(|(name, under)| ((*name).to_owned(), fresh.0.join(under)))
        .collect();
    assert_eq!(problems(&folders, &protected_homes()), Vec::<String>::new());
    assert_isolated(&folders, &protected_homes());
}

#[test]
fn relative_empty_and_dotdot_paths_are_refused() {
    for path in ["", "relative/dir", "/tmp/../tmp/x"] {
        let found = problems(&named("HOME", Path::new(path)), &protected_homes());
        assert_eq!(found.len(), 1, "{path:?}: {found:?}");
    }
}

#[test]
fn nothing_can_be_checked_without_a_known_home() {
    let fresh = Fresh::new("nohome");
    let found = problems(&named("HOME", &fresh.0), &[]);
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
#[should_panic(expected = "real home directory")]
fn the_assertion_panics_for_the_real_home() {
    assert_isolated(&named("HOME", &real_home()), &protected_homes());
}

/// The sandbox itself, tried against a stand-in for the real home (a fresh folder), so that no
/// test ever points a sandbox at the real one.
#[test]
fn a_sandbox_inside_a_protected_home_is_refused_before_anything_is_written() {
    let stand_in = Fresh::new("standin");
    let inside = stand_in.0.join("sandbox-home");
    let protected = vec![resolve(&stand_in.0)];
    let target = inside.clone();
    let result = std::panic::catch_unwind(move || {
        pty_test(BIN, move |ctx| {
            let _ = Sandbox::in_home(ctx, BIN, &target, &protected)?;
            Ok(())
        });
    });
    let panic = result.expect_err("the sandbox must refuse");
    let message = panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(message.contains("real home directory"), "{message}");
    assert!(
        !inside.exists(),
        "nothing was created in the protected home"
    );
}

#[test]
fn a_sandbox_equal_to_a_protected_home_is_refused_too() {
    let stand_in = Fresh::new("equal");
    let protected = vec![resolve(&stand_in.0)];
    let target = stand_in.0.clone();
    let result = std::panic::catch_unwind(move || {
        pty_test(BIN, move |ctx| {
            let _ = Sandbox::in_home(ctx, BIN, &target, &protected)?;
            Ok(())
        });
    });
    assert!(result.is_err());
    assert!(!stand_in.0.join("bin").exists());
}

#[test]
fn a_sandbox_keeps_every_folder_inside_the_test_home() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let folders = sandbox.folders();
        assert_eq!(folders.len(), FOLDERS.len());
        for (name, path) in folders {
            assert!(path.starts_with(ctx.home()), "{name}={}", path.display());
        }
        assert!(sandbox.program().starts_with(ctx.home()));
        assert!(sandbox.program().is_file());
        Ok(())
    });
}

#[test]
fn the_installed_copy_is_not_the_file_in_target() {
    pty_test(BIN, |ctx| {
        let sandbox = Sandbox::new(ctx, BIN)?;
        let installed = fs::canonicalize(sandbox.program())?;
        let built = fs::canonicalize(BIN)?;
        assert_ne!(installed, built);
        assert!(!installed.components().any(|c| c.as_os_str() == "target"));
        Ok(())
    });
}
