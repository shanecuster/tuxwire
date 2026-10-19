//! Optional sync of **read marks** between a person's devices: which feed
//! articles have been opened.
//!
//! **Off by default, with its own switch** ([`super::SyncConfig::read_marks`]),
//! separate from the main sync switch. It only ever runs when both are on. It
//! is separate because it is a different kind of data from saved articles or
//! the Read Later list: those are things the person chose to keep, while read
//! marks are a record of what they opened. Addresses of opened articles are
//! sent to the person's own CouchDB and nowhere else.
//!
//! What moves: the address of an article that was opened, and when. Nothing
//! else about the article (not its title, its feed or its text). Marks only go
//! one way: an article can be opened, never "un-opened", so there is nothing to
//! settle when two devices disagree. If a document exists, the article was
//! opened.
//!
//! **Only recent reads are sent**: those from the last [`MAX_AGE_DAYS`] days.
//! That keeps the first sync small for someone with a long reading history, and
//! older articles have usually dropped out of their feeds anyway. Nothing is
//! ever deleted from the server, and every article opened after the switch is
//! turned on is sent.
//!
//! A mark that arrives for an article this device has not downloaded yet is
//! remembered, and applied the moment that article is fetched.
//!
//! One document per article, named `read:<the address exactly as stored>`, in
//! the same CouchDB database as the other kinds. Builds that do not know
//! `read:` ignore it. Like the other kinds it keeps its **own pull checkpoint**.

#![warn(clippy::missing_docs_in_private_items)]

use super::client::CouchClient;
use super::feeds::is_syncable_url;
use super::{MAX_ROUNDS, PAGE_SIZE, SyncReport, lock};
use crate::storage::Storage;
use anyhow::{Context, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// Every read-mark document's `_id` starts with this, followed by the article's
/// address exactly as stored (trimmed).
pub const READ_DOC_PREFIX: &str = "read:";

/// The document format version this build writes and understands. A document
/// with a *higher* version came from a newer tuxwire and is skipped (and
/// reported) instead of being misread.
pub const READ_DOC_VERSION: u32 = 1;

/// How many days back a read is still sent. Reads older than this are left out
/// (they stay read on the device they happened on).
pub const MAX_AGE_DAYS: i64 = 90;

/// The most read-mark documents one pull will read. A guard against a database
/// gone wrong; years of daily reading stay far below it.
pub const MAX_READ_DOCS: usize = 200_000;

/// The identity of a read mark: the article's address, trimmed. The text is not
/// changed otherwise, because articles are identified by their exact address.
pub fn read_key(url: &str) -> String {
    url.trim().to_string()
}

/// The document name for the article at `url`: the prefix plus [`read_key`].
pub fn read_doc_id(url: &str) -> String {
    format!("{READ_DOC_PREFIX}{}", read_key(url))
}

/// The oldest read time still sent, in milliseconds since 1970, for a sync
/// running at `now_ms`: [`MAX_AGE_DAYS`] days earlier. `saturating_sub` stops
/// at the smallest number instead of wrapping round if the clock is absurd.
pub fn oldest_sendable(now_ms: i64) -> i64 {
    now_ms.saturating_sub(MAX_AGE_DAYS * 24 * 60 * 60 * 1000)
}

/// One read mark as it is stored in CouchDB.
///
/// `#[derive(Serialize, Deserialize)]` makes `serde` write this struct as JSON
/// and read it back; the `rename` lines map our names onto CouchDB's reserved
/// `_id` and `_rev`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadMarkDoc {
    /// CouchDB's document name: `read:` plus the address.
    #[serde(rename = "_id")]
    pub id: String,

    /// CouchDB's revision label, or `None` for a document never sent before.
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,

    /// Format version (see [`READ_DOC_VERSION`]). A missing one reads as 0,
    /// which this build treats as unsupported.
    #[serde(default)]
    pub v: u32,

    /// The article's address, as stored.
    pub url: String,

    /// When it was opened, in milliseconds since 1970 (UTC).
    pub read_at: i64,
}

impl ReadMarkDoc {
    /// Builds the document for the article at `url`, opened at `read_at`.
    pub fn new(url: &str, read_at: i64) -> Self {
        ReadMarkDoc {
            id: read_doc_id(url),
            rev: None,
            v: READ_DOC_VERSION,
            url: read_key(url),
            read_at,
        }
    }

