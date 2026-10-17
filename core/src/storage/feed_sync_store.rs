//! The database half of the optional feed-list sync.
//!
//! Like `sync_store.rs` for saved articles, these are plain `Storage` methods
//! that know nothing about the network. The `sync::feeds` module talks to
//! CouchDB and calls these to decide what to send and to store what arrived.
//!
//! **Where the feed list really lives.** Not here. The terminal app keeps its
//! feeds in `sources.toml` and the Android app in its `feeds` table. This file
//! keeps only the `feed_sync` table: what sync knows about each feed address
//! (is it present or removed, since when, does *this* device have it). Each app
//! tells sync what its list contains ([`Storage::record_local_feeds`]), and
//! afterwards asks what to add ([`Storage::pending_feed_additions`]) or what
//! to ask the person about ([`Storage::pending_feed_removals`]).
//!
//! **The rules, in short.**
//! - A feed another device added is *added here automatically*.
//! - A feed another device removed is *only listed*. This device removes it
//!   after the person says yes; saying no keeps it ([`Storage::dismiss_feed_removal`]).
//! - Of two copies of the same feed's record, the one with the newer change
//!   time wins, as with saved articles.
//! - Only the address (compared in lower case) identifies a feed. Renaming a
//!   feed or moving it to another topic is not synced.

#![warn(clippy::missing_docs_in_private_items)]

use super::Storage;
use crate::sync::doc::remote_wins;
use crate::sync::feeds::{FeedDoc, feed_key, is_syncable_url};
use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

/// The `settings` key holding the feed pull checkpoint (CouchDB's `last_seq`).
/// It is separate from the saved-article checkpoint, so turning feed sync on
/// later never misses feed documents written while it was off.
const FEEDS_SINCE_KEY: &str = "sync.feeds.since";

/// One feed as the apps and sync exchange it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEntry {
    /// The name shown in the app.
    pub name: String,
    /// The feed's address.
    pub url: String,
    /// The topic it is filed under.
    pub topic: String,
}

/// What [`Storage::record_local_feeds`] noticed about this device's own list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LocalFeedChanges {
    /// Feeds in the list that sync had not seen before (or had seen removed):
    /// they will be sent as additions.
    pub added: usize,
    /// Feeds sync had recorded as present here but that are no longer in the
    /// list: they will be sent as removals.
    pub removed: usize,
    /// `true` when the list handed in was empty while sync still believed this
    /// device had feeds. An empty list is far more likely a problem reading it
    /// than the person deleting everything, so no removals were recorded.
    pub removals_skipped: bool,
}

/// A feed whose record changed since it was last sent.
#[derive(Debug, Clone)]
pub struct PendingFeedPush {
    /// The address in lower case (the record's identity).
    pub key: String,
    /// The address as written.
    pub url: String,
    /// The feed's name.
    pub name: String,
    /// The feed's topic.
    pub topic: String,
    /// `true` for "the feed exists", `false` for "the feed was removed".
    pub present: bool,
    /// The record's change time, in milliseconds since 1970.
    pub updated_at: i64,
    /// CouchDB's revision label for the document, if it was sent before.
    pub rev: Option<String>,
}

/// What applying a batch of pulled feed documents did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedApplyCounts {
    /// Addresses sync had never heard of, now recorded.
    pub added: usize,
    /// Known addresses whose record was replaced by a newer copy.
    pub updated: usize,
    /// Documents that changed nothing (already in step, or this device's copy
    /// was newer).
    pub unchanged: usize,
}

/// One row of `feed_sync`, as the rules below need it.
struct FeedRow {
    /// `true` while the feed exists (as far as sync knows).
    present: bool,
    /// The record's change time in milliseconds.
    updated_at: i64,
    /// `true` while this device's own list contains the feed.
    local_has: bool,
}

/// A change time later than `previous`, even when two changes land in the same
/// millisecond or the clock has gone backwards: it is the larger of "one more
/// than the last stamp" and "now".
fn next_stamp(previous: i64) -> i64 {
    (previous + 1).max(Utc::now().timestamp_millis())
}

