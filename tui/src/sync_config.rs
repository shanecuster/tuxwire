//! Reads `~/.config/tuxwire/sync.toml`: the settings for the optional CouchDB
//! sync of saved articles and notes (`docs/ARCHITECTURE.md` § Optional
//! CouchDB sync).
//!
//! This is the terminal-only half of sync. The shared `tuxwire-core` crate
//! does the actual syncing and is told everything it needs through a
//! `SyncConfig`; it must never read `$HOME` or a config file itself (the
//! Android app has neither). Finding the file, expanding `~`, and fetching the
//! password all happen here.
//!
//! ## Safety rules this module enforces
//!
//! - **Off by default.** No `sync.toml` means no sync, and this module never
//!   writes one for you (unlike `export.toml`): a file with your server's
//!   address in it should only exist because you made it.
//! - **The password is never in `sync.toml`.** A `password` line there is an
//!   error. It comes from the `TUXWIRE_SYNC_PASSWORD` environment variable, or
//!   from a `password_file` that only its owner can read.
//! - **Nothing here prints the password.** Error messages name the *file*
//!   (a path on this computer) but never its contents.

#![warn(clippy::missing_docs_in_private_items)]

use anyhow::{Context, anyhow, bail};
use std::path::{Path, PathBuf};
use tuxwire_core::sync::SyncConfig;

/// The environment variable that can hold the password. Checked before
/// `password_file`, so one-off runs (`TUXWIRE_SYNC_PASSWORD=... tuxwire sync`)
/// need no file at all.
pub const PASSWORD_ENV_VAR: &str = "TUXWIRE_SYNC_PASSWORD";

/// What `sync.toml` says, with the password deliberately *not* in it: only
/// the place to find the password is kept. Turn it into the settings core
/// wants with [`SyncSettings::to_core_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncSettings {
    /// The `enabled` switch. `false` unless the file says `true`.
    pub enabled: bool,
    /// The server address, exactly as written (core checks it is safe).
    pub url: String,
    /// The CouchDB database name.
    pub database: String,
    /// The login name.
    pub username: String,
    /// The `password_file` path with `~` already expanded, if one was given.
    pub password_file: Option<PathBuf>,
}

/// The shape of the file as TOML sees it: one `[sync]` table.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    /// The `[sync]` table.
    sync: RawSync,
}

/// The keys allowed inside `[sync]`. `deny_unknown_fields` makes a typo (or a
/// forbidden `password` key) an error instead of being silently ignored.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSync {
    /// Optional; a missing line means off.
    #[serde(default)]
    enabled: bool,
    /// Required: the server address.
    url: String,
    /// Required: the database name.
    database: String,
    /// Required: the login name.
    username: String,
    /// Optional: where the password file is (may start with `~`).
    #[serde(default)]
    password_file: Option<String>,
}

/// Where the settings file lives: `$XDG_CONFIG_HOME/tuxwire/sync.toml`, or
/// `~/.config/tuxwire/sync.toml` when that variable is not set. The same rule
/// as every other tuxwire config file.
pub fn sync_config_path() -> anyhow::Result<PathBuf> {
    // `if let Ok(..)` runs the block only when the variable exists.
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config_home)
            .join("tuxwire")
            .join("sync.toml"));
    }
    // Otherwise fall back to `$HOME/.config`. `.context(..)` adds a readable
    // explanation if `$HOME` is missing.
    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate sync.toml")?;
    Ok(PathBuf::from(home).join(".config/tuxwire/sync.toml"))
}

/// Loads the settings from the usual place. `Ok(None)` means "no `sync.toml`":
/// sync is simply not set up, which is not an error.
pub fn load() -> anyhow::Result<Option<SyncSettings>> {
    let path = sync_config_path()?;
    // `$HOME` is only needed to expand a `~` in `password_file`, so a missing
    // one is passed on as `None` rather than failing here.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    load_at(&path, home.as_deref())
}

