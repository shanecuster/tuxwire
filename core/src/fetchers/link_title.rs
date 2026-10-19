//! Finds the title of a web page, so a link pasted into Read Later can be
//! shown by name instead of by address.
//!
//! This is a courtesy, never a requirement: if the page cannot be reached, is
//! not HTML or has no `<title>`, the caller just uses the address as the title
//! (see [`crate::sync::later::clean_title`], which does that for an empty one).
//!
//! The download is the same careful one feed discovery uses (status check, size
//! limit, time limit); only the first part of the page is searched.

#![warn(clippy::missing_docs_in_private_items)]

use crate::fetchers::rss::{HTTP_CLIENT, fetch_bytes};
use crate::sync::feeds::is_syncable_url;
use crate::text::decode_entities;
use std::time::Duration;

/// The most of a page that is downloaded. Pages are far bigger than feeds'
/// usual size; this is generous without letting a huge file be read.
const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;

/// How much of the start of the page is searched for the title. The `<title>`
/// is in the `<head>`, which is near the top.
const SEARCH_BYTES: usize = 256 * 1024;

/// How long the download may take in all. A person is waiting at a prompt, so
/// this is shorter than the client's usual limit.
const TITLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Downloads the page at `url` and returns its title, or `None` when it has
/// none that is usable.
///
/// Returns an error (with a message fit to show) when `url` is not an ordinary
/// web address or the page could not be downloaded. Callers treat any error
/// here as "no title" and carry on, since a title is only a convenience.
///
/// `url: &str` is borrowed; the `Option<String>` returned is owned by the
/// caller.
pub async fn fetch_page_title(url: &str) -> anyhow::Result<Option<String>> {
    // Step 1: only ordinary web addresses are fetched, so a pasted `file://`
    // path is never opened.
    let url = url.trim();
    if !is_syncable_url(url) {
        anyhow::bail!("that is not a web address (it must start with http:// or https://)");
    }

    // Step 2: download, with the shared careful download.
    let bytes = fetch_bytes(&HTTP_CLIENT, url, MAX_PAGE_BYTES, Some(TITLE_TIMEOUT)).await?;

    // Step 3: look at the start of the page as text. `from_utf8_lossy` never
    // fails: bytes that are not valid UTF-8 become a replacement character,
    // which is harmless for finding a `<title>`.
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(SEARCH_BYTES)]);
    Ok(extract_title(&head))
}

/// Pulls the text between `<title ...>` and `</title>` out of `html`, with
/// character references decoded and white space tidied. `None` if there is no
/// such pair or the text is empty.
///
/// A plain function (no network) so it is easy to test.
pub fn extract_title(html: &str) -> Option<String> {
    // Step 1: search a lower-case copy. `to_ascii_lowercase` changes only the
    // letters A-Z and so keeps every byte position the same, which means a
    // position found in the copy is valid in the original too.
    let lower = html.to_ascii_lowercase();

    // Step 2: find the opening tag. It must be `<title` followed by a space,
    // `>` or `/`, so `<titlebar>` is not mistaken for it.
    let mut search_from = 0;
    let open_end = loop {
        let found = lower[search_from..].find("<title")? + search_from;
        let after = lower.as_bytes().get(found + "<title".len()).copied();
        if matches!(
            after,
            Some(b'>') | Some(b' ') | Some(b'/') | Some(b'\t') | Some(b'\n') | Some(b'\r')
        ) {
            // The end of the opening tag is the next `>`.
            break lower[found..].find('>')? + found + 1;
        }
        search_from = found + "<title".len();
    };

    // Step 3: find the closing tag after it.
    let close = lower[open_end..].find("</title")? + open_end;

    // Step 4: take the text, decode `&amp;` and friends, and collapse every
    // run of white space (including line breaks) into a single space.
    // `split_whitespace` yields the words; `collect::<Vec<_>>().join(" ")`
    // joins them back with one space each.
    let decoded = decode_entities(&html[open_end..close]);
    let tidy = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
    if tidy.is_empty() { None } else { Some(tidy) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An ordinary title is found.
    #[test]
    fn finds_a_plain_title() {
        let html = "<html><head><title>My page</title></head><body></body></html>";
        assert_eq!(extract_title(html).as_deref(), Some("My page"));
    }

    /// Capitals in the tag, attributes, line breaks and entities are handled.
    #[test]
    fn handles_capitals_attributes_whitespace_and_entities() {
        let html = "<HEAD><TITLE lang=\"en\">\n  Tom &amp; Jerry's\n   page  </TITLE>";
        assert_eq!(extract_title(html).as_deref(), Some("Tom & Jerry's page"));
    }

    /// A tag that merely starts with "title" is not the title.
    #[test]
    fn does_not_mistake_titlebar_for_title() {
        let html = "<titlebar>nope</titlebar><title>Real</title>";
        assert_eq!(extract_title(html).as_deref(), Some("Real"));
    }

    /// No title, an empty title, and an unclosed title all give `None`.
    #[test]
    fn missing_empty_or_unclosed_titles_give_none() {
        assert_eq!(extract_title("<html><body>hi</body></html>"), None);
        assert_eq!(extract_title("<title>   </title>"), None);
        assert_eq!(extract_title("<title>never closed"), None);
        assert_eq!(extract_title(""), None);
    }

    /// Multi-byte text before and inside the title does not break positions.
    #[test]
    fn multibyte_text_is_safe() {
        let html = "<meta charset=utf-8>é日本語<title>Ünïcödé — 日本語</title>";
        assert_eq!(extract_title(html).as_deref(), Some("Ünïcödé — 日本語"));
    }

    /// Addresses that are not web addresses are refused before any download.
    #[tokio::test]
    async fn refuses_non_web_addresses() {
        for bad in ["file:///etc/passwd", "javascript:alert(1)", "nonsense", ""] {
            assert!(fetch_page_title(bad).await.is_err(), "{bad}");
        }
    }

    /// A refused connection is an error, not a hang or a panic. (Port 9 on this
    /// computer is not listening; no network is needed.)
    #[tokio::test]
    async fn an_unreachable_page_is_an_error() {
        assert!(fetch_page_title("http://127.0.0.1:9/").await.is_err());
    }
}
