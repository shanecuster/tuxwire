//! Turns the `client` module's raw API shapes into `Article` values and
//! decides which item ids a sync should actually fetch
//! (`docs/ARCHITECTURE.md` § FreshRSS Sync, Phase 4).
//!
//! Every function here is a **pure function**: given the same inputs, it
//! always returns the same output, and it never touches the network, the
//! filesystem, or `Storage`. That's deliberate, not incidental --
//! `freshrss::sync` (the actual async orchestration, a later addition to
//! this module tree) is where the I/O happens; this file is where the
//! *decisions* happen, kept separate so every decision here is testable
//! against a plain fixture or a handful of `Vec`/`HashSet` literals, with
//! no test server, no tokio runtime, and no temporary database involved.

use crate::freshrss::client;
use crate::models::Article;
use chrono::{DateTime, Utc};
use std::collections::HashSet;

/// Turns one `client::Item` (already-normalized id, still otherwise raw
/// API data) into an `Article` ready for `Storage::insert_article_if_new_url`.
///
/// `topic` is passed in already resolved (`"FreshRSS/<category>"` or
/// `"FreshRSS/Uncategorized"`) rather than derived from `item` itself --
/// `item`'s own `origin` only carries the *feed*'s id/name/site url, not
/// its FreshRSS category, so resolving a topic needs the matching
/// `client::Subscription` too. `freshrss::sync` is the one place that has
/// both an `Item` and the full subscription list in hand at once, via
/// `topic_for_category` below; keeping the two functions separate (rather
/// than handing this one the whole subscription list to search through
/// itself) keeps each one doing exactly one job.
pub fn map_item_to_article(item: &client::Item, topic: &str) -> Article {
    Article::new(
        item.title.clone(),
        article_url(item),
        item.origin.title.clone(),
        topic.to_string(),
        timestamp_from_unix_seconds(item.published),
    )
}

/// Picks `item`'s URL: `canonical[0].href` if present, else
/// `alternate[0].href`, else an empty string. Verified against a live
/// server that a real item always has at least a `canonical` entry (see
/// `docs/ARCHITECTURE.md`'s Protocol section), but `client::Item` models
/// both as plain `Vec`s (not a required field) since nothing in the
/// Google Reader API spec actually guarantees either one is present --
/// same "don't assume more than the wire format promises" reasoning
/// `RssFetcher::fetch`'s own link-picking uses for a parsed feed entry
/// that might have zero links.
fn article_url(item: &client::Item) -> String {
    item.canonical
        .first()
        .or(item.alternate.first())
        .map(|href| href.href.clone())
        .unwrap_or_default()
}

/// Converts a Unix timestamp in seconds (`client::Item::published`) into
/// the `DateTime<Utc>` `Article::timestamp` needs.
///
/// `DateTime::from_timestamp(secs, nsecs)` returns `Option` -- it's
/// `None` only for a value so far outside `chrono`'s representable range
/// that it can't be turned into a real calendar date at all, not a
/// realistic outcome for a genuine "when this was published" timestamp a
/// real server sent. Falling back to `Utc::now()` in that unreachable-in-
/// practice case mirrors `fetch_feed`'s own "always have *a* timestamp to
/// sort by" fallback in `fetchers/rss.rs`, rather than making every
/// caller of `map_item_to_article` handle a `Result` for a failure mode
/// that isn't really about this specific item being malformed.
fn timestamp_from_unix_seconds(published: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(published, 0).unwrap_or_else(Utc::now)
}

/// Namespaces a FreshRSS category label into the topic tuxwire actually
/// files an article under, per ARCHITECTURE.md's "Mapping FreshRSS onto
/// the one-topic-per-source rule": `FreshRSS/<category>`, or
/// `FreshRSS/Uncategorized` when the feed has no category at all (or, in
/// principle, more than one -- `client::Subscription::categories` is a
/// `Vec` because the API allows it, but this only ever looks at the
/// first, matching that same ARCHITECTURE.md section's "one FreshRSS
/// category becomes its topic").
///
/// Takes `Option<&str>` (the label itself), not the whole `Subscription`
/// -- `freshrss::sync` is the one that looks a `Subscription` up by the
/// item's `origin.stream_id` and hands this function just the piece it
/// actually needs, keeping this function ignorant of how that lookup
/// happens (a linear search today, maybe a `HashMap` later, is an
/// implementation detail this function shouldn't have to know about to
/// be tested).
pub fn topic_for_category(category_label: Option<&str>) -> String {
    match category_label {
        Some(label) => format!("FreshRSS/{label}"),
        None => "FreshRSS/Uncategorized".to_string(),
    }
}

