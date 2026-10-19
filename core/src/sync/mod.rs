//! Optional sync of saved articles and notes to the person's own CouchDB.
//!
//! **Off by default.** Nothing here runs, and nothing is sent anywhere, unless
//! the person has turned sync on and asked for it: [`sync_now`] refuses to do
//! anything while [`SyncConfig::enabled`] is `false`.
//!
//! What [`sync_now`] shares: which articles are saved, and their notes, and the
//! Read Later list (links the person chose to put there; see [`later`]). Nothing
//! else: not what you have read, not your settings. The feed list can be shared
//! too, but only through the separate, off-by-default [`feeds`] module (its
//! own switch, [`SyncConfig::feeds`]).
//!
//! How a sync goes (one "cycle"):
//! 1. **Pull**: ask CouchDB for everything that changed since last time, and
//!    keep whichever copy of each article is newer (see [`doc::remote_wins`]).
//! 2. **Push**: send the articles whose saved/note state changed on this device.
//! 3. If CouchDB reported a conflict (another device wrote in between), go
//!    round again: pulling settles it, then the push goes through.
//!
//! This module never reads files or environment variables and never decides
//! where a password lives. The program using it (the terminal app or the Android
//! app) keeps the password somewhere suitable and hands it in through a
//! [`SyncConfig`]. See `docs/ARCHITECTURE.md` ("Optional CouchDB sync").

#![warn(clippy::missing_docs_in_private_items)]

pub(crate) mod client;
pub mod doc;
#[cfg(test)]
mod fake;
#[cfg(test)]
mod feed_tests;
pub mod feeds;
pub mod later;
#[cfg(test)]
mod later_tests;
pub mod read_marks;
#[cfg(test)]
mod tests;

use crate::storage::Storage;
use anyhow::{Context, anyhow, bail};
use client::CouchClient;
use doc::{ArticleDoc, DOC_PREFIX};
use reqwest::Url;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

/// How many changes are asked for per request, and how many documents are
/// written per request.
const PAGE_SIZE: usize = 200;

/// How many pull-then-push rounds one sync may take when other devices keep
/// writing in between. Each round settles the conflicts of the one before.
const MAX_ROUNDS: usize = 3;

/// Everything needed to talk to the person's CouchDB.
///
/// The password is private and is left out of the `Debug` output (below), so
/// printing or logging a `SyncConfig` can never reveal it.
#[derive(Clone)]
pub struct SyncConfig {
    /// Whether sync is turned on. Anything that would send or fetch data
    /// refuses to run while this is `false`.
    pub enabled: bool,
    /// Whether the feed list is synced too. A separate switch, `false` unless
    /// the person turns it on, and it does nothing while [`SyncConfig::enabled`]
    /// is `false`. Set it with [`SyncConfig::with_feeds`].
    pub feeds: bool,
    /// The server address, for example `https://couch.example.ts.net`. No
    /// username or password inside it.
    pub url: String,
    /// The database name, for example `tuxwire`.
    pub database: String,
    /// The login name.
    pub username: String,
    /// The password; see [`SyncConfig::new`].
    password: String,
}

impl SyncConfig {
    /// Builds a config. `password` is moved in (the caller gives it up) and
    /// kept private from then on.
    pub fn new(
        enabled: bool,
        url: String,
        database: String,
        username: String,
        password: String,
    ) -> Self {
        SyncConfig {
            enabled,
            feeds: false,
            url,
            database,
            username,
            password,
        }
    }

    /// Returns this config with feed-list sync switched `on` or off. Feed sync
    /// starts off, so a caller that never calls this never syncs feeds.
    /// `mut self` takes the config by value so it can be changed and handed back.
    pub fn with_feeds(mut self, on: bool) -> Self {
        self.feeds = on;
        self
    }

    /// The password, for the client to build its login header. Crate-private
    /// so no other code can read it back out.
    pub(crate) fn password(&self) -> &str {
        &self.password
    }

