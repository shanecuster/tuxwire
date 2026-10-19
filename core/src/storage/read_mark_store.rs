//! Read marks, and the database half of syncing them.
//!
//! A **read mark** says "this feed article was opened". Opening is recorded on
//! the article itself (`articles.read` and `articles.read_at`, set by
//! [`Storage::mark_read`] and friends); this file is about passing that on to
//! the person's other devices, when they have switched that on.
//!
//! These are plain `Storage` methods that know nothing about the network. The
//! `sync::read_marks` module talks to CouchDB and calls the methods here to
//! decide what to send and to store what arrived.
//!
//! **Two tables, two jobs.** `articles` says what this device has read. The
//! `read_marks` table (migration 011) lists the articles whose mark the *server*
//! knows about, whoever sent it, together with CouchDB's revision label. A read
//! article with no row there still has to be sent; a row with no article yet is
//! a mark that arrived early, applied the moment the article is downloaded.
//!
//! **The rules, in short.**
//! - A mark is identified by the article's address, exactly as stored.
//! - Marks only go one way. An article can be opened, never un-opened, so
//!   there is no "newer copy wins": a document that exists means "opened".
//! - Only reads from the last 90 days are sent (see
//!   [`crate::sync::read_marks::MAX_AGE_DAYS`]); the caller passes the cut-off.

#![warn(clippy::missing_docs_in_private_items)]

use super::Storage;
use crate::sync::read_marks::{ReadMarkDoc, read_key};
use rusqlite::{OptionalExtension, params};

/// The `settings` key holding the read-marks pull checkpoint (CouchDB's
/// `last_seq`). Separate from the other checkpoints, so a device updated later
/// still reads the read-mark documents that were written before it was.
const READ_SINCE_KEY: &str = "sync.read.since";

/// An opened article whose mark has not been sent yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingReadMark {
    /// The article's address, as stored.
    pub url: String,
    /// When it was opened, in milliseconds since 1970 (UTC).
    pub read_at: i64,
}

/// What applying a batch of pulled read-mark documents did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadMarkApplyCounts {
    /// Marks this device had not heard of before.
    pub received: usize,
    /// Articles on this device that were unread and are now marked read by a
    /// mark that arrived. (Smaller than `received` when the article was already
    /// read here, or has not been downloaded yet.)
    pub marked: usize,
}

