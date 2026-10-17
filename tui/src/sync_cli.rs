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
//! With `feeds = true` in `sync.toml` the same command also mirrors the feed
//! list (see `feed_sync`): feeds another device added are written to the
//! chosen list's file, and feeds another device removed are only *offered* --
//! `tuxwire sync` asks y/N for each one, and when it is not run from a
//! terminal it lists them and removes nothing.
//!
//! Nothing here runs unless the person asked: there is no timer and no
//! startup sync. And with `enabled = false` in `sync.toml` (or no file at
//! all) `tuxwire sync` refuses before touching the network.

#![warn(clippy::missing_docs_in_private_items)]

use crate::feed_sync::{self, FeedSyncOutcome};
use crate::paths;
use crate::sync_config::{self, SyncSettings};
use anyhow::bail;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Mutex;
use tuxwire_core::storage::FeedEntry;

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
            let full = sync_everything(&settings, &config).await?;
            println!("Synced: {}", full.articles);

            // The feed list, if that second switch is on. A failure here is
            // reported after the articles line above, so a feed problem never
            // hides that the articles synced.
            if let Some(step) = full.feeds {
                let step = step?;
                finish_feed_sync(&step)?;
            }
            Ok(())
        }
    }
}

/// What the feed-list part of a sync found, plus where it happened.
struct FeedStep {
    /// What core and the list file did.
    outcome: FeedSyncOutcome,
    /// The name of the list that was mirrored (for messages).
    list_name: String,
    /// That list's file (for commenting a removed feed out).
    path: PathBuf,
}

/// Everything one sync produced: the articles line, and (only when the feed
/// switch is on) the feed-list result, kept as a `Result` so a feed problem
/// does not throw away a good article sync.
struct FullSync {
    /// Core's one-line report for saved articles and notes.
    articles: String,
    /// `None` when `feeds` is off; otherwise the feed-list result.
    feeds: Option<anyhow::Result<FeedStep>>,
}

/// Opens the database on its own connection and runs one sync on it: the
/// articles and notes always, the feed list too if `feeds = true`.
///
/// A separate connection (rather than the reader's) because core's sync wants
/// the database inside a `Mutex` so it can let go of it while waiting on the
/// network. SQLite lets two connections in one program share a file; they
/// never run at the same moment because the reader waits for the sync.
async fn sync_everything(
    settings: &SyncSettings,
    config: &tuxwire_core::sync::SyncConfig,
) -> anyhow::Result<FullSync> {
    // `Mutex::new` moves the storage inside the lock.
    let storage = Mutex::new(paths::open_storage()?);

    // Step 1: saved articles and notes (as before the feed list existed).
    let report = tuxwire_core::sync::sync_now(&storage, config).await?;
    let articles = report.summary();

    // Step 2: the feed list, only if its own switch is on.
    let feeds = if settings.feeds {
        Some(sync_feed_list(settings, config, &storage).await)
    } else {
        None
    };
    Ok(FullSync { articles, feeds })
}

/// Works out which list to mirror and runs the feed-list sync on it.
async fn sync_feed_list(
    settings: &SyncSettings,
    config: &tuxwire_core::sync::SyncConfig,
    storage: &Mutex<tuxwire_core::storage::Storage>,
) -> anyhow::Result<FeedStep> {
    // Find the lists the same way the reader does, then pick the one named in
    // sync.toml (or the first). Owned copies of the name and path are made so
    // nothing borrowed has to live across the `.await` below.
    let default_sources = crate::fetchers::config::sources_path()?;
    let lists = crate::feedlists::config::load(&default_sources)?;
    let chosen = feed_sync::choose_list(&lists, settings.feed_list.as_deref())?;
    let (list_name, path) = (chosen.name.clone(), chosen.path.clone());

    let outcome = feed_sync::run_feed_sync(storage, config, &path).await?;
    Ok(FeedStep {
        outcome,
        list_name,
        path,
    })
}

