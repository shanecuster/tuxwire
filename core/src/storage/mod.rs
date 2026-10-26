//! SQLite access and migrations (`docs/ARCHITECTURE.md` § Storage).
//!
//! `Storage` is the *only* thing in this codebase that is allowed to know
//! SQL exists. Every other module (fetchers, scoring, the TUI) talks to
//! the database exclusively through the methods below -- e.g. `mark_read`,
//! `save_article` -- never by reaching for `rusqlite` themselves. That's
//! the same "normalize at the boundary" idea `models.rs` describes for
//! `Article`: keeping every raw `SELECT`/`INSERT`/`UPDATE` in one place
//! means the on-disk schema can change (a new migration, a renamed
//! column) without every caller needing to know or care.
//!
//! ## Why `storage/mod.rs` instead of `storage.rs`
//!
//! Rust has two equivalent ways to write a module that will contain
//! submodules of its own: a `storage.rs` file next to a `storage/`
//! directory, or (the older, still very common style used here)
//! `storage/mod.rs` -- the directory itself *is* the module, and `mod.rs`
//! is its "root" file. Either way, code elsewhere refers to this module
//! as `storage`, and anything declared `pub` in this file is reachable as
//! `storage::whatever`. The migration-running logic now lives in its own
//! file, `storage/migrations.rs`, brought in below with `mod migrations;`
//! -- the same pattern `fetchers/mod.rs` uses for `fetchers::rss`.

// `migrations` is a private submodule (no `pub`): its `run` function is an
// implementation detail of *how* `Storage::open_at` gets the schema ready,
// not something any other part of the app should ever call directly.
mod feed_sync_store;
mod migrations;
mod read_later_store;
mod read_mark_store;
mod stats_store;
mod sync_store;

use crate::fetchers::{SourceConfig, SourceType};
use crate::models::Article;
use anyhow::Context;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use std::collections::HashSet;
use std::path::Path;

pub use feed_sync_store::{FeedApplyCounts, FeedEntry, LocalFeedChanges, PendingFeedPush};
pub use read_later_store::{LaterApplyCounts, PendingLaterPush, ReadLaterItem};
pub use read_mark_store::{PendingReadMark, ReadMarkApplyCounts};
pub use sync_store::{ApplyCounts, PendingPush};

/// A handle to tuxwire's SQLite database.
///
/// Holding the open `Connection` inside a struct (rather than passing a
/// bare `Connection` around everywhere) is what lets this module expose
/// purpose-built methods like `insert_article` and `mark_read` as the
/// *only* way the rest of the app touches the database -- see the module
/// doc comment above.
pub struct Storage {
    conn: Connection,
}

impl Storage {
    /// Gives tests in other modules (the sync tests) direct access to the
    /// connection, to set up or inspect rows the public methods do not expose,
    /// such as backdating `read_at`. Compiled only for tests, so the real
    /// program has no way in except the purpose-built methods.
    #[cfg(test)]
    pub(crate) fn connection_for_tests(&self) -> &Connection {
        &self.conn
    }

    /// Opens (creating if necessary) the database file at `path`, and
    /// brings its schema up to date via `migrations::run`.
    ///
    /// ## Why the caller passes the path in
    ///
    /// This used to work out `~/.local/share/tuxwire/tuxwire.db` itself, from
    /// the `HOME` and `XDG_DATA_HOME` environment variables. That only makes
    /// sense on a Linux desktop: an Android app has no `HOME`, and gets its
    /// private data directory handed to it by the operating system instead.
    /// So this crate no longer guesses -- each program (the terminal app, the
    /// Android app) decides where its data lives and tells `Storage` the
    /// exact file. `path: &Path` *borrows* the path rather than taking
    /// ownership: opening a file only needs to read it, and the caller
    /// stays free to keep using its own copy afterwards.
    ///
    /// Tests use `from_connection` directly instead, against an in-memory
    /// database -- see the `tests` module at the bottom of this file.
    pub fn open_at(path: &Path) -> anyhow::Result<Self> {
        // `Path::parent()` gives the directory containing this path
        // (stripping off the file name) as an `Option<&Path>` -- `None`
        // only if `path` had no directory component at all, such as a bare
        // `tuxwire.db`. `create_dir_all` is a no-op if the directory
        // already exists (unlike `create_dir`, which would error in that
        // case) -- exactly what's wanted here, since this runs on every
        // single startup, not just the first.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create data directory {}", parent.display()))?;
        }

        // Open (or create) the SQLite file itself.
        let conn = Connection::open(path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;

        // Run any migrations the file hasn't seen yet, then wrap it.
        Self::from_connection(conn)
    }

    /// A fresh, fully migrated database that lives only in memory. Handy for
    /// tests that need a real `Storage` without touching disk -- including
    /// tests in *other* crates (the terminal UI's), which is why this is an
    /// ordinary public function: `#[cfg(test)]` code is only compiled while
    /// testing *this* crate, so a different crate's tests could not see it.
    pub fn open_in_memory() -> anyhow::Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    /// Wraps an already-open `Connection` in a `Storage`, running
    /// migrations against it before handing it back.
    ///
    /// Splitting this out from `open_at` means the "make sure the schema is
    /// current" step isn't duplicated between the real, file-backed path
    /// and the in-memory connections the test suite below opens with
    /// `Connection::open_in_memory()` -- both end up here.
    fn from_connection(conn: Connection) -> anyhow::Result<Self> {
        migrations::run(&conn)?;
        Ok(Self { conn })
    }

    // --- articles ---------------------------------------------------

    /// Inserts a freshly-fetched `Article` (one with the placeholder
    /// `id: 0` `Article::new` assigns -- see `models.rs`) and returns its
    /// row ID.
    ///
    /// Re-running the fetcher sees the same feed entries every time, so
    /// this has to be idempotent on `url`: if a row with this URL already
    /// exists, nothing is inserted and the *existing* row's ID is returned
    /// instead of a fresh one. `ON CONFLICT(url) DO UPDATE` is what
    /// actually guarantees that at the database level (backed by the
    /// unique index migration 002 adds on `articles.url`) -- the same
    /// atomic insert-or-else pattern `increment_skip_weight` below uses for
    /// `skip_weights`.
    ///
    /// On a conflict the only things touched are `feed_url` and `summary`,
    /// and only if the existing row doesn't have one yet: `COALESCE(a, b)`
    /// returns `a` unless it's NULL, then `b`, and `excluded.feed_url` is the
    /// value this insert *tried* to write. That quietly backfills articles
    /// stored before migration 005 (`feed_url`) or 007 (`summary`) the next
    /// time their feed is fetched, while leaving every other column
    /// (read/saved/note state included) exactly as it was. An article that
    /// already has a `feed_url` or `summary` keeps the first one it got.
    ///
    /// `article: &Article` borrows rather than takes ownership: inserting
    /// a row only needs to *read* the article's fields, and a fetcher
    /// calling this in a loop over many articles would otherwise have to
    /// give up ownership of (and re-fetch, or clone) each one just to
    /// store it.
    pub fn insert_article(&self, article: &Article) -> anyhow::Result<i64> {
        // `params![...]` is a macro that bundles up a list of values into
        // the form `rusqlite`'s `execute` expects, matching them
        // positionally against the `?1`, `?2`, ... placeholders in the
        // SQL string. Binding values this way -- rather than, say,
        // building the SQL string with `format!` -- is what makes this
        // immune to SQL injection: `rusqlite` sends the query and the
        // values to SQLite separately, so a value containing something
        // that looks like SQL (an apostrophe in an article title, say)
        // is never mistaken for part of the query itself.
        self.conn.execute(
            "INSERT INTO articles (title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at, feed_url, summary, read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(url) DO UPDATE SET
                 feed_url = COALESCE(articles.feed_url, excluded.feed_url),
                 summary = COALESCE(articles.summary, excluded.summary)",
            params![
                article.title,
                article.url,
                article.source,
                article.topic,
                article.timestamp,
                article.read,
                article.skipped,
                article.saved,
                article.note,
                article.saved_at,
                article.noted_at,
                article.feed_url,
                article.summary,
                // An article stored already read counts as opened now; an
                // unread one has no time (`then` gives `Some(..)` only when
                // the condition is true).
                article
                    .read
                    .then(|| Utc::now().timestamp_millis()),
            ],
        )?;

        // If another device already told us this article was opened (the
        // optional read-marks sync), mark it read now that it exists here.
        self.apply_waiting_read_mark(&article.url)?;

        // Looking the ID up by URL (rather than trusting
        // `last_insert_rowid`) is what makes this correct in both cases:
        // `last_insert_rowid` doesn't reliably point at this article when
        // the insert hit a conflict (no new row was created), so it could
        // return some *other* row's ID instead of this article's.
        self.conn
            .query_row(
                "SELECT id FROM articles WHERE url = ?1",
                params![article.url],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Best-effort backfill of `feed_url` for articles stored before
    /// migration 005, by matching each article's `source` *name* against
    /// the configured sources in `sources` (`(name, feed_url)` pairs, e.g.
    /// from every feed list). Returns how many rows were filled in.
    ///
    /// Only rows whose `feed_url` is still NULL are touched, so re-running
    /// this every startup is harmless and cheap. A name that maps to more
    /// than one feed URL (two lists each having a source called "News" with
    /// different feeds) is skipped, because there's no way to know which
    /// feed those old rows came from. Guessing could file articles under
    /// the wrong list. Those rows stay NULL: still in Saved, History, and
    /// search, just not in any list's topic view until re-fetched.
    pub fn backfill_feed_urls(&self, sources: &[(String, String)]) -> anyhow::Result<usize> {
        // Step 1: group the feed URLs by source name. A `BTreeSet` keeps
        // each name's URLs unique (the same source listed twice is fine).
        // `entry(...).or_default()` gets the set for a name, creating an
        // empty one the first time.
        let mut by_name: std::collections::BTreeMap<&str, std::collections::BTreeSet<&str>> =
            std::collections::BTreeMap::new();
        for (name, feed_url) in sources {
            by_name.entry(name).or_default().insert(feed_url);
        }

        // Step 2: one UPDATE per unambiguous name. `execute` returns how
        // many rows it changed, which is summed for the caller.
        let mut filled = 0;
        for (name, feed_urls) in by_name {
            if feed_urls.len() != 1 {
                continue;
            }
            // `.iter().next()` is the one URL in the set, and the `if let`
            // just unwraps it.
            if let Some(feed_url) = feed_urls.iter().next() {
                filled += self.conn.execute(
                    "UPDATE articles SET feed_url = ?1 WHERE feed_url IS NULL AND source = ?2",
                    params![feed_url, name],
                )?;
            }
        }
        Ok(filled)
    }

    /// Total number of rows in `articles`, across every topic and source --
    /// the simplest possible signal that re-running the fetcher didn't
    /// create duplicates: this should stay the same across a second run
    /// against a feed whose entries haven't changed.
    pub fn article_count(&self) -> anyhow::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM articles", [], |row| row.get(0))
            .map_err(Into::into)
    }

    /// Where the articles with these row IDs came from: each distinct
    /// (topic, source name, feed address) once, sorted for a stable order.
    ///
    /// A refresh reports the IDs of the articles it added
    /// (`FetchReport::new_article_ids`). The sidebar and the Android feed list
    /// want the *feeds* those belong to, so a marker can be drawn beside each
    /// feed (and its topic) that has something new. This is that translation.
    /// An unknown ID is ignored, and an empty set returns an empty list without
    /// touching the database.
    ///
    /// `ids: &HashSet<i64>` is borrowed because it only needs reading. The IDs
    /// go to SQLite in chunks of 500 because one query can only carry so many
    /// `?` placeholders, and a first refresh of many feeds can add thousands.
    pub fn article_origins(&self, ids: &HashSet<i64>) -> anyhow::Result<Vec<ArticleOrigin>> {
        // Step 1: nothing to look up.
        if ids.is_empty() {
            return Ok(Vec::new());
        }

        // Step 2: a set has no order, so copy the IDs into a `Vec` that can be
        // cut into chunks.
        let all: Vec<i64> = ids.iter().copied().collect();

        // Step 3: ask for each chunk and collect the distinct origins. A
        // `BTreeSet` drops duplicates and keeps the results sorted.
        let mut found = std::collections::BTreeSet::new();
        for chunk in all.chunks(500) {
            // One `?` per ID in this chunk: `"?, ?, ?"`.
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let mut stmt = self.conn.prepare(&format!(
                "SELECT DISTINCT topic, source, feed_url FROM articles WHERE id IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(params_from_iter(chunk.iter()), |row| {
                Ok(ArticleOrigin {
                    topic: row.get(0)?,
                    source: row.get(1)?,
                    feed_url: row.get(2)?,
                })
            })?;
            for row in rows {
                found.insert(row?);
            }
        }
        Ok(found.into_iter().collect())
    }

    /// All articles filed under `topic` within `scope`, most recent first
    /// -- the query backing the TUI's right-hand article pane
    /// (ARCHITECTURE.md's "Right pane: article list for the selected topic,
    /// sorted by recency"). With several feed lists, `scope` limits it to
    /// the active list's feeds (see `Scope`).
    pub fn articles_by_topic(&self, scope: &Scope, topic: &str) -> anyhow::Result<Vec<Article>> {
        // `scope.clause()` adds `AND feed_url IN (?, ?, ...)` for a list, or
        // nothing for `Scope::All`. Every placeholder here is a plain `?`,
        // filled in order: the topic first, then the scope's feed URLs.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at, feed_url, summary
             FROM articles WHERE topic = ?{} ORDER BY timestamp DESC",
            scope.clause()
        ))?;

