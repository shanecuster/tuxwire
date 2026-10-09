-- Migration 005: which feed each article came from.
--
-- `feed_url` is the `url` of the `[[source]]` that fetched the article
-- (the feed's URL, not the article's own `url`). It's what lets several
-- feed lists (lists.toml) share one database: a list's view is "articles
-- whose feed_url is one of this list's sources". Read/saved/note state
-- stays per article, so it's shared across lists.
--
-- Additive only: a new nullable column plus an index, and no existing
-- column or row changes here. Rows from before this migration start as
-- NULL. `Storage::insert_article` fills them in whenever the same article
-- is fetched again, and `Storage::backfill_feed_urls` fills in the rest
-- it can by matching the `source` name against configured sources.
--
-- Why 005 and not 004: the archived FreshRSS branch (tag archive/freshrss)
-- used version 4 for its own tables. A database that ever ran a FreshRSS
-- build is already at `user_version = 4`, so a new migration 4 would be
-- silently skipped there. Version 4 stays unused on master.
ALTER TABLE articles ADD COLUMN feed_url TEXT;

-- Lets the per-list queries ("feed_url IN (...)") use an index rather
-- than scanning every article.
CREATE INDEX idx_articles_feed_url ON articles(feed_url);
