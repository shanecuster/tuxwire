//! The database half of the optional CouchDB sync.
//!
//! These are plain `Storage` methods (this file just keeps them out of the
//! already-long `storage/mod.rs`). They know nothing about the network: the
//! `sync` module fetches and sends documents, and calls these to decide what
//! needs sending and to store what arrived.
//!
//! What is remembered, and where:
//! - `articles.sync_updated_at`: when an article's saved/note state last
//!   changed (milliseconds). `NULL` means "never saved, nothing to sync".
//! - `sync_docs`: per article, the CouchDB revision last seen and the
//!   `sync_updated_at` value that was last pushed or pulled.
//! - the `settings` key `sync.since`: how far through CouchDB's change list we
//!   have read, so the next pull starts there.

#![warn(clippy::missing_docs_in_private_items)]

#[cfg(test)]
use super::migrations;
use super::{Storage, row_to_article};
use crate::models::Article;
#[cfg(test)]
use crate::sync::doc::doc_id_for;
use crate::sync::doc::{ArticleDoc, remote_wins};
use rusqlite::{OptionalExtension, params};

/// The `settings` key holding the pull checkpoint (CouchDB's `last_seq`).
const SINCE_KEY: &str = "sync.since";

/// An article whose saved/note state has changed since it was last sent.
#[derive(Debug, Clone)]
pub struct PendingPush {
    /// The article as it is now.
    pub article: Article,
    /// Its change stamp, in milliseconds (`articles.sync_updated_at`).
    pub updated_at: i64,
    /// CouchDB's revision label for its document, if we have synced it before.
    pub rev: Option<String>,
}

/// What applying a batch of pulled documents did, for the sync report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyCounts {
    /// Articles this device did not have, created from a pulled document.
    pub added: usize,
    /// Existing articles whose saved/note state was replaced by a newer copy.
    pub updated: usize,
    /// Documents that changed nothing (already in step, or local was newer).
    pub unchanged: usize,
}

