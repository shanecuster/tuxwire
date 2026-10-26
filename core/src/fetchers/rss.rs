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
pub(crate) static HTTP_CLIENT: LazyLock<reqwest::Client> =
    LazyLock::new(|| build_client(FETCH_TIMEOUT));

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
    client_builder()
        // The whole request, including reading the body, must finish within
        // this time or it fails with a timeout error.
        .timeout(timeout)
        .build()
        .expect("building the shared HTTP client with bundled root certs should never fail")
}

/// The HTTP client settings every part of tuxwire shares: the bundled root
/// certificates (see [`build_client`]) and the `User-Agent`. Returned as a
/// builder so a caller can add its own limits (the CouchDB sync adds a short
/// connect timeout) before calling `.build()`.
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
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

/// The most bytes of one feed we are willing to read: 20 MiB. Real feeds are
/// a few hundred kilobytes at most; the limit only stops a broken or hostile
/// server from making us read forever and fill the phone's memory.
pub(crate) const FEED_MAX_BYTES: usize = 20 * 1024 * 1024;

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
    // The shared version: no extra time limit beyond the client's own.
    fetch_feed_limited(client, url, None).await
}

/// Downloads and parses the feed at `url`, optionally with a time limit of its
/// own that replaces the client's. Feed discovery uses a shorter limit when it
/// is only *guessing* at addresses, so a slow site cannot stall it for long.
///
/// `timeout: Option<Duration>` is `Some(limit)` or `None` (`None` means "keep
/// the client's own limit").
pub(crate) async fn fetch_feed_limited(
    client: &reqwest::Client,
    url: &str,
    timeout: Option<Duration>,
) -> anyhow::Result<feed_rs::model::Feed> {
    // Step 1: download the bytes. All the HTTP details (status check, size
    // limit, empty reply) live in `fetch_bytes`, which discovery shares.
    let bytes = fetch_bytes(client, url, FEED_MAX_BYTES, timeout).await?;

    // Step 2: parse them with the shared parser (see `parse_feed_bytes`),
    // naming the address in the error so a failure says which feed it was.
    parse_feed_bytes(&bytes).with_context(|| format!("failed to parse feed XML from {url}"))
}

/// Parses the raw bytes of an RSS or Atom feed into a `feed_rs` `Feed`.
///
/// This is the one place the bytes-to-feed step lives, so every way of getting
/// feed bytes shares it: a download ([`fetch_feed_limited`]), discovery
/// (`fetchers::discover`), and a feed *file* a person already has on disk (the
/// terminal app's `type = "file"` source and "add a feed from a file"; the
/// Android app can pass the bytes its file picker gives it). It does no I/O and
/// never looks at a path, which is why it is safe to live in the shared core
/// crate: the program decides where the bytes came from.
///
/// `bytes: &[u8]` borrows a run of bytes (nothing is copied or consumed).
/// `feed_rs::parser::parse` accepts anything implementing `std::io::Read`, and
/// `&[u8]` does. Handing it the raw bytes, instead of text we decoded
/// ourselves, lets it honour the encoding the feed's own
/// `<?xml ... encoding="..."?>` line declares (older feeds are often
/// ISO-8859-1, not UTF-8). The parser is the same for RSS and Atom: `feed-rs`
/// detects the format and returns one unified `Feed` either way.
///
/// The error is the parser's own; callers add context saying which feed or
/// file it was (the `?` operator and `.with_context` do that without losing the
/// original message).
pub fn parse_feed_bytes(bytes: &[u8]) -> anyhow::Result<feed_rs::model::Feed> {
    // `.map_err(anyhow::Error::from)` turns `feed-rs`'s own error type into
    // the general `anyhow::Error` this function promises to return.
    feed_rs::parser::parse(bytes).map_err(anyhow::Error::from)
}

