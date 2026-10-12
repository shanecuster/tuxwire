-- Migration 008: bookkeeping for the optional CouchDB sync.
--
-- Sync is OFF by default and never runs unless the person turns it on. This
-- migration only adds the places to remember things; it changes no existing
-- column and no existing row's meaning, and nothing is sent anywhere.
--
-- 1. `articles.sync_updated_at`: when the saved / note state of an article last
--    changed, in milliseconds since 1970 (UTC). Sync uses it to decide which
--    copy is newer when two devices disagree. NULL means "this article was
--    never saved or noted, so there is nothing to sync".
--
-- 2. `sync_docs`: for each article that has been synced, the revision (version
--    label) CouchDB gave its document last time, and the `sync_updated_at`
--    value that was pushed or pulled then. An article needs pushing when its
--    `sync_updated_at` is newer than `pushed_updated_at`.
--
-- Articles that are already saved when this migration runs are stamped with
-- "now", so the first sync pushes them. This is the only backfill; every other
-- row stays NULL.
-- The whole migration is one transaction: either everything below happens or
-- none of it does, so a failure can never leave the database half-changed.
BEGIN;

ALTER TABLE articles ADD COLUMN sync_updated_at INTEGER;

UPDATE articles
SET sync_updated_at = CAST(strftime('%s', 'now') AS INTEGER) * 1000
WHERE saved = 1;

CREATE TABLE sync_docs (
    doc_id            TEXT PRIMARY KEY,
    rev               TEXT NOT NULL,
    pushed_updated_at INTEGER NOT NULL
);

COMMIT;