        // `query_map` runs the query and lazily applies `row_to_article`
        // to each row as it's read. It hands back an iterator, not a
        // `Vec` directly, because `rusqlite` doesn't know up front whether
        // the caller wants every row materialized at once or would rather
        // stream through them -- `.collect()` below is what actually
        // decides "gather all of them into a `Vec`."
        // `params_from_iter` binds every value an iterator yields, in order.
        // `once(topic).chain(...)` is "the topic, followed by each feed URL".
        let rows = stmt.query_map(
            params_from_iter(std::iter::once(topic).chain(scope.feed_urls())),
            row_to_article,
        )?;

        // Each item out of `rows` is itself a `rusqlite::Result<Article>`
        // (decoding any single row can fail independently). Collecting
        // into `rusqlite::Result<Vec<Article>>` rather than
        // `Vec<rusqlite::Result<Article>>` asks Rust to short-circuit on
        // the first error instead of handing back a mix of successes and
        // failures -- `Result`'s `FromIterator` implementation is what
        // makes `.collect()` able to do that. `.map_err(Into::into)` then
        // converts a `rusqlite::Error` into this function's
        // `anyhow::Error` return type, the same conversion `?` performs
        // automatically -- spelled out explicitly here because `?` can't
        // reach inside the `.collect()` call itself.
        rows.collect::<rusqlite::Result<Vec<Article>>>()
            .map_err(Into::into)
    }

    /// All articles filed under `topic` *and* published by `source`
    /// specifically, most recent first -- backs the sidebar tree's nested
    /// source rows (`docs/ARCHITECTURE.md`'s "Selecting a source row...
    /// filters the article list to just that one source"). Deliberately
    /// the exact same query as `articles_by_topic` above with one more
    /// `AND source = ?2` clause, not a separate query path -- per
    /// ARCHITECTURE.md, "a source's topic is already fixed, so 'articles
    /// from source X' is just 'articles in X's topic, further filtered by
    /// source name.'" `scope` works as in `articles_by_topic`.
    pub fn articles_by_topic_and_source(
        &self,
        scope: &Scope,
        topic: &str,
        source: &str,
    ) -> anyhow::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at, feed_url, summary
             FROM articles WHERE topic = ? AND source = ?{} ORDER BY timestamp DESC",
            scope.clause()
        ))?;

        // Topic, then source, then the scope's feed URLs: the same order as
        // the `?`s in the SQL above.
        let rows = stmt.query_map(
            params_from_iter([topic, source].into_iter().chain(scope.feed_urls())),
            row_to_article,
        )?;
        rows.collect::<rusqlite::Result<Vec<Article>>>()
            .map_err(Into::into)
    }

    /// The articles of one feed row in the sidebar, most recent first, found
    /// by the feed's **address** so that renaming the feed (or filing it under
    /// another topic) in `sources.toml` does not hide its older articles.
    ///
    /// - With `feed_url` known, an article matches when its stored `feed_url`
    ///   is that address, whatever name or topic it was fetched under.
    /// - Articles stored before addresses were recorded have no `feed_url`;
    ///   those still match the old way, by `topic` and `source` name, so
    ///   nothing that used to show disappears.
    /// - With `feed_url` `None` (the row has no known address) only the name
    ///   match applies, exactly as `articles_by_topic_and_source`.
    ///
    /// `scope` works as in `articles_by_topic`.
    pub fn articles_by_feed_or_name(
        &self,
        scope: &Scope,
        topic: &str,
        source: &str,
        feed_url: Option<&str>,
    ) -> anyhow::Result<Vec<Article>> {
        // `feed_url = NULL` is never true in SQL, so a missing address simply
        // matches nothing in the first half of the `OR`.
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at, feed_url, summary
             FROM articles
             WHERE (feed_url = ? OR (feed_url IS NULL AND topic = ? AND source = ?)){} ORDER BY timestamp DESC",
            scope.clause()
        ))?;

        // The placeholders in order: address, topic, source, then the scope's
        // feed URLs. An absent address is passed as SQL NULL.
        let address = feed_url.map(str::to_string);
        let rows = stmt.query_map(
            params_from_iter(
                [
                    rusqlite::types::Value::from(address),
                    topic.to_string().into(),
                    source.to_string().into(),
                ]
                .into_iter()
                .chain(scope.feed_urls().map(|url| url.to_string().into())),
            ),
            row_to_article,
        )?;
        rows.collect::<rusqlite::Result<Vec<Article>>>()
            .map_err(Into::into)
    }

    /// Every saved article, across all topics, most recent first -- backs
    /// the dedicated "Saved" view described in ARCHITECTURE.md, which is
    /// explicitly independent of the topic sidebar.
    pub fn saved_articles(&self) -> anyhow::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at, feed_url, summary
             FROM articles WHERE saved = 1 ORDER BY timestamp DESC",
        )?;

        let rows = stmt.query_map([], row_to_article)?;
        rows.collect::<rusqlite::Result<Vec<Article>>>()
            .map_err(Into::into)
    }

    /// Every article tuxwire has ever fetched, regardless of read/skipped/
    /// saved state, most recent first -- backs the `y` "History" view
    /// (`docs/ARCHITECTURE.md`'s "Notes Retrieval & Search"). Nothing in
    /// tuxwire ever deletes a row from `articles`, so this is genuinely
    /// "every article ever seen," not a query that happens to look that
    /// way today -- it answers "I read something a few days ago, didn't
    /// save it, and want it back" without requiring the person to have
    /// remembered to press `s` in the moment.
    pub fn all_articles(&self) -> anyhow::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at, feed_url, summary
             FROM articles ORDER BY timestamp DESC",
        )?;

        let rows = stmt.query_map([], row_to_article)?;
        rows.collect::<rusqlite::Result<Vec<Article>>>()
            .map_err(Into::into)
    }

    /// Every article whose `title` or `note` contains `query` as a
    /// substring, case-insensitively, most recent first -- the `/` search
    /// keybind (`docs/ARCHITECTURE.md`'s "Notes Retrieval & Search"). Deliberately
    /// a plain `LIKE '%term%'` scan, not SQLite's FTS5 extension: per
    /// ARCHITECTURE.md, "at the scale of one person's article collection
    /// (realistically hundreds, not thousands), a plain substring match...
    /// is proportionate and sufficient" -- FTS5 would mean a second virtual
    /// table kept in sync with `articles` via triggers, a real subsystem
    /// this doesn't need yet.
    ///
    /// Runs against *every* article, unscoped by topic or `saved` -- not
    /// `articles_by_topic`/`saved_articles` with an extra `AND` clause the
    /// way `articles_by_topic_and_source` builds on `articles_by_topic`.
    /// Per ARCHITECTURE.md, "Searches across all articles, not just saved
    /// ones -- this is what actually answers 'find that thing I read a few
    /// days ago.'" The caller (`ui::draw_until_quit`) is what decides which
    /// on-screen view these results replace; this method itself has no
    /// notion of "current view" at all.
    ///
    /// `%` and `_` are SQL `LIKE` wildcard characters -- if `query` itself
    /// contains either (someone searching for a literal `%`), SQLite treats
    /// them as wildcards too, matching more broadly than a strict substring
    /// search would. Not escaped here: a personal search box where that
    /// only ever makes a search *less* strict, never a security concern
    /// (this is bound via `params!`, so it's not a SQL-injection risk
    /// either way -- see `insert_article`'s doc comment on why binding
    /// values, rather than formatting them into the SQL string, is what
    /// actually prevents that class of bug).
    ///
    /// SQLite's `LIKE` is already case-insensitive for ASCII letters by
    /// default (its one departure from `=`'s exact-match semantics) --
    /// nothing extra (a `COLLATE NOCASE`, a `LOWER()` on both sides) is
    /// needed to satisfy "case-insensitive" here.
    pub fn search_articles(&self, query: &str) -> anyhow::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at, feed_url, summary
             FROM articles WHERE title LIKE ?1 OR note LIKE ?1 ORDER BY timestamp DESC",
        )?;

        let pattern = format!("%{query}%");
        let rows = stmt.query_map(params![pattern], row_to_article)?;
        rows.collect::<rusqlite::Result<Vec<Article>>>()
            .map_err(Into::into)
    }

    /// Every distinct topic among the articles in `scope`, alphabetical --
    /// feeds the TUI's topic sidebar. With `Scope::All` that's every topic
    /// in the database, exactly as before feed lists existed.
    ///
    /// `WHERE 1` is "always true", there so the scope's `AND ...` clause
    /// always has a `WHERE` to attach to.
    pub fn topics(&self, scope: &Scope) -> anyhow::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT DISTINCT topic FROM articles WHERE 1{} ORDER BY topic",
            scope.clause()
        ))?;

        let rows = stmt.query_map(params_from_iter(scope.feed_urls()), |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<String>>>()
            .map_err(Into::into)
    }

    /// How many articles in `scope` are unread: not read and not skipped.
    /// Saving marks an article read, so saved ones don't count either. Used
    /// for the feed list picker's per-list count, which comes straight from
    /// the database without fetching anything.
    pub fn unread_count(&self, scope: &Scope) -> anyhow::Result<i64> {
        self.conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM articles WHERE read = 0 AND skipped = 0{}",
                    scope.clause()
                ),
                params_from_iter(scope.feed_urls()),
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    /// Marks an article read -- the `Enter` keybind (open in `$BROWSER`).
    ///
    /// The moment it was opened is remembered in `read_at` (milliseconds since
    /// 1970) the first time only: `CASE WHEN read = 0 ...` keeps the original
    /// time when an already-read article is opened again. The optional
    /// read-marks sync uses that time to send only recent reads.
    pub fn mark_read(&self, id: i64) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE articles
             SET read = 1,
                 read_at = CASE WHEN read = 0 THEN ?2 ELSE read_at END
             WHERE id = ?1",
            params![id, Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    /// Marks an article skipped -- the `x` keybind.
    ///
    /// This deliberately does *not* touch `skip_weights` itself. Which
    /// keyword(s) a skip should count against is a judgment call (title
    /// words? topic? source?) that belongs to the skip-weighting logic in
    /// `scoring.rs`, not to storage -- this method only records the one
    /// fact storage is responsible for: that this particular article was
    /// skipped. The caller (eventually, the TUI's `x`-key handler) is
    /// expected to call both this *and* `increment_skip_weight` for
    /// whatever keyword(s) it derives from the article.
    pub fn mark_skipped(&self, id: i64) -> anyhow::Result<()> {
        self.conn
            .execute("UPDATE articles SET skipped = 1 WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Saves an article, optionally attaching a note, and marks it read in
    /// the same statement -- ARCHITECTURE.md's "Saving auto-marks as
    /// read... pressing `s` sets both `saved = true, read = true` in one
    /// action." Doing both in a single `UPDATE` (rather than two separate
    /// calls) is what actually guarantees that: there's no moment where
    /// the database reflects `saved = true, read = false`.
    ///
    /// Also stamps `saved_at` with the current instant -- per
    /// ARCHITECTURE.md, "`saved_at`, set the moment `s` is pressed." The
    /// stamp is only written when the article was *not already saved*:
    /// pressing `s` on an article that is already saved must not move its
    /// save date forward. (Un-saving clears `saved_at`, so saving again
    /// after an un-save gets a fresh date, which is honest: it really was
    /// saved again then.)
    ///
    /// In the SQL below, `CASE WHEN ... THEN ... ELSE ... END` is SQL's
    /// "if / else". On the right-hand side of an `UPDATE`, column names
    /// still mean the row's *old* values, so `saved` and `saved_at` here
    /// are what the row held before this statement changed anything.
    ///
    /// `note: Option<String>` mirrors `Article::note` exactly: passing
    /// `None` saves the article with no note, `Some(text)` saves it with
    /// one attached.
    pub fn save_article(&self, id: i64, note: Option<String>) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE articles SET saved = 1, read = 1, note = ?2,
                 saved_at = CASE WHEN saved = 1 AND saved_at IS NOT NULL
                                 THEN saved_at ELSE ?3 END,
                 sync_updated_at = CASE
                     WHEN saved = 1 AND note IS ?2 THEN sync_updated_at
                     ELSE MAX(COALESCE(sync_updated_at, 0) + 1, ?4) END,
                 read_at = CASE WHEN read = 0 THEN ?4 ELSE read_at END
             WHERE id = ?1",
            params![id, note, Utc::now(), Utc::now().timestamp_millis()],
        )?;
        Ok(())
    }

    /// Un-saves an article: the opposite of [`Storage::save_article`].
    ///
    /// A note and a save are *locked together*: a note only exists on a
    /// saved article (see [`Storage::update_note`]), so un-saving also
    /// deletes the note and its date. Callers must therefore ask the person
    /// to confirm before calling this on an article that has a note. This
    /// function does not ask; it only does.
    ///
    /// The article itself stays in the database and keeps its read state;
    /// only the saved-article parts are cleared (`saved`, `saved_at`,
    /// `note`, `noted_at`). `NULL` is SQL's "no value", which is how
    /// `None` is stored.
    ///
    /// Returns `true` when an article with this `id` existed and was saved,
    /// and `false` when there was nothing to un-save (unknown id, or the
    /// article was not saved), so the caller can tell the difference.
    pub fn unsave_article(&self, id: i64) -> anyhow::Result<bool> {
        // `execute` returns how many rows the statement changed. The
        // `AND saved = 1` makes an already-unsaved article change zero rows.
        let changed = self.conn.execute(
            "UPDATE articles
             SET saved = 0, saved_at = NULL, note = NULL, noted_at = NULL,
                 sync_updated_at = MAX(COALESCE(sync_updated_at, 0) + 1, ?2)
             WHERE id = ?1 AND saved = 1",
            params![id, Utc::now().timestamp_millis()],
        )?;
        Ok(changed > 0)
    }

    /// Replaces the note on an already-saved article -- the `n` keybind
    /// ("edit note on a saved article").
    ///
    /// Also keeps `noted_at` in sync: per ARCHITECTURE.md, it's "set
    /// whenever `update_note` stores a non-empty note... If the note is
    /// cleared back to empty, `noted_at` clears too (`None`), keeping 'no
    /// note' and 'no note date' consistent with each other." `note.is_some()`
    /// is exactly "is this call storing a non-empty note" -- the `n`
    /// popup in `ui/mod.rs` already turns an empty text buffer into `None`
    /// before ever calling this, so `Some` here always means real text.
    /// `Option::then` is what turns that `bool` into `Some(Utc::now())` /
    /// `None` in one expression, matching `note`'s own shape rather than
    /// writing an `if`/`else` to build the same thing by hand.
    ///
    /// A note and a save are *locked together*: storing a real note also
    /// saves the article (marking it read and stamping `saved_at` if it
    /// had no save date yet), so a note can never sit on an article that
    /// is missing from the Saved view and the export. Clearing a note
    /// (`None`) leaves the article saved. Removing the save is a separate,
    /// confirmed action: [`Storage::unsave_article`].
    pub fn update_note(&self, id: i64, note: Option<String>) -> anyhow::Result<()> {
        let now = Utc::now();
        let noted_at = note.is_some().then_some(now);
        // `has_note` is 1 when a real note is being stored and 0 when it is
        // being cleared; SQLite has no separate boolean type, so 0/1 it is.
        let has_note = i64::from(note.is_some());
        self.conn.execute(
            "UPDATE articles
             SET note = ?2,
                 noted_at = ?3,
                 saved = CASE WHEN ?4 = 1 THEN 1 ELSE saved END,
                 read = CASE WHEN ?4 = 1 THEN 1 ELSE read END,
                 saved_at = CASE WHEN ?4 = 1 AND (saved = 0 OR saved_at IS NULL)
                                 THEN ?5 ELSE saved_at END,
                 sync_updated_at = CASE
                     WHEN note IS NOT ?2 OR (saved = 0 AND ?4 = 1)
                     THEN MAX(COALESCE(sync_updated_at, 0) + 1, ?6)
                     ELSE sync_updated_at END
             WHERE id = ?1",
            params![id, note, noted_at, has_note, now, now.timestamp_millis()],
        )?;
        Ok(())
    }

    // --- skip_weights -------------------------------------------------

    /// Increments `keyword`'s weight in `skip_weights` by one, inserting a
    /// fresh row at weight `1` if this keyword has never been skipped
    /// before. Called once per keyword derived from a skipped article --
    /// see the note on `mark_skipped` above for why that derivation
    /// doesn't happen in this module.
    ///
    /// `ON CONFLICT(keyword) DO UPDATE` is SQLite's "upsert" syntax: try
    /// the `INSERT`, and if it would violate the `PRIMARY KEY` uniqueness
    /// on `keyword` (i.e. a row for this keyword already exists), run the
    /// given `UPDATE` against that existing row instead. This does the
    /// same job as "look up the row, then either insert or update based on
    /// whether it existed" would in application code, but as a single
    /// atomic statement -- no risk of another write sneaking in between a
    /// separate lookup and the following insert/update.
    #[allow(dead_code)] // for scoring.rs's skip-weighting, not built yet; exercised by tests only
    pub fn increment_skip_weight(&self, keyword: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO skip_weights (keyword, weight) VALUES (?1, 1)
             ON CONFLICT(keyword) DO UPDATE SET weight = weight + 1",
            params![keyword],
        )?;
        Ok(())
    }

    /// The current weight for `keyword`, or `0` if it has never been
    /// skipped (i.e. has no row in `skip_weights` at all) -- callers doing
    /// scoring want a plain number to compare/sum, not a `None` they'd
    /// have to special-case at every call site.
    #[allow(dead_code)] // for scoring.rs's skip-weighting, not built yet; exercised by tests only
    pub fn skip_weight(&self, keyword: &str) -> anyhow::Result<i64> {
        // `.optional()` (from the `OptionalExtension` trait, imported
        // above) adapts a query that might legitimately return zero rows:
        // ordinarily, `query_row` treats "no rows" as an error
        // (`rusqlite::Error::QueryReturnedNoRows`), because most callers
        // ask for exactly one row and *expect* it to exist. `.optional()`
        // turns that specific case into `Ok(None)` instead, leaving every
        // other kind of error (a malformed query, a closed connection) as
        // a real `Err` -- exactly the distinction needed here, since "this
        // keyword was never skipped" is an expected, non-error outcome.
        let weight = self
            .conn
            .query_row(
                "SELECT weight FROM skip_weights WHERE keyword = ?1",
                params![keyword],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);

        Ok(weight)
    }

    /// Every keyword tuxwire has ever recorded a skip against, heaviest
    /// first -- exactly the raw view ARCHITECTURE.md promises the
    /// skip-weighting system supports: "you should always be able to look
    /// at `skip_weights` and understand exactly why an article was
    /// deprioritized." Also the natural source for a future stats view's
    /// "top skipped keywords" (see ARCHITECTURE.md's Planned Features).
    #[allow(dead_code)] // for scoring.rs's skip-weighting, not built yet; exercised by tests only
    pub fn all_skip_weights(&self) -> anyhow::Result<Vec<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT keyword, weight FROM skip_weights ORDER BY weight DESC")?;

        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<(String, i64)>>>()
            .map_err(Into::into)
    }

    // --- feeds and settings (used by the Android app) ----------------

    /// Adds a feed to the `feeds` table and returns its new row ID.
    ///
    /// The terminal app never calls this (its feeds live in TOML files); the
    /// Android app keeps its whole feed list here.
    ///
    /// Adding a feed whose `url` is already present is an error rather than a
    /// silent no-op, so the person is told "already added" instead of seeing
    /// nothing happen. `ON CONFLICT(url) DO NOTHING` makes SQLite skip the
    /// insert (instead of failing with a database-level error), and
    /// `execute` returns how many rows were changed, so `0` means it was
    /// skipped.
    pub fn add_feed(
        &self,
        name: &str,
        url: &str,
        topic: &str,
        source_type: SourceType,
    ) -> anyhow::Result<i64> {
        // Step 1: try the insert. The values are bound with `params!` (never
        // pasted into the SQL), as everywhere else in this module.
        let changed = self.conn.execute(
            "INSERT INTO feeds (name, url, topic, source_type) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(url) DO NOTHING",
            params![name, url, topic, source_type.as_str()],
        )?;

        // Step 2: no row changed means the URL was already there.
        if changed == 0 {
            anyhow::bail!("this feed is already added: {url}");
        }

        // Step 3: hand back the new row's ID. `last_insert_rowid` is reliable
        // here because we just confirmed a row really was inserted.
        Ok(self.conn.last_insert_rowid())
    }

    /// Removes the feed with this `id` from the `feeds` table and returns
    /// whether a feed was actually removed (`false` if no such ID existed).
    ///
    /// Only the feed's entry goes: articles already fetched from it stay in
    /// the database. Saved articles and notes are permanent, and read history
    /// should not vanish because a feed was unsubscribed. They remain in
    /// Saved, History and search; they just no longer appear under any feed.
    pub fn remove_feed(&self, id: i64) -> anyhow::Result<bool> {
        let changed = self
            .conn
            .execute("DELETE FROM feeds WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }

    /// Every feed, grouped by topic and then by name (both
    /// case-insensitive), so a screen can list them in a stable order.
    ///
    /// A row whose `source_type` text isn't one we recognise (for example
    /// written by a newer version of the app) is skipped rather than failing
    /// the whole list: `filter_map` below drops the `None`s.
    pub fn list_feeds(&self) -> anyhow::Result<Vec<Feed>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, url, topic, source_type FROM feeds
             ORDER BY topic COLLATE NOCASE, name COLLATE NOCASE",
        )?;

        // Read each row as plain values first: `query_map` wants a closure
        // returning `rusqlite::Result`, and turning the type text into a
        // `SourceType` can fail in a way that is not a database error.
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        // Convert each row into a `Feed`, skipping unknown source types.
        let mut feeds = Vec::new();
        for row in rows {
            let (id, name, url, topic, type_text) = row?;
            if let Some(source_type) = SourceType::from_db_str(&type_text) {
                feeds.push(Feed {
                    id,
                    name,
                    url,
                    topic,
                    source_type,
                });
            }
        }
        Ok(feeds)
    }

    /// The article with this `id`, or `None` if there is no such article.
    ///
    /// Used when a screen needs one article's details (such as its summary)
    /// and the list that showed it only carried the basics.
    pub fn article_by_id(&self, id: i64) -> anyhow::Result<Option<Article>> {
        // `.optional()` turns "no rows" into `Ok(None)` instead of an error, so
        // a missing article is an ordinary answer.
        self.conn
            .query_row(
                "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at, feed_url, summary
                 FROM articles WHERE id = ?1",
                params![id],
                row_to_article,
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every article fetched from the feed at `feed_url`, most recent first.
    ///
    /// This asks only "which feed did it come from", not "which topic is it
    /// filed under": an article keeps the topic it had when it was fetched,
    /// so filtering by the feed's *current* topic could hide older ones.
    pub fn articles_by_feed(&self, feed_url: &str) -> anyhow::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at, feed_url, summary
             FROM articles WHERE feed_url = ?1 ORDER BY timestamp DESC",
        )?;

        let rows = stmt.query_map(params![feed_url], row_to_article)?;
        rows.collect::<rusqlite::Result<Vec<Article>>>()
            .map_err(Into::into)
    }

    /// Reads the setting called `key`, or `None` if it was never saved.
    ///
    /// `.optional()` (from `rusqlite`'s `OptionalExtension`) turns the
    /// "no rows" error that `query_row` raises into `Ok(None)`, so a missing
    /// setting is an ordinary answer rather than a failure.
    pub fn get_setting(&self, key: &str) -> anyhow::Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Saves `value` under `key`, replacing any earlier value.
    ///
    /// `ON CONFLICT(key) DO UPDATE` is the same insert-or-update pattern
    /// `increment_skip_weight` uses: one atomic statement that creates the row
    /// the first time and updates it afterwards. `excluded.value` is the value
    /// this statement tried to insert.
    pub fn set_setting(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

/// Where an article came from: the topic and source name it is filed under,
/// and the address of the feed it was fetched from (`None` for an article
/// stored before addresses were recorded). See [`Storage::article_origins`].
///
/// `Ord` and `PartialOrd` are derived so a set of these sorts by topic, then
/// source, then address.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArticleOrigin {
    /// The topic the article is filed under.
    pub topic: String,
    /// The source's display name, as stored on the article.
    pub source: String,
    /// The feed's address (or file path, or `import:` key); `None` if the
    /// article predates feed addresses.
    pub feed_url: Option<String>,
}

