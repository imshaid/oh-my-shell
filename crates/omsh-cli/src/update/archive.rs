//! Reading the program out of a release archive (`oh-my-shell-<target>.tar.gz`) and checking a
//! download against `sha256.sum`.
//!
//! The tar reader is ours and does one thing: find one regular file by its exact name and
//! return its bytes. It never writes to the disk, so a name such as `../../etc/passwd` in an
//! archive cannot do anything: the only name that matters is the one asked for. The archive is
//! checked against its checksum before this runs, but the reader does not trust it anyway: the
//! size of every entry is bounded, and so is the unpacked total (a gzip bomb).

use std::io::{self, Read};

use flate2::read::GzDecoder;
use ring::digest;

/// The largest program accepted. The real one is about 13 MB.
pub const MAX_PROGRAM: u64 = 64 * 1024 * 1024;
/// The most that is unpacked from one archive, whatever it holds.
const MAX_UNPACKED: u64 = 256 * 1024 * 1024;
const BLOCK: usize = 512;

/// The bytes of the regular file `wanted` in the gzip-compressed tar `archive`.
///
/// # Errors
///
/// A sentence: the archive is not readable, does not hold the file, holds it as something
/// other than a regular file, or the file is too big.
pub fn extract(archive: &[u8], wanted: &str) -> Result<Vec<u8>, String> {
    let mut reader = GzDecoder::new(archive).take(MAX_UNPACKED);
    loop {
        let mut header = [0u8; BLOCK];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(format!("the archive does not hold {wanted}"));
            }
            Err(_) => return Err("the archive cannot be read (is it damaged?)".to_owned()),
        }
        if header.iter().all(|&b| b == 0) {
            return Err(format!("the archive does not hold {wanted}"));
        }
        let name = entry_name(&header);
        let size = octal(&header[124..136]).ok_or("the archive has a bad entry size")?;
        let kind = header[156];
        if name == wanted {
            if !matches!(kind, b'0' | 0) {
                return Err(format!("{wanted} in the archive is not a regular file"));
            }
            if size == 0 || size > MAX_PROGRAM {
                return Err(format!("{wanted} in the archive has an unusable size"));
            }
            let mut program = Vec::new();
            let got = (&mut reader)
                .take(size)
                .read_to_end(&mut program)
                .map_err(|_| "the archive cannot be read (is it damaged?)".to_owned())?;
            if got as u64 != size {
                return Err("the archive is cut short".to_owned());
            }
            return Ok(program);
        }
        // Skip the data of this entry, rounded up to whole blocks.
        let padded = size.div_ceil(BLOCK as u64) * BLOCK as u64;
        let skipped = io::copy(&mut (&mut reader).take(padded), &mut io::sink())
            .map_err(|_| "the archive cannot be read (is it damaged?)".to_owned())?;
        if skipped != padded {
            return Err("the archive is cut short".to_owned());
        }
    }
}

/// `prefix/name` of a ustar header, as text up to the first zero byte of each field.
fn entry_name(header: &[u8; BLOCK]) -> String {
    let field = |range: std::ops::Range<usize>| {
        let bytes = &header[range];
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..end]).into_owned()
    };
    let name = field(0..100);
    // Only the POSIX format (`ustar\0`) has a prefix field there. GNU tar (`ustar  \0`) keeps
    // other times in those bytes.
    let is_posix = &header[257..263] == b"ustar\0";
    let prefix = if is_posix {
        field(345..500)
    } else {
        String::new()
    };
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

/// A tar number: octal digits, padded with zeros, blanks or a zero byte.
fn octal(field: &[u8]) -> Option<u64> {
    let text: Vec<u8> = field
        .iter()
        .copied()
        .take_while(|&b| b != 0)
        .filter(|&b| b != b' ')
        .collect();
    if text.is_empty() {
        return Some(0);
    }
    // A leading 0x80 byte means a base-256 number, used for sizes over 8 GiB: never ours.
    if !text.iter().all(|b| (b'0'..=b'7').contains(b)) {
        return None;
    }
    u64::from_str_radix(std::str::from_utf8(&text).ok()?, 8).ok()
}

/// The lower-case hexadecimal SHA-256 of `data`.
#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = digest::digest(&digest::SHA256, data);
    digest.as_ref().iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

