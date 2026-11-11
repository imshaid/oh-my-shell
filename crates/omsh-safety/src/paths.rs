//! Where a path points, worked out from its text alone (no file system access).

/// Folders whose loss breaks the machine. `/` and the parents of the home folder count too.
const SYSTEM_DIRS: &[&str] = &[
    "etc", "usr", "bin", "sbin", "boot", "var", "lib", "lib32", "lib64", "libx32", "home", "root",
];

/// A stand-in for the home folder when its real path is not known.
const UNKNOWN_HOME: &str = "\0home";

/// What a path stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// `/`, a system folder, or a folder above the home folder (`/home`).
    System,
    /// The home folder.
    Home,
    /// Anything else.
    Elsewhere,
}

/// The result of [`classify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Class {
    /// The folder the path names, or whose whole content it names (`/etc/*`).
    pub place: Place,
}

impl Class {
    /// True for the places a recursive delete must never reach by accident.
    #[must_use]
    pub fn is_critical(self) -> bool {
        self.place != Place::Elsewhere
    }

    /// True for the system places only.
    #[must_use]
    pub fn is_system(self) -> bool {
        self.place == Place::System
    }
}

/// Works out what `text` stands for. `glob` says the word had an unquoted pattern character.
/// A pattern that matches everything in a folder (`*`, `.*`) names the folder's content, which
/// counts as the folder; any other pattern names something inside it.
#[must_use]
pub fn classify(text: &str, glob: bool, cwd: Option<&str>, home: Option<&str>) -> Class {
    let elsewhere = Class {
        place: Place::Elsewhere,
    };
    if text.is_empty() {
        return elsewhere;
    }
    let mut text = text;
    if glob {
        let last = text.rsplit('/').find(|part| !part.is_empty()).unwrap_or("");
        if is_everything(last) {
            let cut = text.trim_end_matches('/').len() - last.len();
            text = &text[..cut];
        }
    }
    let mut stack: Vec<&str> = Vec::new();
    if text.starts_with('/') {
        feed(&mut stack, text);
    } else if text == "~" || text.starts_with("~/") {
        // A tilde that was not resolved: the home folder, wherever it is.
        stack.push(UNKNOWN_HOME);
        feed(&mut stack, &text[1..]);
        let place = match stack.as_slice() {
            [UNKNOWN_HOME] => Place::Home,
            [] => Place::System,
            _ => Place::Elsewhere,
        };
        return Class { place };
    } else if let Some(cwd) = cwd {
        feed(&mut stack, cwd);
        feed(&mut stack, text);
    } else {
        return elsewhere;
    }
    let mut home_parts: Vec<&str> = Vec::new();
    if let Some(home) = home {
        feed(&mut home_parts, home);
    }
    let place = if !home_parts.is_empty() && stack == home_parts {
        Place::Home
    } else if stack.is_empty()
        || (home_parts.starts_with(&stack))
        || (stack.len() == 1 && SYSTEM_DIRS.contains(&stack[0]))
    {
        Place::System
    } else {
        Place::Elsewhere
    };
    Class { place }
}

/// `*`, `**` and `.*`: patterns that match every name in a folder (the shell never matches
/// `.` and `..`).
fn is_everything(part: &str) -> bool {
    part == ".*" || (!part.is_empty() && part.chars().all(|c| c == '*'))
}

/// Adds the parts of `path` to `stack`, resolving `.` and `..` and ignoring `//`.
fn feed<'a>(stack: &mut Vec<&'a str>, path: &'a str) {
    for part in path.split('/').filter(|part| !part.is_empty()) {
        match part {
            "." => {}
            ".." => {
                stack.pop();
            }
            part => stack.push(part),
        }
    }
}

/// `path` joined to `base` (unless it is absolute) and cleaned of `.`, `..` and `//`.
#[must_use]
pub fn resolve(base: &str, path: &str) -> String {
    let mut stack: Vec<&str> = Vec::new();
    if !path.starts_with('/') {
        feed(&mut stack, base);
    }
    feed(&mut stack, path);
    format!("/{}", stack.join("/"))
}

