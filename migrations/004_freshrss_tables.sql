-- Migration 004: FreshRSS sync storage (feature-gated: only compiled in
-- and applied when the crate is built with `--features freshrss` -- see
-- the cfg-gating in src/storage/migrations.rs for how that's enforced).
--
-- Two NEW tables only -- no `ALTER` on `articles` or any other existing
-- table, per docs/ARCHITECTURE.md's "Storage additions (additive only)":
-- a database opened by a build without the `freshrss` feature (including
-- `master`) must see an unchanged `articles`/`skip_weights` schema, and
-- gets exactly that, since this file is never even compiled into such a
-- build, let alone run against its database.

-- Links a FreshRSS-origin `articles` row back to the server's own
-- identifiers, and tracks whether a local read-state change on it still
-- needs to be pushed up to the server.
CREATE TABLE freshrss_items (
    -- `PRIMARY KEY` here (rather than its own independent `id` column) is
    -- what makes this a strict one-to-one link to `articles`, not a
    -- separate entity of its own: this table's only reason to exist is
    -- "one extra row of FreshRSS-specific facts per FreshRSS-origin
    -- article." `REFERENCES articles(id)` documents that relationship
    -- directly in the schema, even though SQLite only enforces foreign
    -- keys when a connection has turned that checking on.
    article_id        INTEGER PRIMARY KEY REFERENCES articles(id),
    -- The FreshRSS item's own ID, already normalized to one consistent
    -- form (see ARCHITECTURE.md's note on short vs. long-form item ID
    -- formats -- normalization is `freshrss::mapping`'s job, a later
    -- build phase). `UNIQUE` is what lets a sync look an already-synced
    -- item up by server ID before deciding whether it needs fetching at
    -- all -- see `Storage::freshrss_article_id_by_remote_id`.
    remote_id         TEXT UNIQUE NOT NULL,
    -- Which FreshRSS feed this item came from -- lets a sync reconcile
    -- read state and group items per feed without a second round trip to
    -- FreshRSS just to ask "which feed was this item in."
    feed_id           TEXT NOT NULL,
    -- Set the moment the person changes this article's read state locally
    -- (`Enter` or `s`) before that change has been confirmed pushed to the
    -- server via `edit-tag` -- ARCHITECTURE.md's "Push read state" step,
    -- and its conflict rule ("a pending local change wins until it has
    -- been pushed"). `NOT NULL DEFAULT 0` matches the same
    -- no-native-boolean convention `articles.read`/`skipped`/`saved`
    -- already use (see migration 001).
    pending_read_push BOOLEAN NOT NULL DEFAULT 0
);

-- A plain key/value table for small, singular facts about the sync
-- itself that don't belong to any one article -- e.g. the timestamp of
-- the last successful sync (ARCHITECTURE.md's own example). A dedicated
-- table with named columns would be overkill for what's currently a
-- single fact, and this shape can hold more keys later without another
-- migration.
CREATE TABLE freshrss_meta (
    key   TEXT PRIMARY KEY,
    value TEXT
);
