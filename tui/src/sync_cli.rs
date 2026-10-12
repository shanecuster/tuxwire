//! The terminal's way of running the optional CouchDB sync: the
//! `tuxwire sync` command, and the same job for the reader's `c` key.
//!
//! ```text
//! tuxwire sync            copy saved articles and notes to / from your server
//! tuxwire sync --test     check the server, database and login; sends nothing
//! ```
//!
//! Both run *instead of* opening the reader and do not download any feeds.
//! The syncing itself is `tuxwire_core::sync` (shared with the Android app);
//! this module only finds the settings (`sync_config`), opens the database,
//! and prints the result.
//!
//! Nothing here runs unless the person asked: there is no timer and no
//! startup sync. And with `enabled = false` in `sync.toml` (or no file at
//! all) `tuxwire sync` refuses before touching the network.

#![warn(clippy::missing_docs_in_private_items)]

use crate::paths;
use crate::sync_config::{self, SyncSettings};
use anyhow::bail;
use std::sync::Mutex;

/// What to do when the person typed `tuxwire sync ...`.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// `tuxwire sync`: run one sync.
    Sync,
    /// `tuxwire sync --test`: only check the connection.
    Test,
}

/// Reads the words typed after `tuxwire`.
///
/// Returns `Ok(None)` when the first word is not `sync` (so the caller can
/// try something else), `Ok(Some(command))` for a good `sync` command, and
/// `Err(message)` when it starts with `sync` but the rest is wrong, so a typo
/// never quietly does something different.
pub fn parse_args(args: &[String]) -> Result<Option<Command>, String> {
    // Not ours: leave it for the other commands.
    if args.first().map(String::as_str) != Some("sync") {
        return Ok(None);
    }
    // Look at everything after the word `sync`.
    match args[1..].iter().map(String::as_str).collect::<Vec<_>>()[..] {
        [] => Ok(Some(Command::Sync)),
        ["--test"] => Ok(Some(Command::Test)),
        _ => Err("sync takes no arguments except --test".to_string()),
    }
}

/// Explains how to set sync up, for when there is no `sync.toml`.
const NOT_SET_UP: &str = "sync is not set up. It is optional and off by default.\n\
To use it, copy sync.toml.example to ~/.config/tuxwire/sync.toml, fill it in, \
and set enabled = true. (The password goes in the TUXWIRE_SYNC_PASSWORD \
environment variable or a private password_file, never in sync.toml.)";

/// Loads `sync.toml`, turning "no file" into the explanatory error above.
fn load_settings() -> anyhow::Result<SyncSettings> {
    match sync_config::load()? {
        Some(settings) => Ok(settings),
        None => bail!("{NOT_SET_UP}"),
    }
}

/// Runs a `tuxwire sync` command and prints what happened.
///
/// Errors come back as plain sentences (core's wording never includes the
/// server address or the password) and `main` prints them.
pub async fn run(command: Command) -> anyhow::Result<()> {
    let settings = load_settings()?;
    match command {
        Command::Test => {
            // Works even while `enabled = false`, so the setup can be
            // checked before turning sync on.
            let config = settings.to_core_config()?;
            tuxwire_core::sync::test_connection(&config).await?;
            println!("Connection works: the server, database and login are all fine.");
            Ok(())
        }
        Command::Sync => {
            // Refuse before reading the password or opening anything.
            if !settings.enabled {
                bail!(
                    "sync is switched off (enabled = false in sync.toml), so nothing was \
                     sent or fetched. Set enabled = true to use it."
                );
            }
            let config = settings.to_core_config()?;
            let summary = sync_with_own_connection(&config).await?;
            println!("Synced: {summary}");
            Ok(())
        }
    }
}

/// Opens the database on its own connection and runs one sync on it.
///
/// A separate connection (rather than the reader's) because core's sync wants
/// the database inside a `Mutex` so it can let go of it while waiting on the
/// network. SQLite lets two connections in one program share a file; they
/// never run at the same moment because the reader waits for the sync.
async fn sync_with_own_connection(
    config: &tuxwire_core::sync::SyncConfig,
) -> anyhow::Result<String> {
    // `Mutex::new` moves the storage inside the lock.
    let storage = Mutex::new(paths::open_storage()?);
    let report = tuxwire_core::sync::sync_now(&storage, config).await?;
    Ok(report.summary())
}

/// What the reader's `c` key calls: run one sync and return a one-line
/// message for the footer. Never panics and never returns the password;
/// problems come back as `Err` text the caller shows.
///
/// The reader's event loop is ordinary (non-`async`) code running inside
/// tokio's runtime, so, like the refresh key, this uses
/// `block_in_place` + `block_on` to wait for the async sync: it tells tokio
/// "this thread is about to block" and then drives the future to the end.
pub fn sync_for_reader() -> anyhow::Result<String> {
    let settings = load_settings()?;
    if !settings.enabled {
        bail!("sync is switched off (enabled = false in sync.toml)");
    }
    let config = settings.to_core_config()?;
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(sync_with_own_connection(&config))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turns string literals into the owned words `parse_args` takes.
    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| word.to_string()).collect()
    }

    /// `sync` and `sync --test` are understood; other words are not ours.
    #[test]
    fn parses_the_sync_commands() {
        assert_eq!(parse_args(&words(&["sync"])), Ok(Some(Command::Sync)));
        assert_eq!(
            parse_args(&words(&["sync", "--test"])),
            Ok(Some(Command::Test))
        );
        assert_eq!(parse_args(&words(&[])), Ok(None));
        assert_eq!(parse_args(&words(&["import-opml", "x"])), Ok(None));
    }

    /// A typo after `sync` is an error, not a different command.
    #[test]
    fn extra_words_after_sync_are_an_error() {
        assert!(parse_args(&words(&["sync", "--tset"])).is_err());
        assert!(parse_args(&words(&["sync", "--test", "now"])).is_err());
    }
}
