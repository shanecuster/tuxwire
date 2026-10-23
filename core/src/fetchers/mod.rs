//! The `Fetcher` trait and one submodule per source type.
//!
//! Per `docs/ARCHITECTURE.md`: "Each source type (RSS/Atom, Reddit JSON,
//! HN/Algolia, future custom sources) implements a common `Fetcher`
//! trait. Every fetcher normalizes whatever it pulls down into the same
//! `Article` struct." This file defines that shared contract; each
//! submodule (starting with `rss`) provides one implementation of it.

use crate::models::Article;
use serde::Deserialize;

/// The one thing every source type has to be able to do: go fetch its
/// articles and hand back a normalized `Vec<Article>`.
///
/// ## What a trait actually is
///
/// A *trait* is Rust's version of an interface: it describes a chunk of
/// behavior ("can be asked to `fetch`") without saying anything about
/// what concrete type provides it. Any struct can `impl Fetcher for
/// MyStruct { ... }` as long as it supplies a `fetch` method matching
/// this signature. Calling code that only knows "I have *something* that
/// implements `Fetcher`" can call `.fetch()` on it without caring whether
/// it's actually an RSS fetcher, a Reddit fetcher, or anything else —
/// that's the whole point: the rest of the app (storage, the refresh
/// loop) is written once against `Fetcher`, not once per source type.
///
/// ## Why `async fn` in a trait needs a comment
///
/// Ordinarily, `.fetch()` would just run to completion and return.
/// Fetching a feed means making a network request, though, which can take
/// an unpredictable amount of time and shouldn't block anything else from
/// happening while it's in flight (per ARCHITECTURE.md: "Fetches run
/// concurrently via tokio, so one slow/dead source never blocks a refresh
/// of everything else"). Marking the method `async fn` means calling
/// `.fetch()` doesn't immediately run the body — it hands back a
/// `Future`, a value representing "this work, not started/finished yet."
/// Nothing actually happens until that `Future` is `.await`ed, and an
/// async runtime like `tokio` can juggle many in-flight `Future`s at
/// once (e.g. one per source) on a small pool of threads, switching
/// between them whenever one is waiting on I/O instead of dedicating a
/// whole OS thread to each. This is Rust's version of what `async`/`await`
/// in JavaScript or Python's `asyncio` does, but the `Future` itself is
/// inert data — nothing runs in the background just because you called an
/// async function, only once something drives it forward with `.await`
/// (or a runtime does that on your behalf).
///
/// Async trait methods are natively supported by the compiler as of
/// recent Rust editions, so no extra crate is needed to write this. The
/// one caveat worth knowing for later: a plain `async fn` in a trait
/// isn't automatically usable behind `dyn Fetcher` (a "trait object", used
/// when you want a `Vec` holding fetchers of different concrete types
/// together). We don't need that yet with a single fetcher implementation
/// — it'll be revisited (likely by boxing the returned `Future`) once a
/// second source type needs to run alongside this one.
//
// `#[allow(async_fn_in_trait)]` silences a compiler lint that appears because
// this trait is now `pub` in a library crate. The lint's worry: code that is
// *generic* over `Fetcher` cannot assume the returned `Future` is `Send`
// (safe to move to another thread), because the trait does not promise it.
// That does not affect us: every caller works with the concrete
// `ConfiguredSource` type, where the compiler can see the real future and
// knows it is `Send`. If we ever need generic, thread-moving code, the fix is
// to rewrite `fetch` as a plain `fn` returning `impl Future + Send`.
#[allow(async_fn_in_trait)]
pub trait Fetcher {
    /// Fetch this source's current articles.
    ///
    /// Returns `anyhow::Result<Vec<Article>>` rather than an infallible
    /// `Vec<Article>` because fetching genuinely can fail — the network
    /// could be down, the feed URL could 404, the XML could be malformed.
    /// `Result<T, E>` is Rust's mechanism for surfacing that in the type
    /// system: a `Result` is either `Ok(value)` or `Err(error)`, and
    /// there's no way to get at the `Vec<Article>` without a caller
    /// acknowledging the possibility of failure (via `?`, `match`,
    /// `.unwrap()`, etc.) — unlike an exception, a `Result` can't
    /// silently propagate past code that isn't expecting it.
    async fn fetch(&self) -> anyhow::Result<Vec<Article>>;
}

