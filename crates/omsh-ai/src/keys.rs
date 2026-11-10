//! Where API keys are kept (blueprint §14.1, D7).
//!
//! 1. The system keyring (Secret Service over D-Bus; GNOME Keyring, KWallet), in an entry named
//!    `oh-my-shell/<provider>`.
//! 2. When there is no keyring, a file `credentials` in the configuration directory: the
//!    directory mode 0700, the file mode 0600, lines `provider=key`.
//!
//! A key is never in `config.toml`, in a log, in history or in an environment variable.
//! Nothing here connects to the keyring until a key is asked for or stored.

use std::collections::HashMap;
use std::fmt;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, ErrorKind, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use keyring_core::api::CredentialStoreApi;
use omsh_config::Provider;

use crate::secret::Secret;

/// The service name of every entry in the keyring.
const SERVICE: &str = "oh-my-shell";

/// Why a key could not be read, stored or deleted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyError {
    /// The store cannot be reached (no D-Bus, no keyring daemon).
    Unavailable(String),
    /// The store would not be safe to use (a credentials file others can read).
    Refused(String),
    /// Any other failure.
    Failed(String),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(why) | Self::Refused(why) | Self::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for KeyError {}

/// A place that keeps keys.
pub trait KeyStore: Send + Sync {
    /// Where this store is, for a message: "the system keyring" or the file's path.
    fn place(&self) -> String;
    /// The key for `provider`, if there is one.
    ///
    /// # Errors
    ///
    /// [`KeyError`] when the store cannot be read.
    fn get(&self, provider: Provider) -> Result<Option<Secret>, KeyError>;
    /// Stores the key, replacing an older one.
    ///
    /// # Errors
    ///
    /// [`KeyError`] when the store cannot be written.
    fn set(&self, provider: Provider, key: &Secret) -> Result<(), KeyError>;
    /// Removes the key. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// [`KeyError`] when the store cannot be changed.
    fn delete(&self, provider: Provider) -> Result<bool, KeyError>;
}

// -- The system keyring -----------------------------------------------------------------

/// The Secret Service, through `keyring-core`.
pub struct SecretService {
    store: Arc<zbus_secret_service_keyring_store::Store>,
}

impl SecretService {
    /// Connects to the keyring daemon.
    ///
    /// # Errors
    ///
    /// [`KeyError::Unavailable`] when there is no session bus or no Secret Service on it.
    pub fn connect() -> Result<Self, KeyError> {
        let store = zbus_secret_service_keyring_store::Store::new()
            .map_err(|error| KeyError::Unavailable(brief(&error)))?;
        Ok(Self { store })
    }

    fn entry(&self, provider: Provider) -> Result<keyring_core::Entry, KeyError> {
        self.store
            .build(SERVICE, provider.as_str(), None)
            .map_err(|error| KeyError::Failed(brief(&error)))
    }
}

/// One line of an error from the keyring crates. Their messages hold no secrets.
fn brief(error: &dyn fmt::Display) -> String {
    let text = error.to_string();
    let line = text.lines().next().unwrap_or_default();
    line.chars().take(160).collect()
}

impl KeyStore for SecretService {
    fn place(&self) -> String {
        "the system keyring".to_owned()
    }

    fn get(&self, provider: Provider) -> Result<Option<Secret>, KeyError> {
        match self.entry(provider)?.get_password() {
            Ok(text) => Ok(Secret::new(text)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(error) => Err(KeyError::Failed(brief(&error))),
        }
    }

    fn set(&self, provider: Provider, key: &Secret) -> Result<(), KeyError> {
        self.entry(provider)?
            .set_password(key.expose())
            .map_err(|error| KeyError::Failed(brief(&error)))
    }

    fn delete(&self, provider: Provider) -> Result<bool, KeyError> {
        match self.entry(provider)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring_core::Error::NoEntry) => Ok(false),
            Err(error) => Err(KeyError::Failed(brief(&error))),
        }
    }
}

// -- The credentials file ---------------------------------------------------------------

/// The fallback: a file only the user can read.
pub struct CredentialsFile {
    path: PathBuf,
}

