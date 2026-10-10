//! Fetches and parses a single RSS or Atom feed into `Article`s.
//!
//! This is the first working milestone for tuxwire: given a feed URL,
//! download it and normalize every entry into the shared `Article` shape
//! from `models.rs`.

use crate::fetchers::Fetcher;
use crate::models::Article;
use crate::text::{SUMMARY_MAX_CHARS, html_to_text, limit_to_sentences, tidy_plain_text};
use anyhow::Context;
use chrono::Utc;
use std::sync::LazyLock;
use std::time::Duration;

/// The longest one feed may take, from starting the request to having read the
/// whole answer. A server that accepts a connection and then goes silent would
/// otherwise stall a refresh forever; with this limit that one feed simply
/// fails ("operation timed out") and the others carry on.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// The `User-Agent` header sent with every request: the program's name and
/// the version of this crate. `env!("CARGO_PKG_VERSION")` is filled in by
/// Cargo while compiling (it reads `version` from `core/Cargo.toml`), and
/// `concat!` joins the pieces into one string at compile time, so there is no
/// work to do while the program runs.
const USER_AGENT: &str = concat!("tuxwire/", env!("CARGO_PKG_VERSION"));

/// The `reqwest::Client` every fetch in this module shares, built once on
/// first use (`LazyLock` runs its initializer exactly once, the first time
/// `HTTP_CLIENT` is dereferenced, and caches the result for every call
/// after that). It uses [`FETCH_TIMEOUT`] as its time limit.
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| build_client(FETCH_TIMEOUT));

/// Builds an HTTP client whose requests fail after `timeout`.
///
/// This is *not* `reqwest::Client::new()` -- see the `reqwest` comment in
/// Cargo.toml. Explicitly handing the builder its own root certs (via
/// `webpki_root_certs`, Mozilla's bundled list) and calling
/// `.tls_certs_only(...)` keeps this client off the OS-backed
/// `rustls-platform-verifier` codepath entirely, which is what avoids the
/// Android/Termux panic that motivated this.
///
/// It is a function (rather than only the `HTTP_CLIENT` value) so that a test
/// can build a client with a very short time limit and check that a silent
/// server really is cut off, without waiting 30 seconds.
fn build_client(timeout: Duration) -> reqwest::Client {
    let root_certs = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|der| {
            reqwest::Certificate::from_der(der.as_ref())
                .expect("webpki-root-certs' bundled certs are always valid DER")
        })
        .collect::<Vec<_>>();

    reqwest::Client::builder()
        .tls_certs_only(root_certs)
        // Say who is asking. A request with no `User-Agent` header looks like
        // a bot or a broken script, and some sites (or the content networks in
        // front of them) answer it with an empty page instead of the feed.
        .user_agent(USER_AGENT)
        // The whole request, including reading the body, must finish within
        // this time or it fails with a timeout error.
        .timeout(timeout)
        .build()
        .expect("building the shared HTTP client with bundled root certs should never fail")
}

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
    // The everyday entry point: the shared client, with its 30-second limit.
    fetch_feed_with(&HTTP_CLIENT, url).await
}

/// The same as [`fetch_feed`], but using the HTTP client it is given.
///
/// `client: &reqwest::Client` borrows a client, so many calls can share one
/// (and its open connections) without copying it. The only reason this is a
/// separate function is so tests can pass a client with a very short time
/// limit.
async fn fetch_feed_with(
    client: &reqwest::Client,
    url: &str,
) -> anyhow::Result<feed_rs::model::Feed> {
    // `client.get(url).send()` is the explicit-`Client` equivalent of
    // the `reqwest::get(url)` convenience function -- we can't use that
    // convenience function here because it always builds its *own*
    // internal client with no root certs of its own, which is exactly the
    // codepath that hits the platform verifier (see `build_client`'s doc
    // comment above). Otherwise this behaves the same way: it's `async`,
    // so this line returns a `Future` immediately; `.await` is what
    // actually drives the request and suspends this function (without
    // blocking the whole program) until a response arrives.
    //
    // `.with_context(...)` (from anyhow) attaches a human-readable
    // message to the error *if* this step fails, without changing
    // anything on the success path. The closure `|| format!(...)`
    // only actually runs when there's an error to attach it to — it's
    // not wasted work formatting a string on every successful fetch.
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("failed to fetch feed at {url}"))?;

    // Check the HTTP status before reading anything. A refusal (403, 429,
    // 500...) usually comes with a web page or nothing at all, and handing
    // that to the XML parser produces a confusing "failed to parse" message
    // that blames the feed. `is_success()` is true only for 200-299.
    // `bail!` returns an `Err` from this function right away, like `?` does.
    // Redirects (301, 302...) never reach this line: reqwest follows them.
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("{url} answered HTTP {status}");
    }

    // Read the whole response body into a `String`. RSS/Atom feeds
    // are XML text, so `.text()` (rather than `.bytes()`) is the
    // natural fit here — it also handles the response's declared
    // character encoding for us.
    let body = response
        .text()
        .await
        .with_context(|| format!("failed to read response body from {url}"))?;

    // A successful status with nothing in it is also not a feed. Say so
    // plainly, instead of letting the parser report "no root element".
    // `trim()` ignores blank space, so a body of only newlines counts as empty.
    if body.trim().is_empty() {
        anyhow::bail!("{url} sent an empty reply (no feed in it)");
    }

    // `feed_rs::parser::parse` accepts anything implementing
    // `std::io::Read`; `&str` doesn't implement `Read` directly, but
    // `.as_bytes()` gives us a `&[u8]` byte slice, which does. This
    // is the same parser regardless of whether the feed turns out to
    // be RSS or Atom — `feed-rs` detects the format and returns one
    // unified `Feed` either way.
    feed_rs::parser::parse(body.as_bytes())
        .with_context(|| format!("failed to parse feed XML from {url}"))
}

