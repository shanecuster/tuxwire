-- Migration 010: the Read Later list (and its sync bookkeeping).
--
-- "Read later" is a list of links the person wants to come back to: an article
-- from a feed, or any web address pasted in. It is NOT the same as saved
-- articles (a permanent archive with notes); an item here is meant to be read
-- and then either discarded or promoted to a saved article.
--
-- This migration only ADDS one table. It changes no existing table, column or
-- row, and nothing is sent anywhere: the list syncs only when the person has
-- turned sync on (the same switch as saved articles).
--
-- `read_later` has one row per link this device has ever heard of, from itself
-- or from another device:
--
--   key               the link exactly as written (trimmed). It is also the name
--                     of the link's document on the server (`later:<key>`), so
--                     two devices that hold the same link agree on it. The
--                     exact text is used, not a lower-cased copy, because the
--                     path of a web address can be case-sensitive and a saved
--                     article is named by its exact address too.
--   url               the same text; kept as its own column so the list reads
--                     like the other tables (and so a later change to how keys
--                     are made would not touch the address).
--   title             what to show. For a pasted link it is the page title.
--   added_at          when it was put on the list, in milliseconds since 1970
--                     (UTC). The list is shown newest first.
--   present           1 = on the list, 0 = taken off it (discarded, or promoted
--                     to a saved article). A removal is a row, not a deletion,
--                     so the other devices can tell "removed later" from
--                     "never heard of it".
--   read              1 = opened. An opened item stays on the list until it is
--                     discarded or saved.
--   updated_at        when anything in the row last changed, in milliseconds
--                     since 1970 (UTC). The newer copy wins when two devices
--                     disagree.
--   rev               CouchDB's revision label for this link's document, or NULL
--                     if it has never been sent or was forgotten.
--   pushed_updated_at the `updated_at` value last sent or received. A row needs
--                     sending when `updated_at` is larger.
--
-- The whole migration is one transaction: all of it happens or none of it does.
BEGIN;

CREATE TABLE read_later (
    key               TEXT PRIMARY KEY,
    url               TEXT NOT NULL,
    title             TEXT NOT NULL,
    added_at          INTEGER NOT NULL,
    present           INTEGER NOT NULL,
    read              INTEGER NOT NULL DEFAULT 0,
    updated_at        INTEGER NOT NULL,
    rev               TEXT,
    pushed_updated_at INTEGER NOT NULL DEFAULT 0
);

COMMIT;
