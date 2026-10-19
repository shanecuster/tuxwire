//! The Read Later list, and the database half of syncing it.
//!
//! Read Later is a list of links the person wants to come back to: an article
//! from a feed, or any web address pasted in. It is **not** the same as Saved
//! (a permanent archive with notes). An item here is meant to be read and then
//! either discarded or promoted to a saved article ([`Storage::promote_read_later`]).
//!
//! These are plain `Storage` methods that know nothing about the network. The
//! `sync::later` module talks to CouchDB and calls the sync half (the `pending`,
//! `record` and `apply` methods) to decide what to send and to store what arrived.
//!
//! **One table does both jobs.** `read_later` (migration 010) is the list the
//! apps show *and* the record of what has been synced, with one row per link:
//! `present` says whether it is on the list, `updated_at` says when it last
//! changed, and `pushed_updated_at` says what was last sent. Taking a link off
//! the list keeps its row (with `present = 0`), so the other devices can tell
//! "removed on purpose, later" from "never heard of it".
//!
//! **The rules, in short.**
//! - A link is identified by its address, exactly as written (trimmed).
//! - Of two copies of the same link's record, the one with the newer change
//!   time wins, as with saved articles.
//! - Putting a link back on the list after it was removed counts as a new
//!   change: it is shown again, unread.

#![warn(clippy::missing_docs_in_private_items)]

use super::Storage;
use super::feed_sync_store::next_stamp;
use crate::models::Article;
use crate::sync::doc::remote_wins;
use crate::sync::feeds::is_syncable_url;
use crate::sync::later::{LaterDoc, clean_title, later_key};
use anyhow::bail;
use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{OptionalExtension, params};

/// The `settings` key holding the Read Later pull checkpoint (CouchDB's
/// `last_seq`). Separate from the other checkpoints, so a device updated later
/// still reads the Read Later documents that were written before it was.
const LATER_SINCE_KEY: &str = "sync.later.since";

/// The source and topic given to the article row made when a pasted link is
/// promoted to a saved article. (An article from a feed keeps its own.)
const PROMOTED_SOURCE: &str = "Read Later";

/// One link on the Read Later list, as the apps show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadLaterItem {
    /// The link's address; also what identifies it.
    pub url: String,
    /// What to show for it.
    pub title: String,
    /// When it was put on the list.
    pub added_at: DateTime<Utc>,
    /// `true` once it has been opened.
    pub read: bool,
}

/// A Read Later record whose change time is newer than what was last sent.
#[derive(Debug, Clone)]
pub struct PendingLaterPush {
    /// The record's identity (the address, trimmed).
    pub key: String,
    /// The address as written.
    pub url: String,
    /// The title.
    pub title: String,
    /// When it was put on the list, in milliseconds since 1970.
    pub added_at: i64,
    /// `true` for "on the list", `false` for "taken off it".
    pub present: bool,
    /// `true` once it has been opened.
    pub read: bool,
    /// The record's change time, in milliseconds since 1970.
    pub updated_at: i64,
    /// CouchDB's revision label for the document, if it was sent before.
    pub rev: Option<String>,
}

/// What applying a batch of pulled Read Later documents did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LaterApplyCounts {
    /// Links this device had never heard of, now recorded.
    pub added: usize,
    /// Known links whose record was replaced by a newer copy.
    pub updated: usize,
    /// Documents that changed nothing (already in step, or this device's copy
    /// was newer).
    pub unchanged: usize,
}

/// The two columns of a `read_later` row that the rules below need.
struct LaterRow {
    /// `true` while the link is on the list.
    present: bool,
    /// The record's change time in milliseconds.
    updated_at: i64,
}

