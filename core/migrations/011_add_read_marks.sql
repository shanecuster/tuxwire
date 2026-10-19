-- Migration 011: remember WHEN an article was opened, and keep the bookkeeping
-- for the optional "read marks" sync.
--
-- Read marks are which feed articles the person has opened. Syncing them is a
-- separate, optional feature with its own switch that is OFF by default (see
-- docs/ARCHITECTURE.md). This migration only adds columns and one table; it
-- changes no existing value, and nothing is sent anywhere.
--
-- 1. `articles.read_at` -- when the article was marked read, in milliseconds
--    since 1970 (UTC), or NULL if it is unread or the time is unknown. The
--    sync uses it to send only recent reads (the last 90 days), so the first
--    sync stays small.
--
--    Articles already marked read before this migration have no recorded time.
--    The best guess is the date the article was PUBLISHED: someone who opened
--    an article from last week has an old publish date only if the article is
--    old, and an old article is exactly what the 90-day limit leaves out. If
--    the publish date cannot be read, `read_at` stays NULL and the article is
--    never sent. Neither guess changes what is shown as read.
--
-- 2. `read_marks` -- one row per article address whose read mark is known to
--    the sync server, whether this device sent it or another device did:
--
--      key      the article's address exactly as stored (trimmed); also the
--               name of the document on the server (`read:<key>`).
--      read_at  when it was opened, in milliseconds since 1970 (UTC).
--      rev      CouchDB's revision label for the document.
--
--    A row also covers a mark that ARRIVED for an article this device has not
--    downloaded yet: when the article shows up, it is marked read at once.
--    Marks only ever go one way (an article can be opened, never "un-opened"),
--    so there is no change time and no conflict to settle: if a document
--    exists, the article was opened.
--
-- The whole migration is one transaction: all of it happens or none of it does.
BEGIN;

ALTER TABLE articles ADD COLUMN read_at INTEGER;

UPDATE articles
SET read_at = CAST(strftime('%s', timestamp) AS INTEGER) * 1000
WHERE read = 1 AND strftime('%s', timestamp) IS NOT NULL;

CREATE TABLE read_marks (
    key     TEXT PRIMARY KEY,
    read_at INTEGER NOT NULL,
    rev     TEXT NOT NULL
);

COMMIT;