impl Storage {
    /// The articles opened at or after `not_before_ms` (milliseconds since
    /// 1970) whose mark the server does not know about yet, oldest first.
    ///
    /// Unread articles, articles whose opening time is unknown (`read_at` is
    /// empty), and articles older than the cut-off are never listed. An article
    /// stays listed until [`Storage::record_read_mark_pushed`] is called for it.
    pub fn pending_read_marks(&self, not_before_ms: i64) -> anyhow::Result<Vec<PendingReadMark>> {
        // `NOT EXISTS (...)` keeps only articles with no row in `read_marks`,
        // meaning the server has no document for them yet.
        let mut stmt = self.conn.prepare(
            "SELECT a.url, a.read_at
             FROM articles a
             WHERE a.read = 1
               AND a.read_at IS NOT NULL
               AND a.read_at >= ?1
               AND NOT EXISTS (SELECT 1 FROM read_marks m WHERE m.key = a.url)
             ORDER BY a.read_at",
        )?;
        let rows = stmt.query_map(params![not_before_ms], |row| {
            Ok(PendingReadMark {
                url: row.get(0)?,
                read_at: row.get(1)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    /// Records that the server now holds the mark for `url`, with revision
    /// `rev`. After this the article no longer counts as pending.
    pub fn record_read_mark_pushed(
        &self,
        url: &str,
        read_at: i64,
        rev: &str,
    ) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO read_marks (key, read_at, rev) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET rev = excluded.rev",
            params![read_key(url), read_at, rev],
        )?;
        Ok(())
    }

    /// Forgets that the server holds the mark for `key`, so an opened article is
    /// sent again. Used when its document was deleted on the server by hand.
    pub fn forget_read_mark_doc(&self, key: &str) -> anyhow::Result<()> {
        self.conn
            .execute("DELETE FROM read_marks WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// How far through CouchDB's change list the read-marks sync has read, or
    /// `None` before its first pull.
    pub fn read_marks_sync_since(&self) -> anyhow::Result<Option<String>> {
        self.get_setting(READ_SINCE_KEY)
    }

    /// Forgets the read-marks sync's server bookkeeping (the checkpoint and the
    /// list of marks the server holds), so the next sync starts from the
    /// beginning and re-checks everything. Which articles are read is **not**
    /// touched. Called by [`Storage::reset_sync_state`].
    pub(super) fn reset_read_marks_sync_state(&self) -> anyhow::Result<()> {
        self.conn.execute("DELETE FROM read_marks", [])?;
        self.conn.execute(
            "DELETE FROM settings WHERE key = ?1",
            params![READ_SINCE_KEY],
        )?;
        Ok(())
    }

    /// Stores a batch of read-mark documents pulled from CouchDB and moves the
    /// checkpoint to `new_since`, **in one transaction** (all or nothing, so the
    /// checkpoint never passes marks that were not stored).
    ///
    /// For each document the mark is remembered, and the article is marked read
    /// if this device has it and it was unread. If the article is not here yet,
    /// the remembered mark is applied when it is downloaded (see
    /// `apply_waiting_read_mark`). Documents must already be cleaned (see
    /// `ReadMarkDoc::cleaned`).
    pub fn apply_remote_read_marks(
        &self,
        docs: Vec<ReadMarkDoc>,
        new_since: &str,
    ) -> anyhow::Result<ReadMarkApplyCounts> {
        let tx = self.conn.unchecked_transaction()?;
        let mut counts = ReadMarkApplyCounts::default();

        for doc in docs {
            // A pulled document always has a revision label; skip one that
            // somehow does not.
            let Some(rev) = doc.rev.clone() else { continue };
            let key = read_key(&doc.url);

            // Step 1: is this mark new to this device?
            let known: Option<i64> = self
                .conn
                .query_row(
                    "SELECT read_at FROM read_marks WHERE key = ?1",
                    params![key],
                    |row| row.get(0),
                )
                .optional()?;

            // Step 2: remember it. A mark already known keeps the earlier
            // time and takes the new revision label.
            self.conn.execute(
                "INSERT INTO read_marks (key, read_at, rev) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET
                     rev = excluded.rev,
                     read_at = MIN(read_marks.read_at, excluded.read_at)",
                params![key, doc.read_at, rev],
            )?;
            if known.is_none() {
                counts.received += 1;
            }

            // Step 3: mark the article read if it is here and unread. Its
            // opening time is when it was opened on the other device.
            let changed = self.conn.execute(
                "UPDATE articles
                 SET read = 1, read_at = COALESCE(read_at, ?2)
                 WHERE url = ?1 AND read = 0",
                params![key, doc.read_at],
            )?;
            counts.marked += changed;
        }

        // Move the checkpoint inside the same transaction.
        self.set_setting(READ_SINCE_KEY, new_since)?;
        tx.commit()?;
        Ok(counts)
    }

    /// Marks the article at `url` read if a mark for it is waiting (it arrived
    /// from another device before the article was downloaded here). Does nothing
    /// otherwise. Called by [`Storage::insert_article`] each time an article is
    /// stored, which is cheap: one lookup by primary key.
    pub(super) fn apply_waiting_read_mark(&self, url: &str) -> anyhow::Result<()> {
        // `EXISTS (...)` makes the update a no-op unless a mark is waiting;
        // the sub-select reads the time it was opened.
        self.conn.execute(
            "UPDATE articles
             SET read = 1,
                 read_at = COALESCE(read_at,
                     (SELECT read_at FROM read_marks WHERE key = ?1))
             WHERE url = ?1 AND read = 0
               AND EXISTS (SELECT 1 FROM read_marks WHERE key = ?1)",
            params![url],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Article;
    use chrono::Utc;

    /// Stores an unread article with the given address and returns its id.
    fn add(storage: &Storage, url: &str) -> i64 {
        let article = Article::new(
            format!("Title of {url}"),
            url.to_string(),
            "Feed".to_string(),
            "tech".to_string(),
            Utc::now(),
        );
        storage.insert_article(&article).unwrap()
    }

    /// The `read` and `read_at` columns of the article at `url`.
    fn state(storage: &Storage, url: &str) -> (bool, Option<i64>) {
        storage
            .conn
            .query_row(
                "SELECT read, read_at FROM articles WHERE url = ?1",
                params![url],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap()
    }

    /// A document as `pull` would hand it over.
    fn remote(url: &str, read_at: i64, rev: &str) -> ReadMarkDoc {
        ReadMarkDoc {
            rev: Some(rev.to_string()),
            ..ReadMarkDoc::new(url, read_at)
        }
    }

    /// Opening stamps the time once; opening again keeps the first time.
    #[test]
    fn opening_an_article_remembers_when_the_first_time_only() {
        let storage = Storage::open_in_memory().unwrap();
        let id = add(&storage, "https://a.example/1");
        assert_eq!(state(&storage, "https://a.example/1"), (false, None));

        storage.mark_read(id).unwrap();
        let (read, first) = state(&storage, "https://a.example/1");
        assert!(read);
        let first = first.expect("a time was recorded");
        assert!((Utc::now().timestamp_millis() - first).abs() < 60_000);

        std::thread::sleep(std::time::Duration::from_millis(5));
        storage.mark_read(id).unwrap();
        assert_eq!(state(&storage, "https://a.example/1").1, Some(first));
    }

    /// Saving an unread article opens it, and stamps the time as well.
    #[test]
    fn saving_also_stamps_the_time() {
        let storage = Storage::open_in_memory().unwrap();
        let id = add(&storage, "https://a.example/2");
        storage.save_article(id, None).unwrap();
        let (read, at) = state(&storage, "https://a.example/2");
        assert!(read && at.is_some());
    }

    /// Only recent, opened, not-yet-sent articles are pending.
    #[test]
    fn only_recent_opened_unsent_articles_are_pending() {
        let storage = Storage::open_in_memory().unwrap();
        let (unread, fresh, old, sent) = (
            "https://a.example/unread",
            "https://a.example/fresh",
            "https://a.example/old",
            "https://a.example/sent",
        );
        for url in [unread, fresh, old, sent] {
            add(&storage, url);
        }
        // Set the times by hand so the test does not depend on the clock.
        for (url, at) in [(fresh, 5_000), (old, 1_000), (sent, 6_000)] {
            storage
                .conn
                .execute(
                    "UPDATE articles SET read = 1, read_at = ?2 WHERE url = ?1",
                    params![url, at],
                )
                .unwrap();
        }
        storage.record_read_mark_pushed(sent, 6_000, "1-a").unwrap();

        let pending = storage.pending_read_marks(2_000).unwrap();
        assert_eq!(
            pending,
            vec![PendingReadMark {
                url: fresh.to_string(),
                read_at: 5_000
            }]
        );

        // Once recorded it is no longer pending; forgetting brings it back.
        storage
            .record_read_mark_pushed(fresh, 5_000, "1-b")
            .unwrap();
        assert!(storage.pending_read_marks(2_000).unwrap().is_empty());
        storage.forget_read_mark_doc(fresh).unwrap();
        assert_eq!(storage.pending_read_marks(2_000).unwrap().len(), 1);
    }

    /// A mark that arrives marks the article read here, keeps its original
    /// time, and is not sent back.
    #[test]
    fn an_arriving_mark_opens_the_article_here() {
        let storage = Storage::open_in_memory().unwrap();
        add(&storage, "https://a.example/1");
        add(&storage, "https://a.example/already");
        let already = storage
            .conn
            .execute(
                "UPDATE articles SET read = 1, read_at = 9 WHERE url = 'https://a.example/already'",
                [],
            )
            .unwrap();
        assert_eq!(already, 1);

        let counts = storage
            .apply_remote_read_marks(
                vec![
                    remote("https://a.example/1", 1_234, "1-a"),
                    remote("https://a.example/already", 7_777, "1-b"),
                ],
                "42",
            )
            .unwrap();
        assert_eq!(
            counts,
            ReadMarkApplyCounts {
                received: 2,
                marked: 1
            }
        );
        assert_eq!(state(&storage, "https://a.example/1"), (true, Some(1_234)));
        // An article already read here keeps the time it had.
        assert_eq!(
            state(&storage, "https://a.example/already"),
            (true, Some(9))
        );
        assert!(storage.pending_read_marks(0).unwrap().is_empty());
        assert_eq!(
            storage.read_marks_sync_since().unwrap().as_deref(),
            Some("42")
        );

        // The same mark again changes nothing and is not counted twice.
        let again = storage
            .apply_remote_read_marks(vec![remote("https://a.example/1", 1_234, "2-a")], "43")
            .unwrap();
        assert_eq!(
            again,
            ReadMarkApplyCounts {
                received: 0,
                marked: 0
            }
        );
    }

    /// A mark for an article that is not here yet waits, and opens the article
    /// when it is downloaded.
    #[test]
    fn a_mark_for_a_missing_article_waits_for_it() {
        let storage = Storage::open_in_memory().unwrap();
        let counts = storage
            .apply_remote_read_marks(vec![remote("https://later.example/x", 3_000, "1-a")], "1")
            .unwrap();
        assert_eq!(
            counts,
            ReadMarkApplyCounts {
                received: 1,
                marked: 0
            }
        );

        add(&storage, "https://later.example/x");
        assert_eq!(
            state(&storage, "https://later.example/x"),
            (true, Some(3_000))
        );
        // And a second fetch of the same article does not disturb it.
        add(&storage, "https://later.example/x");
        assert_eq!(
            state(&storage, "https://later.example/x"),
            (true, Some(3_000))
        );
    }

    /// An article with no mark waiting stays unread when it is stored.
    #[test]
    fn articles_without_a_mark_stay_unread() {
        let storage = Storage::open_in_memory().unwrap();
        add(&storage, "https://a.example/none");
        assert_eq!(state(&storage, "https://a.example/none"), (false, None));
    }

    /// Resetting forgets the bookkeeping but not what is read.
    #[test]
    fn reset_forgets_the_bookkeeping_but_not_the_reading() {
        let storage = Storage::open_in_memory().unwrap();
        add(&storage, "https://a.example/1");
        storage
            .apply_remote_read_marks(vec![remote("https://a.example/1", 50, "1-a")], "9")
            .unwrap();
        storage.reset_sync_state().unwrap();
        assert_eq!(storage.read_marks_sync_since().unwrap(), None);
        let (read, _) = state(&storage, "https://a.example/1");
        assert!(read, "still read");
        // With the server's list forgotten, it counts as unsent again (its time
        // is far in the past here, so ask from the beginning).
        assert_eq!(storage.pending_read_marks(0).unwrap().len(), 1);
    }

    /// The upgrade guess for articles read before migration 011 turns the
    /// stored publish date into milliseconds since 1970.
    #[test]
    fn the_migration_guess_reads_stored_dates() {
        let storage = Storage::open_in_memory().unwrap();
        add(&storage, "https://a.example/guess");
        let guessed: Option<i64> = storage
            .conn
            .query_row(
                "SELECT CAST(strftime('%s', timestamp) AS INTEGER) * 1000
                 FROM articles WHERE url = 'https://a.example/guess'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let guessed = guessed.expect("the stored date parses");
        assert!((Utc::now().timestamp_millis() - guessed).abs() < 60_000);
    }

    /// A database that already had read articles before migration 011 gets
    /// their publish date as the guess for "when it was opened"; an unread
    /// article and one with an unreadable date get nothing.
    #[test]
    fn already_read_articles_get_a_guess_from_the_migration() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        // Bring the schema to just before migration 11, add three articles,
        // then run the remaining migration.
        super::super::migrations::run_up_to(&conn, 10).unwrap();
        for (url, stamp, read) in [
            ("https://old/read", "2026-01-01T00:00:00Z", 1),
            ("https://old/unread", "2026-01-01T00:00:00Z", 0),
            ("https://old/garbled", "not a date", 1),
        ] {
            conn.execute(
                "INSERT INTO articles (title, url, source, topic, timestamp, read, skipped, saved)
                 VALUES ('t', ?1, 's', 'c', ?2, ?3, 0, 0)",
                params![url, stamp, read],
            )
            .unwrap();
        }
        super::super::migrations::run(&conn).unwrap();

        let at = |url: &str| -> Option<i64> {
            conn.query_row(
                "SELECT read_at FROM articles WHERE url = ?1",
                params![url],
                |row| row.get(0),
            )
            .unwrap()
        };
        // 2026-01-01T00:00:00Z is 1767225600 seconds after 1970.
        assert_eq!(at("https://old/read"), Some(1_767_225_600_000));
        assert_eq!(at("https://old/unread"), None);
        assert_eq!(at("https://old/garbled"), None);
    }
}
