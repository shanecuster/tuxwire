-- Migration 002: enforce article URL uniqueness.
--
-- A re-run of the fetcher sees the same feed entries it saw last time, so
-- `url` needs to be a hard uniqueness constraint at the database level --
-- not just something application code happens to check -- to guarantee
-- re-fetching never creates duplicate rows, even if a future caller forgets
-- to check first.

CREATE UNIQUE INDEX idx_articles_url ON articles(url);