    /// Whether this build understands the document's format version.
    pub fn is_supported(&self) -> bool {
        (1..=READ_DOC_VERSION).contains(&self.v)
    }

    /// Checks a document that arrived from the server and returns it tidied, or
    /// `None` if it must not be used. **Treat everything in it as untrusted**:
    /// only plain, sensible values pass.
    ///
    /// - the name must match the address (`read:` plus the address);
    /// - the address must be an ordinary web address ([`is_syncable_url`]);
    /// - the time must be positive.
    ///
    /// `self` is taken by value (the caller gives it up), so the tidied copy
    /// can be returned without cloning.
    pub fn cleaned(mut self) -> Option<Self> {
        self.url = read_key(&self.url);
        let acceptable =
            is_syncable_url(&self.url) && self.id == read_doc_id(&self.url) && self.read_at > 0;
        acceptable.then_some(self)
    }
}

/// Runs the read-marks part of a sync: pull what other devices opened, then send
/// what was opened here, going round again while writes conflict.
///
/// Called by [`super::sync_now`] only when both the main switch and the
/// read-marks switch are on (it checks nothing itself). Counts go into
/// `report`. A failed run changes nothing that matters and is simply tried
/// again next time.
pub(crate) async fn sync_read_marks(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    // Step 1: pull first, so a mark another device already sent is matched
    // instead of being sent again.
    pull_marks(storage, client, report).await?;

    // Step 2: push, and go round again (pull, then push) while writes conflict.
    // Two devices opening the same article at once is the usual cause; pulling
    // records the other device's mark, and then there is nothing left to send.
    for _ in 1..=MAX_ROUNDS {
        let conflicts = push_marks(storage, client, report).await?;
        report.marks_conflicts_left = conflicts;
        if conflicts == 0 {
            break;
        }
        pull_marks(storage, client, report).await?;
    }
    Ok(())
}

/// Reads every page of changes since the read-marks checkpoint and stores the
/// `read:` documents among them, moving the checkpoint with each page.
async fn pull_marks(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    // How many read-mark documents have been read in this pull, against the limit.
    let mut mark_rows = 0usize;
    loop {
        // Step 1: where did we get to last time? (A quick, non-waiting step.)
        let since = lock(storage)?.read_marks_sync_since()?;

        // Step 2: ask the server (the database is unlocked while we wait).
        let page = client.changes(since.as_deref(), PAGE_SIZE).await?;
        let row_count = page.rows.len();

        // Step 3: keep the read-mark documents; everything else is not ours here.
        let mut docs: Vec<ReadMarkDoc> = Vec::new();
        let storage_guard = lock(storage)?;
        for row in page.rows {
            // `strip_prefix` gives the part after `read:`, or `None` for any
            // other kind of document (which the `else` skips).
            let Some(key) = row.id.strip_prefix(READ_DOC_PREFIX) else {
                continue;
            };
            mark_rows += 1;
            if mark_rows > MAX_READ_DOCS {
                bail!("the server holds more read marks than tuxwire will read");
            }
            // Deleted on the server by hand: forget our record of it, so an
            // article opened here is sent again next push.
            if row.deleted {
                storage_guard.forget_read_mark_doc(key)?;
                continue;
            }
            let Some(value) = row.doc else { continue };
            match serde_json::from_value::<ReadMarkDoc>(value) {
                Ok(doc) if !doc.is_supported() => report.skipped_newer_version += 1,
                Ok(doc) => match doc.cleaned() {
                    Some(doc) => docs.push(doc),
                    None => report.skipped_unreadable += 1,
                },
                Err(_) => report.skipped_unreadable += 1,
            }
        }

        // Step 4: store them and move the checkpoint, in one transaction.
        let counts = storage_guard.apply_remote_read_marks(docs, &page.last_seq)?;
        drop(storage_guard);
        report.marks_received += counts.received;
        report.marks_applied += counts.marked;

        // Step 5: a short page means we have reached the end.
        if row_count < PAGE_SIZE {
            return Ok(());
        }
    }
}

