//! Placeholder fetcher for `type = "reddit"` sources.
//!
//! Reddit isn't implemented yet -- fetching one only ever produces a
//! clear, specific `Err`, never a crash. This split matters:
//! `sources.toml` is allowed to *name* a `type = "reddit"` source (it's
//! valid TOML, per `fetchers::config::SourceType`; ARCHITECTURE.md's own
//! example includes one) without the whole app refusing to start over it.
//! The failure only happens the moment something actually tries to
//! `.fetch()` this specific source -- and per `main.rs`/`ui/mod.rs`, a
//! single source's fetch error is caught and skipped, not propagated into
//! aborting every other source's refresh.

use crate::fetchers::Fetcher;
use crate::models::Article;
use anyhow::bail;

/// A `type = "reddit"` source's config, held onto (name/url/topic) even
/// though nothing here can act on it yet -- so the error message below can
/// still say *which* source it is, and so this type is ready to grow a
/// real implementation later without `sources.toml`, `fetchers::config`,
/// or any calling code needing to change at all.
pub struct RedditFetcher {
    pub name: String,
    pub url: String,
    pub topic: String,
}

impl Fetcher for RedditFetcher {
    /// Always fails, with a message identifying this source by name and
    /// explaining why -- "not implemented yet" is a genuinely different
    /// situation from a network error or a malformed feed, so it gets its
    /// own wording rather than reusing `rss::RssFetcher`'s
    /// fetch-failure-style messages.
    async fn fetch(&self) -> anyhow::Result<Vec<Article>> {
        bail!(
            "source '{}' (subreddit '{}') has type = \"reddit\", which isn't implemented yet -- \
             skipping. Only type = \"rss\" sources are fetched in this version \
             (see docs/ARCHITECTURE.md's Roadmap).",
            self.name,
            self.url
        )
    }
}