impl Storage {
    /// Every article whose saved/note state needs sending: it has a change
    /// stamp, and either was never sent or was changed since.
    ///
    /// The `LEFT JOIN` pairs each article with its `sync_docs` row if it has
    /// one (`d` is all `NULL` when it does not). `'article:' || a.url` builds
    /// the same document name [`doc_id_for`] does, inside SQL (`||` joins
    /// text).
    pub fn pending_pushes(&self) -> anyhow::Result<Vec<PendingPush>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.title, a.url, a.source, a.topic, a.timestamp, a.read, a.skipped,
                    a.saved, a.note, a.saved_at, a.noted_at, a.feed_url, a.summary,
                    a.sync_updated_at, d.rev
             FROM articles a
             LEFT JOIN sync_docs d ON d.doc_id = 'article:' || a.url
             WHERE a.sync_updated_at IS NOT NULL
               AND (d.doc_id IS NULL OR d.pushed_updated_at < a.sync_updated_at)
             ORDER BY a.sync_updated_at",
        )?;
        // `row_to_article` reads the first 14 columns; columns 14 and 15 are
        // ours, read by position here.
        let rows = stmt.query_map([], |row| {
            Ok(PendingPush {
                article: row_to_article(row)?,
                updated_at: row.get(14)?,
                rev: row.get(15)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Records that `doc_id` was written to CouchDB with revision `rev`,
    /// carrying the change stamp `updated_at`. After this the article no
    /// longer counts as pending (until it changes again).
    pub fn record_pushed(&self, doc_id: &str, rev: &str, updated_at: i64) -> anyhow::Result<()> {
        upsert_sync_doc(self, doc_id, rev, updated_at)
    }

    /// How far through CouchDB's change list we have read, or `None` before
    /// the first pull (meaning "start from the beginning").
    pub fn sync_since(&self) -> anyhow::Result<Option<String>> {
        self.get_setting(SINCE_KEY)
    }

    /// Forgets all sync bookkeeping: the pull checkpoint and every remembered
    /// revision. Saved articles and notes are **not** touched. The next sync
    /// starts from the beginning and re-checks everything. Used when the
    /// server address or database name changes, or to repair confusion.
    pub fn reset_sync_state(&self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM sync_docs", [])?;
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", params![SINCE_KEY])?;
        Ok(())
    }

    /// Stores a batch of documents pulled from CouchDB and moves the pull
    /// checkpoint to `new_since`, **in one transaction**: either all of it
    /// happens or none of it does, so a crash can never leave the checkpoint
    /// past changes that were not stored.
    ///
    /// For each document the rule is [`remote_wins`]: a strictly newer remote
    /// copy replaces this device's saved/note state (creating the article if
    /// needed); otherwise the local state stays, and only the remembered
    /// revision is updated, so a later push carries the right revision.
    /// Documents are normalized first, so the note lock holds.
    ///
    /// `unchecked_transaction` starts a transaction through a shared `&self`
    /// borrow. Dropping it without `commit()` rolls everything back.
    pub fn apply_remote_batch(
        &self,
        docs: Vec<ArticleDoc>,
        new_since: &str,
    ) -> anyhow::Result<ApplyCounts> {
        let tx = self.conn.unchecked_transaction()?;
        let mut counts = ApplyCounts::default();

        for doc in docs {
            // A pulled document always has a revision label; skip one that
            // somehow does not, rather than store something we cannot update.
            let Some(rev) = doc.rev.clone() else {
                counts.unchanged += 1;
                continue;
            };
            let doc = doc.normalized();

            // Step 1: find this device's copy of the article, if any.
            let local: Option<(i64, Option<i64>)> = self
                .conn
                .query_row(
                    "SELECT id, sync_updated_at FROM articles WHERE url = ?1",
                    params![doc.url],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let local_stamp = local.and_then(|(_, stamp)| stamp);

            // Step 2: apply the newer-wins rule.
            let pushed_value = if remote_wins(local_stamp, doc.updated_at) {
                match local {
                    Some((id, _)) => {
                        self.apply_to_existing(id, &doc)?;
                        counts.updated += 1;
                    }
                    // An un-saved document for an article we never had has
                    // nothing to create.
                    None if !doc.saved => counts.unchanged += 1,
                    None => {
                        self.create_from_doc(&doc)?;
                        counts.added += 1;
                    }
                }
                doc.updated_at
            } else {
                counts.unchanged += 1;
                if local_stamp == Some(doc.updated_at) {
                    // Same version on both sides: nothing left to push.
                    doc.updated_at
                } else {
                    // Local is newer: keep it and keep it pending, but
                    // remember the server's revision so the push can supersede it.
                    self.pushed_value(&doc.id)?.unwrap_or(0)
                }
            };

            // Step 3: remember the revision (and what is already in step).
            upsert_sync_doc(self, &doc.id, &rev, pushed_value)?;
        }

        // Step 4: move the checkpoint, inside the same transaction.
        self.set_setting(SINCE_KEY, new_since)?;
        tx.commit()?;
        Ok(counts)
    }

    /// The `pushed_updated_at` last remembered for `doc_id`, if any.
    fn pushed_value(&self, doc_id: &str) -> anyhow::Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT pushed_updated_at FROM sync_docs WHERE doc_id = ?1",
                params![doc_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Replaces the saved/note state of the existing article `id` with the
    /// (normalized) document's. A saved result also marks the article read,
    /// like saving anywhere else does.
    fn apply_to_existing(&self, id: i64, doc: &ArticleDoc) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE articles
             SET saved = ?2,
                 read = CASE WHEN ?2 = 1 THEN 1 ELSE read END,
                 saved_at = ?3,
                 note = ?4,
                 noted_at = ?5,
                 sync_updated_at = ?6
             WHERE id = ?1",
            params![
                id,
                doc.saved,
                doc.saved_at.and_then(crate::sync::doc::ms_to_utc),
                doc.note,
                doc.noted_at.and_then(crate::sync::doc::ms_to_utc),
                doc.updated_at,
            ],
        )?;
        Ok(())
    }

    /// Creates an article this device did not have, from a pulled document
    /// (always a saved one). It has no feed on this device, so `feed_url` is
    /// empty; it shows in the Saved list under its topic.
    fn create_from_doc(&self, doc: &ArticleDoc) -> anyhow::Result<()> {
        // Build the article, then store it with the normal insert so it gets
        // the same treatment as any other article.
        let mut article = Article::new(
            doc.title.clone(),
            doc.url.clone(),
            doc.source.clone(),
            doc.topic.clone(),
            doc.published_at(),
        );
        article.summary = doc.summary.clone();
        let id = self.insert_article(&article)?;
        self.apply_to_existing(id, doc)
    }
}

/// Inserts or updates the `sync_docs` row for `doc_id`. A free function
/// (not a method) because both [`Storage::record_pushed`] and the batch apply
/// use it. `ON CONFLICT ... DO UPDATE` is SQLite's insert-or-update.
fn upsert_sync_doc(
    storage: &Storage,
    doc_id: &str,
    rev: &str,
    pushed_updated_at: i64,
) -> anyhow::Result<()> {
    storage.conn.execute(
        "INSERT INTO sync_docs (doc_id, rev, pushed_updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(doc_id) DO UPDATE SET
             rev = excluded.rev, pushed_updated_at = excluded.pushed_updated_at",
        params![doc_id, rev, pushed_updated_at],
    )?;
    Ok(())
}

/// The document name for an article, re-exported here so tests in this file
/// read naturally.
#[cfg(test)]
fn id_of(article: &Article) -> String {
    doc_id_for(&article.url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    /// A fresh in-memory database for a test.
    fn storage() -> Storage {
        Storage::open_in_memory().unwrap()
    }

    /// Stores a plain article at `url` and returns its id.
    fn add(storage: &Storage, url: &str) -> i64 {
        let article = Article::new(
            format!("Title of {url}"),
            url.to_string(),
            "Source".to_string(),
            "tech".to_string(),
            Utc::now(),
        );
        storage.insert_article(&article).unwrap()
    }

    /// The change stamp of article `id`, read straight from the table.
    fn stamp(storage: &Storage, id: i64) -> Option<i64> {
        storage
            .conn
            .query_row(
                "SELECT sync_updated_at FROM articles WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// A pulled document for `url`, saved with an optional note.
    fn remote_doc(url: &str, saved: bool, note: Option<&str>, updated_at: i64) -> ArticleDoc {
        ArticleDoc {
            id: doc_id_for(url),
            rev: Some("1-abc".to_string()),
            v: 1,
            url: url.to_string(),
            title: "From elsewhere".to_string(),
            source: "Other feed".to_string(),
            topic: "linux".to_string(),
            published: 1_700_000_000_000,
            summary: None,
            saved,
            saved_at: saved.then_some(1_700_000_100_000),
            note: note.map(str::to_string),
            noted_at: note.map(|_| 1_700_000_200_000),
            updated_at,
        }
    }

    /// Articles never saved have no stamp; saving, noting and un-saving each
    /// move it forward; doing nothing (saving twice) does not.
    #[test]
    fn local_changes_stamp_the_article_and_no_ops_do_not() {
        let s = storage();
        let id = add(&s, "https://a/1");
        assert_eq!(stamp(&s, id), None, "never saved: nothing to sync");

        s.save_article(id, None).unwrap();
        let saved = stamp(&s, id).expect("saving stamps");

        // Saving again with the same (empty) note changes nothing.
        s.save_article(id, None).unwrap();
        assert_eq!(stamp(&s, id), Some(saved));

        s.update_note(id, Some("hello".into())).unwrap();
        let noted = stamp(&s, id).unwrap();
        assert!(noted > saved, "a new note is a change");

        s.unsave_article(id).unwrap();
        assert!(stamp(&s, id).unwrap() > noted, "un-saving is a change too");

        // Clearing a note on an article that is not saved changes nothing.
        let after = stamp(&s, id);
        s.update_note(id, None).unwrap();
        assert_eq!(stamp(&s, id), after);
    }

    /// Stamps always move forward, even for changes within the same millisecond.
    #[test]
    fn stamps_strictly_increase() {
        let s = storage();
        let id = add(&s, "https://a/1");
        s.save_article(id, None).unwrap();
        let mut last = stamp(&s, id).unwrap();
        for text in ["one", "two", "three", "four"] {
            s.update_note(id, Some(text.into())).unwrap();
            let now = stamp(&s, id).unwrap();
            assert!(now > last);
            last = now;
        }
    }

    /// Only changed articles are pending; recording a push clears it until
    /// the next change.
    #[test]
    fn pending_pushes_lists_changes_until_they_are_recorded() {
        let s = storage();
        let a = add(&s, "https://a/1");
        let _unsaved = add(&s, "https://a/2");
        assert!(s.pending_pushes().unwrap().is_empty());

        s.save_article(a, None).unwrap();
        let pending = s.pending_pushes().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].article.url, "https://a/1");
        assert_eq!(pending[0].rev, None);

        s.record_pushed(&id_of(&pending[0].article), "1-x", pending[0].updated_at)
            .unwrap();
        assert!(s.pending_pushes().unwrap().is_empty());

        s.update_note(a, Some("n".into())).unwrap();
        let pending = s.pending_pushes().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].rev.as_deref(),
            Some("1-x"),
            "update carries the revision"
        );
    }

    /// A pulled saved document for an article we never had creates it, saved,
    /// with its note, and moves the checkpoint.
    #[test]
    fn a_pulled_document_creates_a_missing_article_and_moves_the_checkpoint() {
        let s = storage();
        assert_eq!(s.sync_since().unwrap(), None);

        let counts = s
            .apply_remote_batch(
                vec![remote_doc("https://new/1", true, Some("hi"), 500)],
                "7-xyz",
            )
            .unwrap();
        assert_eq!(counts.added, 1);
        assert_eq!(s.sync_since().unwrap().as_deref(), Some("7-xyz"));

        let saved = s.saved_articles().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].note.as_deref(), Some("hi"));
        assert_eq!(saved[0].title, "From elsewhere");
        assert!(
            s.pending_pushes().unwrap().is_empty(),
            "pulled, so nothing to push"
        );
    }

    /// A newer remote copy replaces local state, an older one does not (but
    /// its revision is remembered), and an un-save arriving later removes the
    /// save and the note together.
    #[test]
    fn newer_wins_older_loses_and_unsave_propagates() {
        let s = storage();
        let id = add(&s, "https://a/1");
        s.save_article(id, None).unwrap();
        s.update_note(id, Some("mine".into())).unwrap();
        let mine = stamp(&s, id).unwrap();

        // Older remote copy: ignored, but the push will carry its revision.
        let counts = s
            .apply_remote_batch(
                vec![remote_doc("https://a/1", true, Some("theirs"), mine - 1)],
                "1",
            )
            .unwrap();
        assert_eq!(counts.unchanged, 1);
        assert_eq!(s.saved_articles().unwrap()[0].note.as_deref(), Some("mine"));
        let pending = s.pending_pushes().unwrap();
        assert_eq!(pending.len(), 1, "local is newer, still to push");
        assert_eq!(pending[0].rev.as_deref(), Some("1-abc"));

        // Newer remote copy replaces the note.
        let counts = s
            .apply_remote_batch(
                vec![remote_doc("https://a/1", true, Some("theirs"), mine + 10)],
                "2",
            )
            .unwrap();
        assert_eq!(counts.updated, 1);
        assert_eq!(
            s.saved_articles().unwrap()[0].note.as_deref(),
            Some("theirs")
        );
        assert!(s.pending_pushes().unwrap().is_empty());

        // A still newer un-save removes the save and the note.
        s.apply_remote_batch(vec![remote_doc("https://a/1", false, None, mine + 20)], "3")
            .unwrap();
        assert!(s.saved_articles().unwrap().is_empty());
        assert_eq!(s.article_by_id(id).unwrap().unwrap().note, None);
    }

    /// A document claiming "not saved" but carrying a note is stored as saved:
    /// the note lock holds for anything that arrives.
    #[test]
    fn the_note_lock_holds_for_pulled_documents() {
        let s = storage();
        s.apply_remote_batch(
            vec![remote_doc("https://n/1", false, Some("keep me"), 5)],
            "1",
        )
        .unwrap();
        let saved = s.saved_articles().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].note.as_deref(), Some("keep me"));
    }

    /// If storing a batch fails part-way, the checkpoint does not move.
    #[test]
    fn the_checkpoint_only_moves_with_its_data() {
        let s = storage();
        // Make the batch fail: an article row cannot be created because the
        // table is gone. (The checkpoint write must roll back with it.)
        s.conn.execute("DROP TABLE sync_docs", []).unwrap();
        let result = s.apply_remote_batch(vec![remote_doc("https://x/1", true, None, 5)], "9-z");
        assert!(result.is_err());
        assert_eq!(s.sync_since().unwrap(), None, "checkpoint did not move");
        assert!(
            s.saved_articles().unwrap().is_empty(),
            "article was rolled back"
        );
    }

    /// Resetting forgets the bookkeeping but keeps saved articles and notes.
    #[test]
    fn resetting_sync_state_keeps_the_saved_articles() {
        let s = storage();
        s.apply_remote_batch(vec![remote_doc("https://r/1", true, Some("n"), 5)], "4")
            .unwrap();
        s.reset_sync_state().unwrap();
        assert_eq!(s.sync_since().unwrap(), None);
        let saved = s.saved_articles().unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].note.as_deref(), Some("n"));
        // With the revision forgotten, the article counts as needing a push.
        assert_eq!(s.pending_pushes().unwrap().len(), 1);
    }

    /// A database that already had saved articles before this migration gets
    /// them stamped, so the first sync pushes them.
    #[test]
    fn already_saved_articles_are_stamped_by_the_migration() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Bring the schema to just before migration 8, add a saved article,
        // then run the remaining migration.
        conn.execute_batch("PRAGMA user_version = 0").unwrap();
        migrations::run_up_to(&conn, 7).unwrap();
        conn.execute(
            "INSERT INTO articles (title, url, source, topic, timestamp, read, skipped, saved)
             VALUES ('t', 'https://old/1', 's', 'c', '2026-01-01T00:00:00Z', 1, 0, 1)",
            [],
        )
        .unwrap();
        migrations::run(&conn).unwrap();
        let stamp: Option<i64> = conn
            .query_row("SELECT sync_updated_at FROM articles", [], |r| r.get(0))
            .unwrap();
        assert!(stamp.is_some_and(|s| s > 0));
    }
}