impl Storage {
    /// Tells sync what this device's own feed list contains right now, and
    /// notes what changed since last time. Call it with the whole list (every
    /// syncable feed), each time, just before sending.
    ///
    /// - An address sync has not seen (or had seen removed) becomes a
    ///   present feed with a fresh change time, to be sent.
    /// - An address sync thought this device had, but that is gone from the
    ///   list, becomes a removal with a fresh change time, to be sent. If the
    ///   removal was one *received* from another device and the person
    ///   confirmed it, it was already marked done and nothing more is sent.
    /// - An address another device added, which the list now contains (the
    ///   person added it by hand, or the app just applied it), is simply
    ///   marked as present here.
    ///
    /// Addresses that are not web addresses are ignored. All of it happens in
    /// one transaction. `unchecked_transaction` starts one through a shared
    /// `&self` borrow, and dropping it without `commit()` rolls back.
    pub fn record_local_feeds(&self, current: &[FeedEntry]) -> anyhow::Result<LocalFeedChanges> {
        let tx = self.conn.unchecked_transaction()?;
        let mut changes = LocalFeedChanges::default();
        // The addresses (lower case) in the list, to spot the ones that left it.
        let mut in_list: HashSet<String> = HashSet::new();

        // Step 1: every feed in the list.
        for entry in current {
            let url = entry.url.trim();
            if !is_syncable_url(url) {
                continue;
            }
            let key = feed_key(url);
            // `insert` returns `false` when the address was already there, so
            // a repeated address is handled once.
            if !in_list.insert(key.clone()) {
                continue;
            }
            match self.feed_row(&key)? {
                // Never seen: a brand new feed on this device.
                None => {
                    self.conn.execute(
                        "INSERT INTO feed_sync
                             (key, url, name, topic, present, updated_at, local_has)
                         VALUES (?1, ?2, ?3, ?4, 1, ?5, 1)",
                        params![
                            key,
                            url,
                            entry.name.trim(),
                            entry.topic.trim(),
                            next_stamp(0)
                        ],
                    )?;
                    changes.added += 1;
                }
                // Already known to be here: nothing to do.
                Some(row) if row.local_has => {}
                // Another device's feed that this device now has too.
                Some(row) if row.present => {
                    self.conn.execute(
                        "UPDATE feed_sync SET local_has = 1, dismissed = 0 WHERE key = ?1",
                        params![key],
                    )?;
                }
                // It was removed, and is in the list again: an addition.
                Some(row) => {
                    self.conn.execute(
                        "UPDATE feed_sync
                         SET url = ?2, name = ?3, topic = ?4, present = 1,
                             updated_at = ?5, local_has = 1, dismissed = 0
                         WHERE key = ?1",
                        params![
                            key,
                            url,
                            entry.name.trim(),
                            entry.topic.trim(),
                            next_stamp(row.updated_at)
                        ],
                    )?;
                    changes.added += 1;
                }
            }
        }

        // Step 2: feeds sync thought this device had, that left the list.
        let mut stmt = self
            .conn
            .prepare("SELECT key, present, updated_at FROM feed_sync WHERE local_has = 1")?;
        let had: Vec<(String, bool, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(stmt);

        if in_list.is_empty() {
            // An empty list while feeds were expected: do not treat it as
            // "everything was removed".
            changes.removals_skipped = !had.is_empty();
        } else {
            for (key, present, updated_at) in had {
                if in_list.contains(&key) {
                    continue;
                }
                if present {
                    // The person removed it here: tell the other devices.
                    self.conn.execute(
                        "UPDATE feed_sync
                         SET present = 0, updated_at = ?2, local_has = 0, dismissed = 0
                         WHERE key = ?1",
                        params![key, next_stamp(updated_at)],
                    )?;
                    changes.removed += 1;
                } else {
                    // Already recorded as removed (it was a removal from
                    // another device that the person has now carried out by
                    // hand): nothing to send.
                    self.conn.execute(
                        "UPDATE feed_sync SET local_has = 0, dismissed = 0 WHERE key = ?1",
                        params![key],
                    )?;
                }
            }
        }

        tx.commit()?;
        Ok(changes)
    }

    /// Feeds another device added that this device does not have yet, for the
    /// app to add to its own list (and then confirm with
    /// [`Storage::feed_added_locally`]).
    pub fn pending_feed_additions(&self) -> anyhow::Result<Vec<FeedEntry>> {
        self.feed_entries("present = 1 AND local_has = 0")
    }

    /// Feeds another device removed that this device still has. The app must
    /// **ask the person** about each one: remove it and call
    /// [`Storage::feed_removed_locally`], or keep it and call
    /// [`Storage::dismiss_feed_removal`]. Nothing is removed by sync itself.
    pub fn pending_feed_removals(&self) -> anyhow::Result<Vec<FeedEntry>> {
        self.feed_entries("present = 0 AND local_has = 1 AND dismissed = 0")
    }

    /// Reads feeds matching a fixed `WHERE` condition written in this file
    /// (never text from outside), ordered by topic then name.
    fn feed_entries(&self, condition: &str) -> anyhow::Result<Vec<FeedEntry>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT name, url, topic FROM feed_sync WHERE {condition}
             ORDER BY topic COLLATE NOCASE, name COLLATE NOCASE"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok(FeedEntry {
                name: row.get(0)?,
                url: row.get(1)?,
                topic: row.get(2)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Records that the app added this feed to its own list (after
    /// [`Storage::pending_feed_additions`] named it), so it is no longer
    /// pending. Does nothing for an address that is not a present feed.
    pub fn feed_added_locally(&self, url: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feed_sync SET local_has = 1, dismissed = 0
             WHERE key = ?1 AND present = 1",
            params![feed_key(url)],
        )?;
        Ok(())
    }

    /// Records that the app removed this feed from its own list **after the
    /// person confirmed** a removal from another device, so it is no longer
    /// pending. Does nothing unless that removal really was recorded.
    pub fn feed_removed_locally(&self, url: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feed_sync SET local_has = 0, dismissed = 0
             WHERE key = ?1 AND present = 0",
            params![feed_key(url)],
        )?;
        Ok(())
    }

