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
// source type.
pub mod rss;

/// The source list, standing in for `sources.toml` until config-file
/// loading is built (see ARCHITECTURE.md's Configuration section — this
/// is the same one-entry set `main.rs` used to build inline before this
/// function existed). Pulled out here, rather than left inline in
/// `main.rs`, so it's the single place both the app's initial fetch
/// (`main.rs`) and the TUI's `r` refresh keybind (`ui/mod.rs`) read
/// from — adding a second hardcoded source later means editing this list
/// once, not two call sites that would otherwise be free to drift apart.
pub fn configured_sources() -> Vec<rss::RssFetcher> {
    vec![rss::RssFetcher {
        name: String::from("Phoronix"),
        url: String::from("https://www.phoronix.com/rss.php"),
        topic: String::from("linux-news"),
    }]
}
