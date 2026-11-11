//! Facts about the user the shell runs as.

use nix::unistd::geteuid;

/// True if the shell runs with the rights of root (effective user id 0). The prompt shows `#`
/// instead of `$` then.
#[must_use]
pub fn is_root() -> bool {
    geteuid().is_root()
}

/// The effective user id: whose files count as "yours" in the blast radius.
#[must_use]
pub fn effective_uid() -> u32 {
    geteuid().as_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_uid_is_the_one_root_checks() {
        assert_eq!(effective_uid() == 0, is_root());
    }

    #[test]
    fn matches_the_effective_user_id() {
        assert_eq!(is_root(), nix::unistd::geteuid().as_raw() == 0);
    }
}
