//! Reading a small text file the way the shell reads `config.toml`: never blocking, never
//! without a limit.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::load::O_NONBLOCK;

/// A file larger than this is not a settings file.
const MAX_BYTES: u64 = 1024 * 1024;

/// The text of `file`. `Ok(None)` means there is no such file. A FIFO or a device is refused
/// (opening a FIFO with `O_NONBLOCK` returns at once, so it cannot hang the start), as is a file
/// over 1 MiB or one that is not UTF-8. The error is one sentence for the user.
pub fn read_text_file(file: &Path) -> Result<Option<String>, String> {
    let opened = OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK)
        .open(file);
    let mut opened = match opened {
        Ok(opened) => opened,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read the file: {error}")),
    };
    let meta = opened
        .metadata()
        .map_err(|error| format!("cannot read the file: {error}"))?;
    if !meta.is_file() {
        return Err("this is not a regular file".to_owned());
    }
    if meta.len() > MAX_BYTES {
        return Err(format!("the file is larger than {} KiB", MAX_BYTES / 1024));
    }
    let mut bytes = Vec::new();
    opened
        .by_ref()
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read the file: {error}"))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "the file is not valid UTF-8".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("omsh-read-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_missing_file_is_none() {
        let dir = scratch("missing");
        assert_eq!(read_text_file(&dir.join("nope.toml")), Ok(None));
    }

    #[test]
    fn a_normal_file_is_read() {
        let dir = scratch("normal");
        fs::write(dir.join("a.toml"), "x = 1\n").unwrap();
        assert_eq!(
            read_text_file(&dir.join("a.toml")),
            Ok(Some("x = 1\n".to_owned()))
        );
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = scratch("dir");
        let error = read_text_file(&dir).unwrap_err();
        assert!(error.contains("not a regular file") || error.contains("cannot read"));
    }

    #[test]
    fn a_big_file_is_refused() {
        let dir = scratch("big");
        fs::write(dir.join("big.toml"), vec![b'#'; 1024 * 1024 + 1]).unwrap();
        assert!(
            read_text_file(&dir.join("big.toml"))
                .unwrap_err()
                .contains("larger than 1024 KiB")
        );
    }

    #[test]
    fn bytes_that_are_not_utf8_are_refused() {
        let dir = scratch("utf8");
        fs::write(dir.join("bad.toml"), [0xff, 0xfe, b'x']).unwrap();
        assert!(
            read_text_file(&dir.join("bad.toml"))
                .unwrap_err()
                .contains("UTF-8")
        );
    }

    #[test]
    fn a_fifo_is_refused_and_does_not_hang() {
        let dir = scratch("fifo");
        let fifo = dir.join("f.toml");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status();
        if made.is_ok_and(|status| status.success()) {
            assert!(read_text_file(&fifo).unwrap_err().contains("not a regular"));
        }
    }

    #[test]
    fn a_link_to_a_file_is_followed() {
        let dir = scratch("link");
        fs::write(dir.join("real.toml"), "y = 2\n").unwrap();
        symlink(dir.join("real.toml"), dir.join("link.toml")).unwrap();
        assert_eq!(
            read_text_file(&dir.join("link.toml")),
            Ok(Some("y = 2\n".to_owned()))
        );
    }
}
