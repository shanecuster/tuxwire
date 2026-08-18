-- Migration 001: initial schema.
--
-- Creates the two tables described in docs/ARCHITECTURE.md's "Schema (v1)"
-- section: `articles` (every fetched article, plus the user-facing state
-- that hangs off it -- read/skipped/saved/note) and `skip_weights` (the
-- keyword -> weight lookup behind the skip-weighting "learning" system).
--
-- Every future schema change ships as its OWN new numbered file in this
-- directory (e.g. `002_add_priority.sql`) rather than editing this one.
-- Once a migration has shipped and possibly already run against someone's
-- real database, rewriting it here would make that database's applied
-- history disagree with what a fresh install gets from the same file --
-- the two are supposed to end up identical, and only appending new files
-- guarantees that. See `src/storage/migrations.rs` for how these numbered
-- files get discovered and applied in order.

CREATE TABLE articles (
    -- `INTEGER PRIMARY KEY` (with no `AUTOINCREMENT`) makes this column an
    -- alias for SQLite's own hidden `rowid` -- inserting a row without
    -- specifying `id` has SQLite assign the next one automatically, which
    -- is exactly what `Article::new` (see `src/models.rs`) relies on when
    -- it sets a placeholder `id: 0` for not-yet-inserted articles.
    id        INTEGER PRIMARY KEY,
    title     TEXT NOT NULL,
    url       TEXT NOT NULL,
    source    TEXT NOT NULL,
    topic     TEXT NOT NULL,
    -- Stored as TEXT (an ISO 8601 / RFC 3339 string), not a native SQLite
    -- date type -- SQLite has no dedicated datetime storage class, it only
    -- has TEXT/INTEGER/REAL/BLOB/NULL. `rusqlite`'s `chrono` feature (see
    -- Cargo.toml) is what teaches `chrono::DateTime<Utc>` to convert to and
    -- from this TEXT column automatically, so the Rust side never has to
    -- think about the on-disk representation directly.
    timestamp TEXT NOT NULL,
    -- SQLite has no dedicated boolean storage class either -- it stores
    -- these as an INTEGER 0 or 1. `rusqlite` converts Rust's `bool` to and
    -- from that automatically, so again, nothing to hand-roll on the Rust
    -- side.
    read      BOOLEAN NOT NULL DEFAULT 0,
    skipped   BOOLEAN NOT NULL DEFAULT 0,
    saved     BOOLEAN NOT NULL DEFAULT 0,
    -- No `NOT NULL` here: `note` models an *optional* free-text note (see
    -- `Option<String>` on `Article` in `src/models.rs`), and NULL is SQL's
    -- own way of expressing "absent" -- the same absence `None` expresses
    -- in Rust. `rusqlite` maps `NULL` <-> `None` and a real value <-> `Some`
    -- automatically for `Option<T>` columns.
    note      TEXT
);

CREATE TABLE skip_weights (
    keyword TEXT PRIMARY KEY,
    weight  INTEGER NOT NULL DEFAULT 0
);