/// Downloads `url` and returns the body as raw bytes, refusing anything that
/// is not a normal, non-empty answer.
///
/// - **Status:** a refusal (403, 429, 500...) is reported as such ("answered
///   HTTP 403") rather than handed to a parser, which would blame the content.
///   Redirects never reach the check: reqwest follows them.
/// - **Size:** at most `max_bytes` are read; a longer answer is an error. The
///   body is read in pieces ("chunks") so a huge answer is stopped early
///   instead of being loaded whole first.
/// - **Empty:** a successful status with nothing (or only blank space) in it
///   is reported as an empty reply.
///
/// `pub(crate)` so `fetchers::discover` can use the same careful download.
///
/// `timeout` replaces the client's own limit for this one request when it is
/// `Some`.
pub(crate) async fn fetch_bytes(
    client: &reqwest::Client,
    url: &str,
    max_bytes: usize,
    timeout: Option<Duration>,
) -> anyhow::Result<Vec<u8>> {
    // Step 1: build the request. `let mut` because the optional time limit is
    // added to the builder afterwards (each builder call returns a new one).
    let mut request = client.get(url);
    if let Some(limit) = timeout {
        request = request.timeout(limit);
    }

    // Step 2: send it. `.send().await` returns once the status line and
    // headers have arrived; the body is still on its way. `.with_context`
    // adds a readable message to the error only if the step fails.
    let mut response = request
        .send()
        .await
        .with_context(|| format!("failed to fetch feed at {url}"))?;

    // Step 3: check the HTTP status before reading anything. `is_success()` is
    // true only for 200-299. `bail!` returns an `Err` from this function
    // right away, like `?` does.
    let status = response.status();
    if !status.is_success() {
        // A 403 ("Forbidden") usually means the site turns programs away while
        // letting a browser in (a "checking your browser" page, for one). Every
        // other failure keeps the plain message, because a 404 or a 500 says
        // nothing about being refused. The hint names what works: a file saved
        // from the browser.
        if status == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!(
                "{url} answered HTTP {status}: the feed link is not working for apps like \
                 tuxwire. Try saving the feed from your browser and adding it as a file instead"
            );
        }
        anyhow::bail!("{url} answered HTTP {status}");
    }

    // Step 4: read the body piece by piece, stopping if it grows past the
    // limit. `response.chunk()` yields `Ok(Some(piece))` until the body ends
    // (`Ok(None)`); `while let` repeats for as long as there is a piece.
    let mut body: Vec<u8> = Vec::new();
    while let Some(piece) = response
        .chunk()
        .await
        .with_context(|| format!("failed to read response body from {url}"))?
    {
        if body.len() + piece.len() > max_bytes {
            anyhow::bail!(
                "{url} sent more than {} MiB; refusing to read it",
                max_bytes / (1024 * 1024)
            );
        }
        body.extend_from_slice(&piece);
    }

    // Step 5: a successful status with nothing in it is not a feed or a page.
    // Say so plainly, instead of letting a parser report "no root element".
    // `trim_ascii` ignores blank space at both ends, so a body of only
    // newlines counts as empty.
    if body.trim_ascii().is_empty() {
        anyhow::bail!("{url} sent an empty reply (no feed in it)");
    }

    Ok(body)
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

/// Turns a parsed feed into the app's own `Article`s, one per entry.
///
/// This is the pure half of fetching: no network, no files. `RssFetcher::fetch`
/// downloads a feed and calls it; the terminal app's `type = "file"` source
/// reads a feed file from disk and calls it; the Android app can call it with
/// bytes from its file picker (after [`parse_feed_bytes`]). Having one function
/// means a feed read from a file produces exactly the same articles, summaries
/// and dates as the same feed downloaded.
///
/// - `feed` is taken **by value** (ownership moves in) because each entry's
///   strings are moved into the new `Article`s instead of copied.
/// - `name` is the source's display name and `topic` the topic it is filed
///   under; both are copied onto every article.
/// - `feed_url` is recorded as each article's `feed_url` (the source's address
///   in `sources.toml`, or the file's path for a file source), so feed lists
///   can tell their articles apart.
pub fn articles_from_feed(
    feed: feed_rs::model::Feed,
    name: &str,
    topic: &str,
    feed_url: &str,
) -> Vec<Article> {
    // Build the `Vec<Article>` we're returning. `feed.entries` is a
    // `Vec<feed_rs::model::Entry>` — we walk it with `.into_iter()`
    // (which *consumes* the `Vec`, handing us owned `Entry` values
    // one at a time instead of borrowed references, since we don't
    // need `feed` afterwards) and `.map(...)` each `Entry` into our
    // own `Article` type. `.collect()` gathers the mapped iterator
    // back into a concrete `Vec<Article>` — Rust knows which container
    // to build from this function's return type, because `.collect()` is
    // generic over *many* possible container types, not just `Vec`.
    feed.entries
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

            // Record which feed this came from (`feed_url` is the
            // source's address from `sources.toml`), so feed lists can
            // tell their articles apart. `mut` because the field is set
            // after construction.
            let mut article =
                Article::new(title, url, name.to_string(), topic.to_string(), timestamp);
            article.feed_url = Some(feed_url.to_string());
            article.summary = summary;
            article
        })
        .collect()
}