// One submodule per source type, per the module layout in
// ARCHITECTURE.md ("fetchers/ -- one module per source type"). `rss`
// covers both RSS and Atom feeds, since `feed-rs` (see Cargo.toml) parses
// both into one shape -- from the app's perspective they're the same
// source type. `discover` works out which feed belongs to an address a person
// pasted (a page or a feed). `reddit` is a placeholder (see its module doc comment).
//
// Reading `sources.toml` from disk is NOT done here any more: that is a
// terminal-app concern (an Android app keeps its feeds in the database), so
// the TOML loader lives in the `tui` crate and hands this module plain
// `SourceConfig` values.
pub mod discover;
pub mod file;
pub mod link_title;
pub mod reddit;
pub mod rss;

/// Which concrete fetcher a source's `type` names.
///
/// `"rss"`, `"reddit"` and `"file"` are all valid values a user can type in
/// `sources.toml` today -- but Reddit doesn't fetch anything yet (see
/// `fetchers::reddit`). Modeling this as an `enum` rather than leaving
/// `source_type` a bare `String` means an entry like `type =
/// "atom-ish-typo"` is caught right at config *parse* time, with serde's own
/// "unknown variant" message naming the bad value -- instead of surviving as
/// a `String` that every later piece of code would have to re-validate (or
/// silently mishandle) on its own.
///
/// `#[serde(rename_all = "lowercase")]` is what makes `Rss`/`Reddit` (the
/// idiomatic Rust `PascalCase` for enum variants) match the lowercase
/// `"rss"`/`"reddit"` strings actually written in the TOML file, without
/// needing a `#[serde(rename = "...")]` on each variant individually.
///
/// This type lives in the shared core crate (rather than next to the TOML
/// loader) because every program that fetches feeds -- the terminal app
/// reading `sources.toml`, the Android app reading its database -- needs to
/// describe "what kind of source is this".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    /// An RSS or Atom feed, fetched over HTTP.
    Rss,
    /// A Reddit subreddit (not implemented yet; see `fetchers::reddit`).
    Reddit,
    /// A feed saved in a file on this computer (`fetchers::file`). Only the
    /// terminal app creates these (from `type = "file"` in `sources.toml`);
    /// the `url` of such a source is the file's full path. They are never
    /// synced to other devices and never written to an OPML export, because a
    /// path on one computer means nothing on another.
    File,
}

impl SourceType {
    /// The short name stored in the `feeds.source_type` database column:
    /// `"rss"`, `"reddit"` or `"file"` (the same spellings `sources.toml`
    /// uses).
    ///
    /// Returns `&'static str`, a string baked into the program that lives for
    /// its whole run, so the caller gets it with no allocation and no ownership
    /// to think about.
    pub fn as_str(self) -> &'static str {
        // `match` must cover every variant, so adding a new `SourceType`
        // without a name here is a compile error.
        match self {
            SourceType::Rss => "rss",
            SourceType::Reddit => "reddit",
            SourceType::File => "file",
        }
    }

    /// The reverse of [`SourceType::as_str`]: turns a stored name back into a
    /// `SourceType`, or `None` if the text is not one we know (for example a
    /// value written by a newer version of the app).
    pub fn from_db_str(text: &str) -> Option<Self> {
        match text {
            "rss" => Some(SourceType::Rss),
            "reddit" => Some(SourceType::Reddit),
            "file" => Some(SourceType::File),
            // Anything else is unknown; the caller decides what to do.
            _ => None,
        }
    }
}

/// One fully-validated source description: every field guaranteed present
/// and non-empty by the time anything outside the code that built it sees it.
///
/// This is the plain-data hand-off between *where sources are stored*
/// (a TOML file on the desktop, a database table on Android) and *how they
/// are fetched* (`configured_sources` below turns these into fetchers). It
/// is deliberately a different type from the TOML loader's private
/// "raw" struct: serde deserializes into a shape that can represent "this
/// field was missing" (via `Option`), and the loader's validation turns that
/// into either a clear error or this type, where every field is simply,
/// unconditionally there -- so no caller downstream ever has to think about
/// a source with a missing `topic` again.
#[derive(Debug, Clone)]
pub struct SourceConfig {
    /// Display name, e.g. "Ars Technica". Shown in the UI and stored on each
    /// article as its `source`.
    pub name: String,
    /// Which fetcher implementation handles this source.
    pub source_type: SourceType,
    /// The feed URL (for Reddit, the subreddit name; for a file source, the
    /// file's full path).
    pub url: String,
    /// The single topic this source's articles are filed under.
    pub topic: String,
}