/// One row of the `feeds` table: a feed the Android app fetches.
///
/// This is the stored form. [`Feed::to_source_config`] turns it into the
/// [`SourceConfig`] the fetching code expects, the same type the terminal app
/// builds from its TOML files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    /// The database row ID, used to remove the feed.
    pub id: i64,
    /// Display name, shown in the UI and copied onto each article's `source`.
    pub name: String,
    /// The feed's address. Unique across feeds.
    pub url: String,
    /// The topic this feed's articles are filed under.
    pub topic: String,
    /// Which fetcher handles this feed.
    pub source_type: SourceType,
}

impl Feed {
    /// Converts this stored feed into a [`SourceConfig`], ready for
    /// `fetchers::configured_sources`.
    ///
    /// Takes `&self` and clones the strings, so the `Feed` stays usable
    /// afterwards (a screen may keep showing it while a refresh runs).
    pub fn to_source_config(&self) -> SourceConfig {
        SourceConfig {
            name: self.name.clone(),
            source_type: self.source_type,
            url: self.url.clone(),
            topic: self.topic.clone(),
        }
    }
}

/// Which articles a topic-view query covers: every article (`All`, used
/// when there's only one feed list, so behavior matches the days before
/// lists existed), or only those whose `feed_url` is one of the given feed
/// URLs (`Feeds`, the active list's sources).
///
/// Only the topic sidebar, the per-topic article lists, and the picker's
/// unread counts take a scope. Saved, History, and search deliberately stay
/// global. Articles with no `feed_url` yet (stored before migration 005 and
/// not backfilled) fall outside every `Feeds` scope but remain in those
/// global views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// No filtering.
    All,
    /// Only articles from these feeds (each a `[[source]]`'s `url`).
    Feeds(Vec<String>),
}

