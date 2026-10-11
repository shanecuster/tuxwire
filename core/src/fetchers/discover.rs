//! Finds the feed that belongs to an address, so a person can paste a blog's
//! web page (not only its feed address) and tuxwire works out the rest.
//!
//! Most sites do not tell people where their feed lives, but they do tell
//! programs: the page's hidden `<head>` section holds a line such as
//!
//! ```text
//! <link rel="alternate" type="application/rss+xml" title="Posts" href="/feed.xml">
//! ```
//!
//! [`discover_feed`] follows the same steps a person would, in order, and
//! stops at the first one that works:
//!
//! 1. **The address is already a feed.** It downloads fine and parses as RSS
//!    or Atom. Nothing else is tried. (A feed the person typed on purpose is
//!    never second-guessed, even if it is a comments feed.)
//! 2. **The page advertises a feed.** The `<link>` lines above are read, and
//!    the first one that downloads as a real feed wins.
//! 3. **Common places.** If the page advertises nothing, a short list of
//!    usual feed addresses (`/feed`, `/rss.xml`, `/index.xml`, ...) is tried,
//!    one after another, first under the page's own path and then at the top
//!    of the site.
//!
//! Comment feeds ("Comments for My Blog") are skipped in steps 2 and 3, since
//! nobody wants those by accident. The check is a rule of thumb (the word
//! "comments" in the link's label, in its address, or at the start of the
//! feed's own title), not a guarantee.
//!
//! Like the rest of `core` this knows nothing about terminals or phones: it
//! takes text in and gives a result out, so the terminal app and the Android
//! app share it. The HTML is read by hand (only the `<link>` lines matter), in
//! the same spirit as the OPML reader, so there is no new dependency.

// Has clippy flag any undocumented item here, the same as in
// `core/src/opml.rs`.
#![warn(clippy::missing_docs_in_private_items)]

use std::time::Duration;

use anyhow::bail;

use crate::fetchers::rss::{FEED_MAX_BYTES, HTTP_CLIENT, fetch_bytes, fetch_feed_limited};
use crate::opml::{attr, find_tag_end, is_web_address, parse_attributes, split_tag_name};

/// How much of a web page is searched for `<link>` lines: the first 2 MiB.
/// The lines we want are in the page's head, right at the top, so this is
/// generous; the cap only stops a huge page from costing time and memory.
const MAX_PAGE_BYTES: usize = 2 * 1024 * 1024;

/// How long one *guessed* address may take before it is given up on. Much
/// shorter than a normal fetch, because most guesses are wrong and a wrong
/// guess should fail fast (a missing page answers in a moment), while a slow
/// site must not hold up the whole search.
const GUESS_TIMEOUT: Duration = Duration::from_secs(10);

/// The most `<link>` lines read from one page. Real pages have two or three;
/// the limit stops a page stuffed with thousands from costing anything.
const MAX_LINKS_READ: usize = 20;

/// The most advertised feeds actually downloaded and tried from one page.
const MAX_ADVERTISED_TRIED: usize = 5;

/// The most guessed addresses tried. With six endings tried both under the
/// page's own path and at the top of the site this is exactly twelve.
const MAX_GUESSES: usize = 12;

/// The usual places a site keeps its feed, tried in this order: the common
/// WordPress and generic names first, then the static-site generators (Hugo
/// uses `/index.xml`; Jekyll uses `/feed.xml` or `/atom.xml`).
const COMMON_ENDINGS: [&str; 6] = [
    "/feed",
    "/rss",
    "/rss.xml",
    "/atom.xml",
    "/feed.xml",
    "/index.xml",
];

/// The `type="..."` values that mark a `<link>` line as pointing at a feed.
const FEED_LINK_TYPES: [&str; 3] = [
    "application/rss+xml",
    "application/atom+xml",
    "application/feed+json",
];

/// How the feed was found, so a screen can tell the person what happened
/// ("found a feed advertised by that page").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoundBy {
    /// The address given was itself a feed.
    Direct,
    /// The page named its feed in a `<link>` line.
    Advertised,
    /// The page named nothing, and a common address worked.
    Guessed,
}

