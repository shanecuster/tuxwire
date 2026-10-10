-- Migration 006: feeds and settings, stored in the database.
--
-- The terminal app keeps its feed list in TOML files (`sources.toml`,
-- `lists.toml`) and never reads these two tables. The Android app has no
-- config directory to put such files in, and asking someone to edit TOML on a
-- phone is no way to run a news reader, so it keeps its feeds here instead.
-- `core/` provides the tables and the methods; each program decides whether
-- to use them.
--
-- Additive only: two new tables, and no existing table, column or row
-- changes. A database that never touches these tables behaves exactly as it
-- did before this migration.
--
-- Why 006: 005 was the last migration on master, and 004 stays unused (see
-- the header of 005_add_feed_url.sql).

CREATE TABLE feeds (
    -- `INTEGER PRIMARY KEY` makes this SQLite's own row id, assigned
    -- automatically on insert (the same trick `articles.id` uses).
    id          INTEGER PRIMARY KEY,
    -- Display name, e.g. "Ars Technica". Copied onto each fetched article as
    -- its `source`.
    name        TEXT NOT NULL,
    -- The feed's address. UNIQUE so the same feed cannot be added twice; it is
    -- also what `articles.feed_url` refers to (see migration 005), which is how
    -- an article is matched to the feed it came from.
    url         TEXT NOT NULL UNIQUE,
    -- The one topic this feed's articles are filed under.
    topic       TEXT NOT NULL,
    -- Which fetcher handles the feed: 'rss' or 'reddit' (see
    -- `fetchers::SourceType`). Stored as text so a new type needs no schema
    -- change.
    source_type TEXT NOT NULL DEFAULT 'rss'
);

CREATE TABLE settings (
    -- A short name such as 'theme'. One row per setting, so saving a setting
    -- again simply replaces its value.
    key   TEXT PRIMARY KEY,
    -- Always text; the code reading a setting turns it into whatever it needs.
    value TEXT NOT NULL
);
