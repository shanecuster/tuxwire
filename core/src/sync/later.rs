//! Optional sync of the **Read Later list** between a person's devices.
//!
//! The list rides on the same switch as saved articles: it is something the
//! person builds on purpose (every item is a link they chose to put there), so
//! [`sync_read_later`] runs as part of [`super::sync_now`] and does nothing
//! while sync is off. It sends addresses and titles to the person's own
//! CouchDB and to nowhere else.
//!
//! What moves: which links are on the list, each link's title and the time it
//! was added, and whether it has been opened. Taking a link off the list (discard,
//! or promoting it to a saved article) is passed on too, so the other devices
//! drop it as well. Saving a link permanently is carried by the ordinary
//! saved-article document, not by this one.
//!
//! One document per link, named `later:<the address exactly as written>`, lives
//! in the same CouchDB database as the saved-article (`article:`) and feed
//! (`feed:`) documents. Builds that do not know `later:` ignore it, so a phone
//! and a computer running different versions of tuxwire keep working.
//!
//! Like the feed sync it keeps its **own pull checkpoint**, so a device that is
//! updated later still reads the `later:` documents written before it was.

#![warn(clippy::missing_docs_in_private_items)]

use super::client::CouchClient;
use super::feeds::is_syncable_url;
use super::{MAX_ROUNDS, PAGE_SIZE, SyncReport, lock};
use crate::storage::{PendingLaterPush, Storage};
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// Every Read Later document's `_id` starts with this, followed by the link
/// exactly as written (trimmed).
pub const LATER_DOC_PREFIX: &str = "later:";

/// The document format version this build writes and understands. A document
/// with a *higher* version came from a newer tuxwire and is skipped (and
/// reported) instead of being misread.
pub const LATER_DOC_VERSION: u32 = 1;

/// The most Read Later documents one pull will read. A guard against a
/// database gone wrong; it is far more than anyone keeps on such a list.
pub const MAX_LATER_DOCS: usize = 20_000;

/// The longest title accepted, in characters. Longer titles are cut short when
/// they are stored.
pub const MAX_TITLE_CHARS: usize = 500;

/// The identity of a Read Later link: its address, trimmed. The text is not
/// changed otherwise (see migration 010 for why capitals are kept).
pub fn later_key(url: &str) -> String {
    url.trim().to_string()
}

/// The document name for the link at `url`: the prefix plus [`later_key`].
pub fn later_doc_id(url: &str) -> String {
    format!("{LATER_DOC_PREFIX}{}", later_key(url))
}

/// Tidies a title for storing: control characters (which could carry terminal
/// escape sequences) are dropped, surrounding space is trimmed and the result
/// is cut to [`MAX_TITLE_CHARS`] characters. A title that is empty after that
/// becomes the address itself, so the list never shows a blank line.
///
/// `&str` borrows the text; the returned `String` is a new value the caller
/// owns.
pub fn clean_title(title: &str, url: &str) -> String {
    // Keep only the characters that are not control characters. `filter`
    // drops the others; `collect` gathers what is left into a new `String`.
    let cleaned: String = title.chars().filter(|c| !c.is_control()).collect();
    let cleaned = cleaned.trim();
    // An empty title falls back to the address.
    let chosen = if cleaned.is_empty() {
        url.trim()
    } else {
        cleaned
    };
    // `take(n)` stops after `n` characters, so a long title is cut safely on
    // a character boundary (never in the middle of a multi-byte letter).
    chosen.chars().take(MAX_TITLE_CHARS).collect()
}

/// One Read Later link as it is stored in CouchDB.
///
/// `#[derive(Serialize, Deserialize)]` makes `serde` write this struct as JSON
/// and read it back; the `rename` lines map our names onto CouchDB's reserved
/// `_id` and `_rev`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaterDoc {
    /// CouchDB's document name: `later:` plus the address.
    #[serde(rename = "_id")]
    pub id: String,

    /// CouchDB's revision label, or `None` for a document never sent before.
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,

    /// Format version (see [`LATER_DOC_VERSION`]). A missing one reads as 0,
    /// which this build treats as unsupported.
    #[serde(default)]
    pub v: u32,

    /// The link's address, as written.
    pub url: String,

    /// What to show for the link.
    pub title: String,

    /// When it was put on the list, in milliseconds since 1970 (UTC).
    pub added_at: i64,

    /// `true` while the link is on the list, `false` once it has been taken off.
    /// A removal is a document, not a deletion, so the other devices can tell
    /// "removed later" from "never heard of it".
    pub present: bool,

    /// `true` once the link has been opened.
    #[serde(default)]
    pub read: bool,

    /// When anything about the link last changed, in milliseconds since 1970.
    /// The copy with the larger value wins.
    pub updated_at: i64,
}

