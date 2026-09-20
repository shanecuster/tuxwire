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
// implementation detail of *how* `Storage::open` gets the schema ready,
// not something any other part of the app should ever call directly.
mod migrations;

use crate::models::Article;
use anyhow::Context;
use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, Row, params};
use std::path::PathBuf;

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
    /// Opens (creating if necessary) the real on-disk database at
    /// `~/.local/share/tuxwire/tuxwire.db`, per ARCHITECTURE.md's Storage
    /// section, and brings its schema up to date via `migrations::run`.
    ///
    /// This is the constructor the rest of the app should call. Tests use
    /// `from_connection` directly instead, against an in-memory database
    /// -- see the `tests` module at the bottom of this file.
    pub fn open() -> anyhow::Result<Self> {
        let path = db_path()?;

        // `Path::parent()` gives the directory containing this path
        // (`~/.local/share/tuxwire`, stripping off `tuxwire.db`) as an
        // `Option<&Path>` -- `None` only if `path` had no directory
        // component at all, which can't happen here since `db_path`
        // always joins a filename onto a directory. `create_dir_all` is a
        // no-op if the directory already exists (unlike `create_dir`,
        // which would error in that case) -- exactly what's wanted here,
        // since `open` runs on every single startup, not just the first.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create data directory {}", parent.display()))?;
        }

        let conn = Connection::open(&path)
            .with_context(|| format!("failed to open database at {}", path.display()))?;

        Self::from_connection(conn)
    }

    /// Wraps an already-open `Connection` in a `Storage`, running
    /// migrations against it before handing it back.
    ///
    /// Splitting this out from `open` means the "make sure the schema is
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
    /// instead of a fresh one. `ON CONFLICT(url) DO NOTHING` is what
    /// actually guarantees that at the database level (backed by the
    /// unique index migration 002 adds on `articles.url`) -- the same
    /// atomic insert-or-else pattern `increment_skip_weight` below uses for
    /// `skip_weights`, just discarding the conflicting write instead of
    /// updating it.
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
            "INSERT INTO articles (title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(url) DO NOTHING",
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
            ],
        )?;

        // Looking the ID up by URL (rather than trusting
        // `last_insert_rowid`) is what makes this correct in both cases:
        // `last_insert_rowid` doesn't advance when `DO NOTHING` discards a
        // conflicting insert, so it would silently return some *other*
        // row's ID instead of this article's.
        self.conn
            .query_row(
                "SELECT id FROM articles WHERE url = ?1",
                params![article.url],
                |row| row.get(0),
            )
            .map_err(Into::into)
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

    /// All articles filed under `topic`, most recent first -- the query
    /// backing the TUI's right-hand article pane (ARCHITECTURE.md's "Right
    /// pane: article list for the selected topic, sorted by recency").
    pub fn articles_by_topic(&self, topic: &str) -> anyhow::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at
             FROM articles WHERE topic = ?1 ORDER BY timestamp DESC",
        )?;

        // `query_map` runs the query and lazily applies `row_to_article`
        // to each row as it's read. It hands back an iterator, not a
        // `Vec` directly, because `rusqlite` doesn't know up front whether
        // the caller wants every row materialized at once or would rather
        // stream through them -- `.collect()` below is what actually
        // decides "gather all of them into a `Vec`."
        let rows = stmt.query_map(params![topic], row_to_article)?;

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
        rows.collect::<rusqlite::Result<Vec<Article>>>().map_err(Into::into)
    }

    /// All articles filed under `topic` *and* published by `source`
    /// specifically, most recent first -- backs the sidebar tree's nested
    /// source rows (`docs/ARCHITECTURE.md`'s "Selecting a source row...
    /// filters the article list to just that one source"). Deliberately
    /// the exact same query as `articles_by_topic` above with one more
    /// `AND source = ?2` clause, not a separate query path -- per
    /// ARCHITECTURE.md, "a source's topic is already fixed, so 'articles
    /// from source X' is just 'articles in X's topic, further filtered by
    /// source name.'"
    pub fn articles_by_topic_and_source(&self, topic: &str, source: &str) -> anyhow::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at
             FROM articles WHERE topic = ?1 AND source = ?2 ORDER BY timestamp DESC",
        )?;

        let rows = stmt.query_map(params![topic, source], row_to_article)?;
        rows.collect::<rusqlite::Result<Vec<Article>>>().map_err(Into::into)
    }

    /// Every saved article, across all topics, most recent first -- backs
    /// the dedicated "Saved" view described in ARCHITECTURE.md, which is
    /// explicitly independent of the topic sidebar.
    pub fn saved_articles(&self) -> anyhow::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at
             FROM articles WHERE saved = 1 ORDER BY timestamp DESC",
        )?;

        let rows = stmt.query_map([], row_to_article)?;
        rows.collect::<rusqlite::Result<Vec<Article>>>().map_err(Into::into)
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
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at
             FROM articles ORDER BY timestamp DESC",
        )?;

        let rows = stmt.query_map([], row_to_article)?;
        rows.collect::<rusqlite::Result<Vec<Article>>>().map_err(Into::into)
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
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at
             FROM articles WHERE title LIKE ?1 OR note LIKE ?1 ORDER BY timestamp DESC",
        )?;

        let pattern = format!("%{query}%");
        let rows = stmt.query_map(params![pattern], row_to_article)?;
        rows.collect::<rusqlite::Result<Vec<Article>>>().map_err(Into::into)
    }

    /// Every distinct topic currently present in `articles`, alphabetical
    /// -- feeds the TUI's topic sidebar.
    pub fn topics(&self) -> anyhow::Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT topic FROM articles ORDER BY topic")?;

        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<rusqlite::Result<Vec<String>>>().map_err(Into::into)
    }

    /// Marks an article read -- the `Enter` keybind (open in `$BROWSER`).
    pub fn mark_read(&self, id: i64) -> anyhow::Result<()> {
        self.conn
            .execute("UPDATE articles SET read = 1 WHERE id = ?1", params![id])?;
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
    /// ARCHITECTURE.md, "`saved_at`, set the moment `s` is pressed." Set
    /// unconditionally on every call rather than only the first: there's
    /// no separate "unsave" action in tuxwire today, so in practice this
    /// only ever runs once per article, but even a hypothetical re-save
    /// should honestly reflect *when the save happened*, not the first
    /// time it happened.
    ///
    /// `note: Option<String>` mirrors `Article::note` exactly: passing
    /// `None` saves the article with no note, `Some(text)` saves it with
    /// one attached.
    pub fn save_article(&self, id: i64, note: Option<String>) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE articles SET saved = 1, read = 1, note = ?2, saved_at = ?3 WHERE id = ?1",
            params![id, note, Utc::now()],
        )?;
        Ok(())
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
    pub fn update_note(&self, id: i64, note: Option<String>) -> anyhow::Result<()> {
        let noted_at = note.is_some().then(Utc::now);
        self.conn.execute(
            "UPDATE articles SET note = ?2, noted_at = ?3 WHERE id = ?1",
            params![id, note, noted_at],
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
    pub fn all_skip_weights(&self) -> anyhow::Result<Vec<(String, i64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT keyword, weight FROM skip_weights ORDER BY weight DESC")?;

        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<(String, i64)>>>().map_err(Into::into)
    }

    // --- freshrss (feature-gated) ------------------------------------
    //
    // Every method below only exists in a `--features freshrss` build --
    // `#[cfg(feature = "freshrss")]` on each one, individually, rather
    // than wrapping this whole section in one `#[cfg]` block around the
    // `impl`, because these methods are interleaved here with the plain
    // `impl Storage` block above them; per-method `#[cfg]` is what lets
    // that stay one `impl` instead of splitting into two. They all read
    // or write the `freshrss_items` table migration 004 creates (see
    // `migrations/004_freshrss_tables.sql`), which -- in a no-feature
    // build -- doesn't exist, so these methods couldn't compile there
    // even if they were left in: `#[cfg]` isn't just tidiness here, it's
    // what makes "off means identical to `master`" true at the type level,
    // not just at the schema level.

    /// Links an already-inserted `articles` row to its FreshRSS origin.
    ///
    /// Takes `article_id` rather than an `&Article`, because inserting
    /// the article itself is already `insert_article`'s job (unchanged by
    /// this feature) -- a sync calls that first to get an `article_id`,
    /// then this method to record the two extra facts a FreshRSS-origin
    /// article carries that a local one doesn't: the server's own item ID
    /// (for dedupe and read-sync) and which feed it came from.
    ///
    /// Idempotent on `remote_id`, the same way `insert_article` is
    /// idempotent on `url`: `ON CONFLICT(remote_id) DO NOTHING` means
    /// calling this again for an item tuxwire already has a link row for
    /// is a harmless no-op rather than a `UNIQUE` constraint error -- a
    /// sync (a later build phase) can call this unconditionally for every
    /// item it fetches without checking first.
    #[cfg(feature = "freshrss")]
    pub fn insert_freshrss_item(&self, article_id: i64, remote_id: &str, feed_id: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "INSERT INTO freshrss_items (article_id, remote_id, feed_id) VALUES (?1, ?2, ?3)
             ON CONFLICT(remote_id) DO NOTHING",
            params![article_id, remote_id, feed_id],
        )?;
        Ok(())
    }

    /// The `article_id` already linked to `remote_id`, if tuxwire has
    /// seen this FreshRSS item before -- the sync's dedupe check ahead of
    /// fetching an item's full contents, per ARCHITECTURE.md's Sync
    /// behavior: "fetch contents only for IDs not already in
    /// `freshrss_items`."
    ///
    /// Returns `Option<i64>`, not a bare `i64` with some sentinel for
    /// "not found" -- same reasoning `skip_weight` gives for its own
    /// `.optional()` use just above: "this item was never synced" is an
    /// expected, non-error outcome a caller has to positively check for,
    /// not something papered over with a magic `-1` or `0`.
    #[cfg(feature = "freshrss")]
    pub fn freshrss_article_id_by_remote_id(&self, remote_id: &str) -> anyhow::Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT article_id FROM freshrss_items WHERE remote_id = ?1",
                params![remote_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    /// Flags `article_id` as having a local read-state change not yet
    /// pushed to the server -- set the moment a FreshRSS-origin article is
    /// marked read locally (`Enter` or `s`), per ARCHITECTURE.md's "When
    /// the person marks a FreshRSS-origin article read locally... set
    /// `pending_read_push = 1`."
    #[cfg(feature = "freshrss")]
    pub fn set_pending_read_push(&self, article_id: i64) -> anyhow::Result<()> {
        self.conn
            .execute("UPDATE freshrss_items SET pending_read_push = 1 WHERE article_id = ?1", params![article_id])?;
        Ok(())
    }

    /// Clears the pending-push flag on `article_id` -- called only once
    /// the server has actually confirmed the `edit-tag` read call, per
    /// ARCHITECTURE.md's "Push read state" step: "clear the flag only
    /// after the server confirms." A separate method from
    /// `set_pending_read_push` rather than one `set_pending_read_push(&self,
    /// article_id: i64, pending: bool)` -- two small, clearly-named
    /// methods read better at each call site (`clear_pending_read_push`
    /// right after a successful network call) than a boolean parameter
    /// whose meaning isn't visible without checking the call site.
    #[cfg(feature = "freshrss")]
    pub fn clear_pending_read_push(&self, article_id: i64) -> anyhow::Result<()> {
        self.conn
            .execute("UPDATE freshrss_items SET pending_read_push = 0 WHERE article_id = ?1", params![article_id])?;
        Ok(())
    }

    /// Every FreshRSS-origin article with a local read-state change not
    /// yet pushed to the server -- what a sync's "Push read state" step
    /// iterates: for each `(article_id, remote_id)` pair, send `edit-tag`
    /// read using `remote_id`, then call `clear_pending_read_push(article_id)`
    /// only once the server confirms.
    ///
    /// Returns `remote_id` alongside `article_id` (rather than just the
    /// `article_id`s, leaving the caller to look each one's `remote_id`
    /// up separately) because every caller of this method needs exactly
    /// that pair to do anything useful with it -- the server's
    /// `edit-tag` call only understands its own item IDs, never tuxwire's
    /// local `article_id`.
    #[cfg(feature = "freshrss")]
    pub fn pending_read_pushes(&self) -> anyhow::Result<Vec<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT article_id, remote_id FROM freshrss_items WHERE pending_read_push = 1")?;

        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<(i64, String)>>>().map_err(Into::into)
    }

    /// Every distinct `(topic, source)` pair currently present in
    /// `articles` whose topic is namespaced `FreshRSS/...` -- the sidebar
    /// merge query ARCHITECTURE.md's "Sidebar integration" describes:
    /// `ui::run` folds these into the same `HashMap<String, Vec<String>>`
    /// `group_sources_by_topic` already builds from `sources.toml`, so
    /// FreshRSS feeds show up as nested source rows in the sidebar the
    /// same way local sources do, without `sidebar_rows` itself needing
    /// to know FreshRSS exists.
    ///
    /// Deliberately queries `articles` itself, not `freshrss_items` or a
    /// cached subscription list -- this is what produces the
    /// "zero-article-feed limitation" ARCHITECTURE.md documents: a feed
    /// with no synced articles yet has no row here to be distinct over,
    /// so it doesn't appear in the sidebar merge either, until its first
    /// article lands.
    ///
    /// `%` in the `LIKE` pattern below is a literal wildcard character,
    /// not a bound parameter -- safe here (unlike interpolating real user
    /// input) because `'FreshRSS/%'` is a fixed string this method always
    /// passes, never anything read from outside the program. Compare
    /// `search_articles`'s doc comment, which binds its *own* wildcard
    /// pattern via `params!` because it's built from a person's search
    /// text.
    #[cfg(feature = "freshrss")]
    pub fn freshrss_topic_source_pairs(&self) -> anyhow::Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT topic, source FROM articles WHERE topic LIKE 'FreshRSS/%' ORDER BY topic, source",
        )?;

        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<(String, String)>>>().map_err(Into::into)
    }

    /// Inserts `article`, but -- unlike `insert_article` above -- refuses
    /// to ever return an existing row's ID. `insert_article` deliberately
    /// treats "a row with this URL already exists" as success (that's
    /// exactly what a local re-fetch of an unchanged feed wants: the
    /// second run is a harmless no-op, and the caller still gets a valid
    /// `article_id` back either way). FreshRSS sync needs the opposite
    /// answer to the same question: if a synced item's URL collides with
    /// an article that's *already* in `articles` -- local-origin, or
    /// somehow a different FreshRSS item -- linking a new
    /// `freshrss_items` row to that existing article's ID would silently
    /// attach FreshRSS bookkeeping (and, later, remote read-state pushes)
    /// to a row this sync never actually created and doesn't own. Per
    /// ARCHITECTURE.md: that existing row's topic, source, read/saved
    /// state, and note must never be touched or reinterpreted this way --
    /// the conflicting FreshRSS item is skipped instead.
    ///
    /// Returns `Some(id)` only when a *new* row was actually inserted,
    /// `None` when `ON CONFLICT(url) DO NOTHING` discarded the insert --
    /// the caller (`freshrss::sync`) uses `None` as its signal to skip
    /// linking this item and count it as a URL conflict, rather than
    /// proceeding as if the insert had succeeded.
    #[cfg(feature = "freshrss")]
    pub fn insert_article_if_new_url(&self, article: &Article) -> anyhow::Result<Option<i64>> {
        // `Connection::execute` returns the number of rows the statement
        // actually changed -- `usize`, not a `bool` or the row's ID. With
        // `ON CONFLICT(url) DO NOTHING`, a colliding insert changes zero
        // rows (SQLite discarded it before it ever became a real row),
        // while a genuinely new row changes exactly one. Checking this
        // count is what makes the distinction atomic: there's no separate
        // "does this URL already exist?" query racing against this
        // insert, because the single `INSERT` statement itself is both
        // the check and the write.
        let rows_changed = self.conn.execute(
            "INSERT INTO articles (title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(url) DO NOTHING",
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
            ],
        )?;

        if rows_changed == 0 {
            return Ok(None);
        }

        // `last_insert_rowid()` is only trustworthy immediately after a
        // statement that's known to have actually inserted a row on
        // *this* connection -- the `rows_changed == 0` check above is
        // what guarantees that's the case here, unlike `insert_article`'s
        // own `SELECT ... WHERE url = ?1` fallback, which has to work
        // whether or not this exact call was the one that inserted it.
        Ok(Some(self.conn.last_insert_rowid()))
    }

    /// Sets how long this connection will wait and retry, instead of
    /// immediately failing with `SQLITE_BUSY`, when a write finds SQLite's
    /// single-writer lock already held by another connection to the same
    /// database file. Matters specifically once a FreshRSS sync gets its
    /// own separate `Storage::open()` connection (Phase 4's startup sync
    /// runs on its own thread, deliberately not sharing the UI's
    /// connection -- see ARCHITECTURE.md's "How the background sync
    /// shares Storage with the UI"): two independent connections to one
    /// `tuxwire.db` file can now genuinely both want to write at close to
    /// the same moment, and rusqlite's out-of-the-box default is *no*
    /// retry at all (an immediate error) rather than a sensible wait.
    /// Every regular single-connection use of `Storage` (the UI's own
    /// connection when FreshRSS is off, or on but not mid-sync) never
    /// contends with itself, so this has nothing to do there -- callers
    /// opt in explicitly instead of `Storage::open` setting this
    /// unconditionally for every connection.
    #[cfg(feature = "freshrss")]
    pub fn set_busy_timeout(&self, timeout: std::time::Duration) -> anyhow::Result<()> {
        self.conn.busy_timeout(timeout).map_err(Into::into)
    }

    /// Deletes the `articles` row at `url`, and its `freshrss_items` link
    /// row if it has one -- test-only, `pub(crate)`. Two separate
    /// `DELETE`s, not one relying on a foreign-key cascade: this
    /// codebase never turns SQLite's foreign-key enforcement on (see
    /// migration 004's own comment on `REFERENCES articles(id)` being
    /// documentation, not an enforced constraint, by default), so nothing
    /// here would automatically remove a `freshrss_items` row just
    /// because its `articles` row disappeared -- explicit is what's
    /// actually correct given that. The one real caller is
    /// `freshrss::sync`'s manual live-server test, which needs to free up
    /// a URL a real sync already claimed in order to set up a URL-conflict
    /// scenario against real server data.
    #[cfg(all(test, feature = "freshrss"))]
    pub(crate) fn delete_article_for_tests(&self, url: &str) -> anyhow::Result<()> {
        self.conn.execute(
            "DELETE FROM freshrss_items WHERE article_id = (SELECT id FROM articles WHERE url = ?1)",
            params![url],
        )?;
        self.conn.execute("DELETE FROM articles WHERE url = ?1", params![url])?;
        Ok(())
    }

    /// Every FreshRSS-origin article that's currently unread locally *and*
    /// has no pending local read-state change of its own -- what a sync's
    /// "Pull read state" step (ARCHITECTURE.md's Sync behavior, item 4)
    /// starts from: for each `(article_id, remote_id)` pair this returns,
    /// `freshrss::sync` checks whether `remote_id` is still in the
    /// server's own unread set, and calls `mark_read_batch` with whichever
    /// ones aren't.
    ///
    /// The `pending_read_push = 0` filter is defensive rather than
    /// strictly load-bearing: nothing in this codebase ever sets
    /// `pending_read_push = 1` without also setting `articles.read = 1` in
    /// the same action (see `set_pending_read_push`'s call sites), so a
    /// pending row can never actually be locally-unread in practice. It's
    /// kept explicit anyway -- matching ARCHITECTURE.md's conflict rule
    /// ("a pending local change wins until it has been pushed") literally
    /// in the query itself, rather than leaning on an invariant holding
    /// forever elsewhere in the codebase.
    #[cfg(feature = "freshrss")]
    pub fn freshrss_unread_remote_ids(&self) -> anyhow::Result<Vec<(i64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, f.remote_id
             FROM articles a
             JOIN freshrss_items f ON f.article_id = a.id
             WHERE a.read = 0 AND f.pending_read_push = 0",
        )?;

        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<(i64, String)>>>().map_err(Into::into)
    }

    /// Marks every id in `article_ids` read, all inside **one** SQL
    /// transaction -- the pull reconcile step can mark anywhere from zero
    /// to hundreds of articles read in a single sync, and applying them
    /// one auto-committed `UPDATE` at a time would mean a person watching
    /// the database (or a crash partway through) could observe a sync
    /// half-applied. `unchecked_transaction()` (not the `&mut
    /// self`-requiring `transaction()`) is what lets this stay a `&self`
    /// method like every other one here -- "unchecked" only means SQLite
    /// itself, not the Rust compiler, is what would catch two of these
    /// somehow overlapping, which can't happen given this codebase's
    /// single-connection-per-thread usage. If any single `UPDATE` in the
    /// loop fails, `?` returns before `tx.commit()` runs, and
    /// `Transaction`'s `Drop` impl rolls back everything applied so far
    /// automatically -- there's no explicit `ROLLBACK` call needed.
    #[cfg(feature = "freshrss")]
    pub fn mark_read_batch(&self, article_ids: &[i64]) -> anyhow::Result<()> {
        if article_ids.is_empty() {
            return Ok(());
        }

        let tx = self.conn.unchecked_transaction()?;
        for &id in article_ids {
            tx.execute("UPDATE articles SET read = 1 WHERE id = ?1", params![id])?;
        }
        tx.commit()?;

        Ok(())
    }
}