impl Storage {
    /// Puts a link on the Read Later list.
    ///
    /// `title` is tidied (see `clean_title`); an empty one becomes the address.
    /// Returns `true` when the link is now newly on the list, and `false` when
    /// it was already there (nothing changes). A link that was taken off the
    /// list earlier comes back as a new, unread item.
    ///
    /// Fails, with a message fit for the footer, when `url` is not an ordinary
    /// web address (`http://` or `https://`), so a pasted `file://` path or
    /// `javascript:` text can never end up on the list or be synced.
    pub fn add_read_later(&self, title: &str, url: &str) -> anyhow::Result<bool> {
        // Step 1: only ordinary web addresses are accepted.
        let url = url.trim();
        if !is_syncable_url(url) {
            bail!("that is not a web address (it must start with http:// or https://)");
        }
        let key = later_key(url);
        let title = clean_title(title, url);
        let now = Utc::now().timestamp_millis();

        // Step 2: act on what is already known about this address.
        match self.later_row(&key)? {
            // Never seen: a brand new item.
            None => {
                self.conn.execute(
                    "INSERT INTO read_later
                         (key, url, title, added_at, present, read, updated_at)
                     VALUES (?1, ?2, ?3, ?4, 1, 0, ?5)",
                    params![key, url, title, now, next_stamp(0)],
                )?;
                Ok(true)
            }
            // Already on the list: nothing to do.
            Some(row) if row.present => Ok(false),
            // It was taken off the list; putting it back is a fresh change.
            Some(row) => {
                self.conn.execute(
                    "UPDATE read_later
                     SET url = ?2, title = ?3, added_at = ?4, present = 1,
                         read = 0, updated_at = ?5
                     WHERE key = ?1",
                    params![key, url, title, now, next_stamp(row.updated_at)],
                )?;
                Ok(true)
            }
        }
    }