/// Works out the plain-text summary for one feed entry, or `None` if the entry
/// has no usable text.
///
/// Feeds put the text in one of two places: a short `summary` (RSS
/// `<description>`, Atom `<summary>`) or the full `content` (RSS
/// `<content:encoded>`, Atom `<content>`). The short one is preferred because
/// it is written to be a summary; the full content is the fallback, and is cut
/// down by the same sentence-aware limit.
///
/// `entry: &Entry` borrows the entry, so the caller can keep using it.
fn entry_summary(entry: &feed_rs::model::Entry) -> Option<String> {
    // Step 1: the entry's short summary, cleaned up. `as_ref()` turns
    // `&Option<Text>` into `Option<&Text>` so we can look inside without
    // taking it out of the entry. `subty()` is the second half of the media
    // type, e.g. `html` in `text/html` or `plain` in `text/plain`.
    let from_summary = entry
        .summary
        .as_ref()
        .map(|text| clean_feed_text(&text.content, text.content_type.subty().as_str()));

    // Step 2: the full content, cleaned up the same way. `as_deref()` turns
    // `Option<String>` into `Option<&str>` (a borrow, no copy).
    let from_body = entry.content.as_ref().and_then(|content| {
        content
            .body
            .as_deref()
            .map(|body| clean_feed_text(body, content.content_type.subty().as_str()))
    });

    // Step 3: use the summary if it has any words in it, otherwise the body.
    // The `?` returns `None` from this function if neither has any text.
    let text = from_summary
        .filter(|text| !text.is_empty())
        .or_else(|| from_body.filter(|text| !text.is_empty()))?;

    // Step 4: cut it at the end of a sentence if it is too long.
    Some(limit_to_sentences(&text, SUMMARY_MAX_CHARS))
}

/// Turns the text of one feed element into plain text, depending on its
/// declared type: text that says it is plain is only tidied (so a `<` in it is
/// a real less-than sign), and anything else (HTML, XHTML) has its markup
/// removed.
fn clean_feed_text(raw: &str, subtype: &str) -> String {
    if subtype == "plain" {
        tidy_plain_text(raw)
    } else {
        html_to_text(raw)
    }
}

