//! Optional sync of the **feed list** between a person's devices.
//!
//! **Off by default, and a separate switch from saved-article sync.** A feed
//! list shows what someone reads, so it is its own opt-in: [`sync_feeds`]
//! refuses to do anything unless both [`SyncConfig::enabled`] and
//! [`SyncConfig::feeds`] are `true`.
//!
//! What moves: the feeds themselves (address, name, topic). Adding a feed on one
//! device makes it appear on the others. **Removing** a feed is also passed on,
//! but the other device only *lists* it and asks the person; nothing is ever
//! removed by sync itself. Not synced: renaming a feed, changing its topic,
//! Reddit sources (core cannot fetch them yet), or anything about articles.
//!
//! One document per feed, named `feed:<address in lower case>`, lives in the same
//! CouchDB database as the saved-article documents. Older builds ignore
//! documents whose name does not start with `article:`, so they are unaffected.
//!
//! Each app keeps its feed list in its own way (a TOML file, a database
//! table), so this module never touches that list. The app hands the current
//! list to [`sync_feeds`], and afterwards asks the database what to add
//! (`Storage::pending_feed_additions`) and what to ask the person about
//! (`Storage::pending_feed_removals`). See `docs/ARCHITECTURE.md`.

#![warn(clippy::missing_docs_in_private_items)]

use super::client::CouchClient;
use super::{MAX_ROUNDS, PAGE_SIZE, SyncConfig, lock};
use crate::storage::{FeedEntry, PendingFeedPush, Storage};
use anyhow::{Context, bail};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// Every feed document's `_id` starts with this, followed by the feed's
/// address in lower case.
pub const FEED_DOC_PREFIX: &str = "feed:";

/// The feed document format version this build writes and understands. A
/// document with a *higher* version came from a newer tuxwire and is skipped
/// (and reported) instead of being misread.
pub const FEED_DOC_VERSION: u32 = 1;

/// The most feed documents one sync will read. It guards against a database
/// gone wrong, and is far more feeds than anyone follows.
pub const MAX_FEED_DOCS: usize = 5000;

/// The longest feed address accepted, in characters.
const MAX_URL_CHARS: usize = 2048;

/// The longest feed name accepted, in characters.
const MAX_NAME_CHARS: usize = 200;

/// The longest topic accepted, in characters.
const MAX_TOPIC_CHARS: usize = 100;

/// The one kind of feed that is synced. (Reddit sources are not fetched by
/// core yet, so they are left out.)
const RSS: &str = "rss";

/// The identity of a feed: its address, trimmed and in lower case, so the same
/// feed written with different capitals on two devices is one feed.
pub fn feed_key(url: &str) -> String {
    url.trim().to_lowercase()
}

/// The document name for the feed at `url`: the prefix plus [`feed_key`].
pub fn feed_doc_id(url: &str) -> String {
    format!("{FEED_DOC_PREFIX}{}", feed_key(url))
}

/// Whether `url` is an address a feed may have: an ordinary `http` or `https`
/// web address, not too long, with no control characters in it. Anything else
/// (for example `file://` or `javascript:`) is never synced or applied.
pub fn is_syncable_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || url.chars().count() > MAX_URL_CHARS || url.chars().any(char::is_control) {
        return false;
    }
    // `Url::parse` fails for anything that is not a well-formed address.
    match Url::parse(url) {
        Ok(parsed) => matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some(),
        Err(_) => false,
    }
}

/// `serde` default for [`FeedDoc::source_type`]: older documents without the
/// field are RSS feeds.
fn default_source_type() -> String {
    RSS.to_string()
}

/// One feed as it is stored in CouchDB.
///
/// `#[derive(Serialize, Deserialize)]` makes `serde` write this struct as JSON
/// and read it back; the `rename` lines map our names onto CouchDB's reserved
/// `_id` and `_rev`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeedDoc {
    /// CouchDB's document name: `feed:` plus the address in lower case.
    #[serde(rename = "_id")]
    pub id: String,

    /// CouchDB's revision label, or `None` for a document never sent before.
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,

    /// Format version (see [`FEED_DOC_VERSION`]). A missing one reads as 0,
    /// which this build treats as unsupported.
    #[serde(default)]
    pub v: u32,

    /// The feed's address, as written.
    pub url: String,

    /// The feed's name, shown on the device that receives it.
    pub name: String,

    /// The topic it is filed under.
    pub topic: String,

    /// The kind of feed; only `"rss"` is accepted.
    #[serde(default = "default_source_type")]
    pub source_type: String,

    /// `true` while the feed exists, `false` once it has been removed. A removal
    /// is a document, not a deletion, so the other devices can tell "removed
    /// later" from "never heard of it".
    pub present: bool,

    /// When this changed, in milliseconds since 1970 (UTC). The newer copy wins.
    pub updated_at: i64,
}

