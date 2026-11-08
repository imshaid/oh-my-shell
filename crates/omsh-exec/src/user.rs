//! Facts about the user the shell runs as.

use nix::unistd::geteuid;

/// True if the shell runs with the rights of root (effective user id 0). The prompt shows `#`
/// instead of `$` then.
#[must_use]
pub fn is_root() -> bool {
    geteuid().is_root()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_effective_user_id() {
        assert_eq!(is_root(), nix::unistd::geteuid().as_raw() == 0);
    }
}