/// One configured source, already resolved into whichever concrete
/// fetcher type its `sources.toml` entry's `type = "..."` named (see
/// `SourceType`).
///
/// This exists instead of just returning `Vec<rss::RssFetcher>` (as this
/// function did before config-file loading existed) because
/// `sources.toml` can now list *more than one* source type in the same
/// file -- ARCHITECTURE.md's own example has both `type = "rss"` and
/// `type = "reddit"` entries side by side. Something has to hold "an RSS
/// fetcher, or a Reddit fetcher, or ..." as one list callers can iterate
/// over uniformly. An `enum` is the right tool here rather than `dyn
/// Fetcher` (a "trait object"): as `Fetcher`'s own doc comment notes, a
/// plain `async fn` in a trait isn't usable behind `dyn Fetcher` without
/// extra work (boxing the returned `Future`) that a small, closed set of
/// source types -- known completely at compile time -- doesn't need. An
/// `enum` with one variant per source type gets the same "one list, mixed
/// concrete types" result for free.
pub enum ConfiguredSource {
    Rss(rss::RssFetcher),
    Reddit(reddit::RedditFetcher),
    /// A feed read from a file on this computer (see `fetchers::file`).
    File(file::FileFetcher),
}

impl ConfiguredSource {
    /// This source's human-readable name, regardless of which variant it
    /// is -- used for log/status messages (see `main.rs`, `ui/mod.rs`)
    /// without every caller needing its own `match` to reach into
    /// whichever fetcher type is actually inside.
    pub fn name(&self) -> &str {
        match self {
            ConfiguredSource::Rss(fetcher) => &fetcher.name,
            ConfiguredSource::Reddit(fetcher) => &fetcher.name,
            ConfiguredSource::File(fetcher) => &fetcher.name,
        }
    }

    /// This source's configured topic -- used by the TUI's `r` refresh
    /// (`ui/mod.rs`'s `refresh_topic`) to filter down to just the sources
    /// filed under the topic currently selected.
    pub fn topic(&self) -> &str {
        match self {
            ConfiguredSource::Rss(fetcher) => &fetcher.topic,
            ConfiguredSource::Reddit(fetcher) => &fetcher.topic,
            ConfiguredSource::File(fetcher) => &fetcher.topic,
        }
    }

    /// This source's feed URL from `sources.toml`: what fetched articles
    /// record as their `feed_url`, and what the startup backfill
    /// (`Storage::backfill_feed_urls`) matches older articles against.
    pub fn url(&self) -> &str {
        match self {
            ConfiguredSource::Rss(fetcher) => &fetcher.url,
            ConfiguredSource::Reddit(fetcher) => &fetcher.url,
            // For a file source the "address" is the file's path.
            ConfiguredSource::File(fetcher) => &fetcher.path,
        }
    }
}

impl Fetcher for ConfiguredSource {
    /// Delegates to whichever concrete fetcher this source actually is.
    /// This is the whole payoff of the `enum` over having every caller
    /// `match` on `ConfiguredSource` itself before it can call `.fetch()`
    /// at all -- callers (`main.rs`'s fetch loop, `refresh_topic`) just
    /// call `.fetch()` once, uniformly, same as if there were still only
    /// one source type.
    async fn fetch(&self) -> anyhow::Result<Vec<Article>> {
        match self {
            ConfiguredSource::Rss(fetcher) => fetcher.fetch().await,
            ConfiguredSource::Reddit(fetcher) => fetcher.fetch().await,
            ConfiguredSource::File(fetcher) => fetcher.fetch().await,
        }
    }
}

/// The result of fetching one source: which source it was, and either its
/// articles or the error that stopped it.
///
/// Fetching and storing are kept apart on purpose. `fetch_one` and
/// `fetch_all` only talk to the network and never touch the database, so a
/// caller that shares its `Storage` behind a lock (the Android app) can
/// release that lock while the slow network work runs, and only take it again
/// to store the results with `FetchReport::record_outcome`.
pub struct FetchOutcome {
    /// The source's display name, kept so a failure message can say which
    /// feed it was.
    pub name: String,
    /// `Ok` with the fetched articles, or `Err` with why the fetch failed.
    pub result: anyhow::Result<Vec<Article>>,
}

/// Fetches one source and packages the answer as a [`FetchOutcome`].
///
/// A failure is *returned inside* the outcome rather than propagated with `?`,
/// because one dead feed must never stop the rest of a refresh ("one
/// slow/dead source never blocks a refresh of everything else", per
/// ARCHITECTURE.md).
///
/// `source: &ConfiguredSource` borrows the source: fetching only needs to
/// read it, and the caller keeps it for the next refresh.
pub async fn fetch_one(source: &ConfiguredSource) -> FetchOutcome {
    // `.await` runs the request to completion without blocking the thread.
    let result = source.fetch().await;
    FetchOutcome {
        name: source.name().to_string(),
        result,
    }
}

