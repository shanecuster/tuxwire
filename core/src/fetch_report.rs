//! What happened during one round of fetching: which articles were new,
//! and how many sources failed (and whether they failed because the
//! network is unreachable). Shared by the startup fetch in `main.rs` and
//! the `r` refresh in `ui::refresh_topic`. The UI uses it for the "N new",
//! "offline", and "sources failed" footer messages and for the per-article
//! `Icon::New` marker.
//!
//! It lives in its own module, not in `fetchers` or `storage`, because it
//! touches both (it reads `Storage` counts and inspects fetch errors) and
//! adding it here leaves both of those modules unchanged.

// Has clippy flag any undocumented item here, the same as in
// `core/src/icons/mod.rs`.
#![warn(clippy::missing_docs_in_private_items)]

use crate::fetchers::FetchOutcome;
use crate::models::Article;
use crate::storage::Storage;
use std::collections::HashSet;

/// The running tally for one fetch round. Start with
/// `FetchReport::default()` (all zeros, no IDs), then call
/// [`FetchReport::record_failure`] for each source that failed and
/// [`FetchReport::record_insert`] for each article stored.
///
/// `Default` is derived, so every number starts at `0` and the set starts
/// empty without writing that out by hand.
#[derive(Debug, Default)]
pub struct FetchReport {
    /// How many sources this round tried to fetch.
    pub attempted: usize,
    /// How many of those failed, for any reason.
    pub failed: usize,
    /// How many of the failures were "couldn't reach the server" (a
    /// connection error or timeout), as opposed to a 404, bad XML, or an
    /// unimplemented source type. Always `<= failed`.
    pub unreachable: usize,
    /// Database IDs of articles this round actually added. Articles
    /// already in the database aren't counted, because
    /// `Storage::insert_article` skips duplicate URLs.
    pub new_article_ids: HashSet<i64>,
    /// One readable line per failed source ("Name: why it failed"), in the
    /// order they were recorded, for programs that want to show or log them.
    /// This crate never prints; the caller decides.
    pub failure_messages: Vec<String>,
}

impl FetchReport {
    /// Records one failed source. Takes `&anyhow::Error` (a borrow) because
    /// the caller still prints or drops the error afterward; this only
    /// needs to look at it.
    pub fn record_failure(&mut self, err: &anyhow::Error) {
        self.failed += 1;
        if is_unreachable(err) {
            self.unreachable += 1;
        }
    }

    /// Stores `article` with `Storage::insert_article`, and adds its ID to
    /// `new_article_ids` if it was actually new.
    ///
    /// `insert_article` returns the row ID either way (it's idempotent on
    /// URL), so the ID alone can't tell new from existing. Comparing the
    /// table's row count before and after can: it only grows when a row
    /// was really inserted. That costs one `COUNT(*)` per article, which is
    /// negligible for a personal news database, and it avoids changing
    /// `insert_article` itself.
    pub fn record_insert(&mut self, storage: &Storage, article: &Article) -> anyhow::Result<()> {
        let before = storage.article_count()?;
        let id = storage.insert_article(article)?;
        if storage.article_count()? > before {
            self.new_article_ids.insert(id);
        }
        Ok(())
    }

    /// Records the result of fetching one source: counts it as attempted,
    /// then either stores its articles (counting the new ones) or records the
    /// failure and its message.
    ///
    /// This is the one shared place the "fetch outcome becomes stored articles
    /// and a tally" step lives, so the startup fetch, the `r` refresh and the
    /// Android refresh all behave the same way.
    ///
    /// `outcome` is taken by value because the articles inside it are only
    /// needed once, and the error is formatted into a message here. A
    /// database error while storing is returned (`?`) because that is a real
    /// problem, unlike a dead feed, which is just counted.
    pub fn record_outcome(
        &mut self,
        storage: &Storage,
        outcome: FetchOutcome,
    ) -> anyhow::Result<()> {
        // Every outcome is one attempted source, successful or not.
        self.attempted += 1;

        // `match` on the result: store the articles, or note the failure.
        match outcome.result {
            Ok(articles) => {
                for article in &articles {
                    self.record_insert(storage, article)?;
                }
            }
            Err(err) => {
                // `{err:#}` prints the error and every cause beneath it on one
                // line, so the message says *why*, not just "failed".
                self.failure_messages
                    .push(format!("{}: {err:#}", outcome.name));
                self.record_failure(&err);
            }
        }
        Ok(())
    }

    /// `true` when at least one source was tried and *every* one failed
    /// because the network was unreachable, which is the best sign
    /// available that the machine is offline (rather than one feed being
    /// down).
    pub fn is_offline(&self) -> bool {
        self.attempted > 0 && self.unreachable == self.attempted
    }
}