impl CredentialsFile {
    #[must_use]
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The lines of the file, as (name, key) pairs, in order. `strict` refuses a file that
    /// other users can read: for reading a key out of it. Writing is not strict, so that
    /// storing a key again repairs such a file (it is rewritten with mode 0600).
    fn read_lines(&self, strict: bool) -> Result<Vec<(String, String)>, KeyError> {
        let mut file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(KeyError::Failed(format!(
                    "{}: {error}",
                    self.path.display()
                )));
            }
        };
        let meta = file
            .metadata()
            .map_err(|error| KeyError::Failed(format!("{}: {error}", self.path.display())))?;
        if !meta.is_file() {
            return Err(KeyError::Refused(format!(
                "{} is not a regular file",
                self.path.display()
            )));
        }
        if strict && meta.permissions().mode() & 0o077 != 0 {
            return Err(KeyError::Refused(format!(
                "{} can be read by other users; run: chmod 600 {}",
                self.path.display(),
                self.path.display()
            )));
        }
        if meta.len() > 64 * 1024 {
            return Err(KeyError::Refused(format!(
                "{} is too big to be a credentials file",
                self.path.display()
            )));
        }
        let mut text = String::new();
        file.read_to_string(&mut text)
            .map_err(|error| KeyError::Failed(format!("{}: {error}", self.path.display())))?;
        Ok(text
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(name, key)| (name.trim().to_owned(), key.trim().to_owned()))
            .collect())
    }

    /// Writes the lines: the directory is made 0700 (and tightened to 0700 if it already
    /// exists with looser bits), the file is written to a temporary name with mode 0600 and
    /// renamed over the old one.
    fn write_lines(&self, lines: &[(String, String)]) -> Result<(), KeyError> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let fail = |error: io::Error| KeyError::Failed(format!("{}: {error}", self.path.display()));
        let dir = self
            .path
            .parent()
            .ok_or_else(|| KeyError::Failed("the credentials file has no directory".to_owned()))?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(fail)?;
        let mode = fs::metadata(dir).map_err(fail)?.permissions().mode();
        if mode & 0o077 != 0 {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(fail)?;
        }

        let mut text = String::new();
        for (name, key) in lines {
            text.push_str(name);
            text.push('=');
            text.push_str(key);
            text.push('\n');
        }
        let temporary = dir.join(format!(
            ".credentials.{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut out = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            // Exactly 0600, whatever the umask or an old file had.
            out.set_permissions(fs::Permissions::from_mode(0o600))?;
            out.write_all(text.as_bytes())?;
            out.sync_all()?;
            fs::rename(&temporary, &self.path)
        })();
        if written.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        written.map_err(fail)
    }
}

impl KeyStore for CredentialsFile {
    fn place(&self) -> String {
        self.path.display().to_string()
    }

    fn get(&self, provider: Provider) -> Result<Option<Secret>, KeyError> {
        Ok(self
            .read_lines(true)?
            .into_iter()
            .find(|(name, _)| name == provider.as_str())
            .and_then(|(_, key)| Secret::new(key)))
    }

    fn set(&self, provider: Provider, key: &Secret) -> Result<(), KeyError> {
        let mut lines = self.read_lines(false)?;
        let name = provider.as_str();
        lines.retain(|(existing, _)| existing != name);
        lines.push((name.to_owned(), key.expose().to_owned()));
        self.write_lines(&lines)
    }

    fn delete(&self, provider: Provider) -> Result<bool, KeyError> {
        let mut lines = self.read_lines(false)?;
        let before = lines.len();
        lines.retain(|(existing, _)| existing != provider.as_str());
        if lines.len() == before {
            return Ok(false);
        }
        self.write_lines(&lines)?;
        Ok(true)
    }
}

/// The fallback when there is nowhere to put a file.
struct NoFile;

impl KeyStore for NoFile {
    fn place(&self) -> String {
        "no credentials file (there is no configuration directory)".to_owned()
    }

    fn get(&self, _provider: Provider) -> Result<Option<Secret>, KeyError> {
        Ok(None)
    }

    fn set(&self, _provider: Provider, _key: &Secret) -> Result<(), KeyError> {
        Err(KeyError::Unavailable(
            "there is no configuration directory for a credentials file (is HOME set?)".to_owned(),
        ))
    }

    fn delete(&self, _provider: Provider) -> Result<bool, KeyError> {
        Ok(false)
    }
}

// -- Both together ----------------------------------------------------------------------