/// How many feeds [`fetch_all`] downloads at the same time. Enough that a
/// long list refreshes in a fraction of the time, few enough not to open dozens
/// of connections at once on a phone or hammer one server.
pub const MAX_CONCURRENT_FETCHES: usize = 6;

/// Fetches every source in `sources`, several at the same time (at most
/// [`MAX_CONCURRENT_FETCHES`]), and returns one [`FetchOutcome`] each, **in the
/// same order as `sources`** whichever finishes first.
///
/// A source that fails or times out only affects its own outcome; the others
/// carry on, so one dead feed never stops a refresh. Callers store the outcomes
/// one after another afterwards, so the database is never written to from
/// several places at once.
pub async fn fetch_all(sources: &[ConfiguredSource]) -> Vec<FetchOutcome> {
    // `StreamExt` adds the stream helpers used below (`map`, `buffered`,
    // `collect`) to anything that is a stream.
    use futures_util::stream::{self, StreamExt};

    // A "stream" is like an iterator whose items arrive over time. This one
    // reads as a pipeline:
    //   1. `stream::iter(sources)` yields each source in turn.
    //   2. `.map(fetch_one)` turns each source into a future (a download that
    //      has not started yet).
    //   3. `.buffered(N)` runs up to N of those futures at once, and hands the
    //      finished results back in the original order, holding a quick one
    //      back until the slower ones before it are done.
    //   4. `.collect()` gathers the results into a `Vec`, and `.await` waits
    //      for the whole thing.
    stream::iter(sources)
        .map(fetch_one)
        .buffered(MAX_CONCURRENT_FETCHES)
        .collect()
        .await
}