/// Whether `err` (or anything it wraps) is a `reqwest` connection failure
/// or timeout. DNS failures, "network unreachable", and refused
/// connections all count as connection errors.
///
/// Fetch errors are `anyhow::Error`s with `.context(...)` messages layered
/// on top of the original `reqwest::Error` (see `fetchers::rss`).
/// `err.chain()` walks that stack from the outermost message down to the
/// root cause, and `downcast_ref::<reqwest::Error>()` asks each layer
/// "are you a `reqwest::Error`?", returning `Some(&reqwest::Error)` if so.
/// `any` stops at the first layer that matches.
fn is_unreachable(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<reqwest::Error>()
            .is_some_and(|e| e.is_connect() || e.is_timeout())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A non-network error counts as a failure but not as unreachable, so
    /// one broken feed never reports the whole app as offline.
    #[test]
    fn non_network_failure_is_not_offline() {
        let mut report = FetchReport {
            attempted: 1,
            ..FetchReport::default()
        };
        report.record_failure(&anyhow::anyhow!("failed to parse feed XML"));
        assert_eq!(report.failed, 1);
        assert_eq!(report.unreachable, 0);
        assert!(!report.is_offline());
    }

    /// A real `reqwest` connection failure, wrapped in `anyhow` context the
    /// way `fetchers::rss` wraps it, is recognized as unreachable. Uses
    /// port 1 on loopback (`127.0.0.1`), where nothing listens, so the
    /// connection is refused immediately and no real network is touched.
    ///
    /// `#[tokio::test]` is `#[test]` plus a tokio runtime, needed because
    /// `send()` is `async`. `no_proxy()` stops an `HTTP_PROXY` setting in
    /// the environment from turning this into a proxy request.
    #[tokio::test]
    async fn refused_connection_counts_as_unreachable() {
        use anyhow::Context;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let err = client
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .context("failed to fetch feed")
            .unwrap_err();

        let mut report = FetchReport {
            attempted: 1,
            ..FetchReport::default()
        };
        report.record_failure(&err);
        assert_eq!(report.unreachable, 1, "{err:#}");
        assert!(report.is_offline());
    }

    /// `record_outcome` stores a successful fetch's articles (counting only
    /// the new ones) and counts a failed fetch with its message, so one dead
    /// source does not stop the rest being recorded.
    #[test]
    fn record_outcome_stores_successes_and_counts_failures() {
        let storage = Storage::open_in_memory().unwrap();
        let article = Article::new(
            "A title".to_string(),
            "https://example.com/a".to_string(),
            "Example".to_string(),
            "tech".to_string(),
            chrono::Utc::now(),
        );

        let mut report = FetchReport::default();
        // A source that worked and returned one article.
        report
            .record_outcome(
                &storage,
                FetchOutcome {
                    name: "Example".to_string(),
                    result: Ok(vec![article.clone()]),
                },
            )
            .unwrap();
        // A source that failed.
        report
            .record_outcome(
                &storage,
                FetchOutcome {
                    name: "Broken".to_string(),
                    result: Err(anyhow::anyhow!("failed to parse feed XML")),
                },
            )
            .unwrap();
        // The same article again from a later fetch: stored once, not new.
        report
            .record_outcome(
                &storage,
                FetchOutcome {
                    name: "Example".to_string(),
                    result: Ok(vec![article]),
                },
            )
            .unwrap();

        assert_eq!(report.attempted, 3);
        assert_eq!(report.failed, 1);
        assert_eq!(report.new_article_ids.len(), 1);
        assert_eq!(storage.article_count().unwrap(), 1);
        assert_eq!(report.failure_messages.len(), 1);
        assert!(report.failure_messages[0].starts_with("Broken: "));
    }

    /// `fetch_all` returns one outcome per source, in order, and a dead
    /// source is an `Err` outcome rather than aborting the others. Port 1 on
    /// loopback is refused immediately, so no real network is touched.
    #[tokio::test]
    async fn fetch_all_reports_every_source_even_when_all_fail() {
        use crate::fetchers::{SourceConfig, SourceType, configured_sources, fetch_all};

        // Two sources that both point at a port nothing listens on.
        let sources = configured_sources(vec![
            SourceConfig {
                name: "First".to_string(),
                source_type: SourceType::Rss,
                url: "http://127.0.0.1:1/a".to_string(),
                topic: "t".to_string(),
            },
            SourceConfig {
                name: "Second".to_string(),
                source_type: SourceType::Rss,
                url: "http://127.0.0.1:1/b".to_string(),
                topic: "t".to_string(),
            },
        ]);

        let outcomes = fetch_all(&sources).await;

        // Both are reported, in the order given, and both are failures.
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].name, "First");
        assert_eq!(outcomes[1].name, "Second");
        assert!(outcomes.iter().all(|outcome| outcome.result.is_err()));
    }

    /// Offline means every attempted source was unreachable, and nothing
    /// attempted means not offline.
    #[test]
    fn offline_requires_every_source_unreachable() {
        let empty = FetchReport::default();
        assert!(!empty.is_offline());

        let all_down = FetchReport {
            attempted: 2,
            failed: 2,
            unreachable: 2,
            ..FetchReport::default()
        };
        assert!(all_down.is_offline());

        let one_up = FetchReport {
            attempted: 2,
            failed: 1,
            unreachable: 1,
            ..FetchReport::default()
        };
        assert!(!one_up.is_offline());
    }
}
