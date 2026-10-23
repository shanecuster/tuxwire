//! A feed that lives in a **file on this computer** instead of at a web address.
//!
//! Some sites refuse programs but hand the same feed to a browser. A person can
//! save that feed from their browser to a file, and a `type = "file"` source in
//! `sources.toml` makes tuxwire read the file whenever it refreshes. Everything
//! after the bytes are read is the same as for a downloaded feed: the same
//! parser ([`parse_feed_bytes`]) and the same mapping to articles
//! ([`articles_from_feed`]), so a file produces exactly the articles the
//! download would have.
//!
//! ## Why this is allowed in the shared core crate
//!
//! The core crate must never *decide* where a file lives (that is the app's
//! job). This module never does: the path is handed in already complete (the
//! terminal app expands a leading `~/` before building the source), and nothing
//! here looks at `$HOME` or any other environment variable. Only the terminal
//! app creates file sources; the Android app has no use for a local path (its
//! file picker gives it bytes, which it passes straight to `parse_feed_bytes`).

// Has clippy flag any undocumented item here, the same as in
// `core/src/fetch_report.rs`.
#![warn(clippy::missing_docs_in_private_items)]

use crate::fetchers::Fetcher;
use crate::fetchers::rss::{FEED_MAX_BYTES, articles_from_feed, parse_feed_bytes};
use crate::models::Article;
use anyhow::Context;
use std::path::Path;

/// One `type = "file"` source: a feed read from `path` on this computer.
pub struct FileFetcher {
    /// Display name shown in the sidebar and stored on each article as its
    /// `source`.
    pub name: String,

    /// The full path of the feed file. The program that builds the source has
    /// already turned `~/` into a real folder and rejected relative paths, so
    /// this is used exactly as it is. It is also what articles record as their
    /// `feed_url`, which is how a feed list tells which articles came from this
    /// source.
    pub path: String,

    /// The topic the articles are filed under.
    pub topic: String,
}

/// Reads and parses the feed file at `path`, returning the parsed feed.
///
/// This is public because the terminal app's "add a source" screen also needs
/// the *feed itself* (its own `<title>` to suggest a name), not just articles.
/// A successful parse is the whole validation, the same rule as for a web feed.
///
/// `path: &Path` borrows a file path without copying it. Every step reports
/// its failure in words that name the file, so the person sees which file and
/// what is wrong ("... not found", "... is empty", "... is not an RSS or Atom
/// feed") rather than a bare operating-system code.
pub fn read_feed_file(path: &Path) -> anyhow::Result<feed_rs::model::Feed> {
    // Step 1: look at the file's size before reading it, so a huge or wrong
    // file (a movie, a disk image) is refused instead of loaded into memory.
    // `std::fs::metadata` asks the operating system about the file without
    // reading it. `with_context` adds a readable sentence if it fails; the
    // original error (for example "No such file or directory") stays attached
    // underneath.
    let metadata = std::fs::metadata(path)
        .with_context(|| format!("can't read the feed file {}", path.display()))?;
    if !metadata.is_file() {
        anyhow::bail!("{} is not a file", path.display());
    }
    // `metadata.len()` is a `u64`; the limit is a `usize`, so the limit is
    // widened to `u64` for the comparison (`as u64` never loses information
    // here: a `usize` always fits in a `u64`).
    if metadata.len() > FEED_MAX_BYTES as u64 {
        anyhow::bail!(
            "{} is larger than {} MiB; refusing to read it",
            path.display(),
            FEED_MAX_BYTES / (1024 * 1024)
        );
    }

    // Step 2: read the whole file as raw bytes. Bytes, not text, so the feed's
    // own `<?xml ... encoding="..."?>` line decides the character encoding (see
    // `parse_feed_bytes`). Reading a local file is quick, so this is a plain
    // blocking read rather than something that has to be awaited.
    let bytes = std::fs::read(path)
        .with_context(|| format!("can't read the feed file {}", path.display()))?;

    // Step 3: a file with nothing (or only blank space) in it is not a feed.
    // Say so plainly instead of letting the parser report "no root element".
    // `trim_ascii` ignores blank space at both ends.
    if bytes.trim_ascii().is_empty() {
        anyhow::bail!("{} is empty (no feed in it)", path.display());
    }

    // Step 4: parse. If the bytes are not RSS or Atom (a saved web page, say)
    // the parser fails and we add which file it was.
    parse_feed_bytes(&bytes)
        .with_context(|| format!("{} is not an RSS or Atom feed", path.display()))
}

impl Fetcher for FileFetcher {
    /// Reads the file (via [`read_feed_file`]) and maps its entries into
    /// articles (via `articles_from_feed`). It is `async` only because the
    /// `Fetcher` trait says so; nothing here waits on a network.
    async fn fetch(&self) -> anyhow::Result<Vec<Article>> {
        // `Path::new(&self.path)` views the stored text as a path without
        // copying it. `.with_context` names the source on failure, the same
        // wording the web fetcher uses ("fetching 'Name'").
        let feed = read_feed_file(Path::new(&self.path))
            .with_context(|| format!("fetching '{}'", self.name))?;

        Ok(articles_from_feed(
            feed,
            &self.name,
            &self.topic,
            &self.path,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny feed used by the tests below.
    const FEED: &str = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Site</title>
        <item><title>One</title><link>https://example.com/one</link></item>
        <item><title>Two</title><link>https://example.com/two</link></item>
        </channel></rss>"#;

    /// Makes a fresh, empty folder for one test under the system's temporary
    /// directory, named after the test so tests running side by side never
    /// share files. Returns its path.
    fn scratch(test_name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tuxwire-file-source-{test_name}-{}",
            std::process::id()
        ));
        // Start clean in case an earlier run left something behind.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A feed file becomes articles carrying the source's name and topic, and
    /// the file's path as their `feed_url`.
    #[tokio::test]
    async fn a_feed_file_becomes_articles() {
        let dir = scratch("good");
        let path = dir.join("index.rss");
        std::fs::write(&path, FEED).unwrap();

        let source = FileFetcher {
            name: "Saved Site".to_string(),
            path: path.to_string_lossy().into_owned(),
            topic: "sports".to_string(),
        };
        let articles = source.fetch().await.expect("the file should load");

        assert_eq!(articles.len(), 2);
        assert_eq!(articles[0].source, "Saved Site");
        assert_eq!(articles[0].topic, "sports");
        assert_eq!(articles[0].feed_url.as_deref(), Some(source.path.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every way a file can be wrong gives an error that names the file.
    #[test]
    fn bad_files_are_errors_that_name_the_file() {
        let dir = scratch("bad");

        // A file that does not exist.
        let missing = dir.join("nope.rss");
        let message = format!("{:#}", read_feed_file(&missing).unwrap_err());
        assert!(message.contains("nope.rss"), "{message}");

        // A folder where a file was expected.
        let message = format!("{:#}", read_feed_file(&dir).unwrap_err());
        assert!(message.contains("is not a file"), "{message}");

        // An empty file.
        let empty = dir.join("empty.rss");
        std::fs::write(&empty, "  \n").unwrap();
        let message = format!("{:#}", read_feed_file(&empty).unwrap_err());
        assert!(message.contains("is empty"), "{message}");

        // A saved web page, which is not a feed.
        let page = dir.join("page.html");
        std::fs::write(&page, "<html><body>Hello</body></html>").unwrap();
        let message = format!("{:#}", read_feed_file(&page).unwrap_err());
        assert!(message.contains("is not an RSS or Atom feed"), "{message}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