impl Scope {
    /// The SQL to append to a `WHERE`: nothing for `All`,
    /// ` AND feed_url IN (?, ?, ...)` with one `?` per feed for `Feeds`,
    /// and ` AND 0` (always false, so nothing matches) for an empty
    /// `Feeds`, since `IN ()` isn't valid SQL.
    ///
    /// Only `?` placeholders are ever formatted into the SQL; the URLs
    /// themselves are bound as parameters (see `feed_urls`), never pasted
    /// into the string, so this can't be used for SQL injection.
    fn clause(&self) -> String {
        match self {
            Scope::All => String::new(),
            Scope::Feeds(urls) if urls.is_empty() => " AND 0".to_string(),
            // `vec!["?"; n]` is a list of `n` question marks, joined with
            // commas.
            Scope::Feeds(urls) => {
                format!(" AND feed_url IN ({})", vec!["?"; urls.len()].join(", "))
            }
        }
    }

    /// The values for `clause`'s placeholders, in order, as `&str`s
    /// borrowed from the scope.
    ///
    /// `-> impl Iterator<Item = &str> + '_` means "some iterator of string
    /// slices that borrows from `self`". The two arms return different
    /// iterator types, so both are boxed into one type
    /// (`Box<dyn Iterator>`), and `+ '_` ties the borrow to `self`.
    fn feed_urls(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        match self {
            Scope::All => Box::new(std::iter::empty()),
            Scope::Feeds(urls) => Box::new(urls.iter().map(String::as_str)),
        }
    }
}