impl FeedDoc {
    /// Builds the document that describes a feed record waiting to be sent.
    pub fn from_pending(pending: &PendingFeedPush) -> Self {
        FeedDoc {
            id: feed_doc_id(&pending.url),
            rev: pending.rev.clone(),
            v: FEED_DOC_VERSION,
            url: pending.url.clone(),
            name: pending.name.clone(),
            topic: pending.topic.clone(),
            source_type: RSS.to_string(),
            present: pending.present,
            updated_at: pending.updated_at,
        }
    }

    /// Whether this build understands the document's format version.
    pub fn is_supported(&self) -> bool {
        (1..=FEED_DOC_VERSION).contains(&self.v)
    }

    /// Checks a document that arrived from the server and returns it tidied, or
    /// `None` if it must not be used. **Treat everything in it as untrusted**:
    /// the server is the person's own, but a feed list ends up in a config file
    /// and a terminal, so only plain, sensible values pass.
    ///
    /// - the name must match the address (`feed:` plus the lower-case address);
    /// - the kind must be `rss`;
    /// - the address must be an ordinary web address ([`is_syncable_url`]);
    /// - the name and topic must be non-empty after trimming, within their
    ///   length limits, and free of control characters (which could carry
    ///   terminal escape sequences);
    /// - the change time must be positive.
    ///
    /// `self` is taken by value (the caller gives it up), so the tidied copy
    /// can be returned without cloning.
    pub fn cleaned(mut self) -> Option<Self> {
        self.url = self.url.trim().to_string();
        self.name = self.name.trim().to_string();
        self.topic = self.topic.trim().to_string();

        let text_ok = |text: &str, limit: usize| {
            !text.is_empty() && text.chars().count() <= limit && !text.chars().any(char::is_control)
        };
        let acceptable = self.source_type == RSS
            && is_syncable_url(&self.url)
            && self.id == feed_doc_id(&self.url)
            && text_ok(&self.name, MAX_NAME_CHARS)
            && text_ok(&self.topic, MAX_TOPIC_CHARS)
            && self.updated_at > 0;
        acceptable.then_some(self)
    }
}

/// What one feed sync did, for the app to show.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedSyncReport {
    /// Feeds on this device that sync had not seen before.
    pub found_here: usize,
    /// Feeds taken out of this device's list since last time.
    pub removed_here: usize,
    /// Feed records sent to the server.
    pub pushed: usize,
    /// Documents skipped because they came from a newer version of tuxwire.
    pub skipped_newer_version: usize,
    /// Documents skipped because they were not usable feeds.
    pub skipped_unusable: usize,
    /// Writes still in conflict after the last round; sent again next time.
    pub conflicts_left: usize,
    /// `true` when the list handed in was empty while feeds were expected, so
    /// no removals were recorded (see `LocalFeedChanges::removals_skipped`).
    pub removals_skipped: bool,
}

impl FeedSyncReport {
    /// A one-line summary for a footer or status screen. It does not say what
    /// is waiting to be added or asked about: the app reads that from the
    /// database and words it for its own screen.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!("{} feed changes sent", self.pushed)];
        if self.skipped_newer_version > 0 {
            parts.push(format!(
                "{} skipped (update tuxwire)",
                self.skipped_newer_version
            ));
        }
        if self.skipped_unusable > 0 {
            parts.push(format!("{} unusable", self.skipped_unusable));
        }
        if self.conflicts_left > 0 {
            parts.push(format!("{} to retry next time", self.conflicts_left));
        }
        if self.removals_skipped {
            parts.push("feed list looked empty, so removals were not sent".to_string());
        }
        parts.join(", ")
    }
}

