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
// `src/icons/mod.rs`.
#![warn(clippy::missing_docs_in_private_items)]

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