/// Maps one row from a `SELECT id, title, url, source, topic, timestamp,
/// read, skipped, saved, note, saved_at, noted_at, feed_url, summary FROM articles ...` query
/// into an `Article`.
///
/// Every query in this module that reads from `articles` lists its
/// columns in this exact order specifically so this one function can be
/// reused for all of them, rather than writing the same row-decoding logic
/// out repeatedly. `Row::get(N)` takes a 0-based column index and is
/// generic over any type implementing `rusqlite`'s `FromSql` trait --
/// `String`, `i64`, and (thanks to the `chrono` feature enabled on
/// `rusqlite` in Cargo.toml) `bool` and `chrono::DateTime<Utc>` all
/// implement it, which is why this function is little more than naming
/// each column: there's no manual "parse this TEXT column into a
/// DateTime" logic to write by hand.
///
/// This is a free function rather than a method on `Storage` because it
/// doesn't need `&self` -- it only ever touches the `Row` it's handed --
/// and `query_map` (used throughout `Storage`'s methods above) expects a
/// plain function or closure of exactly this shape: `Fn(&Row) ->
/// rusqlite::Result<T>`.
fn row_to_article(row: &Row) -> rusqlite::Result<Article> {
    Ok(Article {
        id: row.get(0)?,
        title: row.get(1)?,
        url: row.get(2)?,
        source: row.get(3)?,
        topic: row.get(4)?,
        timestamp: row.get(5)?,
        read: row.get(6)?,
        skipped: row.get(7)?,
        saved: row.get(8)?,
        note: row.get(9)?,
        saved_at: row.get(10)?,
        noted_at: row.get(11)?,
        feed_url: row.get(12)?,
        summary: row.get(13)?,
    })
}