/// Runs one feed sync. **Refuses unless both sync and feed sync are turned on.**
///
/// `local` is this device's whole current feed list (every feed it follows).
/// The steps:
/// 1. **Pull** what other devices changed.
/// 2. **Record** this device's own list, noting feeds added or taken out here.
/// 3. **Push** the changes to the server, going round again if another device
///    wrote in between.
///
/// Afterwards the app asks the database for `pending_feed_additions` (add them
/// to its list) and `pending_feed_removals` (ask the person). A failed sync
/// changes nothing that matters and can simply be tried again.
pub async fn sync_feeds(
    storage: &Mutex<Storage>,
    config: &SyncConfig,
    local: Vec<FeedEntry>,
) -> anyhow::Result<FeedSyncReport> {
    // Step 1: never do anything unless the person turned both switches on.
    if !config.enabled {
        bail!("sync is turned off");
    }
    if !config.feeds {
        bail!("feed sync is turned off");
    }
    let base = config.validate()?;
    let client = CouchClient::new(config, base)?;
    let mut report = FeedSyncReport::default();

    // Step 2: pull first, so a feed both devices already have is simply
    // matched instead of being sent as new.
    pull_feeds(storage, &client, &mut report).await?;

    // Step 3: note what this device's list contains.
    let changes = lock(storage)?.record_local_feeds(&local)?;
    report.found_here = changes.added;
    report.removed_here = changes.removed;
    report.removals_skipped = changes.removals_skipped;

    // Step 4: push, and go round again (pull, then push) while writes conflict.
    for _ in 1..=MAX_ROUNDS {
        let conflicts = push_feeds(storage, &client, &mut report).await?;
        report.conflicts_left = conflicts;
        if conflicts == 0 {
            break;
        }
        pull_feeds(storage, &client, &mut report).await?;
    }
    Ok(report)
}

/// Reads every page of changes since the feed checkpoint and stores the feed
/// documents among them, moving the checkpoint with each page.
async fn pull_feeds(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut FeedSyncReport,
) -> anyhow::Result<()> {
    // How many feed documents have been read in this pull, against the limit.
    let mut feed_rows = 0usize;
    loop {
        // Step 1: where did we get to last time? (A quick, non-waiting step.)
        let since = lock(storage)?.feed_sync_since()?;

        // Step 2: ask the server (the database is unlocked while we wait).
        let page = client.changes(since.as_deref(), PAGE_SIZE).await?;
        let row_count = page.rows.len();

        // Step 3: keep the feed documents; everything else is not ours here.
        let mut docs: Vec<FeedDoc> = Vec::new();
        let storage_guard = lock(storage)?;
        for row in page.rows {
            let Some(key) = row.id.strip_prefix(FEED_DOC_PREFIX) else {
                continue;
            };
            feed_rows += 1;
            if feed_rows > MAX_FEED_DOCS {
                bail!("the server holds more feed records than tuxwire will read");
            }
            // Deleted on the server by hand: forget our record of the
            // revision, so the feed is written again next push.
            if row.deleted {
                storage_guard.forget_feed_doc(key)?;
                continue;
            }
            let Some(value) = row.doc else { continue };
            match serde_json::from_value::<FeedDoc>(value) {
                Ok(doc) if !doc.is_supported() => report.skipped_newer_version += 1,
                Ok(doc) => match doc.cleaned() {
                    Some(doc) => docs.push(doc),
                    None => report.skipped_unusable += 1,
                },
                Err(_) => report.skipped_unusable += 1,
            }
        }

        // Step 4: store them and move the checkpoint, in one transaction.
        storage_guard.apply_remote_feed_batch(docs, &page.last_seq)?;
        drop(storage_guard);

        // Step 5: a short page means we have reached the end.
        if row_count < PAGE_SIZE {
            return Ok(());
        }
    }
}