/// Turns validated source descriptions into fetchers that are ready to
/// `.fetch()`.
///
/// This is the file-free half of what the terminal app used to do in one
/// step. The terminal app reads `sources.toml` into a `Vec<SourceConfig>`
/// itself and then calls this; the Android app will build the same
/// `Vec<SourceConfig>` from its database. Either way, everything from here
/// on -- fetching, parsing, storing -- is identical.
///
/// `sources: Vec<SourceConfig>` takes the vector *by value* (ownership
/// moves in) because each fetcher below needs to own its `name`, `url` and
/// `topic` strings; consuming the vector lets us move the strings across
/// instead of cloning every one of them.
pub fn configured_sources(sources: Vec<SourceConfig>) -> Vec<ConfiguredSource> {
    // `into_iter()` walks the vector by value, `map` converts each
    // description into the matching fetcher variant, and `collect()` gathers
    // the results back into a `Vec`. The `match` on `source_type` is
    // exhaustive: adding a new `SourceType` without handling it here is a
    // compile error, which is exactly the safety net we want.
    sources
        .into_iter()
        .map(|source| match source.source_type {
            SourceType::Rss => ConfiguredSource::Rss(rss::RssFetcher {
                name: source.name,
                url: source.url,
                topic: source.topic,
            }),
            SourceType::Reddit => ConfiguredSource::Reddit(reddit::RedditFetcher {
                name: source.name,
                url: source.url,
                topic: source.topic,
            }),
            // A file source keeps its path in the `url` field of the config
            // (one shape for every source); the fetcher calls it what it is.
            SourceType::File => ConfiguredSource::File(file::FileFetcher {
                name: source.name,
                path: source.url,
                topic: source.topic,
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// Starts a tiny server on `127.0.0.1` (this computer only; nothing leaves
    /// the machine) that answers every request with a one-item RSS feed, but
    /// only after waiting `delay`. It counts how many requests are in progress
    /// at once and remembers the highest the count reached.
    ///
    /// Returns the port and that highest-count cell. Each connection runs on
    /// its own thread, so slow answers do not hold each other up. If a proxy is
    /// set in your environment, run the tests with `NO_PROXY=127.0.0.1`.
    fn serve_slowly(delay: Duration) -> (u16, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let peak_for_caller = Arc::clone(&peak);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let (active, peak) = (Arc::clone(&active), Arc::clone(&peak));
                std::thread::spawn(move || {
                    // One more request in progress; raise the peak if needed.
                    let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);

                    // Read the request, wait, then answer.
                    let mut buffer = [0u8; 2048];
                    let _ = stream.read(&mut buffer);
                    std::thread::sleep(delay);
                    let body = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>T</title><item><title>A</title><link>https://example.com/a</link></item></channel></rss>"#;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes());

                    // This request is finished.
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
        });
        (port, peak_for_caller)
    }

    /// Builds `count` sources named `f0`, `f1`, ... all pointing at `port`.
    fn sources_on(port: u16, count: usize) -> Vec<ConfiguredSource> {
        configured_sources(
            (0..count)
                .map(|n| SourceConfig {
                    name: format!("f{n}"),
                    source_type: SourceType::Rss,
                    url: format!("http://127.0.0.1:{port}/{n}"),
                    topic: "t".to_string(),
                })
                .collect(),
        )
    }

    /// Six feeds that each take 300 ms finish in roughly one delay, not six,
    /// and the outcomes still come back in the order the sources were given.
    #[tokio::test]
    async fn slow_feeds_download_at_the_same_time_and_stay_in_order() {
        let (port, peak) = serve_slowly(Duration::from_millis(300));
        let sources = sources_on(port, 6);

        let started = Instant::now();
        let outcomes = fetch_all(&sources).await;
        let elapsed = started.elapsed();

        // In order, all successful.
        let names: Vec<&str> = outcomes.iter().map(|o| o.name.as_str()).collect();
        assert_eq!(names, ["f0", "f1", "f2", "f3", "f4", "f5"]);
        assert!(outcomes.iter().all(|o| o.result.is_ok()));
        // One after another would take at least 6 x 300 ms = 1800 ms.
        assert!(elapsed < Duration::from_millis(1200), "took {elapsed:?}");
        assert!(peak.load(Ordering::SeqCst) >= 2);
    }

    /// With more feeds than the limit, no more than the limit are ever in
    /// progress at once.
    #[tokio::test]
    async fn no_more_than_the_limit_run_at_once() {
        let (port, peak) = serve_slowly(Duration::from_millis(150));
        let sources = sources_on(port, MAX_CONCURRENT_FETCHES * 2 + 2);

        let outcomes = fetch_all(&sources).await;

        assert_eq!(outcomes.len(), sources.len());
        let highest = peak.load(Ordering::SeqCst);
        assert!(highest <= MAX_CONCURRENT_FETCHES, "peak was {highest}");
        assert!(highest >= 2, "peak was {highest}");
    }

    /// One unreachable feed in the middle fails on its own; the ones around it
    /// are fetched normally and the order is kept.
    #[tokio::test]
    async fn one_dead_feed_does_not_stop_the_others() {
        let (port, _) = serve_slowly(Duration::from_millis(50));
        // Find a port nothing is listening on, by binding and then dropping.
        let dead_port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut configs = Vec::new();
        for (name, p) in [("good1", port), ("dead", dead_port), ("good2", port)] {
            configs.push(SourceConfig {
                name: name.to_string(),
                source_type: SourceType::Rss,
                url: format!("http://127.0.0.1:{p}/x"),
                topic: "t".to_string(),
            });
        }
        let outcomes = fetch_all(&configured_sources(configs)).await;

        let results: Vec<(&str, bool)> = outcomes
            .iter()
            .map(|o| (o.name.as_str(), o.result.is_ok()))
            .collect();
        assert_eq!(results, [("good1", true), ("dead", false), ("good2", true)]);
    }

    /// Every source type's database name reads back as the same type, so a
    /// feed stored with `add_feed` is never silently dropped on the way back
    /// out (including the file type).
    #[test]
    fn source_type_names_round_trip() {
        for kind in [SourceType::Rss, SourceType::Reddit, SourceType::File] {
            assert_eq!(SourceType::from_db_str(kind.as_str()), Some(kind));
        }
        assert_eq!(SourceType::from_db_str("carrier-pigeon"), None);
    }

    /// A file source built through `configured_sources` reports its path as
    /// its address and fetches the file's articles.
    #[tokio::test]
    async fn a_file_source_is_configured_and_fetched() {
        // Write a one-item feed to a file in the temporary folder.
        let path = std::env::temp_dir().join(format!(
            "tuxwire-configured-file-source-{}.rss",
            std::process::id()
        ));
        std::fs::write(
            &path,
            r#"<?xml version="1.0"?><rss version="2.0"><channel><title>T</title><item><title>A</title><link>https://example.com/a</link></item></channel></rss>"#,
        )
        .unwrap();

        // Build it the way the apps do, then fetch it with `fetch_one`.
        let sources = configured_sources(vec![SourceConfig {
            name: "Saved".to_string(),
            source_type: SourceType::File,
            url: path.to_string_lossy().into_owned(),
            topic: "t".to_string(),
        }]);
        assert_eq!(sources[0].url(), path.to_string_lossy());
        let outcome = fetch_one(&sources[0]).await;
        let articles = outcome.result.expect("the file should load");
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "A");

        let _ = std::fs::remove_file(&path);
    }
}