/// A feed that was found, with the address it lives at.
#[derive(Debug)]
pub struct Discovered {
    /// The address of the feed itself. This is what should be saved: it is the
    /// address that refreshes will use.
    pub url: String,
    /// The feed's contents, already downloaded and parsed, so a screen can
    /// show its title and article count without a second download.
    pub feed: feed_rs::model::Feed,
    /// Which of the three steps found it.
    pub found_by: FoundBy,
}

/// One feed a page says it has: where it is, and what the page calls it.
#[derive(Debug, PartialEq, Eq)]
struct Advertised {
    /// The feed's full address (a relative `href` has already been resolved
    /// against the page's address).
    url: String,
    /// The `title="..."` of the `<link>` line, or an empty string if it has
    /// none. Used to recognise comment feeds.
    label: String,
}

/// Finds the feed for `input`: a feed address, or the address of a page that
/// has a feed. See the module comment for the steps.
///
/// `input: &str` is borrowed (read, not copied or consumed). The result is
/// `anyhow::Result<Discovered>`: `Ok` with the feed found, or `Err` with a
/// message written for a person to read ("no feed found at ...").
///
/// This does network work, one request at a time, so it can take a while in
/// the worst case (a page that advertises nothing and a site that answers
/// slowly). It never spawns background work and never keeps anything.
pub async fn discover_feed(input: &str) -> anyhow::Result<Discovered> {
    // Step 1: turn what was typed into a checked web address. An address with
    // no `http://` or `https://` gets `https://` put in front.
    let page_url = normalise_address(input)?;
    let client = &HTTP_CLIENT;

    // Step 2: download whatever is there. The size limit is the feed limit,
    // because the address might be a large feed itself. A failure here (the
    // site is down, it answered 404) is reported straight away: if the page
    // itself cannot be reached there is no point guessing at its neighbours.
    let bytes = fetch_bytes(client, page_url.as_str(), FEED_MAX_BYTES, None).await?;

    // Step 3: is it already a feed? `parse` succeeding is the whole proof. The
    // address is kept exactly as normalised; a redirect is not followed into
    // the saved address, so a site that moves its feed keeps working.
    if let Ok(feed) = feed_rs::parser::parse(&bytes[..]) {
        return Ok(Discovered {
            url: page_url.to_string(),
            feed,
            found_by: FoundBy::Direct,
        });
    }

    // Step 4: it is a web page. Read the first part of it as text.
    // `from_utf8_lossy` never fails: bytes that are not valid UTF-8 become a
    // replacement character, which is harmless because the `<link>` lines we
    // look for are plain ASCII. It borrows when it can and copies when it must
    // (the type is a `Cow`, "clone on write"); passing `&page_text` below still
    // works as a plain `&str`.
    let page_text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_PAGE_BYTES)]);
    let advertised = find_advertised_feeds(&page_text, &page_url);

    // What went wrong, kept so the final message can say something useful.
    let mut comment_feeds_skipped = 0usize;
    let mut last_error: Option<anyhow::Error> = None;
    let mut tried: Vec<String> = Vec::new();

    // Step 5: try the advertised feeds, in the order the page lists them.
    for candidate in &advertised {
        // Stop once enough real downloads have been tried.
        if tried.len() >= MAX_ADVERTISED_TRIED {
            break;
        }
        // Comment feeds are skipped without being downloaded.
        if is_comments_link(candidate) {
            comment_feeds_skipped += 1;
            continue;
        }
        tried.push(candidate.url.clone());
        match fetch_feed_limited(client, &candidate.url, None).await {
            Ok(feed) => {
                // The feed's own title can give away a comment feed that was
                // not labelled as one in the page.
                if feed_title_is_comments(&feed) {
                    comment_feeds_skipped += 1;
                    continue;
                }
                return Ok(Discovered {
                    url: candidate.url.clone(),
                    feed,
                    found_by: FoundBy::Advertised,
                });
            }
            // Remember why, and move on to the next one the page offers.
            Err(error) => last_error = Some(error),
        }
    }

    // Step 6: nothing advertised worked. Try the usual places, quickly. A
    // missing page (404) is the normal answer here, so errors are ignored.
    let guesses = guess_addresses(&page_url);
    let guesses_tried = guesses.len();
    for address in guesses {
        // Skip an address already tried in step 5.
        if tried.contains(&address) {
            continue;
        }
        if let Ok(feed) = fetch_feed_limited(client, &address, Some(GUESS_TIMEOUT)).await {
            if feed_title_is_comments(&feed) || mentions_comments(&address_words(&address)) {
                comment_feeds_skipped += 1;
                continue;
            }
            return Ok(Discovered {
                url: address,
                feed,
                found_by: FoundBy::Guessed,
            });
        }
    }

    // Step 7: nothing worked. Pick the message that best explains why.
    if comment_feeds_skipped > 0 && last_error.is_none() {
        bail!(
            "{page_url} only offers a comments feed, which tuxwire does not add \
             (to add one on purpose, type its feed address directly)"
        );
    }
    if let Some(error) = last_error {
        bail!("{page_url} points to a feed, but it could not be read: {error:#}");
    }
    bail!(
        "no feed found at {page_url}: the page does not advertise one, and none of the \
         {guesses_tried} usual addresses ({}) worked",
        COMMON_ENDINGS.join(" ")
    )
}