/// Sends every feed record that changed here. Returns how many writes CouchDB
/// refused as conflicts (they are retried after the next pull).
async fn push_feeds(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut FeedSyncReport,
) -> anyhow::Result<usize> {
    // Step 1: what needs sending? (Quick, non-waiting.)
    let pending = lock(storage)?.pending_feed_pushes()?;
    let mut conflicts = 0;

    // Step 2: send in batches of at most PAGE_SIZE.
    for batch in pending.chunks(PAGE_SIZE) {
        // Build the documents, remembering each one's change time so we can
        // record exactly what was sent, and its key to find the record again.
        let mut docs = Vec::with_capacity(batch.len());
        let mut sent: HashMap<String, (String, i64)> = HashMap::new();
        for item in batch {
            let doc = FeedDoc::from_pending(item);
            sent.insert(doc.id.clone(), (item.key.clone(), item.updated_at));
            docs.push(doc);
        }

        // Step 3: send (database unlocked while waiting).
        let results = client.write_batch(&docs).await?;

        // Step 4: record what worked; count what conflicted.
        let storage_guard = lock(storage)?;
        for result in results {
            match (result.rev, result.error) {
                (Some(rev), None) => {
                    if let Some((key, stamp)) = sent.get(&result.id) {
                        storage_guard
                            .record_feed_pushed(key, &rev, *stamp)
                            .with_context(|| "could not record a sent feed")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A document as it would arrive from the server.
    fn doc() -> FeedDoc {
        FeedDoc {
            id: "feed:https://example.com/feed".to_string(),
            rev: Some("1-a".to_string()),
            v: 1,
            url: "https://example.com/feed".to_string(),
            name: "Example".to_string(),
            topic: "tech".to_string(),
            source_type: "rss".to_string(),
            present: true,
            updated_at: 100,
        }
    }

    /// The identity is the address in lower case, with the prefix on the name.
    #[test]
    fn a_feed_is_named_after_its_lower_case_address() {
        assert_eq!(
            feed_key("  HTTPS://Example.com/Feed "),
            "https://example.com/feed"
        );
        assert_eq!(
            feed_doc_id("https://Example.com/F"),
            "feed:https://example.com/f"
        );
    }

    /// Only ordinary web addresses are accepted.
    #[test]
    fn only_web_addresses_are_syncable() {
        assert!(is_syncable_url("https://example.com/feed.xml"));
        assert!(is_syncable_url("http://example.com/feed"));
        assert!(!is_syncable_url("file:///etc/passwd"));
        assert!(!is_syncable_url("javascript:alert(1)"));
        assert!(!is_syncable_url("ftp://example.com/feed"));
        assert!(!is_syncable_url("not a url"));
        assert!(!is_syncable_url(""));
        assert!(!is_syncable_url("https://example.com/\u{1b}[31m"));
        assert!(!is_syncable_url(&format!(
            "https://example.com/{}",
            "a".repeat(3000)
        )));
    }

    /// A good document passes, is tidied, and survives a round trip as JSON.
    #[test]
    fn a_good_document_is_accepted_and_round_trips() {
        let mut d = doc();
        d.name = "  Example  ".to_string();
        let cleaned = d.cleaned().expect("accepted");
        assert_eq!(cleaned.name, "Example");

        let json = serde_json::to_string(&FeedDoc {
            rev: None,
            ..cleaned.clone()
        })
        .unwrap();
        assert!(!json.contains("_rev"));
        assert!(json.contains("\"_id\""));
        let back: FeedDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back,
            FeedDoc {
                rev: None,
                ..cleaned
            }
        );
    }

    /// Untrusted values are refused: a wrong name, kind or address, empty or
    /// over-long text, control characters, and a missing change time.
    #[test]
    fn unsafe_or_odd_documents_are_refused() {
        let mut d = doc();
        d.id = "feed:https://other.example/feed".to_string();
        assert!(d.cleaned().is_none(), "name must match the address");

        let mut d = doc();
        d.source_type = "reddit".to_string();
        assert!(d.cleaned().is_none());

        let mut d = doc();
        d.url = "file:///home/me".to_string();
        d.id = feed_doc_id(&d.url);
        assert!(d.cleaned().is_none());

        let mut d = doc();
        d.name = "   ".to_string();
        assert!(d.cleaned().is_none());

        let mut d = doc();
        d.name = "evil\u{1b}[2Jname".to_string();
        assert!(d.cleaned().is_none(), "control characters");

        let mut d = doc();
        d.topic = "x".repeat(MAX_TOPIC_CHARS + 1);
        assert!(d.cleaned().is_none());

        let mut d = doc();
        d.updated_at = 0;
        assert!(d.cleaned().is_none());
    }

    /// A document without a kind reads as RSS; a missing version is unsupported;
    /// unknown extra fields are ignored.
    #[test]
    fn missing_fields_default_sensibly() {
        let json = r#"{"_id":"feed:https://x.example/f","url":"https://x.example/f",
            "name":"X","topic":"t","present":true,"updated_at":5,"colour":"red"}"#;
        let d: FeedDoc = serde_json::from_str(json).unwrap();
        assert_eq!(d.source_type, "rss");
        assert!(!d.is_supported(), "no version means unsupported");
        assert!(serde_json::from_str::<FeedDoc>(r#"{"_id":"feed:x"}"#).is_err());
    }
}
