//! Importing a feed from the **bytes of a file**: the Android app's "Import
//! from file" button, for sites that block programs but serve the feed to a
//! browser (OverTake.gg's Cloudflare rule is the example that started this).
//!
//! The app's file picker gives it the file's bytes, never a path, so this
//! module takes bytes. It is a one-time import: the feed is stored under a
//! made-up key (`import:overtake`, see [`import_key`]) and refreshing skips it,
//! because there is nothing to download. To get newer articles the person
//! imports a newer copy, which finds the same key and adds only what is new.
//!
//! Terminal users have `type = "file"` in `sources.toml` instead, which
//! re-reads a path on every refresh; both share the same parsing code.

// Has clippy flag any undocumented item here, the same as in
// `core/src/fetch_report.rs`.
#![warn(clippy::missing_docs_in_private_items)]

use crate::fetch_report::FetchReport;
use crate::fetchers::file::{import_key, parse_feed_file_bytes};
use crate::fetchers::rss::articles_from_feed;
use crate::fetchers::{FetchOutcome, SourceType};
use crate::storage::Storage;

/// What an import did, for the app to show in one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    /// The key the feed is stored under (`import:overtake`). It is the feed's
    /// address in the feed list.
    pub key: String,
    /// How many articles in the file were not in the database yet.
    pub added: u32,
    /// How many articles the file held in all.
    pub total: u32,
    /// `true` when this import created the feed; `false` when the feed was
    /// already there and only newer articles were added.
    pub created: bool,
}

/// Imports the feed in `bytes` as a feed called `name` filed under `topic`.
///
/// - `label` is the file's display name, used only in error messages.
/// - A feed with the same key (same name, ignoring case and punctuation) is
///   updated rather than duplicated. In that case the feed's **stored** name
///   and topic win, so a second import cannot move the feed or rename its
///   articles by accident.
/// - Nothing is stored unless the file parses as a feed, so a bad file (a saved
///   "Checking your browser" page, say) leaves the database untouched.
///
/// `&Storage` is borrowed because inserting only needs the open database, not
/// ownership of it. `bytes: &[u8]` is borrowed for the same reason.
pub fn import_feed_bytes(
    storage: &Storage,
    bytes: &[u8],
    label: &str,
    name: &str,
    topic: &str,
) -> anyhow::Result<ImportReport> {
    // Step 1: tidy and check what the person typed. An empty name or topic
    // would make an unusable row.
    let (name, topic) = (name.trim(), topic.trim());
    if name.is_empty() || topic.is_empty() {
        anyhow::bail!("a feed needs a name and a topic");
    }

    // Step 2: parse before touching the database, so a bad file changes nothing.
    let feed = parse_feed_file_bytes(bytes, label)?;

    // Step 3: find the feed this key already belongs to, or create it. A
    // re-import takes the stored name and topic (see the doc comment above).
    let key = import_key(name);
    let existing = storage
        .list_feeds()?
        .into_iter()
        .find(|feed| feed.url == key);
    let (created, name, topic) = match existing {
        Some(found) => (false, found.name, found.topic),
        None => {
            storage.add_feed(name, &key, topic, SourceType::File)?;
            (true, name.to_string(), topic.to_string())
        }
    };

    // Step 4: turn the entries into articles exactly as a download would, with
    // the key as their `feed_url`, so they appear under this feed.
    let articles = articles_from_feed(feed, &name, &topic, &key);
    let total = articles.len() as u32;

    // Step 5: store them through the same path a refresh uses, which also
    // counts the ones that were new. The `FetchOutcome` is the packaging a
    // fetch result normally arrives in; here the "fetch" is already done.
    let mut report = FetchReport::default();
    report.record_outcome(
        storage,
        FetchOutcome {
            name,
            result: Ok(articles),
        },
    )?;

    Ok(ImportReport {
        key,
        added: report.new_article_ids.len() as u32,
        total,
        created,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small feed, two entries.
    const FEED: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Site</title>
        <item><title>One</title><link>https://example.com/one</link></item>
        <item><title>Two</title><link>https://example.com/two</link></item>
        </channel></rss>"#;

    /// The same feed a day later: one old entry and one new one.
    const NEWER: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Site</title>
        <item><title>Two</title><link>https://example.com/two</link></item>
        <item><title>Three</title><link>https://example.com/three</link></item>
        </channel></rss>"#;

    /// An in-memory database, so no test touches a real file.
    fn db() -> Storage {
        Storage::open_in_memory().expect("in-memory database")
    }

    /// The first import creates the feed and stores every article under it.
    #[test]
    fn first_import_creates_the_feed_and_articles() {
        let storage = db();
        let report = import_feed_bytes(
            &storage,
            FEED.as_bytes(),
            "index.rss",
            "Overtake",
            "sim-racing",
        )
        .unwrap();
        assert_eq!(report.key, "import:overtake");
        assert!(report.created);
        assert_eq!((report.added, report.total), (2, 2));

        let feeds = storage.list_feeds().unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].url, "import:overtake");
        assert_eq!(feeds[0].source_type, SourceType::File);
        assert_eq!(storage.article_count().unwrap(), 2);
    }

    /// A second import with a newer file updates the same feed: no duplicate
    /// feed, only the new article is added, and the stored name and topic win
    /// over what is typed the second time.
    #[test]
    fn second_import_adds_only_what_is_new() {
        let storage = db();
        import_feed_bytes(&storage, FEED.as_bytes(), "a.rss", "Overtake", "sim-racing").unwrap();
        let report =
            import_feed_bytes(&storage, NEWER.as_bytes(), "b.rss", "overtake", "other").unwrap();

        assert!(!report.created);
        assert_eq!((report.added, report.total), (1, 2));
        let feeds = storage.list_feeds().unwrap();
        assert_eq!(feeds.len(), 1);
        assert_eq!(feeds[0].name, "Overtake");
        assert_eq!(feeds[0].topic, "sim-racing");
        assert_eq!(storage.article_count().unwrap(), 3);
    }

    /// A file that is not a feed changes nothing: no feed row, no articles.
    #[test]
    fn a_bad_file_leaves_the_database_untouched() {
        let storage = db();
        let err = import_feed_bytes(
            &storage,
            b"<html>Checking your browser</html>",
            "x.html",
            "Overtake",
            "sim-racing",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("is not an RSS or Atom feed"));
        assert!(storage.list_feeds().unwrap().is_empty());
        assert_eq!(storage.article_count().unwrap(), 0);
    }

    /// Imported feeds are skipped by a refresh (nothing to download), while an
    /// ordinary file source is still fetched.
    #[test]
    fn refresh_skips_imported_feeds() {
        let storage = db();
        import_feed_bytes(&storage, FEED.as_bytes(), "a.rss", "Overtake", "sim-racing").unwrap();
        let sources = crate::fetchers::configured_sources(
            storage
                .list_feeds()
                .unwrap()
                .iter()
                .map(|f| f.to_source_config())
                .collect(),
        );
        assert!(sources.is_empty());
    }
}