/// Turns what a person typed into a web address that is safe to fetch.
///
/// - Blank space around it is ignored.
/// - With no `http://` or `https://`, `https://` is added (so `example.com/blog`
///   works as typed).
/// - Anything that is not `http` or `https` (`file:`, `ftp:`, `javascript:`) is
///   refused, as are addresses with spaces or control characters.
/// - A `#fragment` (the part after `#`) is dropped; it never reaches a server.
///
/// `reqwest::Url` is the standard address type reqwest uses; it checks the
/// address is well formed and can join a relative path onto it.
fn normalise_address(input: &str) -> anyhow::Result<reqwest::Url> {
    // Step 1: trim, and refuse nothing-at-all.
    let trimmed = input.trim();
    if trimmed.is_empty() {
        bail!("enter an address first");
    }

    // Step 2: add `https://` when no scheme was typed. `contains("://")` is how
    // a scheme is recognised; text such as `example.com:8080/feed` has none.
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    // Step 3: parse it. `map_err` swaps the library's technical error for a
    // plain sentence.
    let mut url = reqwest::Url::parse(&with_scheme)
        .map_err(|_| anyhow::anyhow!("'{trimmed}' is not a web address"))?;

    // Step 4: only ordinary web addresses, with the same checks the OPML
    // reader applies to addresses from a file.
    if !matches!(url.scheme(), "http" | "https") || !is_web_address(url.as_str()) {
        bail!("'{trimmed}' is not a web address (it must start with http:// or https://)");
    }

    // Step 5: drop the `#fragment`.
    url.set_fragment(None);
    Ok(url)
}