    /// Records that the person chose to **keep** a feed another device
    /// removed. It stops being listed. Nothing is sent: the other device's
    /// removal stands there, and this device keeps its copy. (A newer change
    /// from the other device asks again.)
    pub fn dismiss_feed_removal(&self, url: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feed_sync SET dismissed = 1
             WHERE key = ?1 AND present = 0 AND local_has = 1",
            params![feed_key(url)],
        )?;
        Ok(())
    }

    /// Every feed record whose change time is newer than what was last sent.
    pub fn pending_feed_pushes(&self) -> anyhow::Result<Vec<PendingFeedPush>> {
        let mut stmt = self.conn.prepare(
            "SELECT key, url, name, topic, present, updated_at, rev
             FROM feed_sync
             WHERE updated_at > pushed_updated_at
             ORDER BY updated_at",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(PendingFeedPush {
                key: row.get(0)?,
                url: row.get(1)?,
                name: row.get(2)?,
                topic: row.get(3)?,
                present: row.get(4)?,
                updated_at: row.get(5)?,
                rev: row.get(6)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Records that the document for `key` was written with revision `rev`,
    /// carrying the change time `updated_at`. After this the record no longer
    /// counts as pending (until it changes again).
    pub fn record_feed_pushed(&self, key: &str, rev: &str, updated_at: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feed_sync SET rev = ?2, pushed_updated_at = ?3 WHERE key = ?1",
            params![key, rev, updated_at],
        )?;
        Ok(())
    }

    /// Forgets the remembered revision for `key`, so the record counts as
    /// "never sent" and is written again. Used when its document was deleted
    /// on the server by hand.
    pub fn forget_feed_doc(&self, key: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE feed_sync SET rev = NULL, pushed_updated_at = 0 WHERE key = ?1",
            params![key],
        )?;
        Ok(())
    }

    /// How far through CouchDB's change list the feed sync has read, or `None`
    /// before its first pull.
    pub fn feed_sync_since(&self) -> anyhow::Result<Option<String>> {
        self.get_setting(FEEDS_SINCE_KEY)
    }

    /// Forgets the feed sync's server bookkeeping (the checkpoint and every
    /// remembered revision), so the next feed sync starts from the beginning
    /// and re-checks everything. This device's own list and any pending
    /// questions are **not** touched. Called by [`Storage::reset_sync_state`].
    pub(super) fn reset_feed_sync_state(&self) -> anyhow::Result<()> {
        self.conn
            .execute("UPDATE feed_sync SET rev = NULL, pushed_updated_at = 0", [])?;
        self.conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            params![FEEDS_SINCE_KEY],
        )?;
        Ok(())
    }

    /// Stores a batch of feed documents pulled from CouchDB and moves the feed
    /// checkpoint to `new_since`, **in one transaction** (all or nothing, so
    /// the checkpoint never passes changes that were not stored).
    ///
    /// For each document the rule is the same as for saved articles: a
    /// strictly newer remote copy replaces this device's record (details, and
    /// present or removed); otherwise this device's record stays and only the
    /// remembered revision is updated, so a later send can supersede the
    /// server's copy. A replaced record keeps `local_has` (what this device's
    /// list holds), and clears any earlier "keep it" answer, because there is
    /// new information to ask about. Documents must already be cleaned (see
    /// `FeedDoc::cleaned`).
    pub fn apply_remote_feed_batch(
        &self,
        docs: Vec<FeedDoc>,
        new_since: &str,
    ) -> anyhow::Result<FeedApplyCounts> {
        let tx = self.conn.unchecked_transaction()?;
        let mut counts = FeedApplyCounts::default();

        for doc in docs {
            // A pulled document always has a revision label; skip one that
            // somehow does not.
            let Some(rev) = doc.rev.clone() else {
                counts.unchanged += 1;
                continue;
            };
            let key = feed_key(&doc.url);

            match self.feed_row(&key)? {
                // An address this device never heard of: remember it. It is
                // not in this device's list yet (`local_has = 0`).
                None => {
                    self.conn.execute(
                        "INSERT INTO feed_sync
                             (key, url, name, topic, present, updated_at, local_has,
                              dismissed, rev, pushed_updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, ?7, ?6)",
                        params![
                            key,
                            doc.url,
                            doc.name,
                            doc.topic,
                            doc.present,
                            doc.updated_at,
                            rev
                        ],
                    )?;
                    counts.added += 1;
                }
                // Known: the newer copy wins.
                Some(row) if remote_wins(Some(row.updated_at), doc.updated_at) => {
                    self.conn.execute(
                        "UPDATE feed_sync
                         SET url = ?2, name = ?3, topic = ?4, present = ?5,
                             updated_at = ?6, dismissed = 0, rev = ?7,
                             pushed_updated_at = ?6
                         WHERE key = ?1",
                        params![
                            key,
                            doc.url,
                            doc.name,
                            doc.topic,
                            doc.present,
                            doc.updated_at,
                            rev
                        ],
                    )?;
                    counts.updated += 1;
                }
                // Same version on both sides: already in step.
                Some(row) if row.updated_at == doc.updated_at => {
                    self.conn.execute(
                        "UPDATE feed_sync SET rev = ?2, pushed_updated_at = ?3 WHERE key = ?1",
                        params![key, rev, doc.updated_at],
                    )?;
                    counts.unchanged += 1;
                }
                // This device's copy is newer: keep it, still to be sent, but
                // remember the server's revision so the send can supersede it.
                Some(_) => {
                    self.conn.execute(
                        "UPDATE feed_sync SET rev = ?2 WHERE key = ?1",
                        params![key, rev],
                    )?;
                    counts.unchanged += 1;
                }
            }
        }

        // Move the checkpoint inside the same transaction.
        self.set_setting(FEEDS_SINCE_KEY, new_since)?;
        tx.commit()?;
        Ok(counts)
    }

    /// The `feed_sync` row for `key`, or `None` if sync has never heard of it.
    fn feed_row(&self, key: &str) -> anyhow::Result<Option<FeedRow>> {
        self.conn
            .query_row(
                "SELECT present, updated_at, local_has FROM feed_sync WHERE key = ?1",
                params![key],
                |row| {
                    Ok(FeedRow {
                        present: row.get(0)?,
                        updated_at: row.get(1)?,
                        local_has: row.get(2)?,
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

    /// A feed entry for a test.
    fn entry(name: &str, url: &str) -> FeedEntry {
        FeedEntry {
            name: name.to_string(),
            url: url.to_string(),
            topic: "tech".to_string(),
        }
    }

    /// A pulled feed document for a test.
    fn doc(url: &str, present: bool, updated_at: i64) -> FeedDoc {
        FeedDoc {
            id: crate::sync::feeds::feed_doc_id(url),
            rev: Some("1-abc".to_string()),
            v: 1,
            url: url.to_string(),
            name: format!("Name of {url}"),
            topic: "linux".to_string(),
            source_type: "rss".to_string(),
            present,
            updated_at,
        }
    }

    /// New feeds in the list become pending sends; telling sync about the same
    /// list again changes nothing; non-web addresses are ignored.
    #[test]
    fn a_new_local_feed_is_recorded_once() {
        let s = storage();
        let list = vec![
            entry("One", "https://one.example/feed"),
            entry("Odd", "ftp://nope.example/feed"),
        ];
        let changes = s.record_local_feeds(&list).unwrap();
        assert_eq!((changes.added, changes.removed), (1, 0));
        assert_eq!(s.pending_feed_pushes().unwrap().len(), 1);

        // Same list again: nothing new.
        let changes = s.record_local_feeds(&list).unwrap();
        assert_eq!((changes.added, changes.removed), (0, 0));
    }

    /// Feeds are matched by address ignoring capitals.
    #[test]
    fn addresses_match_ignoring_capitals() {
        let s = storage();
        s.record_local_feeds(&[entry("A", "https://Example.com/Feed")])
            .unwrap();
        let changes = s
            .record_local_feeds(&[entry("A", "https://example.com/feed")])
            .unwrap();
        assert_eq!((changes.added, changes.removed), (0, 0));
    }

    /// Dropping a feed from the list records a removal to send; an empty list
    /// is distrusted and records nothing.
    #[test]
    fn a_feed_leaving_the_list_is_a_removal_but_an_empty_list_is_not() {
        let s = storage();
        s.record_local_feeds(&[entry("A", "https://a/f"), entry("B", "https://b/f")])
            .unwrap();

        // An empty list: probably a read problem, so nothing is recorded.
        let changes = s.record_local_feeds(&[]).unwrap();
        assert!(changes.removals_skipped);
        assert_eq!(changes.removed, 0);

        // Only B is left: A is a removal.
        let changes = s.record_local_feeds(&[entry("B", "https://b/f")]).unwrap();
        assert_eq!(changes.removed, 1);
        let pushes = s.pending_feed_pushes().unwrap();
        let a = pushes.iter().find(|p| p.key == "https://a/f").unwrap();
        assert!(!a.present);
    }

    /// A feed another device added is pending here until the app confirms it
    /// has added it; one the list already contains is not pending.
    #[test]
    fn remote_additions_are_pending_until_applied() {
        let s = storage();
        let counts = s
            .apply_remote_feed_batch(
                vec![
                    doc("https://new/1", true, 100),
                    doc("https://new/2", true, 100),
                ],
                "5",
            )
            .unwrap();
        assert_eq!(counts.added, 2);
        assert_eq!(s.feed_sync_since().unwrap().as_deref(), Some("5"));
        assert_eq!(s.pending_feed_additions().unwrap().len(), 2);

        // The person already had /2 by hand: not an addition any more.
        s.record_local_feeds(&[entry("Mine", "https://new/2")])
            .unwrap();
        let pending = s.pending_feed_additions().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].url, "https://new/1");

        // The app adds /1 and says so.
        s.feed_added_locally("https://new/1").unwrap();
        assert!(s.pending_feed_additions().unwrap().is_empty());
        // Nothing of this needs sending: both came from elsewhere.
        assert!(s.pending_feed_pushes().unwrap().is_empty());
    }

    /// A removal from another device is only listed; keeping the feed stops the
    /// question; confirming the removal clears it; a newer change asks again.
    #[test]
    fn a_remote_removal_is_listed_not_applied() {
        let s = storage();
        s.record_local_feeds(&[entry("A", "https://a/f"), entry("B", "https://b/f")])
            .unwrap();
        let sent = s.pending_feed_pushes().unwrap();
        for p in &sent {
            s.record_feed_pushed(&p.key, "1-x", p.updated_at).unwrap();
        }
        let stamp = sent.iter().map(|p| p.updated_at).max().unwrap();

        // Another device removed A, later.
        s.apply_remote_feed_batch(vec![doc("https://a/f", false, stamp + 10)], "2")
            .unwrap();
        let removals = s.pending_feed_removals().unwrap();
        assert_eq!(removals.len(), 1);
        assert_eq!(removals[0].url, "https://a/f");
        // The device still has it: sync removed nothing.
        let changes = s
            .record_local_feeds(&[entry("A", "https://a/f"), entry("B", "https://b/f")])
            .unwrap();
        assert_eq!((changes.added, changes.removed), (0, 0));
        assert_eq!(s.pending_feed_removals().unwrap().len(), 1);

        // The person keeps it: the question stops, and nothing is sent.
        s.dismiss_feed_removal("https://a/f").unwrap();
        assert!(s.pending_feed_removals().unwrap().is_empty());
        assert!(s.pending_feed_pushes().unwrap().is_empty());

        // A newer change from the other device asks again.
        s.apply_remote_feed_batch(vec![doc("https://a/f", false, stamp + 20)], "3")
            .unwrap();
        assert_eq!(s.pending_feed_removals().unwrap().len(), 1);

        // The person agrees; the app removes it and says so. Done for good.
        s.feed_removed_locally("https://a/f").unwrap();
        assert!(s.pending_feed_removals().unwrap().is_empty());
        let changes = s.record_local_feeds(&[entry("B", "https://b/f")]).unwrap();
        assert_eq!((changes.added, changes.removed), (0, 0));
        assert!(s.pending_feed_pushes().unwrap().is_empty());
    }

    /// Removing the feed by hand before answering the question settles it too,
    /// without sending a second removal.
    #[test]
    fn removing_by_hand_answers_a_pending_question() {
        let s = storage();
        s.record_local_feeds(&[entry("A", "https://a/f"), entry("B", "https://b/f")])
            .unwrap();
        for p in s.pending_feed_pushes().unwrap() {
            s.record_feed_pushed(&p.key, "1-x", p.updated_at).unwrap();
        }
        s.apply_remote_feed_batch(vec![doc("https://a/f", false, i64::MAX / 2)], "2")
            .unwrap();
        assert_eq!(s.pending_feed_removals().unwrap().len(), 1);

        let changes = s.record_local_feeds(&[entry("B", "https://b/f")]).unwrap();
        assert_eq!(changes.removed, 0);
        assert!(s.pending_feed_removals().unwrap().is_empty());
        assert!(s.pending_feed_pushes().unwrap().is_empty());
    }

    /// The newer copy wins; an older one only updates the remembered revision.
    #[test]
    fn newer_wins_and_older_keeps_the_local_record_pending() {
        let s = storage();
        s.record_local_feeds(&[entry("A", "https://a/f")]).unwrap();
        let mine = s.pending_feed_pushes().unwrap()[0].updated_at;

        // Older remote copy (a removal): ignored, still pending to send.
        let counts = s
            .apply_remote_feed_batch(vec![doc("https://a/f", false, mine - 1)], "1")
            .unwrap();
        assert_eq!(counts.unchanged, 1);
        let pending = s.pending_feed_pushes().unwrap();
        assert_eq!(pending.len(), 1);
        assert!(pending[0].present);
        assert_eq!(pending[0].rev.as_deref(), Some("1-abc"));

        // Newer remote copy: it wins.
        let counts = s
            .apply_remote_feed_batch(vec![doc("https://a/f", false, mine + 5)], "2")
            .unwrap();
        assert_eq!(counts.updated, 1);
        assert!(s.pending_feed_pushes().unwrap().is_empty());
        assert_eq!(s.pending_feed_removals().unwrap().len(), 1);
    }

    /// If storing a batch fails part-way, the checkpoint does not move.
    #[test]
    fn the_feed_checkpoint_only_moves_with_its_data() {
        let s = storage();
        s.conn.execute("DROP TABLE feed_sync", []).unwrap();
        let result = s.apply_remote_feed_batch(vec![doc("https://x/f", true, 5)], "9-z");
        assert!(result.is_err());
        assert_eq!(s.feed_sync_since().unwrap(), None);
    }

    /// Resetting forgets the server bookkeeping but not what this device has.
    #[test]
    fn resetting_keeps_the_local_state() {
        let s = storage();
        s.record_local_feeds(&[entry("A", "https://a/f")]).unwrap();
        for p in s.pending_feed_pushes().unwrap() {
            s.record_feed_pushed(&p.key, "1-x", p.updated_at).unwrap();
        }
        s.set_setting(FEEDS_SINCE_KEY, "7").unwrap();
        assert!(s.pending_feed_pushes().unwrap().is_empty());

        s.reset_sync_state().unwrap();
        assert_eq!(s.feed_sync_since().unwrap(), None);
        // Needs sending again, but the feed is still known to be here.
        assert_eq!(s.pending_feed_pushes().unwrap().len(), 1);
        let changes = s.record_local_feeds(&[entry("A", "https://a/f")]).unwrap();
        assert_eq!((changes.added, changes.removed), (0, 0));
    }

    /// Migration 009 on a database that already has saved articles and sync
    /// bookkeeping from migration 008 only adds the new table: nothing existing
    /// changes.
    #[test]
    fn migration_009_only_adds_a_table() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        super::super::migrations::run_up_to(&conn, 8).unwrap();
        conn.execute(
            "INSERT INTO articles (title, url, source, topic, timestamp, read, skipped, saved)
             VALUES ('t', 'https://old/1', 's', 'c', '2026-01-01T00:00:00Z', 1, 0, 1)",
            [],
        )
        .unwrap();
        let before: Option<i64> = conn
            .query_row("SELECT sync_updated_at FROM articles", [], |r| r.get(0))
            .unwrap();

        super::super::migrations::run(&conn).unwrap();

        let after: Option<i64> = conn
            .query_row("SELECT sync_updated_at FROM articles", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after, "existing rows are untouched");
        let feeds: i64 = conn
            .query_row("SELECT COUNT(*) FROM feed_sync", [], |r| r.get(0))
            .unwrap();
        assert_eq!(feeds, 0, "the new table starts empty");
    }

    /// A feed that was removed and is then added again counts as an addition.
    #[test]
    fn adding_a_removed_feed_again_is_an_addition() {
        let s = storage();
        s.record_local_feeds(&[entry("A", "https://a/f"), entry("B", "https://b/f")])
            .unwrap();
        s.record_local_feeds(&[entry("B", "https://b/f")]).unwrap();
        let changes = s
            .record_local_feeds(&[entry("A", "https://a/f"), entry("B", "https://b/f")])
            .unwrap();
        assert_eq!(changes.added, 1);
        let pushes = s.pending_feed_pushes().unwrap();
        assert!(pushes.iter().all(|p| p.present));
    }
}