impl LaterDoc {
    /// Builds the document that describes a record waiting to be sent.
    pub fn from_pending(pending: &PendingLaterPush) -> Self {
        LaterDoc {
            id: later_doc_id(&pending.url),
            rev: pending.rev.clone(),
            v: LATER_DOC_VERSION,
            url: pending.url.clone(),
            title: pending.title.clone(),
            added_at: pending.added_at,
            present: pending.present,
            read: pending.read,
            updated_at: pending.updated_at,
        }
    }

    /// Whether this build understands the document's format version.
    pub fn is_supported(&self) -> bool {
        (1..=LATER_DOC_VERSION).contains(&self.v)
    }

    /// Checks a document that arrived from the server and returns it tidied, or
    /// `None` if it must not be used. **Treat everything in it as untrusted**:
    /// the server is the person's own, but a title ends up on a terminal screen
    /// and an address may be opened in a browser, so only plain, sensible
    /// values pass.
    ///
    /// - the name must match the address (`later:` plus the address);
    /// - the address must be an ordinary web address ([`is_syncable_url`]);
    /// - the title is tidied by [`clean_title`] (so it is never empty, never
    ///   over-long and never carries control characters);
    /// - the times must be positive.
    ///
    /// `self` is taken by value (the caller gives it up), so the tidied copy
    /// can be returned without cloning.
    pub fn cleaned(mut self) -> Option<Self> {
        self.url = later_key(&self.url);
        self.title = clean_title(&self.title, &self.url);
        let acceptable = is_syncable_url(&self.url)
            && self.id == later_doc_id(&self.url)
            && self.added_at > 0
            && self.updated_at > 0;
        acceptable.then_some(self)
    }
}

/// Runs the Read Later part of a sync: pull what other devices changed, then
/// send what changed here, going round again while writes conflict.
///
/// Called by [`super::sync_now`], which has already checked that sync is on and
/// built the `client`. Counts go into `report`. A failed run changes nothing
/// that matters and is simply tried again next time.
pub(crate) async fn sync_read_later(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    // Step 1: pull first, so a link both devices already have is matched
    // instead of being sent as new.
    pull_later(storage, client, report).await?;

    // Step 2: push, and go round again (pull, then push) while writes conflict.
    for _ in 1..=MAX_ROUNDS {
        let conflicts = push_later(storage, client, report).await?;
        report.later_conflicts_left = conflicts;
        if conflicts == 0 {
            break;
        }
        pull_later(storage, client, report).await?;
    }
    Ok(())
}

/// Reads every page of changes since the Read Later checkpoint and stores the
/// `later:` documents among them, moving the checkpoint with each page.
async fn pull_later(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut SyncReport,
) -> anyhow::Result<()> {
    // How many Read Later documents have been read in this pull, against the limit.
    let mut later_rows = 0usize;
    loop {
        // Step 1: where did we get to last time? (A quick, non-waiting step.)
        let since = lock(storage)?.later_sync_since()?;

        // Step 2: ask the server (the database is unlocked while we wait).
        let page = client.changes(since.as_deref(), PAGE_SIZE).await?;
        let row_count = page.rows.len();

        // Step 3: keep the Read Later documents; everything else is not ours here.
        let mut docs: Vec<LaterDoc> = Vec::new();
        let storage_guard = lock(storage)?;
        for row in page.rows {
            // `strip_prefix` gives the part after `later:`, or `None` for any
            // other kind of document (which the `else` skips).
            let Some(key) = row.id.strip_prefix(LATER_DOC_PREFIX) else {
                continue;
            };
            later_rows += 1;
            if later_rows > MAX_LATER_DOCS {
                bail!("the server holds more Read Later records than tuxwire will read");
            }
            // Deleted on the server by hand: forget our record of the
            // revision, so the link is written again next push.
            if row.deleted {
                storage_guard.forget_later_doc(key)?;
                continue;
            }
            let Some(value) = row.doc else { continue };
            match serde_json::from_value::<LaterDoc>(value) {
                Ok(doc) if !doc.is_supported() => report.skipped_newer_version += 1,
                Ok(doc) => match doc.cleaned() {
                    Some(doc) => docs.push(doc),
                    None => report.skipped_unreadable += 1,
                },
                Err(_) => report.skipped_unreadable += 1,
            }
        }

        // Step 4: store them and move the checkpoint, in one transaction.
        let counts = storage_guard.apply_remote_later_batch(docs, &page.last_seq)?;
        drop(storage_guard);
        report.later_received += counts.added + counts.updated;

        // Step 5: a short page means we have reached the end.
        if row_count < PAGE_SIZE {
            return Ok(());
        }
    }
}