impl Fetcher for RssFetcher {
    /// Fetches and parses `self.url` (via `fetch_feed` above) and maps
    /// every entry in the result into an `Article`.
    async fn fetch(&self) -> anyhow::Result<Vec<Article>> {
        let feed = fetch_feed(&self.url)
            .await
            .with_context(|| format!("fetching '{}'", self.name))?;

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
                // The summary is worked out first, because it only *borrows*
                // the entry (`&entry`); the lines below move pieces out of
                // `entry`, after which it could no longer be borrowed whole.
                let summary = entry_summary(&entry);

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

                // Record which feed this came from (`self.url` is the
                // source's feed URL from `sources.toml`), so feed lists can
                // tell their articles apart. `mut` because the field is set
                // after construction.
                let mut article =
                    Article::new(title, url, self.name.clone(), self.topic.clone(), timestamp);
                article.feed_url = Some(self.url.clone());
                article.summary = summary;
                article
            })
            .collect();

        Ok(articles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server that accepts the connection and then says nothing is cut off
    /// at the time limit instead of hanging, and the error says why.
    ///
    /// Uses a client built with a 300 ms limit (not the real 30 seconds) and a
    /// tiny server on `127.0.0.1` (this computer only) that sleeps for 3
    /// seconds before answering. If a proxy is set in your environment, run
    /// with `NO_PROXY=127.0.0.1`.
    #[tokio::test]
    async fn a_silent_server_times_out() {
        use std::io::Read;
        use std::net::TcpListener;

        // Step 1: a listener on a free port the operating system picks (`:0`).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            // Accept one connection, read the request, then say nothing.
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 1024];
                let _ = stream.read(&mut buffer);
                std::thread::sleep(Duration::from_secs(3));
            }
        });

        // Step 2: fetch with a short limit and time how long it takes.
        let client = build_client(Duration::from_millis(300));
        let started = std::time::Instant::now();
        let result = fetch_feed_with(&client, &format!("http://127.0.0.1:{port}/feed")).await;

        // Step 3: it failed, quickly, and not because of anything else.
        assert!(result.is_err());
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "took {:?}",
            started.elapsed()
        );
    }

    /// Starts a one-shot server on `127.0.0.1` that answers the first request
    /// with `reply` (a complete HTTP response) and returns the address to
    /// fetch plus a channel that delivers the request text the server saw.
    fn serve_once(reply: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // Step 1: listen on a free port the operating system picks (`:0`).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = std::sync::mpsc::channel();

        // Step 2: in the background, accept one connection, pass the request
        // text back through the channel, and write the canned reply.
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let _ = sender.send(String::from_utf8_lossy(&buffer[..read]).into_owned());
                let _ = stream.write_all(reply.as_bytes());
            }
        });

        (format!("http://127.0.0.1:{port}/feed"), receiver)
    }

    /// Every request says who is asking: `User-Agent: tuxwire/<version>`.
    /// (Header names are case-insensitive, so the check lowercases the text.)
    #[tokio::test]
    async fn requests_send_a_user_agent() {
        let (url, seen) =
            serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        // The empty reply makes the fetch fail, which is fine: only the
        // request that was sent matters here.
        let _ = fetch_feed_with(&build_client(Duration::from_secs(5)), &url).await;
        let request = seen.recv().unwrap().to_lowercase();
        let expected = format!("user-agent: tuxwire/{}", env!("CARGO_PKG_VERSION"));
        assert!(request.contains(&expected), "request was: {request}");
    }

    /// A refusal says which status the server sent, not "failed to parse".
    #[tokio::test]
    async fn an_http_error_is_named() {
        let (url, _seen) =
            serve_once("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let error = fetch_feed_with(&build_client(Duration::from_secs(5)), &url)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("403"), "error was: {error}");
        assert!(!error.contains("parse"), "error was: {error}");
    }

    /// A "200 OK" with nothing in it says the reply was empty.
    #[tokio::test]
    async fn an_empty_reply_is_named() {
        let (url, _seen) =
            serve_once("HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let error = fetch_feed_with(&build_client(Duration::from_secs(5)), &url)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("empty reply"), "error was: {error}");
    }

    /// Parses `xml` as a feed and returns the summary of its first entry.
    fn first_summary(xml: &str) -> Option<String> {
        let feed = feed_rs::parser::parse(xml.as_bytes()).expect("test feed should parse");
        entry_summary(&feed.entries[0])
    }

    /// A wrapper around one RSS item, so each test only writes the part that
    /// differs.
    fn rss(item_body: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
<rss version="2.0" xmlns:content="http://purl.org/rss/1.0/modules/content/">
<channel><title>T</title><item><title>A</title><link>https://example.com/a</link>{item_body}</item></channel></rss>"#
        )
    }

    /// An HTML description becomes plain text.
    #[test]
    fn html_description_becomes_plain_text() {
        let xml =
            rss("<description><![CDATA[<p>Hello &amp; welcome.</p><p>Second.</p>]]></description>");
        assert_eq!(
            first_summary(&xml).as_deref(),
            Some("Hello & welcome.\n\nSecond.")
        );
    }

    /// An entry with no description and no content has no summary.
    #[test]
    fn entry_without_text_has_no_summary() {
        assert_eq!(first_summary(&rss("")), None);
        // A description that is only markup also counts as nothing.
        assert_eq!(
            first_summary(&rss("<description><![CDATA[<br/> ]]></description>")),
            None
        );
    }

    /// With no description, the full content is used instead.
    #[test]
    fn full_content_is_the_fallback() {
        let xml = rss("<content:encoded><![CDATA[<p>Body text here.</p>]]></content:encoded>");
        assert_eq!(first_summary(&xml).as_deref(), Some("Body text here."));
    }

    /// When both exist, the short description wins.
    #[test]
    fn description_is_preferred_over_content() {
        let xml = rss("<description>Short one.</description>\
             <content:encoded><![CDATA[<p>Much longer body.</p>]]></content:encoded>");
        assert_eq!(first_summary(&xml).as_deref(), Some("Short one."));
    }

    /// Atom text declared as plain keeps a literal `<`.
    #[test]
    fn plain_atom_summary_keeps_angle_brackets() {
        let xml = r#"<?xml version="1.0"?>
<feed xmlns="http://www.w3.org/2005/Atom"><title>T</title><id>x</id><updated>2026-01-01T00:00:00Z</updated>
<entry><title>A</title><id>y</id><updated>2026-01-01T00:00:00Z</updated>
<link href="https://example.com/a"/><summary type="text">1 &lt; 2 is true.</summary></entry></feed>"#;
        assert_eq!(first_summary(xml).as_deref(), Some("1 < 2 is true."));
    }

    /// A very long description is cut at a sentence end within the limit.
    #[test]
    fn long_description_is_cut_at_a_sentence() {
        let long = "This is a sentence about something. ".repeat(200);
        let xml = rss(&format!("<description>{long}</description>"));
        let summary = first_summary(&xml).unwrap();
        assert!(summary.chars().count() <= SUMMARY_MAX_CHARS);
        assert!(summary.ends_with('.'));
    }
}