impl Fetcher for RssFetcher {
    /// Fetches and parses `self.url` (via `fetch_feed` above) and maps
    /// every entry in the result into an `Article` (via `articles_from_feed`).
    async fn fetch(&self) -> anyhow::Result<Vec<Article>> {
        let feed = fetch_feed(&self.url)
            .await
            .with_context(|| format!("fetching '{}'", self.name))?;

        Ok(articles_from_feed(feed, &self.name, &self.topic, &self.url))
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
    fn serve_once(
        reply: impl Into<Vec<u8>> + Send + 'static,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        // Step 1: listen on a free port the operating system picks (`:0`).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (sender, receiver) = std::sync::mpsc::channel();

        // Step 2: in the background, accept one connection, pass the request
        // text back through the channel, and write the canned reply.
        // `Into<Vec<u8>>` lets a test pass text (`&str`) or raw bytes.
        let reply: Vec<u8> = reply.into();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let _ = sender.send(String::from_utf8_lossy(&buffer[..read]).into_owned());
                let _ = stream.write_all(&reply);
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

    /// Only a 403 gets the "add it as a file" hint; a 404 keeps the plain
    /// message, since it does not mean the site is refusing programs.
    #[tokio::test]
    async fn only_a_403_suggests_adding_a_file() {
        let (url, _seen) =
            serve_once("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let forbidden = fetch_feed_with(&build_client(Duration::from_secs(5)), &url)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            forbidden.contains("adding it as a file"),
            "error was: {forbidden}"
        );

        let (url, _seen) =
            serve_once("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        let missing = fetch_feed_with(&build_client(Duration::from_secs(5)), &url)
            .await
            .unwrap_err()
            .to_string();
        assert!(missing.contains("404"), "error was: {missing}");
        assert!(!missing.contains("as a file"), "error was: {missing}");
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

    /// An answer longer than the limit is refused, not read to the end.
    #[tokio::test]
    async fn an_oversized_reply_is_refused() {
        // A 5000-byte body against a 1000-byte limit.
        let mut reply =
            b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\nConnection: close\r\n\r\n".to_vec();
        reply.extend(std::iter::repeat_n(b'x', 5000));
        let (url, _seen) = serve_once(reply);
        let error = fetch_bytes(&build_client(Duration::from_secs(5)), &url, 1000, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing to read"), "error was: {error}");
    }

    /// A feed that declares an older encoding (ISO-8859-1) in its XML line is
    /// read correctly, because the parser gets the raw bytes. `0xE9` is `é`
    /// in that encoding (and is not valid UTF-8 on its own).
    #[tokio::test]
    async fn a_latin_1_feed_is_read_correctly() {
        let mut body = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?><rss version=\"2.0\"><channel><title>Caf".to_vec();
        body.push(0xE9);
        body.extend_from_slice(b"</title><item><title>A</title></item></channel></rss>");
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        reply.extend(body);
        let (url, _seen) = serve_once(reply);
        let feed = fetch_feed_with(&build_client(Duration::from_secs(5)), &url)
            .await
            .unwrap();
        assert_eq!(feed.title.unwrap().content, "Caf\u{e9}");
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

    /// A feed's bytes become articles without any network: `parse_feed_bytes`
    /// reads the XML and `articles_from_feed` maps each entry, copying the
    /// source's name, topic and address onto every article. This is the path a
    /// feed *file* takes (the terminal app's `type = "file"` source), so the
    /// test uses the shape of a real site's feed: `CDATA` text, an `<author>`
    /// that is only a placeholder address, and a `<content:encoded>` body.
    #[test]
    fn feed_bytes_become_articles_without_network() {
        // Step 1: a small RSS feed with two items, written as raw bytes.
        let xml = br#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0" xmlns:dc="http://purl.org/dc/elements/1.1/"
     xmlns:content="http://purl.org/rss/1.0/modules/content/">
  <channel>
    <title>Articles Feed</title>
    <link>https://example.com/</link>
    <description>Test feed</description>
    <item>
      <title>First &amp; best</title>
      <pubDate>Sun, 18 Oct 2026 12:33:32 +0000</pubDate>
      <link>https://example.com/news/first.1/</link>
      <guid>https://example.com/news/first.1/</guid>
      <author>invalid@example.com (Some Writer)</author>
      <dc:creator>Some Writer</dc:creator>
      <content:encoded><![CDATA[<div><b>Hello</b> world. Second sentence.</div>]]></content:encoded>
    </item>
    <item>
      <title>Second</title>
      <link>https://example.com/news/second.2/</link>
    </item>
  </channel>
</rss>"#;

        // Step 2: parse and map. `to_string` is not needed: the function takes
        // `&str` borrows and copies what it keeps.
        let feed = parse_feed_bytes(xml).expect("the test feed should parse");
        let title = feed.title.as_ref().map(|t| t.content.clone());
        let articles = articles_from_feed(feed, "Test Site", "sports", "/home/me/feed.rss");

        // Step 3: the feed's own title is available for guessing a name, and
        // both entries became articles carrying the source's details.
        assert_eq!(title.as_deref(), Some("Articles Feed"));
        assert_eq!(articles.len(), 2);
        assert_eq!(articles[0].title, "First & best");
        assert_eq!(articles[0].url, "https://example.com/news/first.1/");
        assert_eq!(articles[0].source, "Test Site");
        assert_eq!(articles[0].topic, "sports");
        assert_eq!(articles[0].feed_url.as_deref(), Some("/home/me/feed.rss"));
        // The body text, with the HTML removed, becomes the summary.
        assert!(
            articles[0]
                .summary
                .as_deref()
                .is_some_and(|s| s.contains("Hello world."))
        );
        // An item with no date still gets a timestamp (the current time).
        assert_eq!(articles[1].title, "Second");
    }

    /// Bytes that are not a feed are an error (not an empty list of articles),
    /// so a person who points a file source at the wrong file is told so.
    #[test]
    fn non_feed_bytes_are_an_error() {
        assert!(parse_feed_bytes(b"<html><body>Not a feed</body></html>").is_err());
        assert!(parse_feed_bytes(b"").is_err());
    }
}