/// Decides which of `unread_ids` a sync should actually fetch full
/// contents for: everything not already linked in `freshrss_items`
/// (`already_synced`), capped at `max_items` (`freshrss.toml`'s
/// `max_items_per_sync`), preserving `unread_ids`'s own order.
///
/// Order matters here in a way that's easy to get backwards: `unread_ids`
/// arrives newest-first (verified against a live server -- see
/// `client::Client::unread_item_ids`'s doc comment), and this function
/// doesn't re-sort it. Combined with `.take(max_items)` below, that means
/// a sync facing more unread items than `max_items` always fetches the
/// **newest** ones first, leaving the oldest for a later sync to pick up
/// -- per ARCHITECTURE.md's Phase 4 design, "later syncs backfill"
/// rather than a sync arbitrarily grabbing whichever `max_items` items
/// happen to come first in some other order.
///
/// `already_synced` is a `&HashSet<String>`, not a closure or a `Storage`
/// reference, specifically so this function has no way to perform I/O
/// even by accident -- `freshrss::sync` builds this set with its own
/// `Storage::freshrss_article_id_by_remote_id` lookups *before* calling
/// this function, once per sync, rather than this function reaching for
/// the database itself on every id it considers.
pub fn select_ids_to_fetch(unread_ids: &[String], already_synced: &HashSet<String>, max_items: usize) -> Vec<String> {
    unread_ids.iter().filter(|id| !already_synced.contains(id.as_str())).take(max_items).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal but realistic `client::Item`, matching the real
    /// shape captured in `fixtures/items_contents.json` (see that file's
    /// own commit for where these values came from) -- constructed by
    /// hand here, rather than parsed from the fixture via
    /// `client::parse_item_contents`, since that function is private to
    /// `client` and this module only needs `Item`'s own `pub` fields,
    /// which this struct literal exercises just as directly.
    fn sample_item() -> client::Item {
        client::Item {
            id: "1789934360946639".to_string(),
            title: "Show HN: A browser MMO designed to be played by scripts, not people".to_string(),
            published: 1_789_933_681,
            canonical: vec![client::Href { href: "https://space-core.at/".to_string() }],
            alternate: vec![],
            origin: client::Origin {
                stream_id: "feed/2".to_string(),
                html_url: "https://news.ycombinator.com/newest".to_string(),
                title: "Hacker News: Newest".to_string(),
            },
            summary: Some(client::Summary { content: "<p>Fixture summary content one.</p>".to_string() }),
            author: Some("SpaceCoreDev".to_string()),
        }
    }

    /// The full round trip: every field `map_item_to_article` is
    /// responsible for lands correctly on the `Article`.
    #[test]
    fn maps_a_real_shaped_item_to_an_article() {
        let article = map_item_to_article(&sample_item(), "FreshRSS/Uncategorized");

        assert_eq!(article.title, "Show HN: A browser MMO designed to be played by scripts, not people");
        assert_eq!(article.url, "https://space-core.at/");
        assert_eq!(article.source, "Hacker News: Newest");
        assert_eq!(article.topic, "FreshRSS/Uncategorized");
        assert_eq!(article.timestamp.timestamp(), 1_789_933_681);

        // A freshly-mapped article is exactly as new as any other
        // freshly-fetched one -- `map_item_to_article` doesn't (and
        // shouldn't) know anything about read/saved state, that's
        // `Storage`'s job once this `Article` actually gets inserted.
        assert!(!article.read);
        assert!(!article.saved);
    }

    /// `alternate[0].href` is used when `canonical` is empty -- the
    /// documented fallback.
    #[test]
    fn article_url_falls_back_to_alternate_when_canonical_is_empty() {
        let mut item = sample_item();
        item.canonical = vec![];
        item.alternate = vec![client::Href { href: "https://example.com/alt".to_string() }];

        let article = map_item_to_article(&item, "FreshRSS/Uncategorized");

        assert_eq!(article.url, "https://example.com/alt");
    }

    /// Neither `canonical` nor `alternate` present is a real, if
    /// unlikely, possibility per the Google Reader API spec -- an empty
    /// URL rather than a panic or a fabricated placeholder.
    #[test]
    fn article_url_is_empty_when_neither_link_is_present() {
        let mut item = sample_item();
        item.canonical = vec![];
        item.alternate = vec![];

        let article = map_item_to_article(&item, "FreshRSS/Uncategorized");

        assert_eq!(article.url, "");
    }

    #[test]
    fn topic_for_category_namespaces_a_real_label() {
        assert_eq!(topic_for_category(Some("kernel")), "FreshRSS/kernel");
    }

    #[test]
    fn topic_for_category_falls_back_to_uncategorized() {
        assert_eq!(topic_for_category(None), "FreshRSS/Uncategorized");
    }

    /// Already-synced ids are filtered out, remaining ids keep their
    /// original (newest-first) order, and the result never exceeds
    /// `max_items` -- the three properties `freshrss::sync` relies on.
    #[test]
    fn select_ids_to_fetch_filters_synced_ids_and_respects_the_cap() {
        let unread_ids = vec!["3".to_string(), "2".to_string(), "1".to_string()];
        let mut already_synced = HashSet::new();
        already_synced.insert("2".to_string());

        let selected = select_ids_to_fetch(&unread_ids, &already_synced, 10);
        assert_eq!(selected, vec!["3".to_string(), "1".to_string()]);
    }

    /// With more unfetched ids than `max_items`, the *newest* ones are
    /// kept (the front of `unread_ids`, per its documented newest-first
    /// order) and the rest are left for a later sync -- this is the
    /// "later syncs backfill" behavior ARCHITECTURE.md's Phase 4 design
    /// depends on.
    #[test]
    fn select_ids_to_fetch_keeps_the_newest_ids_when_over_the_cap() {
        let unread_ids = vec!["5".to_string(), "4".to_string(), "3".to_string(), "2".to_string(), "1".to_string()];
        let already_synced = HashSet::new();

        let selected = select_ids_to_fetch(&unread_ids, &already_synced, 2);
        assert_eq!(selected, vec!["5".to_string(), "4".to_string()]);
    }

    /// Nothing left to fetch (every unread id is already synced) is a
    /// plain empty `Vec`, not an error -- `freshrss::sync` treats this as
    /// "no new items this round," not a failure.
    #[test]
    fn select_ids_to_fetch_returns_empty_when_everything_is_already_synced() {
        let unread_ids = vec!["1".to_string(), "2".to_string()];
        let mut already_synced = HashSet::new();
        already_synced.insert("1".to_string());
        already_synced.insert("2".to_string());

        assert!(select_ids_to_fetch(&unread_ids, &already_synced, 500).is_empty());
    }
}