/// Sends the read marks of articles opened here in the last [`MAX_AGE_DAYS`]
/// days that the server does not have yet. Returns how many writes CouchDB
/// refused as conflicts (they are retried after the next pull).
async fn push_marks(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut SyncReport,
) -> anyhow::Result<usize> {
    // Step 1: what needs sending? Only recent reads, and only ordinary web
    // addresses (anything else would be refused by every other device).
    // `filter` keeps the matching entries; `collect` gathers them in a `Vec`.
    let cutoff = oldest_sendable(Utc::now().timestamp_millis());
    let pending: Vec<_> = lock(storage)?
        .pending_read_marks(cutoff)?
        .into_iter()
        .filter(|mark| is_syncable_url(&mark.url))
        .collect();
    let mut conflicts = 0;

    // Step 2: send in batches of at most PAGE_SIZE.
    for batch in pending.chunks(PAGE_SIZE) {
        // Build the documents, remembering each one's address and time so we
        // can record exactly what was sent.
        let mut docs = Vec::with_capacity(batch.len());
        let mut sent: HashMap<String, (String, i64)> = HashMap::new();
        for mark in batch {
            let doc = ReadMarkDoc::new(&mark.url, mark.read_at);
            sent.insert(doc.id.clone(), (mark.url.clone(), mark.read_at));
            docs.push(doc);
        }

        // Step 3: send (database unlocked while waiting).
        let results = client.write_batch(&docs).await?;

        // Step 4: record what worked; count what conflicted.
        let storage_guard = lock(storage)?;
        for result in results {
            match (result.rev, result.error) {
                (Some(rev), None) => {
                    if let Some((url, read_at)) = sent.get(&result.id) {
                        storage_guard
                            .record_read_mark_pushed(url, *read_at, &rev)
                            .with_context(|| "could not record a sent read mark")?;
                        report.marks_sent += 1;
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
    fn doc() -> ReadMarkDoc {
        ReadMarkDoc {
            id: "read:https://example.com/Post".to_string(),
            rev: Some("1-a".to_string()),
            v: 1,
            url: "https://example.com/Post".to_string(),
            read_at: 200,
        }
    }

    /// The document is named after the exact address, capitals included.
    #[test]
    fn a_mark_is_named_after_the_exact_address() {
        assert_eq!(
            read_doc_id("  https://Example.com/Post "),
            "read:https://Example.com/Post"
        );
        assert_eq!(
            ReadMarkDoc::new(" https://x.example/a ", 5).id,
            "read:https://x.example/a"
        );
    }

    /// The cut-off is exactly 90 days before `now`.
    #[test]
    fn the_cut_off_is_ninety_days_back() {
        let day = 24 * 60 * 60 * 1000;
        assert_eq!(oldest_sendable(100 * day), 10 * day);
        assert_eq!(oldest_sendable(i64::MIN), i64::MIN, "never wraps round");
    }

    /// A good document passes and survives a round trip as JSON.
    #[test]
    fn a_good_document_is_accepted_and_round_trips() {
        let cleaned = doc().cleaned().expect("accepted");
        let sent = ReadMarkDoc {
            rev: None,
            ..cleaned
        };
        let json = serde_json::to_string(&sent).unwrap();
        assert!(!json.contains("_rev"));
        assert!(json.contains("\"_id\""));
        assert_eq!(serde_json::from_str::<ReadMarkDoc>(&json).unwrap(), sent);
    }

    /// Untrusted values are refused: a wrong name, an address that is not a web
    /// address, and a missing time.
    #[test]
    fn unsafe_or_odd_documents_are_refused() {
        let mut d = doc();
        d.id = "read:https://other.example/".to_string();
        assert!(d.cleaned().is_none(), "name must match the address");

        for bad in ["javascript:alert(1)", "file:///home/me"] {
            let mut d = doc();
            d.url = bad.to_string();
            d.id = read_doc_id(bad);
            assert!(d.cleaned().is_none(), "{bad}");
        }

        let mut d = doc();
        d.read_at = 0;
        assert!(d.cleaned().is_none());
    }

    /// A missing version is unsupported, and unknown extra fields are ignored.
    #[test]
    fn missing_fields_default_sensibly() {
        let json = r#"{"_id":"read:https://x.example/p","url":"https://x.example/p",
            "read_at":6,"colour":"red"}"#;
        let d: ReadMarkDoc = serde_json::from_str(json).unwrap();
        assert!(!d.is_supported(), "no version means unsupported");
        assert!(serde_json::from_str::<ReadMarkDoc>(r#"{"_id":"read:x"}"#).is_err());
    }
}
