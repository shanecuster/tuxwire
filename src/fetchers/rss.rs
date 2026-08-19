//! Fetches and parses a single RSS or Atom feed into `Article`s.
//!
//! This is the first working milestone for tuxwire: given a feed URL,
//! download it and normalize every entry into the shared `Article` shape
//! from `models.rs`.

use crate::fetchers::Fetcher;
use crate::models::Article;
use anyhow::Context;
use chrono::Utc;

/// One RSS/Atom source, built from a `[[source]]` block in `sources.toml`
/// with `type = "rss"` (see `fetchers::config` and ARCHITECTURE.md's
/// Configuration section) by `fetchers::configured_sources`.
pub struct RssFetcher {
    /// Human-readable source name (e.g. "Phoronix"), copied onto every
    /// `Article` this fetcher produces so the TUI can show where each
    /// article came from.
    pub name: String,

    /// The feed URL itself (e.g. `.../rss.php`, not the site's homepage).
    pub url: String,

    /// Which topic sidebar entry these articles should be filed under.
    pub topic: String,
}

/// Downloads and parses the feed at `url`, returning the raw
/// `feed_rs::model::Feed` rather than a `Vec<Article>`.
///
/// `RssFetcher::fetch` below is *built on* this — it just maps the
/// resulting `Feed`'s entries into `Article`s — but this function has a
/// second caller: the in-app "add a source" flow (`ui`'s `a` keybind, see
/// `docs/ARCHITECTURE.md`'s Adding Sources section) needs the *feed's
/// own* `<title>` to guess a name for the confirm screen, which only
/// exists on the `Feed` itself, not on any individual entry. A successful
/// parse here is also literally the entire validation that flow performs
/// — "tuxwire validates it by trying to parse it, that's the whole
/// mechanism" — so one function correctly serves both jobs instead of
/// duplicating this fetch-then-parse logic in `ui`.
///
/// ## Reading this function as a Rust newcomer
///
/// The `?` operator appears after almost every step. Each of those
/// steps returns a `Result<T, E>` — `reqwest::get` can fail (DNS
/// error, connection refused, timeout...), reading the response body
/// can fail (connection dropped mid-transfer), and parsing the feed
/// can fail (the response wasn't valid RSS/Atom XML). `?` says: "if
/// this is `Err`, stop this function immediately and return that
/// error to *my* caller; if it's `Ok`, unwrap the value and keep
/// going." It's how Rust does what other languages use exceptions
/// for, except the possibility of failure is visible in every
/// function signature (`-> anyhow::Result<...>`) rather than being an
/// invisible control-flow path that might or might not be caught
/// somewhere up the call stack.
///
/// For `?` to work here, whatever error type each step produces has
/// to be convertible into this function's error type
/// (`anyhow::Error`, via the `anyhow::Result<...>` return
/// type). `anyhow::Error` can be built from *any* type implementing
/// Rust's standard `std::error::Error` trait, which is exactly why
/// `anyhow` is convenient here: `reqwest`'s error type and
/// `feed_rs`'s error type are unrelated to each other, but `?` can
/// convert either of them into one common `anyhow::Error` without us
/// writing any conversion code by hand.
pub async fn fetch_feed(url: &str) -> anyhow::Result<feed_rs::model::Feed> {
    // `reqwest::get` is a convenience wrapper around building a
    // `Client` and issuing a GET request. It's `async`, so this line
    // returns a `Future` immediately; `.await` is what actually
    // drives the request and suspends this function (without
    // blocking the whole program) until a response arrives.
    //
    // `.with_context(...)` (from anyhow) attaches a human-readable
    // message to the error *if* this step fails, without changing
    // anything on the success path. The closure `|| format!(...)`
    // only actually runs when there's an error to attach it to — it's
    // not wasted work formatting a string on every successful fetch.
    let response = reqwest::get(url)
        .await
        .with_context(|| format!("failed to fetch feed at {url}"))?;

    // Read the whole response body into a `String`. RSS/Atom feeds
    // are XML text, so `.text()` (rather than `.bytes()`) is the
    // natural fit here — it also handles the response's declared
    // character encoding for us.
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read response body from {url}"))?;

    // `feed_rs::parser::parse` accepts anything implementing
    // `std::io::Read`; `&str` doesn't implement `Read` directly, but
    // `.as_bytes()` gives us a `&[u8]` byte slice, which does. This
    // is the same parser regardless of whether the feed turns out to
    // be RSS or Atom — `feed-rs` detects the format and returns one
    // unified `Feed` either way.
    feed_rs::parser::parse(body.as_bytes()).with_context(|| format!("failed to parse feed XML from {url}"))
}

impl Fetcher for RssFetcher {
    /// Fetches and parses `self.url` (via `fetch_feed` above) and maps
    /// every entry in the result into an `Article`.
    async fn fetch(&self) -> anyhow::Result<Vec<Article>> {
        let feed = fetch_feed(&self.url).await.with_context(|| format!("fetching '{}'", self.name))?;

        // Build the `Vec<Article>` we're returning. `feed.entries` is a
        // `Vec<feed_rs::model::Entry>` — we walk it with `.into_iter()`
        // (which *consumes* the `Vec`, handing us owned `Entry` values
        // one at a time instead of borrowed references, since we don't
        // need `feed` afterwards) and `.map(...)` each `Entry` into our
        // own `Article` type. `.collect()` gathers the mapped iterator
        // back into a concrete `Vec<Article>` — Rust needs the `let
        // articles: Vec<Article> = ...` type annotation (or one inferred
        // from the function's return type) because `.collect()` is
        // generic over *many* possible container types, not just `Vec`.
        let articles: Vec<Article> = feed
            .entries
            .into_iter()
            .map(|entry| {
                // `entry.title` is `Option<feed_rs::model::Text>` — a
                // feed entry isn't guaranteed to have a title. `.map(...)`
                // on an `Option` transforms the value *if present*
                // without needing an explicit `if`/`match`; `.unwrap_or_else(...)`
                // then supplies a fallback if it was `None`. Chaining the
                // two reads almost like a sentence: "the title's text
                // content, or '(untitled)' if there wasn't one."
                let title = entry
                    .title
                    .map(|t| t.content)
                    .unwrap_or_else(|| String::from("(untitled)"));

                // `entry.links` is a `Vec<Link>` — most feeds put exactly
                // one link on each entry, but the type allows more, so we
                // take the first one. `.first()` returns `Option<&Link>`
                // (a reference, since it's borrowing out of the `Vec`
                // rather than removing from it), hence `.map(|link|
                // link.href.clone())` to get an owned `String` out of the
                // borrowed `Link`.
                let url = entry
                    .links
                    .first()
                    .map(|link| link.href.clone())
                    .unwrap_or_default();

                // Prefer the entry's `published` date; fall back to
                // `updated` (some feeds only set one or the other); fall
                // back to "right now" as a last resort so every `Article`
                // always has *a* timestamp to sort by, even from a feed
                // that omits dates entirely. `.or(...)` on an `Option`
                // returns the first `Some`, trying the next `Option` only
                // if the first was `None` — the same short-circuiting
                // idea as `||` for booleans, just for `Option`s.
                let timestamp = entry.published.or(entry.updated).unwrap_or_else(Utc::now);

                Article::new(title, url, self.name.clone(), self.topic.clone(), timestamp)
            })
            .collect();

        Ok(articles)
    }
}