    /// Every link on the list, newest first.
    pub fn list_read_later(&self) -> anyhow::Result<Vec<ReadLaterItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT url, title, added_at, read FROM read_later
             WHERE present = 1
             ORDER BY added_at DESC, key",
        )?;
        let rows = stmt.query_map([], |row| {
            let added_ms: i64 = row.get(2)?;
            Ok(ReadLaterItem {
                url: row.get(0)?,
                title: row.get(1)?,
                // Milliseconds since 1970 back into a date; a nonsense value
                // (which sync refuses anyway) falls back to the epoch.
                added_at: Utc
                    .timestamp_millis_opt(added_ms)
                    .single()
                    .unwrap_or_default(),
                read: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// How many links are on the list.
    pub fn read_later_count(&self) -> anyhow::Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM read_later WHERE present = 1",
            [],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Marks a link on the list as opened. Returns `true` when something
    /// changed, and `false` when the link is not on the list or was already
    /// marked. The item stays on the list.
    pub fn mark_read_later_read(&self, url: &str) -> anyhow::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE read_later
             SET read = 1, updated_at = MAX(updated_at + 1, ?2)
             WHERE key = ?1 AND present = 1 AND read = 0",
            params![later_key(url), Utc::now().timestamp_millis()],
        )?;
        Ok(changed > 0)
    }

    /// Takes a link off the list (discard). The record stays, marked as
    /// removed, so the other devices drop it too. Returns `true` when the link
    /// was on the list.
    ///
    /// This does not ask for confirmation; the app must, as with un-saving.
    pub fn discard_read_later(&self, url: &str) -> anyhow::Result<bool> {
        let changed = self.conn.execute(
            "UPDATE read_later
             SET present = 0, updated_at = MAX(updated_at + 1, ?2)
             WHERE key = ?1 AND present = 1",
            params![later_key(url), Utc::now().timestamp_millis()],
        )?;
        Ok(changed > 0)
    }

    /// Saves a Read Later link permanently and takes it off the list. Returns
    /// the saved article's id.
    ///
    /// - If the link is already an article in the database (it came from a
    ///   feed), that article is saved. One that is already saved is left alone,
    ///   so its note is never touched.
    /// - Otherwise a new article row is made for the pasted link (source and
    ///   topic "Read Later") and saved. Saved, Search and export work across
    ///   topics, so this is harmless.
    ///
    /// Saving stamps the article for sync, so other devices receive it as an
    /// ordinary saved article. Everything happens in one transaction: if any
    /// step fails, nothing changes. Fails if the link is not on the list.
    pub fn promote_read_later(&self, url: &str) -> anyhow::Result<i64> {
        let tx = self.conn.unchecked_transaction()?;
        let key = later_key(url);

        // Step 1: the link must be on the list; remember its title.
        let title: Option<String> = self
            .conn
            .query_row(
                "SELECT title FROM read_later WHERE key = ?1 AND present = 1",
                params![key],
                |row| row.get(0),
            )
            .optional()?;
        let Some(title) = title else {
            bail!("that link is not on the Read Later list");
        };

        // Step 2: find the article for this address, or make one.
        let existing: Option<(i64, bool)> = self
            .conn
            .query_row(
                "SELECT id, saved FROM articles WHERE url = ?1",
                params![key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let id = match existing {
            // Already saved: leave it (and any note) exactly as it is.
            Some((id, true)) => id,
            // An article from a feed, not saved yet: save it.
            Some((id, false)) => {
                self.save_article(id, None)?;
                id
            }
            // A pasted link with no article yet: make one, then save it.
            None => {
                let article = Article::new(
                    title,
                    key.clone(),
                    PROMOTED_SOURCE.to_string(),
                    PROMOTED_SOURCE.to_string(),
                    Utc::now(),
                );
                let id = self.insert_article(&article)?;
                self.save_article(id, None)?;
                id
            }
        };

        // Step 3: take it off the list.
        self.conn.execute(
            "UPDATE read_later
             SET present = 0, updated_at = MAX(updated_at + 1, ?2)
             WHERE key = ?1",
            params![key, Utc::now().timestamp_millis()],
        )?;

        tx.commit()?;
        Ok(id)
    }

    /// Every Read Later record whose change time is newer than what was last sent.
    pub fn pending_later_pushes(&self) -> anyhow::Result<Vec<PendingLaterPush>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, url, title, added_at, present, read, updated_at, rev
             FROM read_later
             WHERE updated_at > pushed_updated_at
             ORDER BY updated_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PendingLaterPush {
                key: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                added_at: row.get(3)?,
                present: row.get(4)?,
                read: row.get(5)?,
                updated_at: row.get(6)?,
                rev: row.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Records that the document for `key` was written with revision `rev`,
    /// carrying the change time `updated_at`. After this the record no longer
    /// counts as pending (until it changes again).
    pub fn record_later_pushed(&self, key: &str, rev: &str, updated_at: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE read_later SET rev = ?2, pushed_updated_at = ?3 WHERE key = ?1",
            params![key, rev, updated_at],
        )?;
        Ok(())
    }

    /// Forgets the remembered revision for `key`, so the record counts as
    /// "never sent" and is written again. Used when its document was deleted on
    /// the server by hand.
    pub fn forget_later_doc(&self, key: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE read_later SET rev = NULL, pushed_updated_at = 0 WHERE key = ?1",
            params![key],
        )?;
        Ok(())
    }

    /// How far through CouchDB's change list the Read Later sync has read, or
    /// `None` before its first pull.
    pub fn later_sync_since(&self) -> anyhow::Result<Option<String>> {
        self.get_setting(LATER_SINCE_KEY)
    }

    /// Forgets the Read Later sync's server bookkeeping (the checkpoint and every
    /// remembered revision), so the next sync starts from the beginning and
    /// re-checks everything. The list itself is **not** touched. Called by
    /// [`Storage::reset_sync_state`].
    pub(super) fn reset_later_sync_state(&self) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE read_later SET rev = NULL, pushed_updated_at = 0",
            [],
        )?;
        self.conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            params![LATER_SINCE_KEY],
        )?;
        Ok(())
    }

    /// Stores a batch of Read Later documents pulled from CouchDB and moves the
    /// checkpoint to `new_since`, **in one transaction** (all or nothing, so the
    /// checkpoint never passes changes that were not stored).
    ///
    /// For each document the rule is the same as for saved articles: a strictly
    /// newer remote copy replaces this device's record; otherwise this device's
    /// record stays and only the remembered revision is updated, so a later send
    /// can supersede the server's copy. Documents must already be cleaned (see
    /// `LaterDoc::cleaned`).
    pub fn apply_remote_later_batch(
        &self,
        docs: Vec<LaterDoc>,
        new_since: &str,
    ) -> anyhow::Result<LaterApplyCounts> {
        let tx = self.conn.unchecked_transaction()?;
        let mut counts = LaterApplyCounts::default();

        for doc in docs {
            // A pulled document always has a revision label; skip one that
            // somehow does not.
            let Some(rev) = doc.rev.clone() else {
                counts.unchanged += 1;
                continue;
            };
            let key = later_key(&doc.url);

            match self.later_row(&key)? {
                // An address this device never heard of: remember it.
                None => {
                    self.conn.execute(
                        "INSERT INTO read_later
                             (key, url, title, added_at, present, read, updated_at,
                              rev, pushed_updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?7)",
                        params![
                            key,
                            doc.url,
                            doc.title,
                            doc.added_at,
                            doc.present,
                            doc.read,
                            doc.updated_at,
                            rev
                        ],
                    )?;
                    counts.added += 1;
                }
                // Known: the newer copy wins.
                Some(row) if remote_wins(Some(row.updated_at), doc.updated_at) => {
                    self.conn.execute(
                        "UPDATE read_later
                         SET url = ?2, title = ?3, added_at = ?4, present = ?5,
                             read = ?6, updated_at = ?7, rev = ?8,
                             pushed_updated_at = ?7
                         WHERE key = ?1",
                        params![
                            key,
                            doc.url,
                            doc.title,
                            doc.added_at,
                            doc.present,
                            doc.read,
                            doc.updated_at,
                            rev
                        ],
                    )?;
                    counts.updated += 1;
                }
                // Same version on both sides: already in step.
                Some(row) if row.updated_at == doc.updated_at => {
                    self.conn.execute(
                        "UPDATE read_later SET rev = ?2, pushed_updated_at = ?3 WHERE key = ?1",
                        params![key, rev, doc.updated_at],
                    )?;
                    counts.unchanged += 1;
                }
                // This device's copy is newer: keep it, still to be sent, but
                // remember the server's revision so the send can supersede it.
                Some(_) => {
                    self.conn.execute(
                        "UPDATE read_later SET rev = ?2 WHERE key = ?1",
                        params![key, rev],
                    )?;
                    counts.unchanged += 1;
                }
            }
        }

        // Move the checkpoint inside the same transaction.
        self.set_setting(LATER_SINCE_KEY, new_since)?;
        tx.commit()?;
        Ok(counts)
    }

    /// The `read_later` row for `key`, or `None` if nothing is known about it.
    fn later_row(&self, key: &str) -> anyhow::Result<Option<LaterRow>> {
        self.conn
            .query_row(
                "SELECT present, updated_at FROM read_later WHERE key = ?1",
                params![key],
                |row| {
                    Ok(LaterRow {
                        present: row.get(0)?,
                        updated_at: row.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh in-memory database for a test.
    fn storage() -> Storage {
        Storage::open_in_memory().unwrap()
    }

    /// A pulled document for a test.
    fn doc(url: &str, present: bool, updated_at: i64) -> LaterDoc {
        LaterDoc {
            id: crate::sync::later::later_doc_id(url),
            rev: Some("1-abc".to_string()),
            v: 1,
            url: url.to_string(),
            title: format!("Title of {url}"),
            added_at: 1000,
            present,
            read: false,
            updated_at,
        }
    }

    /// Adding puts a link on the list once; adding it again changes nothing.
    #[test]
    fn adding_a_link_twice_is_a_no_op() {
        let s = storage();
        assert!(s.add_read_later("A post", "https://example.com/p").unwrap());
        assert!(!s.add_read_later("A post", "https://example.com/p").unwrap());
        let items = s.list_read_later().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "A post");
        assert!(!items[0].read);
        assert_eq!(s.read_later_count().unwrap(), 1);
    }

    /// Only ordinary web addresses are accepted.
    #[test]
    fn only_web_addresses_can_be_added() {
        let s = storage();
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "hello", ""] {
            assert!(s.add_read_later("x", bad).is_err(), "{bad}");
        }
        assert_eq!(s.read_later_count().unwrap(), 0);
        assert!(s.pending_later_pushes().unwrap().is_empty());
    }

    /// An empty title becomes the address, and the list is newest first.
    #[test]
    fn titles_fall_back_and_the_list_is_newest_first() {
        let s = storage();
        s.add_read_later("", "https://example.com/one").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        s.add_read_later("Two", "https://example.com/two").unwrap();
        let items = s.list_read_later().unwrap();
        assert_eq!(items[0].title, "Two");
        assert_eq!(items[1].title, "https://example.com/one");
    }

    /// Opening an item marks it read and keeps it on the list.
    #[test]
    fn opening_marks_read_and_keeps_the_item() {
        let s = storage();
        s.add_read_later("A", "https://example.com/a").unwrap();
        assert!(s.mark_read_later_read("https://example.com/a").unwrap());
        assert!(!s.mark_read_later_read("https://example.com/a").unwrap());
        assert!(!s.mark_read_later_read("https://example.com/none").unwrap());
        let items = s.list_read_later().unwrap();
        assert_eq!(items.len(), 1);
        assert!(items[0].read);
    }

    /// Discarding takes it off the list but keeps a record to send, and adding
    /// it again later brings it back unread.
    #[test]
    fn discard_leaves_a_record_and_the_link_can_return() {
        let s = storage();
        s.add_read_later("A", "https://example.com/a").unwrap();
        s.mark_read_later_read("https://example.com/a").unwrap();
        assert!(s.discard_read_later("https://example.com/a").unwrap());
        assert!(!s.discard_read_later("https://example.com/a").unwrap());
        assert!(s.list_read_later().unwrap().is_empty());
        let pending = s.pending_later_pushes().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(!pending[0].present, "the removal is a record to send");

        assert!(
            s.add_read_later("A again", "https://example.com/a")
                .unwrap()
        );
        let items = s.list_read_later().unwrap();
        assert_eq!(items.len(), 1);
        assert!(!items[0].read, "it comes back unread");
        assert_eq!(items[0].title, "A again");
    }

    /// Promoting a pasted link makes a saved article and takes the link off the
    /// list; the saved article is stamped so it syncs.
    #[test]
    fn promoting_a_pasted_link_saves_a_new_article() {
        let s = storage();
        s.add_read_later("Cool tool", "https://example.com/tool")
            .unwrap();
        let id = s.promote_read_later("https://example.com/tool").unwrap();
        assert!(s.list_read_later().unwrap().is_empty());

        let saved = s.saved_articles().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].id, id);
        assert_eq!(saved[0].url, "https://example.com/tool");
        assert_eq!(saved[0].title, "Cool tool");
        assert!(saved[0].saved);
        assert_eq!(
            s.pending_pushes().unwrap().len(),
            1,
            "the new saved article is waiting to sync"
        );
    }

    /// Promoting a link that is already an article saves that article, and an
    /// article that is already saved keeps its note.
    #[test]
    fn promoting_an_existing_article_reuses_it() {
        let s = storage();
        // An unsaved article from a feed.
        let fresh = Article::new(
            "From a feed".to_string(),
            "https://example.com/feed-post".to_string(),
            "Blog".to_string(),
            "tech".to_string(),
            Utc::now(),
        );
        let fresh_id = s.insert_article(&fresh).unwrap();
        s.add_read_later("From a feed", "https://example.com/feed-post")
            .unwrap();
        assert_eq!(
            s.promote_read_later("https://example.com/feed-post")
                .unwrap(),
            fresh_id
        );
        assert_eq!(s.saved_articles().unwrap()[0].source, "Blog");

        // An article that is already saved, with a note.
        let noted = Article::new(
            "Noted".to_string(),
            "https://example.com/noted".to_string(),
            "Blog".to_string(),
            "tech".to_string(),
            Utc::now(),
        );
        let noted_id = s.insert_article(&noted).unwrap();
        s.save_article(noted_id, Some("remember this".to_string()))
            .unwrap();
        s.add_read_later("Noted", "https://example.com/noted")
            .unwrap();
        s.promote_read_later("https://example.com/noted").unwrap();
        let kept = s
            .saved_articles()
            .unwrap()
            .into_iter()
            .find(|a| a.id == noted_id)
            .unwrap();
        assert_eq!(kept.note.as_deref(), Some("remember this"));
    }

    /// Promoting a link that is not on the list fails and changes nothing.
    #[test]
    fn promoting_a_missing_link_fails() {
        let s = storage();
        assert!(s.promote_read_later("https://example.com/none").is_err());
        assert!(s.saved_articles().unwrap().is_empty());
    }

    /// A pulled document for an unknown link is recorded as already sent; a
    /// newer one replaces a known record; an older one does not.
    #[test]
    fn the_newer_copy_wins_when_applying_pulled_documents() {
        let s = storage();
        let url = "https://example.com/x";

        let counts = s
            .apply_remote_later_batch(vec![doc(url, true, 100)], "seq-1")
            .unwrap();
        assert_eq!(counts.added, 1);
        assert_eq!(s.list_read_later().unwrap().len(), 1);
        assert!(
            s.pending_later_pushes().unwrap().is_empty(),
            "nothing to send back"
        );
        assert_eq!(s.later_sync_since().unwrap().as_deref(), Some("seq-1"));

        // Newer: the removal wins.
        let counts = s
            .apply_remote_later_batch(vec![doc(url, false, 200)], "seq-2")
            .unwrap();
        assert_eq!(counts.updated, 1);
        assert!(s.list_read_later().unwrap().is_empty());

        // Older: ignored.
        let counts = s
            .apply_remote_later_batch(vec![doc(url, true, 150)], "seq-3")
            .unwrap();
        assert_eq!(counts.unchanged, 1);
        assert!(s.list_read_later().unwrap().is_empty());
    }

    /// A local change newer than the server's stays pending and keeps the
    /// server's revision for the send.
    #[test]
    fn a_newer_local_change_stays_pending() {
        let s = storage();
        let url = "https://example.com/y";
        s.add_read_later("Y", url).unwrap();
        // The server's copy is ancient.
        s.apply_remote_later_batch(vec![doc(url, false, 1)], "seq-1")
            .unwrap();
        assert_eq!(s.list_read_later().unwrap().len(), 1, "local copy kept");
        let pending = s.pending_later_pushes().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].rev.as_deref(), Some("1-abc"));
    }

    /// Recording a send clears the pending state, and a reset sends everything
    /// again without touching the list.
    #[test]
    fn recording_a_send_and_resetting() {
        let s = storage();
        let url = "https://example.com/z";
        s.add_read_later("Z", url).unwrap();
        let pending = s.pending_later_pushes().unwrap();
        s.record_later_pushed(&pending[0].key, "1-xyz", pending[0].updated_at)
            .unwrap();
        assert!(s.pending_later_pushes().unwrap().is_empty());

        s.apply_remote_later_batch(vec![], "seq-9").unwrap();
        s.reset_sync_state().unwrap();
        assert_eq!(s.later_sync_since().unwrap(), None);
        assert_eq!(s.pending_later_pushes().unwrap().len(), 1);
        assert_eq!(
            s.list_read_later().unwrap().len(),
            1,
            "the list is untouched"
        );
    }

    /// Forgetting a revision (its document was deleted on the server) makes the
    /// record pending again.
    #[test]
    fn a_forgotten_document_is_sent_again() {
        let s = storage();
        s.add_read_later("Q", "https://example.com/q").unwrap();
        let p = s.pending_later_pushes().unwrap();
        s.record_later_pushed(&p[0].key, "1-a", p[0].updated_at)
            .unwrap();
        s.forget_later_doc(&p[0].key).unwrap();
        assert_eq!(s.pending_later_pushes().unwrap().len(), 1);
    }
}
