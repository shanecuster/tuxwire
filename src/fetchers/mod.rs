//! The `Fetcher` trait and one submodule per source type.
//!
//! Per `docs/ARCHITECTURE.md`: "Each source type (RSS/Atom, Reddit JSON,
//! HN/Algolia, future custom sources) implements a common `Fetcher`
//! trait. Every fetcher normalizes whatever it pulls down into the same
//! `Article` struct." This file defines that shared contract; each
//! submodule (starting with `rss`) provides one implementation of it.

use crate::models::Article;

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
// ARCHITECTURE.md ("fetchers/ — one module per source type"). `rss`
// covers both RSS and Atom feeds, since `feed-rs` (see Cargo.toml) parses
// both into one shape — from the app's perspective they're the same
// source type. `reddit` is a placeholder (see its module doc comment) --
// `config` is the `sources.toml` loader/validator, not a source type
// itself, but lives here rather than as a top-level module since it only
// exists to produce the values this module hands out.
pub mod config;
pub mod reddit;
pub mod rss;

/// One configured source, already resolved into whichever concrete
/// fetcher type its `sources.toml` entry's `type = "..."` named (see
/// `config::SourceType`).
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
        }
    }

    /// This source's configured topic -- used by the TUI's `r` refresh
    /// (`ui/mod.rs`'s `refresh_topic`) to filter down to just the sources
    /// filed under the topic currently selected.
    pub fn topic(&self) -> &str {
        match self {
            ConfiguredSource::Rss(fetcher) => &fetcher.topic,
            ConfiguredSource::Reddit(fetcher) => &fetcher.topic,
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
        }
    }
}

/// Loads and validates `sources.toml` (via `config::load_sources`) and
/// resolves every entry into a real `ConfiguredSource` ready to
/// `.fetch()`. This is the single place both the app's initial fetch
/// (`main.rs`) and the TUI's `r` refresh keybind (`ui/mod.rs`) read
/// from -- same reasoning as before config-file loading existed, just now
/// backed by a real file instead of a hardcoded `vec![...]`.
///
/// Returns `anyhow::Result` (unlike the hardcoded version this replaces,
/// which couldn't fail) because loading a config file genuinely can:
/// the file might not parse as TOML, or a source in it might be missing
/// its required `topic`. Every caller already propagates this with `?`,
/// same as any other fallible step.
pub fn configured_sources() -> anyhow::Result<Vec<ConfiguredSource>> {
    let sources = config::load_sources()?;

    Ok(sources
        .into_iter()
        .map(|source| match source.source_type {
            config::SourceType::Rss => ConfiguredSource::Rss(rss::RssFetcher {
                name: source.name,
                url: source.url,
                topic: source.topic,
            }),
            config::SourceType::Reddit => ConfiguredSource::Reddit(reddit::RedditFetcher {
                name: source.name,
                url: source.url,
                topic: source.topic,
            }),
        })
        .collect())
}