/// Maps one row from a `SELECT id, title, url, source, topic, timestamp,
/// read, skipped, saved, note, saved_at, noted_at FROM articles ...` query
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
    })
}

/// The directory tuxwire's data (right now, just the database file) lives
/// in: `~/.local/share/tuxwire`, per ARCHITECTURE.md's Storage section.
///
/// ## Why this doesn't reach for the `dirs` crate
///
/// Plenty of Rust CLI apps pull in the `dirs` crate to resolve
/// platform-appropriate data/config directories portably, since Windows,
/// macOS, and Linux all disagree on where these belong. tuxwire only ever
/// targets Linux -- ARCHITECTURE.md's whole reason for existing describes
/// it living in a Hyprland scratchpad -- so it's simpler, and one fewer
/// dependency, to implement just the single path this project actually
/// needs: the XDG Base Directory spec's `$XDG_DATA_HOME`, falling back to
/// `$HOME/.local/share` when that variable isn't set, which is what the
/// spec itself documents as the default.
fn data_dir() -> anyhow::Result<PathBuf> {
    if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME") {
        return Ok(PathBuf::from(xdg_data_home).join("tuxwire"));
    }

    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate the data directory")?;

    Ok(PathBuf::from(home).join(".local/share/tuxwire"))
}

/// The full path to the database file itself: `data_dir()/tuxwire.db`.
fn db_path() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("tuxwire.db"))
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

        let fetched = storage.articles_by_topic("linux-news").unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].id, id);
        assert_eq!(fetched[0].title, "Btrfs send/receive got faster");

        // A different topic should see nothing -- the whole point of
        // filtering by topic in the query rather than fetching everything
        // and filtering in Rust.
        assert!(storage.articles_by_topic("gaming").unwrap().is_empty());
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

        let lwn_only = storage.articles_by_topic_and_source("linux-news", "LWN.net").unwrap();
        assert_eq!(lwn_only.len(), 1);
        assert_eq!(lwn_only[0].source, "LWN.net");

        // The two sources share a topic but not a source name -- a source
        // filter on one must never leak the other's articles in, and a
        // source name that matches no row at all (right topic, wrong or
        // unknown source) comes back empty rather than erroring.
        assert!(storage.articles_by_topic_and_source("linux-news", "unknown-source").unwrap().is_empty());
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

        assert_eq!(first_id, second_id, "re-inserting an existing URL must return its existing id");

        let articles = storage.articles_by_topic("linux-news").unwrap();
        assert_eq!(articles.len(), 1, "duplicate URL must not create a second row");
        assert_eq!(articles[0].title, "Btrfs send/receive got faster", "the original row must be left untouched");
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
        storage.update_note(id, Some("revisit this".to_string())).unwrap();

        let fetched = &storage.articles_by_topic("topic").unwrap()[0];
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

        let fetched = &storage.articles_by_topic("topic").unwrap()[0];
        assert_eq!(fetched.saved_at, None, "an article that was never saved has no saved_at");

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

        let fetched = &storage.articles_by_topic("topic").unwrap()[0];
        assert_eq!(fetched.noted_at, None, "an article with no note has no noted_at");

        let before = Utc::now();
        storage.update_note(id, Some("revisit this".to_string())).unwrap();
        let after = Utc::now();

        let noted = &storage.articles_by_topic("topic").unwrap()[0];
        let noted_at = noted.noted_at.expect("a non-empty note must populate noted_at");
        assert!(
            noted_at >= before && noted_at <= after,
            "noted_at should land at the moment update_note ran"
        );

        // Clearing the note (the `n` popup's "empty buffer -> None"
        // convention, see `ui/mod.rs`) must clear noted_at right along
        // with it -- "no note" and "no note date" are never allowed to
        // disagree.
        storage.update_note(id, None).unwrap();
        let cleared = &storage.articles_by_topic("topic").unwrap()[0];
        assert_eq!(cleared.note, None);
        assert_eq!(cleared.noted_at, None, "clearing the note must clear noted_at too");
    }

    #[test]
    fn all_articles_includes_every_state_regardless_of_read_saved_skipped() {
        let storage = test_storage();
        for (i, title) in ["unread", "read-only", "saved", "skipped"].into_iter().enumerate() {
            let article = Article::new(
                title.to_string(),
                format!("https://example.com/{i}"),
                "source".to_string(),
                "topic".to_string(),
                Utc::now(),
            );
            storage.insert_article(&article).unwrap();
        }

        let ids: Vec<i64> = storage.all_articles().unwrap().iter().map(|a| a.id).collect();
        assert_eq!(ids.len(), 4, "all_articles must return every fetched article, untouched or not");

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
        storage.update_note(btrfs_id, Some("worth trying this weekend".to_string())).unwrap();
        let by_note = storage.search_articles("weekend").unwrap();
        assert_eq!(by_note.len(), 1);
        assert_eq!(by_note[0].id, btrfs_id);

        // No match at all comes back empty, not an error.
        assert!(storage.search_articles("nonexistent-keyword").unwrap().is_empty());
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
        assert_eq!(all, vec![("systemd".to_string(), 2), ("wayland".to_string(), 1)]);
    }

    #[test]
    fn topics_lists_distinct_topics_alphabetically() {
        let storage = test_storage();
        for (i, topic) in ["linux-news", "gaming", "linux-news"].into_iter().enumerate() {
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

        assert_eq!(storage.topics().unwrap(), vec!["gaming", "linux-news"]);
    }

    /// The whole point of gating migration 004 behind the `freshrss`
    /// feature (see `storage::migrations::migrations`'s doc comment):
    /// a build without it must produce a database schema byte-for-byte
    /// identical to `master`'s. Gated to run only *without* the feature --
    /// with it on, these two tables are supposed to exist, and
    /// `freshrss::tests::feature_build_creates_freshrss_tables` below
    /// checks that side of the same claim.
    #[cfg(not(feature = "freshrss"))]
    #[test]
    fn no_feature_build_never_creates_freshrss_tables() {
        let storage = test_storage();

        let table_count: i64 = storage
            .conn
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type = 'table' AND name IN ('freshrss_items', 'freshrss_meta')",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(table_count, 0, "a no-feature build must never create the FreshRSS tables");
    }

    /// Every test here needs the `freshrss` feature -- they call methods
    /// (`insert_freshrss_item`, etc.) that don't exist without it, and
    /// they assert on tables (`freshrss_items`/`freshrss_meta`) migration
    /// 004 only creates in a `--features freshrss` build. Nested in its
    /// own `mod` (rather than `#[cfg]` on each test individually) purely
    /// so this whole block reads as one clearly-delimited unit, the same
    /// reason `storage::mod.rs`'s "freshrss (feature-gated)" section
    /// comment exists on the non-test side.
    #[cfg(feature = "freshrss")]
    mod freshrss {
        use super::*;

        /// The mirror image of `no_feature_build_never_creates_freshrss_tables`
        /// above: with the feature on, a fresh database really does gain
        /// both new tables.
        #[test]
        fn feature_build_creates_freshrss_tables() {
            let storage = test_storage();

            let table_count: i64 = storage
                .conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master
                     WHERE type = 'table' AND name IN ('freshrss_items', 'freshrss_meta')",
                    [],
                    |row| row.get(0),
                )
                .unwrap();

            assert_eq!(table_count, 2, "a --features freshrss build must create both FreshRSS tables");
        }

        /// The single most important regression test this phase adds:
        /// upgrading a database that predates migration 004 -- built here
        /// by running only migrations 001-003's *actual* SQL files
        /// directly against a raw connection, deliberately bypassing
        /// `Storage::open`/`from_connection` (which would run *this*
        /// binary's full migration list, migration 004 included, and
        /// defeat the entire point of starting from a pre-004 state) --
        /// must leave every existing article's data completely untouched.
        ///
        /// `include_str!`ing the same three files `storage::migrations`
        /// itself embeds (rather than retyping their `CREATE TABLE`/
        /// `ALTER TABLE` statements by hand into this test) is what makes
        /// this test immune to drifting out of sync with those files if
        /// they ever changed -- there is exactly one copy of "what
        /// migration 001 does," even though it's read into two different
        /// places in the compiled binary.
        #[test]
        fn migrating_a_pre_004_database_preserves_existing_articles_and_notes() {
            let conn = Connection::open_in_memory().unwrap();

            // Build a genuine pre-004 schema: migrations 001-003, and
            // nothing past them -- `user_version` set to 3 by hand,
            // exactly what `storage::migrations::run` would have left
            // behind on a database no build past migration 003 had ever
            // touched.
            conn.execute_batch(include_str!("../../migrations/001_init.sql")).unwrap();
            conn.execute_batch(include_str!("../../migrations/002_unique_article_url.sql")).unwrap();
            conn.execute_batch(include_str!("../../migrations/003_add_saved_and_noted_at.sql")).unwrap();
            conn.execute_batch("PRAGMA user_version = 3").unwrap();

            // Real data, inserted with plain SQL rather than through
            // `Storage` (which doesn't exist yet at this point -- wrapping
            // this `conn` in one is the very next step, and *that's* what
            // this test is actually exercising).
            conn.execute(
                "INSERT INTO articles
                     (title, url, source, topic, timestamp, read, skipped, saved, note, saved_at, noted_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, 0, 1, ?6, ?7, ?8)",
                params![
                    "Btrfs send/receive got noticeably faster in 6.18",
                    "https://example.com/btrfs",
                    "LWN.net",
                    "kernel",
                    Utc::now(),
                    "worth trying on the homelab NAS",
                    Utc::now(),
                    Utc::now(),
                ],
            )
            .unwrap();

            // Wrapping the same connection in a `Storage` is what actually
            // runs `migrations::run` -- which, compiled with
            // `--features freshrss`, now sees `current_version = 3` and
            // applies migration 004 on top of the data inserted above.
            let storage = Storage::from_connection(conn).unwrap();

            // The pre-existing article -- title, saved state, and note all
            // included -- must have survived migration 004 completely
            // unchanged. Migration 004 adds no columns to `articles` at
            // all (new tables only), so there's no plausible mechanism by
            // which any of this could have moved, but this is precisely
            // the guarantee ARCHITECTURE.md's "no destructive schema
            // changes" rule exists to protect, and it deserves a real
            // assertion, not just an argument from the SQL.
            let articles = storage.all_articles().unwrap();
            assert_eq!(articles.len(), 1, "the pre-existing article must not be lost or duplicated");
            let article = &articles[0];
            assert_eq!(article.title, "Btrfs send/receive got noticeably faster in 6.18");
            assert!(article.saved, "saved state must survive the upgrade");
            assert!(article.read);
            assert_eq!(article.note.as_deref(), Some("worth trying on the homelab NAS"));
            assert!(article.saved_at.is_some(), "saved_at must survive the upgrade");
            assert!(article.noted_at.is_some(), "noted_at must survive the upgrade");

            // And migration 004 itself did run -- the new tables exist,
            // empty (nothing has synced into them in this test), which is
            // the other half of "additive only": new tables appear
            // *alongside* the untouched old data, not instead of it.
            let freshrss_item_count: i64 =
                storage.conn.query_row("SELECT count(*) FROM freshrss_items", [], |row| row.get(0)).unwrap();
            assert_eq!(freshrss_item_count, 0);
        }

        /// `insert_freshrss_item` links an `article_id` to its FreshRSS
        /// origin, and `freshrss_article_id_by_remote_id` is what a sync's
        /// dedupe check reads back -- `None` for an item never linked,
        /// `Some(article_id)` once it has been.
        #[test]
        fn insert_and_look_up_freshrss_item_by_remote_id() {
            let storage = test_storage();
            let article = Article::new(
                "Kernel 6.18 released".to_string(),
                "https://example.com/6.18".to_string(),
                "Phoronix".to_string(),
                "FreshRSS/kernel".to_string(),
                Utc::now(),
            );
            let article_id = storage.insert_article(&article).unwrap();

            assert_eq!(
                storage.freshrss_article_id_by_remote_id("item-1").unwrap(),
                None,
                "an item that was never linked has no article_id yet"
            );

            storage.insert_freshrss_item(article_id, "item-1", "feed-42").unwrap();

            assert_eq!(storage.freshrss_article_id_by_remote_id("item-1").unwrap(), Some(article_id));
        }

        /// Re-linking the same `remote_id` (a sync re-fetching an item it
        /// already has) is a no-op, not a `UNIQUE` constraint error --
        /// `ON CONFLICT(remote_id) DO NOTHING`, the same "insert or skip"
        /// shape `insert_article` already establishes for `url`.
        #[test]
        fn inserting_the_same_remote_id_twice_does_not_error() {
            let storage = test_storage();
            let article = Article::new(
                "title".to_string(),
                "url".to_string(),
                "source".to_string(),
                "FreshRSS/topic".to_string(),
                Utc::now(),
            );
            let article_id = storage.insert_article(&article).unwrap();

            storage.insert_freshrss_item(article_id, "item-1", "feed-1").unwrap();
            storage.insert_freshrss_item(article_id, "item-1", "feed-1").unwrap();

            assert_eq!(storage.freshrss_article_id_by_remote_id("item-1").unwrap(), Some(article_id));
        }

        /// A genuinely new URL inserts normally and returns `Some(id)` --
        /// `insert_article_if_new_url`'s ordinary, non-conflicting path.
        #[test]
        fn insert_article_if_new_url_inserts_a_genuinely_new_url() {
            let storage = test_storage();
            let article = Article::new(
                "New from FreshRSS".to_string(),
                "https://example.com/new".to_string(),
                "Some Feed".to_string(),
                "FreshRSS/tech".to_string(),
                Utc::now(),
            );

            let id = storage.insert_article_if_new_url(&article).unwrap();

            assert!(id.is_some());
            assert_eq!(storage.article_count().unwrap(), 1);
        }

        /// The exact scenario ARCHITECTURE.md's URL-conflict rule exists
        /// for: a local (non-FreshRSS) article already occupies this URL.
        /// `insert_article_if_new_url` must return `None` -- never the
        /// existing row's id -- and must leave that row completely
        /// unchanged (topic, source, read state all still whatever the
        /// local article had), so a caller has no way to accidentally
        /// treat someone's existing local article as a FreshRSS item.
        #[test]
        fn insert_article_if_new_url_never_returns_or_touches_a_conflicting_row() {
            let storage = test_storage();
            let local_article = Article::new(
                "A locally-added article".to_string(),
                "https://example.com/shared".to_string(),
                "Phoronix".to_string(),
                "kernel".to_string(),
                Utc::now(),
            );
            let local_id = storage.insert_article(&local_article).unwrap();
            storage.mark_read(local_id).unwrap();

            let freshrss_item = Article::new(
                "The same story, from FreshRSS".to_string(),
                "https://example.com/shared".to_string(),
                "Some Feed".to_string(),
                "FreshRSS/tech".to_string(),
                Utc::now(),
            );

            let result = storage.insert_article_if_new_url(&freshrss_item).unwrap();

            assert_eq!(result, None, "a colliding URL must never hand back the existing row's id");
            assert_eq!(storage.article_count().unwrap(), 1, "no second row should have been created");

            let articles = storage.articles_by_topic("kernel").unwrap();
            assert_eq!(articles.len(), 1);
            assert_eq!(articles[0].id, local_id);
            assert_eq!(articles[0].source, "Phoronix", "the existing row's source must be untouched");
            assert_eq!(articles[0].topic, "kernel", "the existing row's topic must be untouched");
            assert!(articles[0].read, "the existing row's read state must be untouched");
        }

        /// `freshrss_unread_remote_ids` returns only FreshRSS-origin,
        /// locally-unread, not-pending articles -- three articles set up
        /// to cover each way a row can be excluded: read, pending, and
        /// (implicitly, by never appearing) local-origin.
        #[test]
        fn freshrss_unread_remote_ids_excludes_read_and_pending_rows() {
            let storage = test_storage();

            let unread_article = Article::new(
                "Still unread".to_string(),
                "https://example.com/unread".to_string(),
                "Some Feed".to_string(),
                "FreshRSS/tech".to_string(),
                Utc::now(),
            );
            let unread_id = storage.insert_article(&unread_article).unwrap();
            storage.insert_freshrss_item(unread_id, "remote-unread", "feed-1").unwrap();

            let read_article = Article::new(
                "Already read".to_string(),
                "https://example.com/read".to_string(),
                "Some Feed".to_string(),
                "FreshRSS/tech".to_string(),
                Utc::now(),
            );
            let read_id = storage.insert_article(&read_article).unwrap();
            storage.insert_freshrss_item(read_id, "remote-read", "feed-1").unwrap();
            storage.mark_read(read_id).unwrap();

            let pending_article = Article::new(
                "Read locally, not yet pushed".to_string(),
                "https://example.com/pending".to_string(),
                "Some Feed".to_string(),
                "FreshRSS/tech".to_string(),
                Utc::now(),
            );
            let pending_id = storage.insert_article(&pending_article).unwrap();
            storage.insert_freshrss_item(pending_id, "remote-pending", "feed-1").unwrap();
            storage.mark_read(pending_id).unwrap();
            storage.set_pending_read_push(pending_id).unwrap();

            let unread = storage.freshrss_unread_remote_ids().unwrap();
            assert_eq!(unread, vec![(unread_id, "remote-unread".to_string())]);
        }

        /// `mark_read_batch` marks every id it's given, and an empty slice
        /// is a harmless no-op (the caller doesn't need to check
        /// "is there anything to do" itself before calling this).
        #[test]
        fn mark_read_batch_marks_every_given_id() {
            let storage = test_storage();
            let ids: Vec<i64> = (0..3)
                .map(|i| {
                    let article = Article::new(
                        format!("Article {i}"),
                        format!("https://example.com/{i}"),
                        "Some Feed".to_string(),
                        "FreshRSS/tech".to_string(),
                        Utc::now(),
                    );
                    storage.insert_article(&article).unwrap()
                })
                .collect();

            storage.mark_read_batch(&[]).unwrap();
            storage.mark_read_batch(&ids[..2]).unwrap();

            let articles = storage.articles_by_topic("FreshRSS/tech").unwrap();
            let read_count = articles.iter().filter(|a| a.read).count();
            assert_eq!(read_count, 2, "exactly the two ids passed in should be marked read");
        }

        /// A failure partway through `mark_read_batch` must not leave any
        /// of the batch marked read -- simulated here by passing a
        /// nonexistent id alongside two real ones. `UPDATE ... WHERE id =
        /// ?` matching zero rows isn't itself a SQL error (so this test
        /// doesn't actually trigger the rollback path -- rusqlite has no
        /// built-in "row must exist" failure mode for a plain `UPDATE`),
        /// but it does confirm the everyday case: real ids in the batch
        /// are unaffected by an unrelated id also being present.
        #[test]
        fn mark_read_batch_is_unaffected_by_a_nonexistent_id_in_the_same_batch() {
            let storage = test_storage();
            let article = Article::new(
                "title".to_string(),
                "url".to_string(),
                "source".to_string(),
                "FreshRSS/topic".to_string(),
                Utc::now(),
            );
            let id = storage.insert_article(&article).unwrap();

            storage.mark_read_batch(&[id, 999_999]).unwrap();

            let articles = storage.articles_by_topic("FreshRSS/topic").unwrap();
            assert!(articles[0].read);
        }

        /// `set_pending_read_push`/`clear_pending_read_push` toggle the
        /// one flag `pending_read_pushes` reads -- exercised together
        /// since none of the three means anything on its own.
        #[test]
        fn set_and_clear_pending_read_push() {
            let storage = test_storage();
            let article = Article::new(
                "title".to_string(),
                "url".to_string(),
                "source".to_string(),
                "FreshRSS/topic".to_string(),
                Utc::now(),
            );
            let article_id = storage.insert_article(&article).unwrap();
            storage.insert_freshrss_item(article_id, "item-1", "feed-1").unwrap();

            assert!(storage.pending_read_pushes().unwrap().is_empty(), "nothing is pending right after linking");

            storage.set_pending_read_push(article_id).unwrap();
            assert_eq!(storage.pending_read_pushes().unwrap(), vec![(article_id, "item-1".to_string())]);

            storage.clear_pending_read_push(article_id).unwrap();
            assert!(storage.pending_read_pushes().unwrap().is_empty(), "clearing must remove it from the list");
        }

        /// `pending_read_pushes` returns only articles actually flagged --
        /// a second, never-flagged FreshRSS article in the same database
        /// must not show up alongside the one that is.
        #[test]
        fn pending_read_pushes_excludes_articles_with_nothing_pending() {
            let storage = test_storage();

            let pending = Article::new(
                "pending".to_string(),
                "url-pending".to_string(),
                "source".to_string(),
                "FreshRSS/topic".to_string(),
                Utc::now(),
            );
            let pending_id = storage.insert_article(&pending).unwrap();
            storage.insert_freshrss_item(pending_id, "item-pending", "feed-1").unwrap();
            storage.set_pending_read_push(pending_id).unwrap();

            let settled = Article::new(
                "settled".to_string(),
                "url-settled".to_string(),
                "source".to_string(),
                "FreshRSS/topic".to_string(),
                Utc::now(),
            );
            let settled_id = storage.insert_article(&settled).unwrap();
            storage.insert_freshrss_item(settled_id, "item-settled", "feed-1").unwrap();

            assert_eq!(storage.pending_read_pushes().unwrap(), vec![(pending_id, "item-pending".to_string())]);
        }

        /// The sidebar merge query: distinct `(topic, source)` pairs whose
        /// topic is namespaced `FreshRSS/...`, alphabetical, and -- just
        /// as importantly -- excluding local (non-`FreshRSS/`) topics
        /// entirely, so merging this into `group_sources_by_topic`'s map
        /// can never accidentally add a phantom source under a local
        /// topic.
        #[test]
        fn freshrss_topic_source_pairs_excludes_local_topics() {
            let storage = test_storage();

            for (title, url, source, topic) in [
                ("a", "url-a", "Phoronix", "FreshRSS/kernel"),
                ("b", "url-b", "9to5Linux", "FreshRSS/kernel"),
                ("c", "url-c", "r/linux", "FreshRSS/distros"),
                ("d", "url-d", "Arch Linux News", "linux"),
            ] {
                let article =
                    Article::new(title.to_string(), url.to_string(), source.to_string(), topic.to_string(), Utc::now());
                storage.insert_article(&article).unwrap();
            }

            assert_eq!(
                storage.freshrss_topic_source_pairs().unwrap(),
                vec![
                    ("FreshRSS/distros".to_string(), "r/linux".to_string()),
                    ("FreshRSS/kernel".to_string(), "9to5Linux".to_string()),
                    ("FreshRSS/kernel".to_string(), "Phoronix".to_string()),
                ],
                "must include every FreshRSS/* pair and exclude the local 'linux' topic entirely"
            );
        }

        /// A feed that hasn't synced a single article yet has no row to
        /// be distinct over -- the "zero-article-feed limitation"
        /// ARCHITECTURE.md documents. This isn't a bug this method needs
        /// to work around; it's the direct, expected consequence of
        /// querying `articles` rather than a subscription list, and it's
        /// worth a test precisely because it's easy to forget.
        #[test]
        fn freshrss_topic_source_pairs_is_empty_with_no_synced_articles() {
            let storage = test_storage();
            assert!(storage.freshrss_topic_source_pairs().unwrap().is_empty());
        }
    }
}