/// Where a key was stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub place: String,
    /// Why the keyring was not used, when the file was.
    pub keyring_problem: Option<String>,
}

type Connect = Box<dyn Fn() -> Result<Box<dyn KeyStore>, KeyError> + Send + Sync>;

/// The keyring if there is one, the credentials file otherwise (D7).
///
/// Reading looks in the keyring first and then in the file, so a key stored while no keyring
/// was running is still found later. The keyring is connected to on first use.
pub struct Keys {
    connect: Connect,
    keyring: OnceLock<Result<Box<dyn KeyStore>, KeyError>>,
    file: Box<dyn KeyStore>,
}

impl Keys {
    /// The real stores: the Secret Service, and `credentials_file` as the fallback. With no
    /// file (there is no configuration directory) the fallback holds nothing and takes
    /// nothing.
    #[must_use]
    pub fn system(credentials_file: Option<PathBuf>) -> Self {
        let file: Box<dyn KeyStore> = match credentials_file {
            Some(path) => Box::new(CredentialsFile::new(path)),
            None => Box::new(NoFile),
        };
        Self::with(
            Box::new(|| SecretService::connect().map(|s| Box::new(s) as Box<dyn KeyStore>)),
            file,
        )
    }

    /// Any two stores. `connect` is called once, at first use.
    #[must_use]
    pub fn with(connect: Connect, file: Box<dyn KeyStore>) -> Self {
        Self {
            connect,
            keyring: OnceLock::new(),
            file,
        }
    }

    fn keyring(&self) -> Result<&dyn KeyStore, &KeyError> {
        self.keyring
            .get_or_init(|| (self.connect)())
            .as_ref()
            .map(|store| &**store)
    }

    /// The key for `provider` and where it was found.
    ///
    /// # Errors
    ///
    /// A [`KeyError`] when a store that exists cannot be read and the key is not in the other.
    pub fn get(&self, provider: Provider) -> Result<Option<(Secret, String)>, KeyError> {
        let mut trouble = None;
        // No keyring on this machine is normal; the file is the place then.
        if let Ok(keyring) = self.keyring() {
            match keyring.get(provider) {
                Ok(Some(key)) => return Ok(Some((key, keyring.place()))),
                Ok(None) => {}
                Err(error) => trouble = Some(error),
            }
        }
        match self.file.get(provider) {
            Ok(Some(key)) => Ok(Some((key, self.file.place()))),
            Ok(None) => trouble.map_or(Ok(None), Err),
            Err(error) => Err(error),
        }
    }

    /// Stores the key in the keyring, or in the file when the keyring cannot be used. After a
    /// store in the keyring, an older copy in the file is removed.
    ///
    /// # Errors
    ///
    /// A [`KeyError`] when neither store takes it.
    pub fn set(&self, provider: Provider, key: &Secret) -> Result<Stored, KeyError> {
        let problem = match self.keyring() {
            Ok(keyring) => match keyring.set(provider, key) {
                Ok(()) => {
                    // One copy only; a failure here changes nothing for the user.
                    let _ = self.file.delete(provider);
                    return Ok(Stored {
                        place: keyring.place(),
                        keyring_problem: None,
                    });
                }
                Err(error) => error.to_string(),
            },
            Err(error) => error.to_string(),
        };
        self.file.set(provider, key)?;
        Ok(Stored {
            place: self.file.place(),
            keyring_problem: Some(problem),
        })
    }

    /// Removes the key from both stores. Returns whether there was one.
    ///
    /// # Errors
    ///
    /// A [`KeyError`] when a store that exists cannot be changed.
    pub fn delete(&self, provider: Provider) -> Result<bool, KeyError> {
        let mut removed = false;
        if let Ok(keyring) = self.keyring() {
            removed |= keyring.delete(provider)?;
        }
        removed |= self.file.delete(provider)?;
        Ok(removed)
    }
}