/// Loads the settings from `path`. `home` is what a leading `~` in
/// `password_file` becomes. Split from [`load`] so tests can use a throwaway
/// folder without touching environment variables.
///
/// Returns `Ok(None)` when the file does not exist; any other problem
/// (unreadable, malformed, a `password` line) is an error with a plain
/// explanation.
pub fn load_at(path: &Path, home: Option<&Path>) -> anyhow::Result<Option<SyncSettings>> {
    // Step 1: read the file. "Not found" means sync is not set up.
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    // Step 2: refuse a password written into the file, with a message that
    // says what to do instead. Done on the loose TOML table first because the
    // strict parse below would only say "unknown field".
    let table: toml::Table =
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    if table
        .get("sync")
        .and_then(|value| value.as_table())
        .is_some_and(|sync| sync.contains_key("password"))
    {
        bail!(
            "{} has a `password` line. The password must not be kept in this file: \
             remove the line, then use the {PASSWORD_ENV_VAR} environment variable \
             or a `password_file` (see sync.toml.example).",
            path.display()
        );
    }

    // Step 3: parse it for real. A typo in a key name is an error here.
    let raw: RawFile =
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    let raw = raw.sync;

    // Step 4: expand `~` in the password file's path, if there is one.
    let password_file = match raw.password_file {
        Some(text) => Some(expand_tilde(&text, home)?),
        None => None,
    };

    Ok(Some(SyncSettings {
        enabled: raw.enabled,
        url: raw.url,
        database: raw.database,
        username: raw.username,
        password_file,
    }))
}

/// Replaces a leading `~` with the home directory (a shell habit that files
/// and Rust do not do for you). Anything else is returned unchanged.
fn expand_tilde(raw: &str, home: Option<&Path>) -> anyhow::Result<PathBuf> {
    // `strip_prefix` returns the rest of the text if it starts with "~/".
    if let Some(rest) = raw.strip_prefix("~/") {
        let home =
            home.ok_or_else(|| anyhow!("can't expand '~' in password_file: HOME is not set"))?;
        return Ok(home.join(rest));
    }
    Ok(PathBuf::from(raw))
}

impl SyncSettings {
    /// Builds the settings object core wants, fetching the password the way
    /// the file's header describes: the environment variable first, then the
    /// `password_file`.
    ///
    /// Reads `TUXWIRE_SYNC_PASSWORD` from the real environment. See
    /// [`SyncSettings::to_core_config_with`] for the version tests use.
    pub fn to_core_config(&self) -> anyhow::Result<SyncConfig> {
        // `.ok()` turns "variable missing" into `None`.
        self.to_core_config_with(std::env::var(PASSWORD_ENV_VAR).ok())
    }

    /// Like [`SyncSettings::to_core_config`], but the environment variable's
    /// value is passed in (`None` = not set), so tests do not have to change
    /// the real environment.
    pub fn to_core_config_with(&self, env_password: Option<String>) -> anyhow::Result<SyncConfig> {
        // Step 1: an environment password wins, if it is not empty.
        let password = match env_password.filter(|value| !value.is_empty()) {
            Some(value) => value,
            // Step 2: otherwise read the password file (checking who can read it).
            None => match &self.password_file {
                Some(path) => read_password_file(path)?,
                None => bail!(
                    "no password: set the {PASSWORD_ENV_VAR} environment variable, \
                     or add a `password_file` to sync.toml"
                ),
            },
        };

        // Step 3: hand everything to core. The password is moved into the
        // object and is hidden from its `Debug` output.
        Ok(SyncConfig::new(
            self.enabled,
            self.url.clone(),
            self.database.clone(),
            self.username.clone(),
            password,
        ))
    }
}

