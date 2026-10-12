//! The shape of one synced article, and the rule for which copy wins.
//!
//! Sync shares two things between devices: which articles are **saved**, and
//! the **notes** on them. Everything else (what you have read, what you have
//! skipped, your feeds) stays on each device. Each saved or noted article
//! becomes one CouchDB document, named after the article's own address, so two
//! devices that hold the same article agree on what to call it. (Local article
//! numbers differ from device to device, so they cannot be used.)
//!
//! Un-saving is written as a document too, with `saved: false`, rather than by
//! deleting it from CouchDB. A deletion leaves nothing behind to compare dates
//! with, so the other device could never tell "un-saved on purpose, later" from
//! "never heard of it" and would save it again. A document that says
//! `saved: false` with a newer `updated_at` settles it.
//!
//! This file is pure data and rules: no network, no database. That keeps it
//! easy to test and means the terminal app and the Android app agree exactly.

#![warn(clippy::missing_docs_in_private_items)]

use crate::models::Article;
use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Every article document's `_id` starts with this, followed by the article's
/// address. Anything in the database without this prefix is not ours and is
/// ignored (CouchDB also keeps its own `_design/...` documents).
pub const DOC_PREFIX: &str = "article:";

/// The document format version this build writes and understands. A document
/// with a *higher* version came from a newer tuxwire; this build skips it
/// (and says so) instead of misreading it, and keeps syncing the rest.
pub const DOC_VERSION: u32 = 1;

/// The `_id` for the article at `url`: the prefix plus the address.
///
/// `&str` borrows the address; the returned `String` is a new, owned value
/// the caller keeps.
pub fn doc_id_for(url: &str) -> String {
    format!("{DOC_PREFIX}{url}")
}

/// One saved-or-un-saved article as it is stored in CouchDB.
///
/// `#[derive(Serialize, Deserialize)]` makes `serde` write this struct out as
/// JSON and read it back, field by field, with no hand-written code. The
/// `#[serde(rename = "_id")]` lines map our field names onto CouchDB's
/// reserved names (CouchDB insists on `_id` and `_rev`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArticleDoc {
    /// CouchDB's document name: `article:` plus the address.
    #[serde(rename = "_id")]
    pub id: String,

    /// CouchDB's revision label for this document. It is `None` when we are
    /// creating a document for the first time; CouchDB fills it in. It must be
    /// sent back when updating, or CouchDB answers "conflict". `skip_serializing_if`
    /// leaves the field out of the JSON entirely when it is `None`.
    #[serde(rename = "_rev", default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<String>,

    /// Format version (see [`DOC_VERSION`]). A document with no `v` reads as 0,
    /// which this build treats as unsupported.
    #[serde(default)]
    pub v: u32,

    /// The article's own address; the same article on two devices has the
    /// same one.
    pub url: String,

    /// Headline, kept so a device that has never seen the article can show it.
    pub title: String,

    /// The name of the feed it came from (for display).
    pub source: String,

    /// The topic it is filed under on the device that saved it.
    pub topic: String,

    /// When the article was published, in milliseconds since 1970 (UTC).
    pub published: i64,

    /// The feed's short plain-text summary, if it had one.
    #[serde(default)]
    pub summary: Option<String>,

    /// `true` while the article is saved; `false` once it has been un-saved.
    pub saved: bool,

    /// When it was saved, in milliseconds since 1970. `None` when not saved.
    #[serde(default)]
    pub saved_at: Option<i64>,

    /// The note, or `None`. A note only exists on a saved article.
    #[serde(default)]
    pub note: Option<String>,

    /// When the note was last written, in milliseconds since 1970.
    #[serde(default)]
    pub noted_at: Option<i64>,

    /// When the saved/note state last changed, in milliseconds since 1970.
    /// The copy with the larger value wins (see [`remote_wins`]).
    pub updated_at: i64,
}

impl ArticleDoc {
    /// Builds the document that describes `article`'s saved/note state.
    ///
    /// `updated_at` is the article's change stamp from the database, and `rev`
    /// is the revision label CouchDB last gave us for this document (`None` if
    /// we have never synced it).
    pub fn from_article(article: &Article, updated_at: i64, rev: Option<String>) -> Self {
        ArticleDoc {
            id: doc_id_for(&article.url),
            rev,
            v: DOC_VERSION,
            url: article.url.clone(),
            title: article.title.clone(),
            source: article.source.clone(),
            topic: article.topic.clone(),
            published: article.timestamp.timestamp_millis(),
            summary: article.summary.clone(),
            saved: article.saved,
            saved_at: article.saved_at.map(|d| d.timestamp_millis()),
            note: article.note.clone(),
            noted_at: article.noted_at.map(|d| d.timestamp_millis()),
            updated_at,
        }
    }

    /// Returns this document with the **note lock** applied, so a document
    /// from any source ends up consistent:
    ///
    /// - a note with real text means the article is saved;
    /// - an empty or all-space note counts as no note;
    /// - an article that is not saved has no note and no dates.
    ///
    /// `mut self` takes the document by value (the caller gives it up) so we
    /// can change it and hand it back without copying.
    pub fn normalized(mut self) -> Self {
        // Step 1: a blank note is no note.
        if self.note.as_deref().is_some_and(|n| n.trim().is_empty()) {
            self.note = None;
        }
        // Step 2: a real note forces "saved".
        if self.note.is_some() {
            self.saved = true;
        }
        // Step 3: not saved means nothing else is kept.
        if !self.saved {
            self.note = None;
            self.saved_at = None;
        }
        // Step 4: no note means no note date.
        if self.note.is_none() {
            self.noted_at = None;
        }
        self
    }

