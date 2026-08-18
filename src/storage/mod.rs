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
            "INSERT INTO articles (title, url, source, topic, timestamp, read, skipped, saved, note)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
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
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note
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

    /// Every saved article, across all topics, most recent first -- backs
    /// the dedicated "Saved" view described in ARCHITECTURE.md, which is
    /// explicitly independent of the topic sidebar.
    pub fn saved_articles(&self) -> anyhow::Result<Vec<Article>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, url, source, topic, timestamp, read, skipped, saved, note
             FROM articles WHERE saved = 1 ORDER BY timestamp DESC",
        )?;

        let rows = stmt.query_map([], row_to_article)?;
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
    /// `note: Option<String>` mirrors `Article::note` exactly: passing
    /// `None` saves the article with no note, `Some(text)` saves it with
    /// one attached.
    pub fn save_article(&self, id: i64, note: Option<String>) -> anyhow::Result<()> {
        self.conn.execute(
            "UPDATE articles SET saved = 1, read = 1, note = ?2 WHERE id = ?1",
            params![id, note],
        )?;
        Ok(())
    }

    /// Replaces the note on an already-saved article -- the `n` keybind
    /// ("edit note on a saved article").
    pub fn update_note(&self, id: i64, note: Option<String>) -> anyhow::Result<()> {
        self.conn
            .execute("UPDATE articles SET note = ?2 WHERE id = ?1", params![id, note])?;
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
}

/// Maps one row from a `SELECT id, title, url, source, topic, timestamp,
/// read, skipped, saved, note FROM articles ...` query into an `Article`.
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
    use chrono::Utc;

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
}