/// Reads a password file, but only if nobody but its owner can read it.
///
/// On Linux a file's "mode" says who may read it. A password file that other
/// users (or a group) can read is refused, with the command that fixes it, so
/// a careless `chmod` cannot quietly expose the password. The file's contents
/// are never put in an error message.
fn read_password_file(path: &Path) -> anyhow::Result<String> {
    // Step 1: check the permissions before reading anything.
    #[cfg(unix)]
    {
        // `PermissionsExt` adds `.mode()` to file permissions on Unix.
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)
            .with_context(|| format!("failed to read the password file {}", path.display()))?;
        // `0o077` selects the "group" and "everyone else" bits; any of them
        // set means someone besides the owner could read the file.
        if meta.permissions().mode() & 0o077 != 0 {
            bail!(
                "the password file {0} can be read by other users. Fix it with: chmod 600 {0}",
                path.display()
            );
        }
    }

    // Step 2: read it, and drop the line ending an editor adds at the end.
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read the password file {}", path.display()))?;
    let password = text.trim_end_matches(['\n', '\r']).to_string();
    if password.is_empty() {
        bail!("the password file {} is empty", path.display());
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temp folder, unique per call (process id + counter) so tests
    /// running in parallel never share one.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tuxwire-sync-cfg-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes `text` as `sync.toml` in a fresh folder and returns its path.
    fn write_config(text: &str) -> PathBuf {
        let path = tempdir().join("sync.toml");
        std::fs::write(&path, text).unwrap();
        path
    }

    /// A complete, valid file used by several tests.
    const GOOD: &str = r#"
[sync]
enabled = true
url = "https://couch.example.net"
database = "tuxwire"
username = "tuxwire"
password_file = "~/pw"
"#;

    /// No file means "not set up", which is not an error.
    #[test]
    fn a_missing_file_means_sync_is_not_set_up() {
        let path = tempdir().join("sync.toml");
        assert_eq!(load_at(&path, None).unwrap(), None);
    }

    /// A good file loads, and `~` in the password file becomes the home folder.
    #[test]
    fn a_good_file_loads_and_expands_the_tilde() {
        let path = write_config(GOOD);
        let settings = load_at(&path, Some(Path::new("/home/me")))
            .unwrap()
            .unwrap();
        assert!(settings.enabled);
        assert_eq!(settings.database, "tuxwire");
        assert_eq!(settings.password_file, Some(PathBuf::from("/home/me/pw")));
    }

    /// Sync is off unless the file says otherwise.
    #[test]
    fn enabled_defaults_to_off() {
        let path = write_config(
            "[sync]\nurl = \"https://x.example\"\ndatabase = \"d\"\nusername = \"u\"\n",
        );
        assert!(!load_at(&path, None).unwrap().unwrap().enabled);
    }

    /// A password in the file is refused with a message about what to do, and
    /// the message does not repeat the password.
    #[test]
    fn a_password_in_the_file_is_refused_without_repeating_it() {
        let path = write_config(
            "[sync]\nurl = \"https://x.example\"\ndatabase = \"d\"\nusername = \"u\"\npassword = \"hunter2-secret\"\n",
        );
        let err = format!("{:#}", load_at(&path, None).unwrap_err());
        assert!(err.contains("must not be kept in this file"), "{err}");
        assert!(!err.contains("hunter2-secret"), "password leaked: {err}");
    }

    /// A misspelt key is an error, not silently ignored.
    #[test]
    fn an_unknown_key_is_an_error() {
        let path = write_config(
            "[sync]\nurl = \"https://x.example\"\ndatabase = \"d\"\nusername = \"u\"\nenabeld = true\n",
        );
        assert!(load_at(&path, None).is_err());
    }

    /// The environment password is used first; with neither it nor a file,
    /// the error says what to do.
    #[test]
    fn the_environment_password_wins_and_missing_is_explained() {
        let settings = SyncSettings {
            enabled: true,
            url: "https://x.example".into(),
            database: "d".into(),
            username: "u".into(),
            password_file: None,
        };
        assert!(settings.to_core_config_with(Some("pw".into())).is_ok());
        // An empty variable counts as not set.
        let err = settings
            .to_core_config_with(Some(String::new()))
            .unwrap_err()
            .to_string();
        assert!(err.contains(PASSWORD_ENV_VAR), "{err}");
        assert!(settings.to_core_config_with(None).is_err());
    }

    /// A password file only its owner can read is accepted (line ending
    /// dropped); one others can read is refused, with the fix named and the
    /// password not shown.
    #[cfg(unix)]
    #[test]
    fn a_password_file_must_be_private() {
        use std::os::unix::fs::PermissionsExt;
        let file = tempdir().join("pw");
        std::fs::write(&file, "topsecret-value\n").unwrap();
        let settings = SyncSettings {
            enabled: true,
            url: "https://x.example".into(),
            database: "d".into(),
            username: "u".into(),
            password_file: Some(file.clone()),
        };

        // Readable by others: refused.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = format!("{:#}", settings.to_core_config_with(None).unwrap_err());
        assert!(err.contains("chmod 600"), "{err}");
        assert!(!err.contains("topsecret-value"), "password leaked: {err}");

        // Owner only: accepted.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(settings.to_core_config_with(None).is_ok());

        // Empty file: refused.
        std::fs::write(&file, "\n").unwrap();
        assert!(settings.to_core_config_with(None).is_err());
    }
}