    /// Whether this build understands the document's format version.
    pub fn is_supported(&self) -> bool {
        (1..=DOC_VERSION).contains(&self.v)
    }

    /// The article's publish date as a `DateTime`. A value that cannot be a
    /// real date falls back to "now" rather than failing the whole sync.
    pub fn published_at(&self) -> DateTime<Utc> {
        ms_to_utc(self.published).unwrap_or_else(Utc::now)
    }
}

/// Turns milliseconds since 1970 into a `DateTime<Utc>`, or `None` if the
/// number is not a valid date. (`single()` picks the one answer; for UTC there
/// is never more than one.)
pub fn ms_to_utc(ms: i64) -> Option<DateTime<Utc>> {
    Utc.timestamp_millis_opt(ms).single()
}

/// **The rule for which copy wins:** the remote document replaces the local
/// state only if its `updated_at` is strictly newer than the local change
/// stamp. `local` is `None` when this device has never saved or noted the
/// article (treated as 0, the oldest possible).
///
/// Equal stamps mean both sides already hold the same version, so nothing
/// changes. The whole saved-and-note state moves together: a newer remote copy
/// replaces the note too, never just one half.
pub fn remote_wins(local: Option<i64>, remote_updated_at: i64) -> bool {
    remote_updated_at > local.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A saved article with a note, for building documents in tests.
    fn saved_article() -> Article {
        let mut article = Article::new(
            "A title".to_string(),
            "https://example.com/a".to_string(),
            "Example".to_string(),
            "tech".to_string(),
            Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap(),
        );
        article.saved = true;
        article.saved_at = Some(Utc.with_ymd_and_hms(2026, 10, 2, 12, 0, 0).unwrap());
        article.note = Some("worth reading".to_string());
        article.noted_at = Some(Utc.with_ymd_and_hms(2026, 10, 3, 12, 0, 0).unwrap());
        article
    }

    /// The document is named after the address, and survives a round trip
    /// through JSON unchanged.
    #[test]
    fn a_document_is_named_after_the_address_and_round_trips() {
        let doc = ArticleDoc::from_article(&saved_article(), 1_000, None);
        assert_eq!(doc.id, "article:https://example.com/a");

        let json = serde_json::to_string(&doc).unwrap();
        // A document never synced before has no `_rev` field at all.
        assert!(!json.contains("_rev"));
        assert!(json.contains("\"_id\""));

        let back: ArticleDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(back, doc);
    }

    /// A revision label from CouchDB is read from `_rev` and sent back.
    #[test]
    fn the_revision_label_uses_couchdbs_field_name() {
        let doc = ArticleDoc::from_article(&saved_article(), 1, Some("3-abc".to_string()));
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("\"_rev\":\"3-abc\""));
    }

    /// The note lock: a real note forces saved; a blank note is no note; an
    /// unsaved article keeps no note or dates.
    #[test]
    fn normalizing_applies_the_note_lock() {
        // A note on a document that says "not saved": the note wins.
        let mut doc = ArticleDoc::from_article(&saved_article(), 1, None);
        doc.saved = false;
        assert!(doc.normalized().saved);

        // A blank note is dropped, with its date.
        let mut doc = ArticleDoc::from_article(&saved_article(), 1, None);
        doc.note = Some("   ".to_string());
        let doc = doc.normalized();
        assert_eq!(doc.note, None);
        assert_eq!(doc.noted_at, None);
        assert!(doc.saved, "a blank note does not un-save the article");

        // An un-saved document carries nothing.
        let mut doc = ArticleDoc::from_article(&saved_article(), 1, None);
        doc.saved = false;
        doc.note = None;
        let doc = doc.normalized();
        assert_eq!(
            (doc.saved, doc.saved_at, doc.note, doc.noted_at),
            (false, None, None, None)
        );
    }

    /// Only strictly newer remote copies win; equal means already in step.
    #[test]
    fn the_newer_copy_wins_and_equal_means_no_change() {
        assert!(remote_wins(None, 1));
        assert!(remote_wins(Some(5), 6));
        assert!(!remote_wins(Some(5), 5));
        assert!(!remote_wins(Some(5), 4));
        assert!(!remote_wins(None, 0));
    }

    /// Format versions: this build reads 1 and skips anything newer or missing.
    #[test]
    fn newer_or_missing_versions_are_not_supported() {
        let mut doc = ArticleDoc::from_article(&saved_article(), 1, None);
        assert!(doc.is_supported());
        doc.v = DOC_VERSION + 1;
        assert!(!doc.is_supported());
        doc.v = 0;
        assert!(!doc.is_supported());
    }

    /// A document with unknown extra fields still reads (a newer app may add
    /// fields), and one missing required fields is an error, not a crash.
    #[test]
    fn unknown_fields_are_ignored_and_missing_ones_are_errors() {
        let json = r#"{"_id":"article:https://x/y","v":1,"url":"https://x/y","title":"t",
            "source":"s","topic":"c","published":0,"saved":true,"updated_at":5,"colour":"red"}"#;
        let doc: ArticleDoc = serde_json::from_str(json).unwrap();
        assert_eq!(doc.updated_at, 5);

        assert!(serde_json::from_str::<ArticleDoc>(r#"{"_id":"article:x"}"#).is_err());
    }
}