// `#[cfg(test)]` means this entire module is compiled only when running
// `cargo test` -- it doesn't exist at all in a normal `cargo build`, so
// none of this adds to the size of the real binary. This is also where
// the pub methods above earn their keep as *documentation*: reading these
// tests should be enough on its own to understand how to call `Storage`,
// separately from actually verifying the behavior is correct.
#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, fully-migrated `Storage` backed by an in-memory SQLite
    /// database -- `Connection::open_in_memory()` never touches disk at
    /// all, which is what makes these tests fast and safe to run without
    /// risking a real `~/.local/share/tuxwire/tuxwire.db`. Each call
    /// returns a brand new, empty database; nothing here is shared between
    /// tests.
    fn test_storage() -> Storage {
        Storage::from_connection(Connection::open_in_memory().unwrap()).unwrap()
    }

    #[test]
    fn migrations_create_expected_tables() {
        let storage = test_storage();

        let table_count: i64 = storage
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('articles', 'skip_weights')",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(table_count, 2);
    }

    #[test]
    fn insert_and_fetch_by_topic() {
        let storage = test_storage();
        let article = Article::new(
            "Btrfs send/receive got faster".to_string(),
            "https://example.com/btrfs".to_string(),
            "LWN.net".to_string(),
            "linux-news".to_string(),
            Utc::now(),
        );

        let id = storage.insert_article(&article).unwrap();
        assert!(id > 0);

        let fetched = storage
            .articles_by_topic(&Scope::All, "linux-news")
            .unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].id, id);
        assert_eq!(fetched[0].title, "Btrfs send/receive got faster");

        // A different topic should see nothing -- the whole point of
        // filtering by topic in the query rather than fetching everything
        // and filtering in Rust.
        assert!(
            storage
                .articles_by_topic(&Scope::All, "gaming")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn articles_by_topic_and_source_filters_to_just_that_source() {
        let storage = test_storage();
        let lwn = Article::new(
            "Btrfs send/receive got faster".to_string(),
            "https://example.com/btrfs".to_string(),
            "LWN.net".to_string(),
            "linux-news".to_string(),
            Utc::now(),
        );
        let phoronix = Article::new(
            "New kernel release".to_string(),
            "https://example.com/kernel".to_string(),
            "Phoronix".to_string(),
            "linux-news".to_string(),
            Utc::now(),
        );
        storage.insert_article(&lwn).unwrap();
        storage.insert_article(&phoronix).unwrap();

        let lwn_only = storage
            .articles_by_topic_and_source(&Scope::All, "linux-news", "LWN.net")
            .unwrap();
        assert_eq!(lwn_only.len(), 1);
        assert_eq!(lwn_only[0].source, "LWN.net");

        // The two sources share a topic but not a source name -- a source
        // filter on one must never leak the other's articles in, and a
        // source name that matches no row at all (right topic, wrong or
        // unknown source) comes back empty rather than erroring.
        assert!(
            storage
                .articles_by_topic_and_source(&Scope::All, "linux-news", "unknown-source")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn inserting_duplicate_url_does_not_create_duplicate() {
        let storage = test_storage();
        let article = Article::new(
            "Btrfs send/receive got faster".to_string(),
            "https://example.com/btrfs".to_string(),
            "LWN.net".to_string(),
            "linux-news".to_string(),
            Utc::now(),
        );

        let first_id = storage.insert_article(&article).unwrap();
        // Simulates re-running the fetcher against the same feed: same
        // URL, but a distinct `Article` value (e.g. the title casing
        // changed upstream) -- the row already on disk should win either
        // way.
        let mut refetched = article.clone();
        refetched.title = "Btrfs send/receive got even faster".to_string();
        let second_id = storage.insert_article(&refetched).unwrap();

        assert_eq!(
            first_id, second_id,
            "re-inserting an existing URL must return its existing id"
        );

        let articles = storage
            .articles_by_topic(&Scope::All, "linux-news")
            .unwrap();
        assert_eq!(
            articles.len(),
            1,
            "duplicate URL must not create a second row"
        );
        assert_eq!(
            articles[0].title, "Btrfs send/receive got faster",
            "the original row must be left untouched"
        );
    }

    #[test]
    fn saving_marks_read_too() {
        let storage = test_storage();
        let article = Article::new(
            "title".to_string(),
            "url".to_string(),
            "source".to_string(),
            "topic".to_string(),
            Utc::now(),
        );
        let id = storage.insert_article(&article).unwrap();

        storage
            .save_article(id, Some("worth trying this weekend".to_string()))
            .unwrap();

        let saved = storage.saved_articles().unwrap();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].saved);
        assert!(saved[0].read, "save_article must also set read = true");
        assert_eq!(saved[0].note.as_deref(), Some("worth trying this weekend"));
    }

    #[test]
    fn mark_read_mark_skipped_and_update_note() {
        let storage = test_storage();
        let article = Article::new(
            "title".to_string(),
            "url".to_string(),
            "source".to_string(),
            "topic".to_string(),
            Utc::now(),
        );
        let id = storage.insert_article(&article).unwrap();

        storage.mark_read(id).unwrap();
        storage.mark_skipped(id).unwrap();
        storage
            .update_note(id, Some("revisit this".to_string()))
            .unwrap();

        let fetched = &storage.articles_by_topic(&Scope::All, "topic").unwrap()[0];
        assert!(fetched.read);
        assert!(fetched.skipped);
        assert_eq!(fetched.note.as_deref(), Some("revisit this"));
    }

    /// `saved_at` starts `None` (never saved) and gets populated the
    /// moment `save_article` runs -- the `s` keybind's whole point, per
    /// ARCHITECTURE.md's "`saved_at`, set the moment `s` is pressed."
    #[test]
    fn save_article_populates_saved_at() {
        let storage = test_storage();
        let article = Article::new(
            "title".to_string(),
            "url".to_string(),
            "source".to_string(),
            "topic".to_string(),
            Utc::now(),
        );
        let id = storage.insert_article(&article).unwrap();

        let fetched = &storage.articles_by_topic(&Scope::All, "topic").unwrap()[0];
        assert_eq!(
            fetched.saved_at, None,
            "an article that was never saved has no saved_at"
        );

        let before = Utc::now();
        storage.save_article(id, None).unwrap();
        let after = Utc::now();

        let saved = &storage.saved_articles().unwrap()[0];
        let saved_at = saved.saved_at.expect("save_article must populate saved_at");
        assert!(
            saved_at >= before && saved_at <= after,
            "saved_at should land at the moment save_article ran, not some other time"
        );
    }

    /// `noted_at` populates when `update_note` stores a non-empty note, and
    /// clears back to `None` when the note is cleared back to empty --
    /// ARCHITECTURE.md's "If the note is cleared back to empty, `noted_at`
    /// clears too (`None`), keeping 'no note' and 'no note date' consistent
    /// with each other."
    #[test]
    fn update_note_populates_and_clears_noted_at() {
        let storage = test_storage();
        let article = Article::new(
            "title".to_string(),
            "url".to_string(),
            "source".to_string(),
            "topic".to_string(),
            Utc::now(),
        );
        let id = storage.insert_article(&article).unwrap();

        let fetched = &storage.articles_by_topic(&Scope::All, "topic").unwrap()[0];
        assert_eq!(
            fetched.noted_at, None,
            "an article with no note has no noted_at"
        );

        let before = Utc::now();
        storage
            .update_note(id, Some("revisit this".to_string()))
            .unwrap();
        let after = Utc::now();

        let noted = &storage.articles_by_topic(&Scope::All, "topic").unwrap()[0];
        let noted_at = noted
            .noted_at
            .expect("a non-empty note must populate noted_at");
        assert!(
            noted_at >= before && noted_at <= after,
            "noted_at should land at the moment update_note ran"
        );

        // Clearing the note (the `n` popup's "empty buffer -> None"
        // convention, see `ui/mod.rs`) must clear noted_at right along
        // with it -- "no note" and "no note date" are never allowed to
        // disagree.
        storage.update_note(id, None).unwrap();
        let cleared = &storage.articles_by_topic(&Scope::All, "topic").unwrap()[0];
        assert_eq!(cleared.note, None);
        assert_eq!(
            cleared.noted_at, None,
            "clearing the note must clear noted_at too"
        );
    }

    /// Builds a throwaway article and stores it, returning its id. Used by
    /// the save / un-save tests below so each test body stays short.
    fn stored_article(storage: &Storage) -> i64 {
        let article = Article::new(
            "title".to_string(),
            "url".to_string(),
            "source".to_string(),
            "topic".to_string(),
            Utc::now(),
        );
        storage.insert_article(&article).unwrap()
    }

    /// Saving an article that is already saved must not move its save date.
    #[test]
    fn saving_again_keeps_the_original_saved_at() {
        let storage = test_storage();
        let id = stored_article(&storage);

        storage.save_article(id, None).unwrap();
        let first = storage.saved_articles().unwrap()[0].saved_at;

        // A second save (for example `s` pressed twice) changes nothing.
        storage.save_article(id, None).unwrap();
        let second = storage.saved_articles().unwrap()[0].saved_at;

        assert!(first.is_some());
        assert_eq!(first, second, "a repeat save must keep the first save date");
    }

    /// Un-saving removes the article from the saved list and clears the
    /// note and both dates, but keeps the article itself (and its read flag).
    #[test]
    fn unsave_article_clears_save_note_and_dates() {
        let storage = test_storage();
        let id = stored_article(&storage);
        storage
            .save_article(id, Some("worth keeping".to_string()))
            .unwrap();
        storage
            .update_note(id, Some("worth keeping".to_string()))
            .unwrap();

        // `true` means "there was a save to remove".
        assert!(storage.unsave_article(id).unwrap());

        assert!(storage.saved_articles().unwrap().is_empty());
        let after = storage.article_by_id(id).unwrap().expect("article remains");
        assert!(!after.saved);
        assert!(after.read, "un-saving does not make the article unread");
        assert_eq!(after.note, None, "the note goes with the save");
        assert_eq!(after.noted_at, None);
        assert_eq!(after.saved_at, None);
    }

    /// Un-saving something that is not saved (or does not exist) reports
    /// `false` and changes nothing.
    #[test]
    fn unsave_article_reports_when_there_was_nothing_to_remove() {
        let storage = test_storage();
        let id = stored_article(&storage);

        assert!(!storage.unsave_article(id).unwrap(), "never saved");
        assert!(!storage.unsave_article(9_999).unwrap(), "unknown id");
    }

    /// Saving again after an un-save gets a new, fresh save date.
    #[test]
    fn saving_after_an_unsave_stamps_a_new_saved_at() {
        let storage = test_storage();
        let id = stored_article(&storage);
        storage.save_article(id, None).unwrap();
        storage.unsave_article(id).unwrap();

        let before = Utc::now();
        storage.save_article(id, None).unwrap();

        let saved_at = storage.saved_articles().unwrap()[0]
            .saved_at
            .expect("a re-save is stamped");
        assert!(saved_at >= before);
    }

    /// The lock between a note and a save: storing a note saves the article
    /// too, and clearing the note leaves it saved.
    #[test]
    fn a_note_saves_the_article_and_clearing_it_keeps_the_save() {
        let storage = test_storage();
        let id = stored_article(&storage);

        // A note on an article that was never saved saves it.
        storage
            .update_note(id, Some("remember this".to_string()))
            .unwrap();
        let saved = storage.saved_articles().unwrap();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].saved_at.is_some());
        assert!(saved[0].read);

        // Editing the note keeps the original save date.
        let first_saved_at = saved[0].saved_at;
        storage
            .update_note(id, Some("remember this, edited".to_string()))
            .unwrap();
        assert_eq!(
            storage.saved_articles().unwrap()[0].saved_at,
            first_saved_at
        );

        // Clearing the note removes only the note.
        storage.update_note(id, None).unwrap();
        let after = storage.saved_articles().unwrap();
        assert_eq!(after.len(), 1, "the article is still saved");
        assert_eq!(after[0].note, None);
    }

    #[test]
    fn all_articles_includes_every_state_regardless_of_read_saved_skipped() {
        let storage = test_storage();
        for (i, title) in ["unread", "read-only", "saved", "skipped"]
            .into_iter()
            .enumerate()
        {
            let article = Article::new(
                title.to_string(),
                format!("https://example.com/{i}"),
                "source".to_string(),
                "topic".to_string(),
                Utc::now(),
            );
            storage.insert_article(&article).unwrap();
        }

        let ids: Vec<i64> = storage
            .all_articles()
            .unwrap()
            .iter()
            .map(|a| a.id)
            .collect();
        assert_eq!(
            ids.len(),
            4,
            "all_articles must return every fetched article, untouched or not"
        );

        storage.mark_read(ids[1]).unwrap();
        storage.save_article(ids[2], None).unwrap();
        storage.mark_skipped(ids[3]).unwrap();

        // Still every article, regardless of the state each one is now in
        // -- this is the whole point of the History view: nothing is ever
        // filtered out of it based on read/saved/skipped.
        assert_eq!(storage.all_articles().unwrap().len(), 4);
    }

    #[test]
    fn search_articles_matches_title_or_note_case_insensitively_across_all_articles() {
        let storage = test_storage();
        let btrfs = Article::new(
            "Btrfs send/receive got faster".to_string(),
            "https://example.com/btrfs".to_string(),
            "LWN.net".to_string(),
            "linux-news".to_string(),
            Utc::now(),
        );
        let wayland = Article::new(
            "Wayland compositor update".to_string(),
            "https://example.com/wayland".to_string(),
            "Phoronix".to_string(),
            "linux-news".to_string(),
            Utc::now(),
        );
        let btrfs_id = storage.insert_article(&btrfs).unwrap();
        storage.insert_article(&wayland).unwrap();

        // Matches by title, mixed case -- SQLite's LIKE is case-insensitive
        // for ASCII by default.
        let by_title = storage.search_articles("BTRFS").unwrap();
        assert_eq!(by_title.len(), 1);
        assert_eq!(by_title[0].title, "Btrfs send/receive got faster");

        // Matches by note content too, even though the query text doesn't
        // appear anywhere in the title -- and even though this article was
        // never saved, since search runs across all articles, not just
        // saved ones.
        storage
            .update_note(btrfs_id, Some("worth trying this weekend".to_string()))
            .unwrap();
        let by_note = storage.search_articles("weekend").unwrap();
        assert_eq!(by_note.len(), 1);
        assert_eq!(by_note[0].id, btrfs_id);

        // No match at all comes back empty, not an error.
        assert!(
            storage
                .search_articles("nonexistent-keyword")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn skip_weights_start_at_zero_and_increment() {
        let storage = test_storage();

        assert_eq!(storage.skip_weight("systemd").unwrap(), 0);

        storage.increment_skip_weight("systemd").unwrap();
        storage.increment_skip_weight("systemd").unwrap();
        storage.increment_skip_weight("wayland").unwrap();

        assert_eq!(storage.skip_weight("systemd").unwrap(), 2);
        assert_eq!(storage.skip_weight("wayland").unwrap(), 1);

        let all = storage.all_skip_weights().unwrap();
        assert_eq!(
            all,
            vec![("systemd".to_string(), 2), ("wayland".to_string(), 1)]
        );
    }

    #[test]
    fn topics_lists_distinct_topics_alphabetically() {
        let storage = test_storage();
        for (i, topic) in ["linux-news", "gaming", "linux-news"]
            .into_iter()
            .enumerate()
        {
            // Distinct `url`s per row -- `url` is unique now (migration
            // 002), and this test wants three real rows, not one row two
            // of these inserts silently collide into.
            let article = Article::new(
                "t".to_string(),
                format!("u{i}"),
                "s".to_string(),
                topic.to_string(),
                Utc::now(),
            );
            storage.insert_article(&article).unwrap();
        }

        assert_eq!(
            storage.topics(&Scope::All).unwrap(),
            vec!["gaming", "linux-news"]
        );
    }

    /// A plain test article from source `source` with article URL `url`
    /// and no `feed_url`, as if stored before migration 005.
    fn old_article(url: &str, source: &str) -> Article {
        Article::new(
            "title".to_string(),
            url.to_string(),
            source.to_string(),
            "linux".to_string(),
            Utc::now(),
        )
    }

    /// `feed_url` is stored and read back, and a fresh in-memory database
    /// gets migration 005 (the column exists).
    #[test]
    fn feed_url_round_trips() {
        let storage = test_storage();
        let mut article = old_article("https://example.com/a", "LWN");
        article.feed_url = Some("https://lwn.net/feed".to_string());
        storage.insert_article(&article).unwrap();
        let stored = &storage.articles_by_topic(&Scope::All, "linux").unwrap()[0];
        assert_eq!(stored.feed_url.as_deref(), Some("https://lwn.net/feed"));
    }

    /// Re-inserting an article fills in a missing `feed_url`, never
    /// replaces an existing one, and never touches read/saved state.
    #[test]
    fn insert_fills_missing_feed_url_only() {
        let storage = test_storage();
        let id = storage
            .insert_article(&old_article("https://example.com/a", "LWN"))
            .unwrap();
        storage
            .save_article(id, Some("keep me".to_string()))
            .unwrap();

        let mut again = old_article("https://example.com/a", "LWN");
        again.feed_url = Some("https://lwn.net/feed".to_string());
        assert_eq!(storage.insert_article(&again).unwrap(), id);

        let mut other = old_article("https://example.com/a", "LWN");
        other.feed_url = Some("https://elsewhere/feed".to_string());
        storage.insert_article(&other).unwrap();

        let stored = &storage.articles_by_topic(&Scope::All, "linux").unwrap()[0];
        assert_eq!(stored.feed_url.as_deref(), Some("https://lwn.net/feed"));
        assert!(stored.saved && stored.read);
        assert_eq!(stored.note.as_deref(), Some("keep me"));
        assert_eq!(storage.article_count().unwrap(), 1);
    }

    /// The backfill fills NULL `feed_url`s by unique source name, skips
    /// names that map to more than one feed, leaves rows that already have
    /// a `feed_url` alone, and is harmless to run twice.
    #[test]
    fn backfill_feed_urls_by_unambiguous_name() {
        let storage = test_storage();
        storage
            .insert_article(&old_article("https://example.com/1", "LWN"))
            .unwrap();
        storage
            .insert_article(&old_article("https://example.com/2", "LWN"))
            .unwrap();
        storage
            .insert_article(&old_article("https://example.com/3", "News"))
            .unwrap();
        storage
            .insert_article(&old_article("https://example.com/4", "Gone"))
            .unwrap();
        let mut known = old_article("https://example.com/5", "LWN");
        known.feed_url = Some("https://old/feed".to_string());
        storage.insert_article(&known).unwrap();

        let sources = vec![
            ("LWN".to_string(), "https://lwn.net/feed".to_string()),
            ("LWN".to_string(), "https://lwn.net/feed".to_string()),
            ("News".to_string(), "https://a.example/news".to_string()),
            ("News".to_string(), "https://b.example/news".to_string()),
        ];
        assert_eq!(storage.backfill_feed_urls(&sources).unwrap(), 2);
        assert_eq!(storage.backfill_feed_urls(&sources).unwrap(), 0);

        let feed_of = |url: &str| {
            storage
                .all_articles()
                .unwrap()
                .into_iter()
                .find(|a| a.url == url)
                .unwrap()
                .feed_url
        };
        assert_eq!(
            feed_of("https://example.com/1").as_deref(),
            Some("https://lwn.net/feed")
        );
        assert_eq!(
            feed_of("https://example.com/2").as_deref(),
            Some("https://lwn.net/feed")
        );
        assert_eq!(feed_of("https://example.com/3"), None, "ambiguous name");
        assert_eq!(feed_of("https://example.com/4"), None, "not configured");
        assert_eq!(
            feed_of("https://example.com/5").as_deref(),
            Some("https://old/feed")
        );
    }

    /// A database that is already at `user_version = 4` (one that ever ran
    /// the archived FreshRSS build, which used version 4) still gets
    /// migration 005's `feed_url` column. This is why the migration is
    /// numbered 5.
    #[test]
    fn migration_005_runs_on_a_version_4_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../migrations/001_init.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../../migrations/002_unique_article_url.sql"))
            .unwrap();
        conn.execute_batch(include_str!(
            "../../migrations/003_add_saved_and_noted_at.sql"
        ))
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 4").unwrap();

        let storage = Storage::from_connection(conn).unwrap();
        let mut article = old_article("https://example.com/a", "LWN");
        article.feed_url = Some("https://lwn.net/feed".to_string());
        storage.insert_article(&article).unwrap();
        assert_eq!(
            storage.all_articles().unwrap()[0].feed_url.as_deref(),
            Some("https://lwn.net/feed")
        );
        let version: u32 = storage
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        // 005 ran, and then 006 to 010 after it, so the database ends at the
        // latest version.
        assert_eq!(version, 11);
    }

    /// A fresh database has the `feeds` and `settings` tables (migration 006).
    #[test]
    fn migration_006_creates_feeds_and_settings() {
        let storage = test_storage();
        let table_count: i64 = storage
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('feeds', 'settings')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 2);
    }

    /// Feeds can be added, listed in topic-then-name order, and removed.
    #[test]
    fn add_list_and_remove_feeds() {
        let storage = test_storage();
        assert!(storage.list_feeds().unwrap().is_empty());

        let lwn = storage
            .add_feed("LWN", "https://lwn.net/feed", "linux", SourceType::Rss)
            .unwrap();
        storage
            .add_feed("Ars", "https://ars/feed", "tech", SourceType::Rss)
            .unwrap();
        storage
            .add_feed("Arch", "https://arch/feed", "linux", SourceType::Rss)
            .unwrap();

        // Ordered by topic ("linux" before "tech"), then name ("Arch" before
        // "LWN").
        let names: Vec<String> = storage
            .list_feeds()
            .unwrap()
            .into_iter()
            .map(|feed| feed.name)
            .collect();
        assert_eq!(names, ["Arch", "LWN", "Ars"]);

        assert!(storage.remove_feed(lwn).unwrap());
        assert_eq!(storage.list_feeds().unwrap().len(), 2);
        // Removing it again finds nothing to remove.
        assert!(!storage.remove_feed(lwn).unwrap());
    }

    /// Adding the same URL twice fails with a clear message and does not
    /// create a second row.
    #[test]
    fn adding_a_duplicate_feed_url_is_an_error() {
        let storage = test_storage();
        storage
            .add_feed("LWN", "https://lwn.net/feed", "linux", SourceType::Rss)
            .unwrap();
        let err = storage
            .add_feed(
                "LWN again",
                "https://lwn.net/feed",
                "other",
                SourceType::Rss,
            )
            .unwrap_err();
        assert!(err.to_string().contains("already added"));
        assert_eq!(storage.list_feeds().unwrap().len(), 1);
    }

    /// Removing a feed leaves its articles (and their saved state) in the
    /// database.
    #[test]
    fn removing_a_feed_keeps_its_articles() {
        let storage = test_storage();
        let feed_id = storage
            .add_feed("LWN", "https://lwn.net/feed", "linux", SourceType::Rss)
            .unwrap();
        let article_id = storage
            .insert_article(&from_feed("https://e/1", "linux", "https://lwn.net/feed"))
            .unwrap();
        storage
            .save_article(article_id, Some("keep".to_string()))
            .unwrap();

        storage.remove_feed(feed_id).unwrap();

        assert_eq!(storage.article_count().unwrap(), 1);
        assert_eq!(storage.saved_articles().unwrap().len(), 1);
    }

    /// `articles_by_feed` returns only that feed's articles, whatever topic
    /// they were filed under.
    /// A renamed or re-filed feed still shows its old articles when found by
    /// address; articles with no stored address still match by name; another
    /// feed's articles never leak in.
    #[test]
    fn a_feed_row_is_found_by_address_not_name() {
        let storage = test_storage();
        // Two articles fetched under the old name and topic.
        storage
            .insert_article(&from_feed("https://e/1", "old-topic", "https://a/feed"))
            .unwrap();
        storage
            .insert_article(&from_feed("https://e/2", "old-topic", "https://a/feed"))
            .unwrap();
        // Another feed that happens to share the new name.
        storage
            .insert_article(&from_feed("https://e/3", "new-topic", "https://b/feed"))
            .unwrap();
        // An old article from before addresses were recorded.
        let mut old = from_feed("https://e/4", "new-topic", "https://zzz");
        old.feed_url = None;
        old.source = "New Name".to_string();
        storage.insert_article(&old).unwrap();

        // The row now called "New Name" in "new-topic", whose feed is a/feed:
        // both old articles show, plus the one that matches by name.
        let shown = storage
            .articles_by_feed_or_name(&Scope::All, "new-topic", "New Name", Some("https://a/feed"))
            .unwrap();
        assert_eq!(shown.len(), 3);
        // The other feed's article is not among them.
        assert!(shown.iter().all(|a| a.url != "https://e/3"));

        // With no known address only the name match applies.
        let by_name = storage
            .articles_by_feed_or_name(&Scope::All, "new-topic", "New Name", None)
            .unwrap();
        assert_eq!(by_name.len(), 1);
    }

    #[test]
    fn articles_by_feed_ignores_topic() {
        let storage = test_storage();
        storage
            .insert_article(&from_feed("https://e/1", "linux", "https://a/feed"))
            .unwrap();
        storage
            .insert_article(&from_feed("https://e/2", "old-topic", "https://a/feed"))
            .unwrap();
        storage
            .insert_article(&from_feed("https://e/3", "linux", "https://b/feed"))
            .unwrap();

        assert_eq!(storage.articles_by_feed("https://a/feed").unwrap().len(), 2);
        assert_eq!(storage.articles_by_feed("https://b/feed").unwrap().len(), 1);
        assert!(storage.articles_by_feed("https://none").unwrap().is_empty());
    }

    /// A setting is absent until saved, reads back after saving, and saving
    /// again replaces the value.
    #[test]
    fn settings_round_trip_and_overwrite() {
        let storage = test_storage();
        assert_eq!(storage.get_setting("theme").unwrap(), None);

        storage.set_setting("theme", "Nord").unwrap();
        assert_eq!(
            storage.get_setting("theme").unwrap().as_deref(),
            Some("Nord")
        );

        storage.set_setting("theme", "Gruvbox").unwrap();
        assert_eq!(
            storage.get_setting("theme").unwrap().as_deref(),
            Some("Gruvbox")
        );
    }

    /// A stored feed converts into the `SourceConfig` the fetchers expect.
    #[test]
    fn feed_converts_to_source_config() {
        let storage = test_storage();
        storage
            .add_feed("LWN", "https://lwn.net/feed", "linux", SourceType::Rss)
            .unwrap();
        let config = storage.list_feeds().unwrap()[0].to_source_config();
        assert_eq!(config.name, "LWN");
        assert_eq!(config.url, "https://lwn.net/feed");
        assert_eq!(config.topic, "linux");
        assert_eq!(config.source_type, SourceType::Rss);
    }

    /// An article in topic `topic` from feed `feed`, unread.
    fn from_feed(url: &str, topic: &str, feed: &str) -> Article {
        let mut article = Article::new(
            "title".to_string(),
            url.to_string(),
            "source".to_string(),
            topic.to_string(),
            Utc::now(),
        );
        article.feed_url = Some(feed.to_string());
        article
    }

    /// A `Feeds` scope limits topics, topic articles, and unread counts to
    /// those feeds. `All` sees everything, including articles with no
    /// `feed_url`, and an empty `Feeds` sees nothing.
    #[test]
    fn scope_limits_topic_views_to_a_lists_feeds() {
        let storage = test_storage();
        storage
            .insert_article(&from_feed("https://e/1", "linux", "https://main/feed"))
            .unwrap();
        storage
            .insert_article(&from_feed("https://e/2", "gaming", "https://casual/feed"))
            .unwrap();
        let read_id = storage
            .insert_article(&from_feed("https://e/3", "linux", "https://casual/feed"))
            .unwrap();
        storage.mark_read(read_id).unwrap();
        storage
            .insert_article(&old_article("https://e/4", "Old"))
            .unwrap();

        let main = Scope::Feeds(vec!["https://main/feed".to_string()]);
        let casual = Scope::Feeds(vec!["https://casual/feed".to_string()]);

        assert_eq!(storage.topics(&main).unwrap(), ["linux"]);
        assert_eq!(storage.topics(&casual).unwrap(), ["gaming", "linux"]);
        assert_eq!(storage.topics(&Scope::All).unwrap(), ["gaming", "linux"]);

        assert_eq!(storage.articles_by_topic(&main, "linux").unwrap().len(), 1);
        assert_eq!(
            storage.articles_by_topic(&casual, "linux").unwrap().len(),
            1
        );
        assert_eq!(
            storage
                .articles_by_topic(&Scope::All, "linux")
                .unwrap()
                .len(),
            3
        );
        assert_eq!(
            storage
                .articles_by_topic_and_source(&casual, "gaming", "source")
                .unwrap()
                .len(),
            1
        );

        assert_eq!(storage.unread_count(&main).unwrap(), 1);
        assert_eq!(storage.unread_count(&casual).unwrap(), 1);
        assert_eq!(storage.unread_count(&Scope::All).unwrap(), 3);

        let nothing = Scope::Feeds(Vec::new());
        assert!(storage.topics(&nothing).unwrap().is_empty());
        assert_eq!(storage.unread_count(&nothing).unwrap(), 0);
    }

    /// A fresh database has the `summary` column (migration 007), and a
    /// summary is stored and read back.
    #[test]
    fn summary_round_trips() {
        let storage = test_storage();
        let mut article = old_article("https://example.com/a", "LWN");
        article.summary = Some("A short summary.".to_string());
        let id = storage.insert_article(&article).unwrap();

        let stored = storage.article_by_id(id).unwrap().unwrap();
        assert_eq!(stored.summary.as_deref(), Some("A short summary."));
        // Every list query returns it too.
        let listed = &storage.all_articles().unwrap()[0];
        assert_eq!(listed.summary.as_deref(), Some("A short summary."));
    }

    /// Re-fetching an article fills in a missing summary, keeps an existing
    /// one, and never touches read/saved/note state.
    #[test]
    fn insert_fills_missing_summary_only() {
        let storage = test_storage();
        // First fetch: the feed had no description.
        let id = storage
            .insert_article(&old_article("https://example.com/a", "LWN"))
            .unwrap();
        storage
            .save_article(id, Some("keep me".to_string()))
            .unwrap();
        assert_eq!(storage.article_by_id(id).unwrap().unwrap().summary, None);

        // Second fetch: now it has one. It is filled in.
        let mut again = old_article("https://example.com/a", "LWN");
        again.summary = Some("First summary.".to_string());
        assert_eq!(storage.insert_article(&again).unwrap(), id);

        // Third fetch with different text: the first one is kept.
        let mut other = old_article("https://example.com/a", "LWN");
        other.summary = Some("Changed later.".to_string());
        storage.insert_article(&other).unwrap();

        let stored = storage.article_by_id(id).unwrap().unwrap();
        assert_eq!(stored.summary.as_deref(), Some("First summary."));
        assert!(stored.saved && stored.read);
        assert_eq!(stored.note.as_deref(), Some("keep me"));
        assert_eq!(storage.article_count().unwrap(), 1);
    }

    /// Looking up an article that does not exist is `None`, not an error.
    #[test]
    fn article_by_id_of_a_missing_article_is_none() {
        let storage = test_storage();
        assert!(storage.article_by_id(12345).unwrap().is_none());
    }

    /// A database already at version 6 (everything before this migration) gets
    /// the `summary` column when it is next opened, and its old articles read
    /// back with no summary.
    #[test]
    fn migration_007_runs_on_a_version_6_database() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../migrations/001_init.sql"))
            .unwrap();
        conn.execute_batch(include_str!("../../migrations/002_unique_article_url.sql"))
            .unwrap();
        conn.execute_batch(include_str!(
            "../../migrations/003_add_saved_and_noted_at.sql"
        ))
        .unwrap();
        conn.execute_batch(include_str!("../../migrations/005_add_feed_url.sql"))
            .unwrap();
        conn.execute_batch(include_str!(
            "../../migrations/006_add_feeds_and_settings.sql"
        ))
        .unwrap();
        // An article that was stored before the summary column existed.
        conn.execute(
            "INSERT INTO articles (title, url, source, topic, timestamp)
             VALUES ('Old', 'https://example.com/old', 'LWN', 'linux', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        conn.execute_batch("PRAGMA user_version = 6").unwrap();

        let storage = Storage::from_connection(conn).unwrap();
        let old = &storage.all_articles().unwrap()[0];
        assert_eq!(old.title, "Old");
        assert_eq!(old.summary, None);
    }

    /// Origins are the distinct (topic, source, feed) of the given IDs: two
    /// articles from one feed give one origin, an unknown ID is ignored, and
    /// an empty set needs no query.
    #[test]
    fn article_origins_are_distinct_per_feed() {
        let storage = test_storage();
        let mut ids = HashSet::new();
        for (n, (source, topic, feed)) in [
            ("Arch", "linux", "https://arch/feed"),
            ("Arch", "linux", "https://arch/feed"),
            ("Overtake", "sim-racing", "import:overtake"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut article = Article::new(
                format!("title {n}"),
                format!("https://example.com/{n}"),
                source.to_string(),
                topic.to_string(),
                Utc::now(),
            );
            article.feed_url = Some(feed.to_string());
            ids.insert(storage.insert_article(&article).unwrap());
        }
        // An ID that does not exist is ignored.
        ids.insert(9999);

        let origins = storage.article_origins(&ids).unwrap();
        assert_eq!(origins.len(), 2);
        assert_eq!(origins[0].source, "Arch");
        assert_eq!(origins[1].feed_url.as_deref(), Some("import:overtake"));
        assert!(storage.article_origins(&HashSet::new()).unwrap().is_empty());
    }
}