/// Reads the `<link>` lines in a page's head that point at feeds, in the order
/// they appear.
///
/// A line counts when its `rel` includes `alternate`, its `type` is a feed type
/// (`application/rss+xml`, `application/atom+xml`, `application/feed+json`;
/// anything after a `;` such as `charset=utf-8` is ignored), and its `href`
/// resolves to an `http`/`https` address. Lines inside HTML comments are
/// ignored, and nothing after `</head>` is read, so a sample of HTML in the
/// body of an article cannot be mistaken for the page's own feed.
///
/// `html: &str` is the page text; `base` is the page's own address, used to
/// turn a relative `href="/feed.xml"` into a full address.
fn find_advertised_feeds(html: &str, base: &reqwest::Url) -> Vec<Advertised> {
    // Step 1: a lowercase copy for searching. `to_ascii_lowercase` only changes
    // letters A-Z, so every position in it is also the right position in
    // `html`, which lets us search the copy and read values from the original
    // (values such as `title` must keep their capitals).
    let lower = html.to_ascii_lowercase();

    // Step 2: only the head is searched. If `</head` never appears (a page
    // fragment), the whole text is.
    let end = lower.find("</head").unwrap_or(lower.len());

    let mut found: Vec<Advertised> = Vec::new();
    let mut position = 0usize;

    // Step 3: walk through the head, hopping from tag to tag.
    while position < end && found.len() < MAX_LINKS_READ {
        let rest = &lower[position..end];
        let next_comment = rest.find("<!--");
        let next_link = rest.find("<link");

        // Step 3a: if an HTML comment starts before the next `<link`, skip the
        // whole comment (up to `-->`, or the end if it never closes).
        // `is_none_or` is true when there is no `<link` at all, or when the
        // comment comes first.
        if let Some(comment_at) = next_comment
            && next_link.is_none_or(|link_at| comment_at < link_at)
        {
            let after_marker = &rest[comment_at + 4..];
            let skip = match after_marker.find("-->") {
                Some(close) => close + 3,
                None => after_marker.len(),
            };
            position += comment_at + 4 + skip;
            continue;
        }

        // Step 3b: no more `<link` tags: done.
        let Some(link_at) = next_link else {
            break;
        };

        // Step 3c: find where this tag ends. `html[start..end]` is the text
        // from the `<` onward; `find_tag_end` ignores a `>` inside a quoted
        // value. A tag that never ends means a damaged page: stop.
        let start = position + link_at;
        let tag_text = &html[start..end];
        let Some(tag_end) = find_tag_end(tag_text) else {
            break;
        };
        position = start + tag_end + 1;

        // Step 3d: split `link rel="alternate" ...` into its name and the
        // attribute text, and make sure the name really is `link` (not
        // `<linkedin>`, say). A self-closing `<link .../>` leaves a trailing
        // `/` on the last word, which is trimmed off.
        let inner = &tag_text[1..tag_end];
        let (name, attribute_text) = split_tag_name(inner);
        if !name.trim_end_matches('/').eq_ignore_ascii_case("link") {
            continue;
        }
        let attrs = parse_attributes(attribute_text);

        // Step 3e: `rel` is a list of words; one of them must be `alternate`.
        let is_alternate = attr(&attrs, "rel").is_some_and(|rel| {
            rel.split_whitespace()
                .any(|word| word.eq_ignore_ascii_case("alternate"))
        });

        // Step 3f: `type` must be one of the feed types. Anything after a `;`
        // (`; charset=utf-8`) is dropped before comparing.
        let is_feed_type = attr(&attrs, "type").is_some_and(|kind| {
            let kind = kind.split(';').next().unwrap_or("").trim();
            FEED_LINK_TYPES
                .iter()
                .any(|known| kind.eq_ignore_ascii_case(known))
        });
        if !is_alternate || !is_feed_type {
            continue;
        }

        // Step 3g: turn `href` into a full address. `join` handles absolute
        // addresses, `/rooted` paths and `relative.xml` alike. Only ordinary
        // web addresses are kept.
        let Some(href) = attr(&attrs, "href") else {
            continue;
        };
        let Ok(mut resolved) = base.join(href.trim()) else {
            continue;
        };
        resolved.set_fragment(None);
        if !matches!(resolved.scheme(), "http" | "https") || !is_web_address(resolved.as_str()) {
            continue;
        }

        // Step 3h: keep it, unless the page lists the same address twice. The
        // label is tidied: blank space squeezed, cut to 100 characters.
        let url = resolved.to_string();
        if found.iter().any(|existing| existing.url == url) {
            continue;
        }
        let label: String = attr(&attrs, "title")
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(100)
            .collect();
        found.push(Advertised { url, label });
    }
    found
}

/// The addresses to try when a page advertises no feed: each of
/// [`COMMON_ENDINGS`] added to the page's own address first (so
/// `https://example.com/news` tries `https://example.com/news/feed`), then to
/// the top of the site (`https://example.com/feed`). At most [`MAX_GUESSES`],
/// with no address repeated.
fn guess_addresses(page: &reqwest::Url) -> Vec<String> {
    // Step 1: the top of the site: the same address with the path, query and
    // fragment removed, and the trailing `/` taken off so an ending can be
    // added with its own leading `/`.
    let mut top = page.clone();
    top.set_path("/");
    top.set_query(None);
    top.set_fragment(None);
    let top_base = top.as_str().trim_end_matches('/').to_string();

    // Step 2: the page's own address, if it is deeper than the top of the site.
    let own_path = page.path().trim_end_matches('/');
    let mut bases: Vec<String> = Vec::new();
    if !own_path.is_empty() {
        bases.push(format!("{top_base}{own_path}"));
    }
    bases.push(top_base);

    // Step 3: every base with every ending, in order, no repeats, up to the
    // limit.
    let mut guesses: Vec<String> = Vec::new();
    for base in &bases {
        for ending in COMMON_ENDINGS {
            let address = format!("{base}{ending}");
            if !guesses.contains(&address) && guesses.len() < MAX_GUESSES {
                guesses.push(address);
            }
        }
    }
    guesses
}