/// Reads `provider=key` pairs the way the file store does; used by tests that look at the
/// file from outside.
#[must_use]
pub fn parse_credentials(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, key)| (name.trim().to_owned(), key.trim().to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::fake::MemoryStore;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "omsh-keys-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn key(text: &str) -> Secret {
        Secret::new(text.to_owned()).unwrap()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn the_file_store_round_trips_and_deletes() {
        let dir = Scratch::new();
        let store = CredentialsFile::new(dir.0.join("conf").join("credentials"));
        assert!(store.get(Provider::OpenAi).unwrap().is_none());
        store.set(Provider::OpenAi, &key("sk-one")).unwrap();
        store.set(Provider::Anthropic, &key("sk-two")).unwrap();
        assert_eq!(
            store.get(Provider::OpenAi).unwrap().unwrap().expose(),
            "sk-one"
        );
        assert_eq!(
            store.get(Provider::Anthropic).unwrap().unwrap().expose(),
            "sk-two"
        );
        store.set(Provider::OpenAi, &key("sk-new")).unwrap();
        assert_eq!(
            store.get(Provider::OpenAi).unwrap().unwrap().expose(),
            "sk-new"
        );
        assert!(store.delete(Provider::OpenAi).unwrap());
        assert!(!store.delete(Provider::OpenAi).unwrap());
        assert!(store.get(Provider::OpenAi).unwrap().is_none());
        assert_eq!(
            store.get(Provider::Anthropic).unwrap().unwrap().expose(),
            "sk-two"
        );
    }

    #[test]
    fn the_new_directory_is_0700_and_the_file_0600() {
        let dir = Scratch::new();
        let file = dir.0.join("oh-my-shell").join("credentials");
        CredentialsFile::new(file.clone())
            .set(Provider::Gemini, &key("AIza-test"))
            .unwrap();
        assert_eq!(mode(file.parent().unwrap()), 0o700);
        assert_eq!(mode(&file), 0o600);
        // No temporary file is left.
        assert_eq!(fs::read_dir(file.parent().unwrap()).unwrap().count(), 1);
    }

    #[test]
    fn a_directory_that_is_too_open_is_tightened() {
        let dir = Scratch::new();
        let conf = dir.0.join("oh-my-shell");
        fs::create_dir(&conf).unwrap();
        fs::set_permissions(&conf, fs::Permissions::from_mode(0o755)).unwrap();
        CredentialsFile::new(conf.join("credentials"))
            .set(Provider::OpenAi, &key("sk-x"))
            .unwrap();
        assert_eq!(mode(&conf), 0o700);
    }

    #[test]
    fn a_file_others_can_read_is_not_read_but_storing_again_repairs_it() {
        let dir = Scratch::new();
        let path = dir.0.join("credentials");
        fs::write(&path, "openai=sk-old\nollama-note=keep\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let store = CredentialsFile::new(path.clone());
        let error = store.get(Provider::OpenAi).unwrap_err();
        assert!(matches!(error, KeyError::Refused(_)));
        assert!(error.to_string().contains("chmod 600"), "{error}");
        assert!(!error.to_string().contains("sk-old"));

        store.set(Provider::Anthropic, &key("sk-new")).unwrap();
        assert_eq!(mode(&path), 0o600);
        assert_eq!(
            store.get(Provider::OpenAi).unwrap().unwrap().expose(),
            "sk-old"
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("ollama-note=keep"),
            "unknown lines are kept: {text}"
        );
    }

    #[test]
    fn a_group_readable_file_is_refused_too() {
        let dir = Scratch::new();
        let path = dir.0.join("credentials");
        fs::write(&path, "openai=sk-old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(matches!(
            CredentialsFile::new(path).get(Provider::OpenAi),
            Err(KeyError::Refused(_))
        ));
    }

    #[test]
    fn the_keyring_is_used_when_there_is_one_and_the_file_copy_goes() {
        let dir = Scratch::new();
        let file = CredentialsFile::new(dir.0.join("credentials"));
        file.set(Provider::OpenAi, &key("sk-in-file")).unwrap();
        let ring = MemoryStore::named("the test keyring");
        let keys = Keys::with(
            Box::new({
                let ring = ring.clone();
                move || Ok(Box::new(ring.clone()) as Box<dyn KeyStore>)
            }),
            Box::new(CredentialsFile::new(dir.0.join("credentials"))),
        );
        let stored = keys.set(Provider::OpenAi, &key("sk-in-ring")).unwrap();
        assert_eq!(stored.place, "the test keyring");
        assert!(stored.keyring_problem.is_none());
        let (found, place) = keys.get(Provider::OpenAi).unwrap().unwrap();
        assert_eq!(
            (found.expose(), place.as_str()),
            ("sk-in-ring", "the test keyring")
        );
        assert!(
            file.get(Provider::OpenAi).unwrap().is_none(),
            "one copy only"
        );
    }

    #[test]
    fn without_a_keyring_the_file_is_used_and_the_reason_is_given() {
        let dir = Scratch::new();
        let path = dir.0.join("credentials");
        let keys = Keys::with(
            Box::new(|| Err(KeyError::Unavailable("no session bus".to_owned()))),
            Box::new(CredentialsFile::new(path.clone())),
        );
        let stored = keys.set(Provider::Anthropic, &key("sk-ant")).unwrap();
        assert_eq!(stored.place, path.display().to_string());
        assert_eq!(stored.keyring_problem.as_deref(), Some("no session bus"));
        let (found, place) = keys.get(Provider::Anthropic).unwrap().unwrap();
        assert_eq!(found.expose(), "sk-ant");
        assert_eq!(place, path.display().to_string());
    }

    #[test]
    fn a_keyring_that_refuses_a_write_falls_back_to_the_file() {
        let dir = Scratch::new();
        let ring = MemoryStore::named("ring");
        ring.fail_with("the keyring is locked");
        let keys = Keys::with(
            Box::new({
                let ring = ring.clone();
                move || Ok(Box::new(ring.clone()) as Box<dyn KeyStore>)
            }),
            Box::new(CredentialsFile::new(dir.0.join("credentials"))),
        );
        let stored = keys.set(Provider::OpenAi, &key("sk-x")).unwrap();
        assert!(stored.place.ends_with("credentials"));
        assert_eq!(
            stored.keyring_problem.as_deref(),
            Some("the keyring is locked")
        );
    }

    #[test]
    fn a_locked_keyring_is_reported_and_not_mistaken_for_no_key() {
        let dir = Scratch::new();
        let ring = MemoryStore::named("ring");
        ring.fail_with("the keyring is locked");
        let keys = Keys::with(
            Box::new({
                let ring = ring.clone();
                move || Ok(Box::new(ring.clone()) as Box<dyn KeyStore>)
            }),
            Box::new(CredentialsFile::new(dir.0.join("credentials"))),
        );
        assert_eq!(
            keys.get(Provider::OpenAi).unwrap_err(),
            KeyError::Failed("the keyring is locked".to_owned())
        );
    }

    #[test]
    fn delete_removes_from_both() {
        let dir = Scratch::new();
        let file = CredentialsFile::new(dir.0.join("credentials"));
        file.set(Provider::Gemini, &key("g-file")).unwrap();
        let ring = MemoryStore::named("ring");
        ring.set(Provider::Gemini, &key("g-ring")).unwrap();
        let keys = Keys::with(
            Box::new({
                let ring = ring.clone();
                move || Ok(Box::new(ring.clone()) as Box<dyn KeyStore>)
            }),
            Box::new(CredentialsFile::new(dir.0.join("credentials"))),
        );
        assert!(keys.delete(Provider::Gemini).unwrap());
        assert!(keys.get(Provider::Gemini).unwrap().is_none());
        assert!(!keys.delete(Provider::Gemini).unwrap());
    }

    #[test]
    fn the_keyring_is_not_connected_until_it_is_needed() {
        use std::sync::atomic::AtomicBool;
        let touched = Arc::new(AtomicBool::new(false));
        let dir = Scratch::new();
        let keys = Keys::with(
            Box::new({
                let touched = Arc::clone(&touched);
                move || {
                    touched.store(true, Ordering::Relaxed);
                    Err(KeyError::Unavailable("x".to_owned()))
                }
            }),
            Box::new(CredentialsFile::new(dir.0.join("credentials"))),
        );
        assert!(
            !touched.load(Ordering::Relaxed),
            "building Keys connects to nothing"
        );
        let _ = keys.get(Provider::OpenAi);
        assert!(touched.load(Ordering::Relaxed));
    }

    #[test]
    fn the_entry_names_are_service_and_provider() {
        assert_eq!(SERVICE, "oh-my-shell");
        let names: Vec<_> = Provider::ALL.iter().map(|p| p.as_str()).collect();
        assert_eq!(
            names,
            [
                "ollama",
                "anthropic",
                "openai",
                "gemini",
                "openai-compatible"
            ]
        );
    }
}
