-- Migration 003: track when an article was saved, and when its note was
-- last written, separately from the article's own publish `timestamp`.
--
-- docs/ARCHITECTURE.md's "Notes Retrieval & Search" section explains why
-- these are two genuinely different facts from `timestamp`: `timestamp` is
-- when the *source* published the article, not when the person using
-- tuxwire saved it or wrote a note on it -- export and retrieval both want
-- the latter two, not the former.
--
-- `ALTER TABLE ... ADD COLUMN` (rather than recreating the table) is what
-- keeps this non-destructive, per ARCHITECTURE.md's "no destructive schema
-- changes" rule: every existing row keeps its `id`/`title`/`note`/etc.
-- untouched, just gains two new columns. Both are nullable with no
-- `DEFAULT`, so every row that existed before this migration ran gets
-- `NULL` in both -- exactly right, since "saved before tuxwire tracked
-- when" is a real, honest state to be in (see `src/ui/mod.rs`'s
-- `saved_meta_line` for how the UI renders that gracefully rather than
-- assuming a value is always present).

ALTER TABLE articles ADD COLUMN saved_at TEXT;
ALTER TABLE articles ADD COLUMN noted_at TEXT;