    /// Checks the settings are usable and returns the parsed server address.
    /// It does **not** require `enabled`, so "test connection" can be tried
    /// before sync is switched on.
    ///
    /// The rules:
    /// - the address must be `https://`; plain `http://` is allowed only for
    ///   this computer itself (`localhost`, `127.0.0.1`, `::1`), because a
    ///   password sent over plain http to another machine can be read by
    ///   anyone on the way;
    /// - the address must not contain a username or password (they are
    ///   separate fields, so they never end up in a log or a history);
    /// - the database name must be a valid CouchDB name;
    /// - a username and a password are required.
    pub fn validate(&self) -> anyhow::Result<Url> {
        let url = Url::parse(self.url.trim())
            .map_err(|_| anyhow!("the server address is not a valid web address"))?;

        // Step 1: no credentials hiding inside the address.
        if !url.username().is_empty() || url.password().is_some() {
            bail!("put the username and password in their own fields, not in the address");
        }

        // Step 2: https, or plain http only to this computer.
        match url.scheme() {
            "https" => {}
            "http" if is_this_computer(&url) => {}
            "http" => bail!(
                "the server address must start with https:// (plain http would send your password unprotected)"
            ),
            _ => bail!("the server address must start with https://"),
        }

        // Step 3: a CouchDB database name: starts with a lowercase letter,
        // then lowercase letters, digits and `_ $ ( ) + -`.
        let mut chars = self.database.chars();
        let valid_name = chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_$()+-".contains(c));
        if !valid_name {
            bail!(
                "the database name must start with a lowercase letter and use only a-z, 0-9 and _ $ ( ) + -"
            );
        }

        // Step 4: both login fields are needed.
        if self.username.trim().is_empty() {
            bail!("enter the username");
        }
        if self.password.is_empty() {
            bail!("enter the password");
        }
        Ok(url)
    }
}

/// Prints the config for debugging **without the password**.
///
/// Implementing `Debug` by hand (instead of `#[derive(Debug)]`) is what keeps
/// the password out: the derived version would print every field.
impl std::fmt::Debug for SyncConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncConfig")
            .field("enabled", &self.enabled)
            .field("feeds", &self.feeds)
            .field("url", &"<hidden>")
            .field("database", &self.database)
            .field("username", &self.username)
            .field("password", &"<hidden>")
            .finish()
    }
}

/// Whether `url` points at this very computer.
fn is_this_computer(url: &Url) -> bool {
    matches!(
        url.host_str(),
        Some("localhost") | Some("127.0.0.1") | Some("[::1]") | Some("::1")
    )
}

/// What one sync did, for the app to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Saved articles this device did not have, created from the server.
    pub pulled_added: usize,
    /// Articles whose saved/note state was replaced by a newer copy.
    pub pulled_updated: usize,
    /// Articles sent to the server.
    pub pushed: usize,
    /// Documents skipped because they came from a newer version of tuxwire.
    pub skipped_newer_version: usize,
    /// Documents skipped because they were not readable articles.
    pub skipped_unreadable: usize,
    /// Writes that were still in conflict after the last round; they are sent
    /// again on the next sync.
    pub conflicts_left: usize,
    /// Read Later changes sent to the server.
    pub later_sent: usize,
    /// Read Later changes received from the server (links added, changed or
    /// taken off the list here).
    pub later_received: usize,
    /// Read Later writes still in conflict after the last round; they are sent
    /// again on the next sync.
    pub later_conflicts_left: usize,
}

impl SyncReport {
    /// A one-line summary for the footer or a status screen.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "{} sent, {} received",
            self.pushed,
            self.pulled_added + self.pulled_updated
        )];
        // The Read Later counts are mentioned only when there is something to
        // say, so the line is unchanged for someone who does not use the list.
        if self.later_sent > 0 || self.later_received > 0 {
            parts.push(format!(
                "Read Later: {} sent, {} received",
                self.later_sent, self.later_received
            ));
        }
        if self.skipped_newer_version > 0 {
            parts.push(format!(
                "{} skipped (update tuxwire)",
                self.skipped_newer_version
            ));
        }
        if self.skipped_unreadable > 0 {
            parts.push(format!("{} unreadable", self.skipped_unreadable));
        }
        let retries = self.conflicts_left + self.later_conflicts_left;
        if retries > 0 {
            parts.push(format!("{retries} to retry next time"));
        }
        parts.join(", ")
    }
}

/// Checks that the server, database and login work. Reads nothing of the
/// person's data and writes nothing. Works whether or not `enabled` is set, so
/// it can be tried before sync is switched on.
pub async fn test_connection(config: &SyncConfig) -> anyhow::Result<()> {
    let base = config.validate()?;
    CouchClient::new(config, base)?.check().await
}

/// Locks the database for a short step, turning a poisoned lock (a thread that
/// crashed while holding it) into an ordinary error.
///
/// Callers must **not** hold the returned guard across an `.await`: network
/// waits happen with the database unlocked, so the rest of the app is never
/// blocked while sync talks to the server.
fn lock(storage: &Mutex<Storage>) -> anyhow::Result<MutexGuard<'_, Storage>> {
    storage
        .lock()
        .map_err(|_| anyhow!("the database lock was poisoned by an earlier crash"))
}