/// The checksum that `sums` (the text of `sha256.sum`, `sha256sum` format) lists for the file
/// called `name`: `<hex>  <name>` or `<hex> *<name>`. The same rule as `install.sh`.
#[must_use]
pub fn wanted_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let sum = parts.next()?;
        let file = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name).then(|| sum.to_ascii_lowercase())
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::GzEncoder;

    use super::*;

    /// A ustar entry.
    fn entry(name: &str, kind: u8, data: &[u8]) -> Vec<u8> {
        let mut header = [0u8; BLOCK];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..107].copy_from_slice(b"0000755");
        header[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        let mut out = header.to_vec();
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
        out
    }

    fn gz(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut tar: Vec<u8> = parts.concat();
        tar.extend_from_slice(&[0u8; BLOCK * 2]);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap()
    }

    const NAME: &str = "oh-my-shell-x86_64-unknown-linux-gnu/oh-my-shell";

    #[test]
    fn the_wanted_file_is_found_among_others() {
        let archive = gz(&[
            entry("oh-my-shell-x86_64-unknown-linux-gnu/", b'5', b""),
            entry(
                "oh-my-shell-x86_64-unknown-linux-gnu/LICENSE",
                b'0',
                &[7; 700],
            ),
            entry(NAME, b'0', b"the program"),
            entry("oh-my-shell-x86_64-unknown-linux-gnu/README.md", b'0', b"x"),
        ]);
        assert_eq!(extract(&archive, NAME).unwrap(), b"the program");
    }

    #[test]
    fn the_first_file_is_found_too() {
        let archive = gz(&[entry(NAME, b'0', b"first")]);
        assert_eq!(extract(&archive, NAME).unwrap(), b"first");
    }

    #[test]
    fn a_missing_file_is_an_error() {
        let archive = gz(&[entry("other/file", b'0', b"x")]);
        let error = extract(&archive, NAME).unwrap_err();
        assert!(error.contains("does not hold"), "{error}");
    }

    #[test]
    fn only_the_exact_name_counts() {
        for name in [
            "oh-my-shell",
            "../oh-my-shell-x86_64-unknown-linux-gnu/oh-my-shell",
            "x/oh-my-shell-x86_64-unknown-linux-gnu/oh-my-shell",
            "oh-my-shell-x86_64-unknown-linux-gnu/oh-my-shell2",
        ] {
            let archive = gz(&[entry(name, b'0', b"x")]);
            assert!(extract(&archive, NAME).is_err(), "{name}");
        }
    }

    #[test]
    fn a_link_with_the_name_is_refused() {
        for kind in *b"125" {
            let archive = gz(&[entry(NAME, kind, b"")]);
            let error = extract(&archive, NAME).unwrap_err();
            assert!(error.contains("not a regular file"), "{error}");
        }
    }

    #[test]
    fn an_empty_file_is_refused() {
        let archive = gz(&[entry(NAME, b'0', b"")]);
        assert!(
            extract(&archive, NAME)
                .unwrap_err()
                .contains("unusable size")
        );
    }

    #[test]
    fn a_file_that_is_too_big_is_refused_before_it_is_read() {
        let mut header = entry(NAME, b'0', b"");
        // 65 MiB in octal.
        header[124..135].copy_from_slice(format!("{:011o}", MAX_PROGRAM + 1024 * 1024).as_bytes());
        let archive = gz(&[header]);
        assert!(
            extract(&archive, NAME)
                .unwrap_err()
                .contains("unusable size")
        );
    }

    #[test]
    fn a_cut_archive_is_an_error_and_not_a_short_file() {
        let full = gz(&[entry(NAME, b'0', &vec![1u8; 100_000])]);
        for keep in [full.len() / 2, full.len() - 30] {
            assert!(extract(&full[..keep], NAME).is_err(), "kept {keep}");
        }
        // A tar whose data stops before the size in its header says.
        let mut tar = entry(NAME, b'0', &vec![1u8; 5000]);
        tar.truncate(BLOCK + 1000);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(&tar).unwrap();
        let archive = encoder.finish().unwrap();
        assert!(extract(&archive, NAME).unwrap_err().contains("cut short"));
    }

    #[test]
    fn nonsense_is_an_error() {
        assert!(extract(b"not gzip at all", NAME).is_err());
        assert!(extract(b"", NAME).is_err());
    }

    #[test]
    fn a_bad_size_field_is_an_error() {
        let mut header = entry("other", b'0', b"x");
        header[124..135].copy_from_slice(b"zzzzzzzzzzz");
        let archive = gz(&[header]);
        assert!(
            extract(&archive, NAME)
                .unwrap_err()
                .contains("bad entry size")
        );
    }

    #[test]
    fn a_ustar_prefix_is_part_of_the_name() {
        let mut header = entry("oh-my-shell", b'0', b"prefixed");
        header[345..345 + 38].copy_from_slice(b"oh-my-shell-x86_64-unknown-linux-gnu\0\0");
        // Keep the name field and the prefix as two parts, as `tar` writes long names.
        let archive = gz(&[header]);
        assert_eq!(extract(&archive, NAME).unwrap(), b"prefixed");
    }

    #[test]
    fn the_hash_is_the_known_one() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn the_sum_of_a_file_is_found_by_its_exact_name() {
        let sums = "AAAA  oh-my-shell-x86_64-unknown-linux-gnu.tar.gz\n\
                    bbbb *oh-my-shell-aarch64-unknown-linux-gnu.tar.gz\n\
                    cccc  install.sh\n\
                    dddd  not a valid line with many words\n";
        assert_eq!(
            wanted_sum(sums, "oh-my-shell-x86_64-unknown-linux-gnu.tar.gz").as_deref(),
            Some("aaaa")
        );
        assert_eq!(
            wanted_sum(sums, "oh-my-shell-aarch64-unknown-linux-gnu.tar.gz").as_deref(),
            Some("bbbb")
        );
        assert_eq!(wanted_sum(sums, "install.sh").as_deref(), Some("cccc"));
        assert_eq!(wanted_sum(sums, "install"), None);
        assert_eq!(wanted_sum(sums, "missing"), None);
        assert_eq!(wanted_sum("", "install.sh"), None);
    }
}
