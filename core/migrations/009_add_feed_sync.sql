-- Migration 009: bookkeeping for syncing the feed list (optional, off by default).
--
-- Feed sync is a SEPARATE switch from saved-article sync and never runs unless
-- the person turns it on. This migration only adds a place to remember things:
-- it changes no existing table, column or row, and nothing is sent anywhere.
--
-- `feed_sync` has one row per feed address this device has ever heard of, from
-- itself or from another device. It is NOT the feed list itself (the terminal
-- app keeps that in `sources.toml`, the Android app in `feeds`); it records what
-- sync knows about each address:
--
--   key               the address in lower case. Two devices that write the same
--                     address in different capitals still mean the same feed.
--   url               the address as it was written (used to fetch the feed).
--   name, topic       shown on the other device when the feed is added there.
--   present           1 = the feed exists, 0 = it was removed. A removal is a
--                     row, not a deletion, so the other device can tell "removed
--                     on purpose, later" from "never heard of it".
--   updated_at        when `present` (or the details) last changed, in
--                     milliseconds since 1970 (UTC). The newer copy wins.
--   local_has         1 = this device's own feed list currently contains it.
--   dismissed         1 = the person was asked about a removal that came from
--                     another device and chose to keep the feed here.
--   rev               CouchDB's revision label for this feed's document, or NULL
--                     if it has never been sent or was forgotten.
--   pushed_updated_at the `updated_at` value last sent or received. A feed needs
--                     sending when `updated_at` is larger.
--
-- The whole migration is one transaction: all of it happens or none of it does.
BEGIN;

CREATE TABLE feed_sync (
    key               TEXT PRIMARY KEY,
    url               TEXT NOT NULL,
    name              TEXT NOT NULL,
    topic             TEXT NOT NULL,
    present           INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    local_has         INTEGER NOT NULL,
    dismissed         INTEGER NOT NULL DEFAULT 0,
    rev               TEXT,
    pushed_updated_at INTEGER NOT NULL DEFAULT 0
);

COMMIT;
