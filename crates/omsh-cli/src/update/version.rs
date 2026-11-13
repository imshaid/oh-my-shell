//! Version numbers (`X.Y.Z`, optionally `-pre`), as far as `omsh update` needs them: to read a
//! release tag and to decide whether it is newer than the program that is running. No crate:
//! this is the SemVer ordering in forty lines.

use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// The part after `-`, as dot-separated identifiers. A release has none.
    pre: Option<String>,
}

impl Version {
    /// Reads `1.2.3`, `v1.2.3` and `1.2.3-rc.1`. Build metadata (`+abc`) is dropped. `None`
    /// for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let text = text.split('+').next().unwrap_or_default();
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (text, None),
        };
        let mut numbers = core.split('.');
        let major = number(numbers.next()?)?;
        let minor = number(numbers.next()?)?;
        let patch = number(numbers.next()?)?;
        if numbers.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => None,
            Some(pre) => {
                let valid = !pre.is_empty()
                    && pre.split('.').all(|part| {
                        !part.is_empty()
                            && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    });
                if !valid {
                    return None;
                }
                Some(pre.to_owned())
            }
        };
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// The version of the program that is running.
    #[must_use]
    pub fn current() -> Self {
        // The workspace version is always valid; a build with another text is a bug that the
        // test `the_running_version_is_valid` catches.
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
            pre: None,
        })
    }
}

fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// SemVer's rule for the identifiers after `-`: numbers compare as numbers and are lower than
/// words; a longer list is higher when the start is the same.
fn compare_pre(a: &str, b: &str) -> Ordering {
    let mut a = a.split('.');
    let mut b = b.split('.');
    loop {
        match (a.next(), b.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (number(x), number(y)) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                // A pre-release is lower than the release it leads to.
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => compare_pre(a, b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if let Some(pre) = &self.pre {
            write!(f, "-{pre}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap_or_else(|| panic!("{text} should parse"))
    }

    #[test]
    fn the_running_version_is_valid() {
        assert_ne!(Version::current().to_string(), "0.0.0");
    }

    #[test]
    fn plain_and_tagged_versions_parse() {
        assert_eq!(v("1.2.3").to_string(), "1.2.3");
        assert_eq!(v("v1.2.3").to_string(), "1.2.3");
        assert_eq!(v(" v10.20.30\n").to_string(), "10.20.30");
        assert_eq!(v("1.0.0-rc.1").to_string(), "1.0.0-rc.1");
        assert_eq!(v("1.0.0+build5").to_string(), "1.0.0");
    }

    #[test]
    fn anything_else_is_refused() {
        for text in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "a.b.c",
            "1.2.x",
            "1..3",
            "-1.2.3",
            "1.2.3-",
            "1.2.3-a..b",
            "1.2.3-é",
            "vv1.2.3",
            "99999999999999999999.0.0",
        ] {
            assert_eq!(Version::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn numbers_are_compared_as_numbers() {
        assert!(v("0.10.0") > v("0.9.0"));
        assert!(v("1.0.0") > v("0.99.99"));
        assert!(v("0.1.10") > v("0.1.9"));
        assert_eq!(v("1.2.3"), v("v1.2.3"));
    }

    #[test]
    fn a_pre_release_is_lower_than_its_release() {
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.1") > v("0.9.0"));
    }

    #[test]
    fn pre_release_identifiers_follow_the_semver_rule() {
        // The order from the SemVer specification.
        let order = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in order.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
    }
}