#[cfg(test)]
mod tests {
    use super::{Place, classify, resolve};

    const CWD: Option<&str> = Some("/home/me/work");
    const HOME: Option<&str> = Some("/home/me");

    fn place(text: &str, glob: bool) -> Place {
        classify(text, glob, CWD, HOME).place
    }

    #[test]
    fn the_root_and_system_folders_are_system() {
        for text in [
            "/",
            "//",
            "/.",
            "/etc",
            "/etc/",
            "/usr/..",
            "/usr/../etc",
            "/bin",
            "/boot",
            "/var",
            "/home",
            "/root",
            "/lib64",
            "/sbin",
        ] {
            assert_eq!(place(text, false), Place::System, "{text}");
        }
    }

    #[test]
    fn a_pattern_for_everything_names_the_folder() {
        for text in ["/*", "/etc/*", "/etc/.*", "/**", "/etc//*"] {
            assert_eq!(place(text, true), Place::System, "{text}");
        }
        assert_eq!(place("~/*", true), Place::Home);
    }

    #[test]
    fn any_other_pattern_names_something_inside() {
        assert_eq!(place("/etc/*.conf", true), Place::Elsewhere);
        assert_eq!(place("/*/x", true), Place::Elsewhere);
        assert_eq!(place("/e*", true), Place::Elsewhere);
    }

    #[test]
    fn folders_inside_system_folders_are_elsewhere() {
        for text in [
            "/etc/nginx",
            "/usr/local",
            "/var/www",
            "/tmp",
            "/opt",
            "/home/other",
        ] {
            assert_eq!(place(text, false), Place::Elsewhere, "{text}");
        }
    }

    #[test]
    fn the_home_folder_and_its_parents() {
        assert_eq!(place("/home/me", false), Place::Home);
        assert_eq!(place("/home/me/", false), Place::Home);
        assert_eq!(place("/home/me/work/..", false), Place::Home);
        assert_eq!(place("/home/me/work/../*", true), Place::Home);
        assert_eq!(place("/home/me/work/../..", false), Place::System);
        assert_eq!(place("/home/me/work", false), Place::Elsewhere);
    }

    #[test]
    fn relative_paths_start_at_the_working_directory() {
        assert_eq!(place(".", false), Place::Elsewhere);
        assert_eq!(place("..", false), Place::Home);
        assert_eq!(place("../..", false), Place::System);
        assert_eq!(place("../../..", false), Place::System);
        assert_eq!(place("*", true), Place::Elsewhere);
        assert_eq!(
            classify("*", true, Some("/home/me"), HOME).place,
            Place::Home
        );
        assert_eq!(classify(".", false, Some("/"), HOME).place, Place::System);
        assert_eq!(classify("etc", false, Some("/"), HOME).place, Place::System);
    }

    #[test]
    fn without_a_working_directory_a_relative_path_is_not_judged() {
        assert_eq!(classify("..", false, None, HOME).place, Place::Elsewhere);
        assert_eq!(classify("/", false, None, HOME).place, Place::System);
    }

    #[test]
    fn a_tilde_that_was_not_resolved_is_the_home_folder() {
        for text in ["~", "~/", "~/.", "~/x/.."] {
            assert_eq!(
                classify(text, false, CWD, None).place,
                Place::Home,
                "{text}"
            );
        }
        assert_eq!(classify("~/*", true, CWD, None).place, Place::Home);
        assert_eq!(classify("~/..", false, CWD, None).place, Place::System);
        assert_eq!(classify("~/x", false, CWD, None).place, Place::Elsewhere);
    }

    #[test]
    fn the_empty_text_is_not_a_place() {
        assert_eq!(place("", false), Place::Elsewhere);
    }

    #[test]
    fn resolve_joins_and_cleans() {
        assert_eq!(resolve("/a/b", "c"), "/a/b/c");
        assert_eq!(resolve("/a/b", "../c"), "/a/c");
        assert_eq!(resolve("/a/b", "/x//y/./z"), "/x/y/z");
        assert_eq!(resolve("/a", "../../.."), "/");
    }
}