/// Reports a finished feed-list sync and deals with feeds another device
/// removed, for the `tuxwire sync` command.
///
/// On a terminal each removal is asked about, one at a time. When the command
/// is not run from a terminal (a pipe, a timer) nobody can answer, so the
/// removals are listed and nothing is removed or dismissed: they are asked
/// about again next time.
fn finish_feed_sync(step: &FeedStep) -> anyhow::Result<()> {
    let outcome = &step.outcome;
    println!("Feeds ({}): {}", step.list_name, outcome.report);
    if outcome.added > 0 {
        println!(
            "  {} new feed(s) from other devices added to {}; their articles load the next time you open tuxwire.",
            outcome.added,
            step.path.display()
        );
    }
    if outcome.removals.is_empty() {
        return Ok(());
    }

    // Not a terminal: list and stop.
    if !std::io::stdin().is_terminal() {
        println!(
            "  {} feed(s) were removed on another device. Nothing was removed here. Run `tuxwire sync` in a terminal to be asked about each one:",
            outcome.removals.len()
        );
        for entry in &outcome.removals {
            println!("    - {} ({})", entry.name, entry.url);
        }
        return Ok(());
    }

    // A terminal: ask. Only a "y" or "yes" removes anything.
    let storage = paths::open_storage()?;
    let (removed, kept) =
        feed_sync::resolve_removals(&storage, &step.path, &outcome.removals, |entry| {
            ask_to_remove(entry, &step.list_name)
        })?;
    println!(
        "  {removed} feed(s) taken out of {} (commented out, not deleted), {kept} kept.",
        step.path.display()
    );
    Ok(())
}

/// Asks the person, on the keyboard, whether to take one feed out of the list.
/// Anything but `y` or `yes` (including just pressing Enter, or a closed
/// input) means no.
fn ask_to_remove(entry: &FeedEntry, list_name: &str) -> bool {
    use std::io::Write;
    // `print!` has no newline, so `flush` pushes the question to the screen
    // before we wait for the answer.
    print!(
        "  Another device removed \"{}\" ({}). Take it out of your list {:?}? It stays in the file as comments. [y/N] ",
        entry.name, entry.url, list_name
    );
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    // `read_line` returns an error or 0 bytes at end-of-input; both leave
    // `answer` empty, which `is_yes` treats as no.
    let _ = std::io::stdin().read_line(&mut answer);
    is_yes(&answer)
}

/// Whether a typed answer means yes: `y` or `yes`, in any capitals, with
/// spaces around it allowed. Everything else, including nothing, is no.
fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "y" | "yes")
}

/// What the reader's `c` key calls: run one sync and return a one-line
/// message for the footer. Never panics and never returns the password;
/// problems come back as `Err` text the caller shows.
///
/// The reader's event loop is ordinary (non-`async`) code running inside
/// tokio's runtime, so, like the refresh key, this uses
/// `block_in_place` + `block_on` to wait for the async sync: it tells tokio
/// "this thread is about to block" and then drives the future to the end.
///
/// With the feed switch on, the message also says how many feeds were added
/// and how many removals are waiting. The reader never asks about removals
/// itself (that needs the keyboard in a plain terminal): it says to run
/// `tuxwire sync`.
pub fn sync_for_reader() -> anyhow::Result<String> {
    let settings = load_settings()?;
    if !settings.enabled {
        bail!("sync is switched off (enabled = false in sync.toml)");
    }
    let config = settings.to_core_config()?;
    let full = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(sync_everything(&settings, &config))
    })?;

    // Build the footer line: the articles part, then the feed part if any.
    let mut message = full.articles;
    if let Some(step) = full.feeds {
        match step {
            Ok(step) => message.push_str(&reader_feed_note(&step.outcome)),
            // `{err:#}` prints the error and its causes on one line.
            Err(err) => message.push_str(&format!("; feeds: {err:#}")),
        }
    }
    Ok(message)
}

/// The feed part of the reader's footer message: what was added and what is
/// waiting. Empty when there is nothing to say.
fn reader_feed_note(outcome: &FeedSyncOutcome) -> String {
    let mut note = String::new();
    if outcome.added > 0 {
        note.push_str(&format!(
            "; {} new feed(s) added - restart to load them",
            outcome.added
        ));
    }
    if !outcome.removals.is_empty() {
        note.push_str(&format!(
            "; {} feed removal(s) waiting - run tuxwire sync",
            outcome.removals.len()
        ));
    }
    note
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

    /// Only "y" and "yes" mean yes; nothing at all means no.
    #[test]
    fn only_y_or_yes_removes_a_feed() {
        assert!(is_yes("y\n"));
        assert!(is_yes("  YES \n"));
        assert!(!is_yes("\n"));
        assert!(!is_yes(""));
        assert!(!is_yes("n"));
        assert!(!is_yes("yep"));
    }

    /// The reader's note mentions additions and waiting removals, and is
    /// empty when there is nothing to say.
    #[test]
    fn the_reader_note_says_what_happened() {
        let mut outcome = FeedSyncOutcome::default();
        assert_eq!(reader_feed_note(&outcome), "");
        outcome.added = 2;
        outcome.removals.push(FeedEntry {
            name: "A".into(),
            url: "https://a.example/f".into(),
            topic: "t".into(),
        });
        let note = reader_feed_note(&outcome);
        assert!(note.contains("2 new feed(s) added - restart"), "{note}");
        assert!(note.contains("1 feed removal(s) waiting - run tuxwire sync"));
    }
}