/// Runs one sync cycle: saved articles and notes, then the Read Later list.
/// **Refuses while sync is turned off.**
///
/// `storage` is the app's database behind a `Mutex`, locked only for short,
/// non-waiting steps. Fails with a plain-sentence error when the server cannot
/// be reached or the login is wrong; a failed sync changes nothing that matters
/// (it only means saved articles and notes are not yet shared) and can simply
/// be tried again.
pub async fn sync_now(storage: &Mutex<Storage>, config: &SyncConfig) -> anyhow::Result<SyncReport> {
    // Step 1: never do anything unless the person turned sync on.
    if !config.enabled {
        bail!("sync is turned off");
    }
    let base = config.validate()?;
    let client = CouchClient::new(config, base)?;
    let mut report = SyncReport::default();

    // Step 2: pull, push, and go round again while writes conflict.
    for _ in 1..=MAX_ROUNDS {
        pull_all(storage, &client, &mut report).await?;
        let conflicts = push_pending(storage, &client, &mut report).await?;
        report.conflicts_left = conflicts;
        if conflicts == 0 {
            break;
        }
    }

    // Step 3: the Read Later list, on the same switch (see `later`).
    later::sync_read_later(storage, &client, &mut report).await?;
    Ok(report)
}

/// Reads every page of changes since the stored checkpoint and stores them,
/// moving the checkpoint with each page.
async fn pull_all(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    loop {
        // Step 1: where did we get to last time? (A quick, non-waiting step.)
        let since = lock(storage)?.sync_since()?;

        // Step 2: ask the server (the database is unlocked while we wait).
        let page = client.changes(since.as_deref(), PAGE_SIZE).await?;
        let row_count = page.rows.len();

        // Step 3: sort the rows into documents to apply, and ones to skip.
        let mut docs: Vec<ArticleDoc> = Vec::new();
        let storage_guard = lock(storage)?;
        for row in page.rows {
            // Not one of ours (CouchDB's own `_design/...` documents, or
            // something another app put in the same database).
            if !row.id.starts_with(DOC_PREFIX) {
                continue;
            }
            // Deleted on the server by hand: forget our record of it, so the
            // article is written again next push.
            if row.deleted {
                storage_guard.forget_sync_doc(&row.id)?;
                continue;
            }
            let Some(value) = row.doc else { continue };
            match serde_json::from_value::<ArticleDoc>(value) {
                Ok(doc) if doc.is_supported() => docs.push(doc),
                Ok(_) => report.skipped_newer_version += 1,
                Err(_) => report.skipped_unreadable += 1,
            }
        }

        // Step 4: store them and move the checkpoint, in one transaction.
        let counts = storage_guard.apply_remote_batch(docs, &page.last_seq)?;
        drop(storage_guard);
        report.pulled_added += counts.added;
        report.pulled_updated += counts.updated;

        // Step 5: a short page means we have reached the end.
        if row_count < PAGE_SIZE {
            return Ok(());
        }
    }
}

/// Sends every article whose saved/note state changed here. Returns how many
/// writes CouchDB refused as conflicts (they are retried after the next pull).
async fn push_pending(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut SyncReport,
) -> anyhow::Result<usize> {
    // Step 1: what needs sending? (Quick, non-waiting.)
    let pending = lock(storage)?.pending_pushes()?;
    let mut conflicts = 0;

    // Step 2: send in batches. `chunks` splits the list into slices of at
    // most PAGE_SIZE items.
    for batch in pending.chunks(PAGE_SIZE) {
        // Build the documents, and remember each one's change stamp so we can
        // record exactly what was sent. `normalized` applies the note lock.
        let mut docs = Vec::with_capacity(batch.len());
        let mut stamps: HashMap<String, i64> = HashMap::new();
        for item in batch {
            let doc = ArticleDoc::from_article(&item.article, item.updated_at, item.rev.clone())
                .normalized();
            stamps.insert(doc.id.clone(), item.updated_at);
            docs.push(doc);
        }

        // Step 3: send (database unlocked while waiting).
        let results = client.write_batch(&docs).await?;

        // Step 4: record what worked; count what conflicted.
        let storage_guard = lock(storage)?;
        for result in results {
            match (result.rev, result.error) {
                (Some(rev), None) => {
                    if let Some(stamp) = stamps.get(&result.id) {
                        storage_guard
                            .record_pushed(&result.id, &rev, *stamp)
                            .with_context(|| "could not record a sent article")?;
                        report.pushed += 1;
                    }
                }
                (_, Some(error)) if error == "conflict" => conflicts += 1,
                (_, Some(error)) => bail!("the server refused a write ({error})"),
                (None, None) => {}
            }
        }
    }
    Ok(conflicts)
}