/// Whether text contains the whole word "comments" (ignoring capitals), where
/// words are runs of letters and digits. "Comments Feed" and
/// "/post/comments.rss" match; "Commentary" and "Comment is free" do not,
/// because blogs with such names are real and must not be rejected.
fn mentions_comments(text: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric())
        .any(|word| word.eq_ignore_ascii_case("comments"))
}

/// The path and query of an address as plain text for [`mentions_comments`],
/// leaving out the site name (a site called `comments.example.com` is not a
/// comments feed). Text that is not a valid address gives an empty string.
fn address_words(address: &str) -> String {
    match reqwest::Url::parse(address) {
        Ok(url) => format!("{} {}", url.path(), url.query().unwrap_or("")),
        Err(_) => String::new(),
    }
}

/// Whether an advertised `<link>` looks like a comments feed, by its label or
/// by its address (WordPress uses `/comments/feed/` and `?feed=comments-rss2`).
fn is_comments_link(link: &Advertised) -> bool {
    mentions_comments(&link.label) || mentions_comments(&address_words(&link.url))
}

/// Whether a downloaded feed's own title marks it as a comments feed: the title
/// *starts with* the word "comments" ("Comments for My Blog", "Comments on:
/// A Post") or ends with "comments feed". Deliberately narrower than the link
/// check, so a blog whose name merely contains the word is not turned away.
fn feed_title_is_comments(feed: &feed_rs::model::Feed) -> bool {
    let Some(title) = feed.title.as_ref() else {
        return false;
    };
    // Lowercase words, so "Comments" and "COMMENTS" compare the same.
    let words: Vec<String> = title
        .content
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(|word| word.to_lowercase())
        .collect();
    let starts_with_comments = words.first().is_some_and(|word| word == "comments");
    let ends_with_comments_feed = words.len() >= 2
        && words[words.len() - 2] == "comments"
        && words[words.len() - 1] == "feed";
    starts_with_comments || ends_with_comments_feed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A small valid RSS feed with the given channel title.
    fn feed_xml(title: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><rss version="2.0"><channel><title>{title}</title><item><title>A post</title><link>https://example.com/a</link></item></channel></rss>"#
        )
    }

    /// A web page whose head holds `head_lines`.
    fn page(head_lines: &str) -> String {
        format!(
            "<!doctype html><html><head><title>Site</title>{head_lines}</head><body><p>Hi</p></body></html>"
        )
    }

    /// Starts a tiny web server on `127.0.0.1` (this computer only) that
    /// answers each request from `routes`: a list of (path, body). A path not
    /// in the list gets a 404. Returns the server's address (no trailing `/`)
    /// and a counter of how many requests it has served, so a test can check
    /// the number of guesses.
    ///
    /// The server thread runs for the rest of the test run, which is fine for
    /// a test. If a proxy is set in your environment, run the tests with
    /// `NO_PROXY=127.0.0.1`.
    fn serve(routes: Vec<(&'static str, String)>) -> (String, Arc<AtomicUsize>) {
        // Step 1: listen on a free port the operating system picks (`:0`).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let counter = Arc::new(AtomicUsize::new(0));
        let served = Arc::clone(&counter);

        // Step 2: in the background, answer one connection after another.
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };

                // Read the request and pull the path out of its first line:
                // `GET /feed HTTP/1.1`.
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
                served.fetch_add(1, Ordering::SeqCst);

                // Answer from the routes, or with a 404.
                let reply = match routes.iter().find(|(route, _)| *route == path) {
                    Some((_, body)) => format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    ),
                    None => {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            .to_string()
                    }
                };
                let _ = stream.write_all(reply.as_bytes());
            }
        });

        (format!("http://127.0.0.1:{port}"), counter)
    }

    /// Parses `base` as the page address the tests use.
    fn url(text: &str) -> reqwest::Url {
        reqwest::Url::parse(text).unwrap()
    }

    // ---- reading `<link>` lines ---------------------------------------

    /// The advertised addresses found in `html`, as plain strings.
    fn links(html: &str) -> Vec<String> {
        find_advertised_feeds(html, &url("https://example.com/blog/"))
            .into_iter()
            .map(|found| found.url)
            .collect()
    }

    /// The usual line is found, and a relative `href` is resolved.
    #[test]
    fn a_feed_link_is_found_and_resolved() {
        let html = page(
            r#"<link rel="alternate" type="application/rss+xml" title="Posts" href="/feed.xml">
               <link rel="alternate" type="application/atom+xml" href="atom.xml">"#,
        );
        assert_eq!(
            links(&html),
            [
                "https://example.com/feed.xml",
                "https://example.com/blog/atom.xml"
            ]
        );
    }

    /// Attribute order, quote style, capitals and self-closing do not matter,
    /// and a `charset` after the type is ignored.
    #[test]
    fn link_lines_are_read_leniently() {
        let html = page(
            r#"<LINK HREF='/a.xml' TYPE='application/rss+xml; charset=utf-8' REL='alternate' />"#,
        );
        assert_eq!(links(&html), ["https://example.com/a.xml"]);
    }

    /// Lines that are not feed links are ignored: wrong rel, wrong type, no
    /// href, a different tag whose name merely starts with "link".
    #[test]
    fn other_links_are_ignored() {
        let html = page(
            r#"<link rel="stylesheet" type="text/css" href="/s.css">
               <link rel="alternate" type="text/html" href="/other-language">
               <link rel="alternate" type="application/rss+xml">
               <linkedin rel="alternate" type="application/rss+xml" href="/x.xml">
               <link rel="alternate stylesheet" type="application/rss+xml" href="/ok.xml">"#,
        );
        assert_eq!(links(&html), ["https://example.com/ok.xml"]);
    }

    /// A line inside an HTML comment, and anything after `</head>`, is not
    /// the page's own feed.
    #[test]
    fn commented_out_and_body_links_are_ignored() {
        let html = r#"<html><head>
            <!-- <link rel="alternate" type="application/rss+xml" href="/old.xml"> -->
            <link rel="alternate" type="application/rss+xml" href="/real.xml">
            </head><body><link rel="alternate" type="application/rss+xml" href="/sample.xml"></body></html>"#;
        assert_eq!(links(html), ["https://example.com/real.xml"]);
    }

    /// Entities in an address are decoded, repeats are dropped, and unsafe
    /// schemes are ignored.
    #[test]
    fn addresses_are_decoded_deduplicated_and_checked() {
        let html = page(
            r#"<link rel="alternate" type="application/rss+xml" href="/f?a=1&amp;b=2">
               <link rel="alternate" type="application/rss+xml" href="/f?a=1&amp;b=2">
               <link rel="alternate" type="application/rss+xml" href="javascript:alert(1)">
               <link rel="alternate" type="application/rss+xml" href="ftp://example.com/x">"#,
        );
        assert_eq!(links(&html), ["https://example.com/f?a=1&b=2"]);
    }

    /// Damaged and random pages never cause a panic. 4000 pseudo-random
    /// strings built from the pieces most likely to confuse the scanner.
    #[test]
    fn garbled_pages_never_panic() {
        let pieces = [
            "<",
            ">",
            "<link",
            "<!--",
            "-->",
            "\"",
            "'",
            "=",
            " ",
            "rel",
            "alternate",
            "type",
            "application/rss+xml",
            "href",
            "/",
            "é",
            "</head",
            "\u{1F600}",
            "&amp;",
            "\n",
        ];
        // A simple pseudo-random number generator (no extra crate needed).
        let mut state: u64 = 0x2545F4914F6CDD1D;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        for _ in 0..4000 {
            let mut text = String::new();
            for _ in 0..(next() % 40) {
                text.push_str(pieces[next() % pieces.len()]);
            }
            let _ = find_advertised_feeds(&text, &url("https://example.com/"));
        }
    }

    // ---- comment feeds ------------------------------------------------

    /// The whole word "comments" is caught; look-alikes are not.
    #[test]
    fn only_the_word_comments_counts() {
        assert!(mentions_comments("Site » Comments Feed"));
        assert!(mentions_comments("/post/123/comments.rss"));
        assert!(mentions_comments("feed=comments-rss2"));
        assert!(!mentions_comments("Commentary Magazine"));
        assert!(!mentions_comments("Comment is free"));
        assert!(!mentions_comments("/blog/feed"));
    }

    /// A feed's own title marks it as a comments feed only at the start or as
    /// "comments feed" at the end.
    #[test]
    fn feed_titles_are_judged_narrowly() {
        let judge = |title: &str| {
            let feed = feed_rs::parser::parse(feed_xml(title).as_bytes()).unwrap();
            feed_title_is_comments(&feed)
        };
        assert!(judge("Comments for My Blog"));
        assert!(judge("Comments on: A Post"));
        assert!(judge("My Blog Comments Feed"));
        assert!(!judge("Notes on Comments in Code"));
        assert!(!judge("Commentary"));
        // A site name containing "comments" in its host is not enough.
        assert!(!mentions_comments(&address_words(
            "https://comments.example.com/feed"
        )));
    }

    // ---- guessing -----------------------------------------------------

    /// Guesses go under the page's own path first, then the top of the site,
    /// and never exceed the limit.
    #[test]
    fn guesses_start_with_the_page_path() {
        let guesses = guess_addresses(&url("https://example.com/news/?page=2#top"));
        assert_eq!(guesses.len(), 12);
        assert_eq!(guesses[0], "https://example.com/news/feed");
        assert_eq!(guesses[5], "https://example.com/news/index.xml");
        assert_eq!(guesses[6], "https://example.com/feed");
        // At the top of a site there is only one base, so only six guesses.
        assert_eq!(guess_addresses(&url("https://example.com/")).len(), 6);
    }

    // ---- addresses ----------------------------------------------------

    /// What a person types is tidied; anything unsafe is refused.
    #[test]
    fn typed_addresses_are_checked() {
        assert_eq!(
            normalise_address("  example.com/blog#top ")
                .unwrap()
                .as_str(),
            "https://example.com/blog"
        );
        assert_eq!(
            normalise_address("http://example.com").unwrap().as_str(),
            "http://example.com/"
        );
        assert!(normalise_address("").is_err());
        assert!(normalise_address("   ").is_err());
        assert!(normalise_address("ftp://example.com/feed").is_err());
        assert!(normalise_address("file:///etc/passwd").is_err());
        assert!(normalise_address("javascript:alert(1)").is_err());
        assert!(normalise_address("exa mple.com").is_err());
    }

    // ---- the whole search, against a local server -----------------------

    /// An address that is already a feed is returned as it is, with one request.
    #[tokio::test]
    async fn a_feed_address_is_used_as_it_is() {
        let (site, counter) = serve(vec![("/feed.xml", feed_xml("Direct"))]);
        let found = discover_feed(&format!("{site}/feed.xml")).await.unwrap();
        assert_eq!(found.found_by, FoundBy::Direct);
        assert_eq!(found.url, format!("{site}/feed.xml"));
        assert_eq!(found.feed.title.unwrap().content, "Direct");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    /// A page that advertises a feed gives that feed's address, not the page's.
    #[tokio::test]
    async fn an_advertised_feed_is_found() {
        let html = page(r#"<link rel="alternate" type="application/rss+xml" href="/blog/rss">"#);
        let (site, _) = serve(vec![("/blog", html), ("/blog/rss", feed_xml("Posts"))]);
        let found = discover_feed(&format!("{site}/blog")).await.unwrap();
        assert_eq!(found.found_by, FoundBy::Advertised);
        assert_eq!(found.url, format!("{site}/blog/rss"));
    }

    /// A comments feed listed first is skipped in favour of the real one, both
    /// when the label gives it away and when only the feed's own title does.
    #[tokio::test]
    async fn comment_feeds_are_skipped() {
        let html = page(
            r#"<link rel="alternate" type="application/rss+xml" title="Site » Comments Feed" href="/comments/feed">
               <link rel="alternate" type="application/rss+xml" title="Updates" href="/updates">
               <link rel="alternate" type="application/rss+xml" title="Posts" href="/posts">"#,
        );
        let (site, _) = serve(vec![
            ("/", html),
            ("/comments/feed", feed_xml("Comments for Site")),
            // Labelled innocently, but its own title says what it is.
            ("/updates", feed_xml("Comments on: Everything")),
            ("/posts", feed_xml("Posts")),
        ]);
        let found = discover_feed(&site).await.unwrap();
        assert_eq!(found.url, format!("{site}/posts"));
    }

    /// When a page only offers a comments feed, the message says so, rather
    /// than adding it or claiming nothing exists.
    #[tokio::test]
    async fn only_a_comments_feed_is_reported_as_such() {
        let html = page(
            r#"<link rel="alternate" type="application/rss+xml" title="Comments" href="/comments.xml">"#,
        );
        let (site, _) = serve(vec![("/", html), ("/comments.xml", feed_xml("Comments"))]);
        let error = discover_feed(&site).await.unwrap_err().to_string();
        assert!(
            error.contains("only offers a comments feed"),
            "error was: {error}"
        );
    }

    /// With nothing advertised, a common address is tried and used, and a
    /// 404 on the earlier guesses is not an error.
    #[tokio::test]
    async fn a_common_address_is_guessed() {
        let (site, _) = serve(vec![("/", page("")), ("/index.xml", feed_xml("Hugo Blog"))]);
        let found = discover_feed(&site).await.unwrap();
        assert_eq!(found.found_by, FoundBy::Guessed);
        assert_eq!(found.url, format!("{site}/index.xml"));
    }

    /// Guesses under the page's own path are tried before the top of the site.
    #[tokio::test]
    async fn the_page_path_is_guessed_first() {
        let (site, _) = serve(vec![
            ("/news", page("")),
            ("/news/feed", feed_xml("News")),
            ("/feed", feed_xml("Whole Site")),
        ]);
        let found = discover_feed(&format!("{site}/news")).await.unwrap();
        assert_eq!(found.url, format!("{site}/news/feed"));
    }

    /// A guessed address that is a web page (not a feed) is not accepted, and
    /// a guessed comments feed is skipped.
    #[tokio::test]
    async fn guesses_must_really_be_feeds() {
        let (site, _) = serve(vec![
            ("/", page("")),
            ("/feed", page("")),
            ("/rss", feed_xml("Comments for Site")),
            ("/atom.xml", feed_xml("The Real One")),
        ]);
        let found = discover_feed(&site).await.unwrap();
        assert_eq!(found.url, format!("{site}/atom.xml"));
    }

    /// When nothing is found the message says what was tried, and the number
    /// of requests stays within the limit (one for the page plus the guesses).
    #[tokio::test]
    async fn nothing_found_is_a_clear_message_and_bounded() {
        let (site, counter) = serve(vec![("/news/", page(""))]);
        let error = discover_feed(&format!("{site}/news/"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("no feed found"), "error was: {error}");
        assert!(error.contains("/index.xml"), "error was: {error}");
        assert!(counter.load(Ordering::SeqCst) <= 1 + MAX_GUESSES);
    }

    /// An unreachable page is reported at once, without guessing at its
    /// neighbours (the only request is the page itself).
    #[tokio::test]
    async fn a_missing_page_is_reported_without_guessing() {
        let (site, counter) = serve(vec![]);
        let error = discover_feed(&format!("{site}/gone"))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("404"), "error was: {error}");
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    /// An address with no scheme is refused cleanly when it cannot work, and a
    /// blank one asks for an address.
    #[tokio::test]
    async fn bad_input_is_refused_before_any_request() {
        let error = discover_feed("   ").await.unwrap_err().to_string();
        assert_eq!(error, "enter an address first");
        assert!(discover_feed("ftp://example.com/feed").await.is_err());
    }
}