/// Sends every Read Later record that changed here. Returns how many writes
/// CouchDB refused as conflicts (they are retried after the next pull).
async fn push_later(
    storage: &Mutex<Storage>,
    client: &CouchClient,
    report: &mut SyncReport,
) -> anyhow::Result<usize> {
    // Step 1: what needs sending? (Quick, non-waiting.)
    let pending = lock(storage)?.pending_later_pushes()?;
    let mut conflicts = 0;

    // Step 2: send in batches of at most PAGE_SIZE.
    for batch in pending.chunks(PAGE_SIZE) {
        // Build the documents, remembering each one's change time so we can
        // record exactly what was sent, and its key to find the record again.
        let mut docs = Vec::with_capacity(batch.len());
        let mut sent: HashMap<String, (String, i64)> = HashMap::new();
        for item in batch {
            let doc = LaterDoc::from_pending(item);
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
                            .record_later_pushed(key, &rev, *stamp)
                            .with_context(|| "could not record a sent Read Later link")?;
                        report.later_sent += 1;
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
    fn doc() -> LaterDoc {
        LaterDoc {
            id: "later:https://example.com/Post".to_string(),
            rev: Some("1-a".to_string()),
            v: 1,
            url: "https://example.com/Post".to_string(),
            title: "A post".to_string(),
            added_at: 100,
            present: true,
            read: false,
            updated_at: 200,
        }
    }

    /// The document is named after the exact address, capitals included.
    #[test]
    fn a_link_is_named_after_its_exact_address() {
        assert_eq!(
            later_doc_id("  https://Example.com/Post "),
            "later:https://Example.com/Post"
        );
    }

    /// Titles lose control characters and surrounding space, are cut to the
    /// limit, and an empty one becomes the address.
    #[test]
    fn titles_are_tidied() {
        assert_eq!(
            clean_title("  Hi\u{1b}[2J there \n", "https://x/"),
            "Hi[2J there"
        );
        assert_eq!(clean_title("   ", "https://x/y"), "https://x/y");
        let long = "é".repeat(MAX_TITLE_CHARS + 50);
        assert_eq!(
            clean_title(&long, "https://x/").chars().count(),
            MAX_TITLE_CHARS
        );
    }

    /// A good document passes and survives a round trip as JSON.
    #[test]
    fn a_good_document_is_accepted_and_round_trips() {
        let cleaned = doc().cleaned().expect("accepted");
        let json = serde_json::to_string(&LaterDoc {
            rev: None,
            ..cleaned.clone()
        })
        .unwrap();
        assert!(!json.contains("_rev"));
        assert!(json.contains("\"_id\""));
        let back: LaterDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back,
            LaterDoc {
                rev: None,
                ..cleaned
            }
        );
    }

    /// Untrusted values are refused: a wrong name, an address that is not a web
    /// address, and missing times.
    #[test]
    fn unsafe_or_odd_documents_are_refused() {
        let mut d = doc();
        d.id = "later:https://other.example/".to_string();
        assert!(d.cleaned().is_none(), "name must match the address");

        let mut d = doc();
        d.url = "javascript:alert(1)".to_string();
        d.id = later_doc_id(&d.url);
        assert!(d.cleaned().is_none());

        let mut d = doc();
        d.url = "file:///home/me".to_string();
        d.id = later_doc_id(&d.url);
        assert!(d.cleaned().is_none());

        let mut d = doc();
        d.added_at = 0;
        assert!(d.cleaned().is_none());

        let mut d = doc();
        d.updated_at = 0;
        assert!(d.cleaned().is_none());
    }

    /// A missing version is unsupported, a missing `read` reads as unread, and
    /// unknown extra fields are ignored.
    #[test]
    fn missing_fields_default_sensibly() {
        let json = r#"{"_id":"later:https://x.example/p","url":"https://x.example/p",
            "title":"T","added_at":5,"present":true,"updated_at":6,"colour":"red"}"#;
        let d: LaterDoc = serde_json::from_str(json).unwrap();
        assert!(!d.read);
        assert!(!d.is_supported(), "no version means unsupported");
        assert!(serde_json::from_str::<LaterDoc>(r#"{"_id":"later:x"}"#).is_err());
    }
}
