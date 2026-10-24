//! Reading and writing OPML, the standard file format for feed lists.
//!
//! OPML is a small XML file that almost every feed reader can export and
//! import. Feeds sit inside "folders", which tuxwire calls topics:
//!
//! ```text
//! <opml version="2.0">
//!   <body>
//!     <outline text="Linux">
//!       <outline type="rss" text="LWN" xmlUrl="https://lwn.net/headlines/rss"/>
//!     </outline>
//!   </body>
//! </opml>
//! ```
//!
//! This module has two jobs and touches no files and no network, so the
//! terminal app and the Android app can both use it:
//!
//! 1. [`parse_opml`] turns the text of an OPML file into a list of feeds.
//! 2. [`write_opml`] turns a list of feeds into the text of an OPML file.
//!
//! It is written by hand, rather than with an XML library, because OPML uses
//! only a small corner of XML and the project avoids new dependencies where a
//! few hundred lines will do. That makes it deliberately *strict about what it
//! accepts*: a file from another program is untrusted input, so the reader
//! limits the file size, the nesting depth and the number of feeds, never
//! expands custom entities, and only accepts `http` and `https` addresses.

// Has clippy flag any undocumented item here, the same as in
// `core/src/icons/mod.rs`.
#![warn(clippy::missing_docs_in_private_items)]

use std::collections::HashSet;

use anyhow::bail;

use crate::fetchers::{SourceConfig, SourceType};
use crate::text::decode_entities;

/// The biggest OPML file [`parse_opml`] will read: 2 MiB. A list of thousands
/// of feeds is well under 1 MiB, so anything bigger is not a feed list.
pub const MAX_OPML_BYTES: usize = 2 * 1024 * 1024;

/// The topic given to a feed that is not inside any folder in the file.
pub const DEFAULT_TOPIC: &str = "Uncategorized";

/// The most feeds read from one file. Any further ones are counted as ignored.
const MAX_FEEDS: usize = 5000;

/// How many folders may be nested inside one another. Real files use one or
/// two; the limit stops a hostile file from making the reader hold a huge
/// stack of open folders.
const MAX_DEPTH: usize = 32;

/// The longest feed name kept, in characters. Longer names are cut.
const MAX_NAME_CHARS: usize = 200;

/// The longest topic name kept, in characters. Longer names are cut.
const MAX_TOPIC_CHARS: usize = 100;

/// The longest feed address accepted, in bytes. Longer ones are ignored.
const MAX_URL_BYTES: usize = 2048;

/// What [`parse_opml`] found in a file.
#[derive(Debug, Clone)]
pub struct OpmlImport {
    /// The usable feeds, in file order, each already filed under a topic.
    /// They are all `SourceType::Rss`: OPML has no notion of other types.
    pub feeds: Vec<SourceConfig>,
    /// How many feed entries were unusable: not an `http`/`https` address, an
    /// address that is too long, or beyond the per-file limit.
    pub ignored: usize,
    /// How many entries repeated an address already seen earlier in the same
    /// file. Only the first copy is kept.
    pub repeated: usize,
}

/// Reads the text of an OPML file and returns the feeds in it.
///
/// Each `<outline>` that has an `xmlUrl` attribute is a feed. Its topic is the
/// name of the innermost folder (an `<outline>` without `xmlUrl`) around it,
/// or [`DEFAULT_TOPIC`] if there is none. Everything the reader does not
/// understand (comments, the `<head>`, empty folders) is skipped.
///
/// Returns an error, rather than a partial result, when the file is too big,
/// is not OPML at all, is damaged (an unclosed comment or tag), or is nested
/// absurdly deep.
pub fn parse_opml(text: &str) -> anyhow::Result<OpmlImport> {
    // Step 1: refuse a file that is too big before looking inside it.
    if text.len() > MAX_OPML_BYTES {
        bail!(
            "this file is too big to be a feed list (the limit is {} MB)",
            MAX_OPML_BYTES / (1024 * 1024)
        );
    }

    // Step 2: set up what the scan below fills in.
    let mut feeds: Vec<SourceConfig> = Vec::new();
    let mut ignored = 0usize;
    let mut repeated = 0usize;
    // Addresses seen so far (lower-cased), to spot repeats. A `HashSet` answers
    // "have I seen this?" quickly however long the list grows.
    let mut seen: HashSet<String> = HashSet::new();
    // One entry per currently open `<outline>`. `Some(name)` is a folder with a
    // name; `None` is something that gives children no topic of its own (an
    // unnamed folder, or a feed that happens to have children).
    let mut folders: Vec<Option<String>> = Vec::new();
    let mut found_root = false;

    // Step 3: walk through the text one tag at a time. `rest` is the part not
    // yet looked at; it shrinks as we go. Slicing a `&str` (`&rest[a..b]`)
    // borrows part of the text without copying it.
    let mut rest = text;
    while let Some(lt) = rest.find('<') {
        rest = &rest[lt..];

        // Skip the kinds of markup that carry no feeds, each up to its own
        // closing marker. `strip_prefix` returns the text after the prefix if
        // it is there, or `None` if not.
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = skip_past(after, "-->")?;
            continue;
        }
        if let Some(after) = rest.strip_prefix("<![CDATA[") {
            rest = skip_past(after, "]]>")?;
            continue;
        }
        if let Some(after) = rest.strip_prefix("<?") {
            rest = skip_past(after, "?>")?;
            continue;
        }
        if rest.starts_with("<!") {
            // A `<!DOCTYPE ...>`. It may hold a `[ ... ]` block, which can
            // contain `>` characters, so skip that block first. Custom
            // entities declared there are never used.
            rest = skip_declaration(rest)?;
            continue;
        }

        // An ordinary tag. Find where it ends (a `>` outside quotes).
        let Some(end) = find_tag_end(rest) else {
            bail!("this file is damaged: a tag is never closed");
        };
        let inner = &rest[1..end];
        rest = &rest[end + 1..];

        // Split off a leading `/` (a closing tag) and a trailing `/` (a tag
        // that closes itself, like `<outline .../>`).
        let (closing, inner) = match inner.strip_prefix('/') {
            Some(after) => (true, after),
            None => (false, inner),
        };
        let (self_closing, inner) = match inner.strip_suffix('/') {
            Some(before) => (true, before),
            None => (false, inner),
        };
        let (tag_name, attr_text) = split_tag_name(inner);

        // React to the tags that matter. Names are compared ignoring case so
        // an odd file using `<OUTLINE>` still works.
        if tag_name.eq_ignore_ascii_case("opml") && !closing {
            found_root = true;
        } else if tag_name.eq_ignore_ascii_case("outline") {
            if closing {
                // A folder (or feed with children) ended. Popping an empty
                // list does nothing, so a stray closing tag is harmless.
                folders.pop();
                continue;
            }

            let attrs = parse_attributes(attr_text);
            let label = attr(&attrs, "text").or_else(|| attr(&attrs, "title"));
            let address = attr(&attrs, "xmlUrl").map(str::trim).unwrap_or("");

            if address.is_empty() {
                // No feed address: this is a folder. An empty one
                // (self-closing) has nothing to hold, so only a real one is
                // remembered for its children.
                if !self_closing {
                    if folders.len() >= MAX_DEPTH {
                        bail!("this file nests folders too deeply to be a feed list");
                    }
                    let name = label
                        .map(|l| clean_label(l, MAX_TOPIC_CHARS))
                        .filter(|l| !l.is_empty());
                    folders.push(name);
                }
            } else {
                // A feed. Decide whether it is usable.
                if feeds.len() >= MAX_FEEDS || !is_web_address(address) {
                    ignored += 1;
                } else if !seen.insert(address.to_ascii_lowercase()) {
                    // `insert` returns `false` when the value was already
                    // there, which is how a repeat is detected.
                    repeated += 1;
                } else {
                    // The topic is the innermost folder that has a name.
                    // `iter().rev()` walks the open folders from the inside
                    // out; `flatten()` skips the `None`s.
                    let topic = folders
                        .iter()
                        .rev()
                        .flatten()
                        .next()
                        .cloned()
                        .unwrap_or_else(|| DEFAULT_TOPIC.to_string());
                    let name = label
                        .map(|l| clean_label(l, MAX_NAME_CHARS))
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| address.to_string());
                    feeds.push(SourceConfig {
                        name,
                        source_type: SourceType::Rss,
                        url: address.to_string(),
                        topic,
                    });
                }
                // A feed with children is unusual, but its closing tag will
                // still arrive, so keep the open-tag count balanced.
                if !self_closing {
                    if folders.len() >= MAX_DEPTH {
                        bail!("this file nests folders too deeply to be a feed list");
                    }
                    folders.push(None);
                }
            }
        }
    }

    // Step 4: a file with no `<opml>` tag is not OPML, however well-formed.
    if !found_root {
        bail!("this does not look like an OPML file");
    }
    Ok(OpmlImport {
        feeds,
        ignored,
        repeated,
    })
}

/// Turns a list of feeds into the text of an OPML file.
///
/// Feeds are grouped into one folder per topic, in the order each topic first
/// appears, and keep their order within a topic. Everything written into an
/// attribute is escaped (`&`, `<`, `>`, quotes), so any name or address is
/// safe. A Reddit source has no feed address of its own, so it is written as
/// its ordinary feed (`https://www.reddit.com/r/NAME/.rss`), which other
/// readers can use; read back, it becomes a plain RSS feed. A **file** source
/// (a feed saved in a file on this computer) is left out: its path means
/// nothing to another program or computer, and OPML holds web addresses.
pub fn write_opml(sources: &[SourceConfig]) -> String {
    // Step 1: group the feeds by topic. Each entry is a topic name plus the
    // feeds filed under it. A linear search is fine: there are few topics.
    let mut groups: Vec<(&str, Vec<&SourceConfig>)> = Vec::new();
    for source in sources {
        // File sources have no web address, so they cannot be exported.
        if source.source_type == SourceType::File {
            continue;
        }
        match groups
            .iter_mut()
            .find(|(topic, _)| *topic == source.topic.as_str())
        {
            Some((_, members)) => members.push(source),
            None => groups.push((source.topic.as_str(), vec![source])),
        }
    }

    // Step 2: write the header, then one folder per topic.
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<opml version=\"2.0\">\n");
    out.push_str("  <head>\n    <title>tuxwire feeds</title>\n  </head>\n");
    out.push_str("  <body>\n");
    for (topic, members) in groups {
        let topic = xml_escape(topic);
        out.push_str(&format!(
            "    <outline text=\"{topic}\" title=\"{topic}\">\n"
        ));
        for source in members {
            let name = xml_escape(&source.name);
            let address = xml_escape(&feed_address(source));
            out.push_str(&format!(
                "      <outline type=\"rss\" text=\"{name}\" title=\"{name}\" xmlUrl=\"{address}\"/>\n"
            ));
        }
        out.push_str("    </outline>\n");
    }
    out.push_str("  </body>\n</opml>\n");
    out
}

/// The web address to write for a source: its own address for an RSS feed,
/// or the subreddit's ordinary feed address for a Reddit source. (A file source
/// has no web address; [`write_opml`] skips those, so for one this just returns
/// the stored path, which is only useful for comparing.)
///
/// Public so that the terminal app can tell whether an imported feed is
/// already in a list by comparing addresses on the same terms the export uses.
pub fn feed_address(source: &SourceConfig) -> String {
    match source.source_type {
        SourceType::Rss | SourceType::File => source.url.clone(),
        SourceType::Reddit => {
            let stored = source.url.trim();
            // A value that is already a web address is used as it is.
            if is_web_address(stored) {
                stored.to_string()
            } else {
                // Otherwise it is a subreddit name, possibly written `r/name`.
                let name = stored.trim_matches('/');
                let name = name.strip_prefix("r/").unwrap_or(name);
                format!("https://www.reddit.com/r/{name}/.rss")
            }
        }
    }
}

/// Escapes text for use inside a double-quoted XML attribute, and drops
/// control characters that XML cannot hold at all.
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Tab, newline and carriage return are the only control characters
            // XML allows; they are written as spaces so a name stays on one line.
            '\t' | '\n' | '\r' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Whether `address` is an `http` or `https` web address with something after
/// the scheme, no spaces, and a sensible length. Anything else (`javascript:`,
/// `file:`, `ftp:`, plain text) is refused.
pub(crate) fn is_web_address(address: &str) -> bool {
    if address.len() > MAX_URL_BYTES || address.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return false;
    }
    let lower = address.to_ascii_lowercase();
    // `strip_prefix` gives what follows the scheme; it must not be empty.
    let after = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"));
    matches!(after, Some(host) if !host.is_empty())
}

/// Tidies a name from the file: control characters removed, runs of spaces
/// and line breaks squeezed to one space, and cut to `max_chars` characters.
fn clean_label(label: &str, max_chars: usize) -> String {
    // `split_whitespace` ignores any amount of whitespace between words, and
    // `join(" ")` glues the words back together with single spaces.
    let squeezed = label
        .chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    squeezed.chars().take(max_chars).collect()
}

/// Skips ahead in `text` to just after the first `marker`, or fails if the
/// marker never appears (a damaged file, such as a comment that never ends).
fn skip_past<'a>(text: &'a str, marker: &str) -> anyhow::Result<&'a str> {
    match text.find(marker) {
        Some(at) => Ok(&text[at + marker.len()..]),
        None => bail!("this file is damaged: a section is never closed (expected {marker:?})"),
    }
}

/// Skips a `<!DOCTYPE ...>` style declaration that starts at the beginning of
/// `text`, including a bracketed `[ ... ]` block inside it, and returns what
/// follows. Nothing in the declaration is ever acted on.
fn skip_declaration(text: &str) -> anyhow::Result<&str> {
    let first_gt = text.find('>');
    let first_bracket = text.find('[');
    // With a `[` before the first `>`, the declaration ends at `]` then `>`.
    if let (Some(bracket), Some(gt)) = (first_bracket, first_gt)
        && bracket < gt
    {
        let after_bracket = &text[bracket..];
        let Some(close) = after_bracket.find(']') else {
            bail!("this file is damaged: a declaration is never closed");
        };
        let after_close = &after_bracket[close..];
        return match after_close.find('>') {
            Some(at) => Ok(&after_close[at + 1..]),
            None => bail!("this file is damaged: a declaration is never closed"),
        };
    }
    match first_gt {
        Some(gt) => Ok(&text[gt + 1..]),
        None => bail!("this file is damaged: a declaration is never closed"),
    }
}

/// The byte position of the `>` that ends the tag at the start of `text`
/// (which begins with `<`), ignoring any `>` inside a quoted attribute value.
/// `None` if the tag never ends.
pub(crate) fn find_tag_end(text: &str) -> Option<usize> {
    // `Some('"')` while inside a double-quoted value, and so on; `None` outside.
    let mut quote: Option<char> = None;
    for (position, ch) in text.char_indices() {
        match (quote, ch) {
            (None, '"') | (None, '\'') => quote = Some(ch),
            (Some(open), c) if c == open => quote = None,
            (None, '>') => return Some(position),
            _ => {}
        }
    }
    None
}

/// Splits the inside of a tag into its name and the attribute text after it:
/// `outline text="a"` becomes `("outline", " text=\"a\"")`.
pub(crate) fn split_tag_name(inner: &str) -> (&str, &str) {
    match inner.find(char::is_whitespace) {
        Some(at) => (&inner[..at], &inner[at..]),
        None => (inner, ""),
    }
}

/// Reads `name="value"` pairs from the text after a tag's name. Values may be
/// in single or double quotes (or, leniently, bare), and have their entities
/// decoded (`&amp;` becomes `&`). Names are kept as written.
pub(crate) fn parse_attributes(text: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    let mut rest = text.trim_start();

    while !rest.is_empty() {
        // The attribute name runs up to `=` or whitespace.
        let name_end = rest
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(rest.len());
        let name = &rest[..name_end];
        rest = rest[name_end..].trim_start();

        // No `=` means a bare word with no value; skip it.
        let Some(after_eq) = rest.strip_prefix('=') else {
            rest = rest.trim_start();
            if name.is_empty() {
                // Nothing consumed and nothing to skip: step over one
                // character so the loop always makes progress.
                let mut chars = rest.chars();
                chars.next();
                rest = chars.as_str();
            }
            continue;
        };
        rest = after_eq.trim_start();

        // The value: quoted, or bare up to whitespace.
        let value = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let body = &rest[1..];
                match body.find(q) {
                    Some(close) => {
                        let value = &body[..close];
                        rest = body[close + 1..].trim_start();
                        value
                    }
                    // A quote that never closes: take the rest and stop.
                    None => {
                        rest = "";
                        body
                    }
                }
            }
            _ => {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                let value = &rest[..end];
                rest = rest[end..].trim_start();
                value
            }
        };
        if !name.is_empty() {
            attrs.push((name.to_string(), decode_entities(value)));
        }
    }
    attrs
}

/// The value of the attribute called `name`, ignoring upper/lower case in the
/// name (`xmlUrl`, `xmlurl` and `XMLURL` all match). `None` if absent.
pub(crate) fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps outlines in the usual OPML skeleton, to keep the tests short.
    fn opml(body: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><opml version=\"2.0\"><head><title>x</title></head><body>{body}</body></opml>"
        )
    }

    /// Shorthand: the feeds as (name, address, topic) triples.
    fn triples(import: &OpmlImport) -> Vec<(String, String, String)> {
        import
            .feeds
            .iter()
            .map(|f| (f.name.clone(), f.url.clone(), f.topic.clone()))
            .collect()
    }

    /// Feeds inside folders get the folder's name; feeds outside any folder
    /// are filed under "Uncategorized".
    #[test]
    fn folders_become_topics_and_loose_feeds_are_uncategorized() {
        let text = opml(
            r#"<outline text="Linux">
                 <outline type="rss" text="LWN" xmlUrl="https://lwn.net/rss"/>
               </outline>
               <outline type="rss" text="Loose" xmlUrl="https://loose.example/feed"/>"#,
        );
        let import = parse_opml(&text).unwrap();
        assert_eq!(
            triples(&import),
            [
                ("LWN".into(), "https://lwn.net/rss".into(), "Linux".into()),
                (
                    "Loose".into(),
                    "https://loose.example/feed".into(),
                    "Uncategorized".into()
                ),
            ]
        );
        assert_eq!((import.ignored, import.repeated), (0, 0));
    }

    /// With folders inside folders, the innermost one names the topic, and a
    /// feed after the inner folder closes goes back to the outer one.
    #[test]
    fn the_innermost_folder_is_the_topic() {
        let text = opml(
            r#"<outline text="Tech">
                 <outline text="Linux">
                   <outline text="A" xmlUrl="https://a.example/f"/>
                 </outline>
                 <outline text="B" xmlUrl="https://b.example/f"/>
               </outline>"#,
        );
        let import = parse_opml(&text).unwrap();
        let topics: Vec<&str> = import.feeds.iter().map(|f| f.topic.as_str()).collect();
        assert_eq!(topics, ["Linux", "Tech"]);
    }

    /// Entities in names and addresses are decoded, `title` is the fallback
    /// name, and an unnamed feed is named after its address.
    #[test]
    fn entities_are_decoded_and_names_fall_back() {
        let text = opml(
            r#"<outline text="Ben &amp; Jerry" xmlUrl="https://x.example/f?a=1&amp;b=2"/>
               <outline title="Only title" xmlUrl="https://y.example/f"/>
               <outline xmlUrl="https://z.example/f"/>"#,
        );
        let import = parse_opml(&text).unwrap();
        let names: Vec<&str> = import.feeds.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Ben & Jerry", "Only title", "https://z.example/f"]);
        assert_eq!(import.feeds[0].url, "https://x.example/f?a=1&b=2");
    }

    /// Addresses that are not http/https are ignored and counted, never added.
    #[test]
    fn unsafe_addresses_are_ignored_and_counted() {
        let text = opml(
            r#"<outline text="a" xmlUrl="javascript:alert(1)"/>
               <outline text="b" xmlUrl="file:///etc/passwd"/>
               <outline text="c" xmlUrl="ftp://x.example/f"/>
               <outline text="d" xmlUrl="https://"/>
               <outline text="e" xmlUrl="not a url"/>
               <outline text="ok" xmlUrl="HTTPS://Ok.Example/f"/>"#,
        );
        let import = parse_opml(&text).unwrap();
        assert_eq!(import.feeds.len(), 1);
        assert_eq!(import.feeds[0].url, "HTTPS://Ok.Example/f");
        assert_eq!(import.ignored, 5);
    }

    /// A repeated address (ignoring case) is kept once and counted.
    #[test]
    fn repeats_inside_the_file_are_counted() {
        let text = opml(
            r#"<outline text="a" xmlUrl="https://x.example/f"/>
               <outline text="b" xmlUrl="https://X.example/f"/>"#,
        );
        let import = parse_opml(&text).unwrap();
        assert_eq!(import.feeds.len(), 1);
        assert_eq!(import.repeated, 1);
    }

    /// Comments, processing instructions, a DOCTYPE with a bracketed block,
    /// CDATA, a `>` inside a quoted value and odd capitalisation are all fine.
    #[test]
    fn markup_the_reader_does_not_need_is_skipped() {
        let text = "<?xml version=\"1.0\"?>\n\
            <!DOCTYPE opml [ <!ENTITY big \"aaaa\"> ]>\n\
            <!-- a comment with <outline xmlUrl=\"https://no.example/f\"/> inside -->\n\
            <OPML version=\"2.0\"><body>\n\
            <![CDATA[ <outline xmlUrl=\"https://no.example/g\"/> ]]>\n\
            <OUTLINE TEXT=\"a > b\" XMLURL=\"https://ok.example/f\"/>\n\
            </body></OPML>";
        let import = parse_opml(text).unwrap();
        assert_eq!(triples(&import).len(), 1);
        assert_eq!(import.feeds[0].name, "a > b");
        assert_eq!(import.feeds[0].url, "https://ok.example/f");
    }

    /// Single-quoted attributes work too.
    #[test]
    fn single_quoted_attributes_work() {
        let text = opml("<outline text='Q' xmlUrl='https://q.example/f'/>");
        assert_eq!(parse_opml(&text).unwrap().feeds[0].name, "Q");
    }

    /// Things that are not OPML, or are damaged, are errors with a message.
    #[test]
    fn bad_files_are_errors() {
        // Well-formed but not OPML.
        assert!(parse_opml("<html><body>hi</body></html>").is_err());
        // Empty.
        assert!(parse_opml("").is_err());
        // An unterminated comment, tag and declaration.
        assert!(parse_opml("<opml><!-- never ends").is_err());
        assert!(parse_opml("<opml><outline text=\"x").is_err());
        assert!(parse_opml("<opml><!DOCTYPE x").is_err());
        // Folders nested far too deep.
        let deep = format!("<opml>{}</opml>", "<outline text=\"f\">".repeat(100));
        assert!(parse_opml(&deep).is_err());
    }

    /// Garbled input never crashes the reader: it either gives a result or an
    /// error. This builds thousands of random strings out of the pieces that
    /// matter to the reader (angle brackets, quotes, slashes, entities, the
    /// words it looks for and multi-byte characters) and feeds them in.
    ///
    /// The numbers come from a tiny "linear congruential generator", a few
    /// lines of arithmetic that give repeatable pseudo-random values without
    /// needing a random-number crate. The same seed gives the same strings
    /// every run, so a failure can be reproduced.
    #[test]
    fn garbled_input_never_panics() {
        const PIECES: &[&str] = &[
            "<",
            ">",
            "</",
            "/>",
            "\"",
            "'",
            "=",
            " ",
            "\n",
            "&",
            "&amp;",
            "&#x41;",
            "&#;",
            "<!--",
            "-->",
            "<![CDATA[",
            "]]>",
            "<?",
            "?>",
            "<!DOCTYPE",
            "[",
            "]",
            "opml",
            "outline",
            "OUTLINE",
            "xmlUrl",
            "text",
            "https://a.example/f",
            "\u{e9}",
            "\u{1F600}",
            "\u{0}",
        ];
        let mut state: u64 = 12345;
        for _ in 0..5000 {
            let mut input = String::new();
            // Between 1 and 40 random pieces per input.
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let count = 1 + (state >> 33) as usize % 40;
            for _ in 0..count {
                state = state
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                input.push_str(PIECES[(state >> 33) as usize % PIECES.len()]);
            }
            // The result does not matter; reaching this line without a panic does.
            let _ = parse_opml(&input);
        }
    }

    /// A file over the size limit is refused before it is read.
    #[test]
    fn an_oversized_file_is_refused() {
        let big = format!("<opml>{}</opml>", " ".repeat(MAX_OPML_BYTES));
        let message = parse_opml(&big).unwrap_err().to_string();
        assert!(message.contains("too big"), "{message}");
    }

    /// Extra feeds beyond the per-file cap are counted as ignored.
    #[test]
    fn the_feed_count_is_capped() {
        let mut body = String::new();
        for n in 0..(MAX_FEEDS + 3) {
            body.push_str(&format!(
                "<outline text=\"f{n}\" xmlUrl=\"https://x.example/{n}\"/>"
            ));
        }
        let import = parse_opml(&opml(&body)).unwrap();
        assert_eq!(import.feeds.len(), MAX_FEEDS);
        assert_eq!(import.ignored, 3);
    }

    /// Long names are cut and control characters and line breaks tidied.
    #[test]
    fn names_are_tidied() {
        let long = "x".repeat(500);
        let text = opml(&format!(
            "<outline text=\"  two\n   words\u{7} \" xmlUrl=\"https://a.example/f\"/>\
             <outline text=\"{long}\" xmlUrl=\"https://b.example/f\"/>"
        ));
        let import = parse_opml(&text).unwrap();
        assert_eq!(import.feeds[0].name, "two words");
        assert_eq!(import.feeds[1].name.chars().count(), MAX_NAME_CHARS);
    }

    /// Shorthand for building a source in the writer tests.
    fn source(name: &str, url: &str, topic: &str, source_type: SourceType) -> SourceConfig {
        SourceConfig {
            name: name.into(),
            source_type,
            url: url.into(),
            topic: topic.into(),
        }
    }

    /// Writing groups feeds by topic (in first-seen order) and reading the
    /// result back gives the same feeds, even with awkward characters.
    #[test]
    fn writing_then_reading_round_trips() {
        let sources = vec![
            source(
                "Ars",
                "https://ars.example/f?a=1&b=2",
                "Tech",
                SourceType::Rss,
            ),
            source(
                "Tom \"T\" <b> & Co",
                "https://t.example/f",
                "News",
                SourceType::Rss,
            ),
            source("LWN", "https://lwn.example/f", "Tech", SourceType::Rss),
        ];
        let text = write_opml(&sources);
        let back = parse_opml(&text).unwrap();
        // Topic order is first-seen (Tech, News); within Tech, feed order kept.
        assert_eq!(
            triples(&back),
            [
                (
                    "Ars".into(),
                    "https://ars.example/f?a=1&b=2".into(),
                    "Tech".into()
                ),
                ("LWN".into(), "https://lwn.example/f".into(), "Tech".into()),
                (
                    "Tom \"T\" <b> & Co".into(),
                    "https://t.example/f".into(),
                    "News".into()
                ),
            ]
        );
        // The raw text never contains an unescaped special character in a value.
        assert!(text.contains("a=1&amp;b=2"));
        assert!(text.contains("&lt;b&gt;"));
    }

    /// A Reddit source is exported as its ordinary feed address.
    #[test]
    fn reddit_sources_are_written_as_their_feed_address() {
        let sources = vec![
            source("Linux", "linux", "Reddit", SourceType::Reddit),
            source("Rust", "r/rust", "Reddit", SourceType::Reddit),
            source(
                "Odd",
                "https://example.com/r/odd/.rss",
                "Reddit",
                SourceType::Reddit,
            ),
        ];
        let back = parse_opml(&write_opml(&sources)).unwrap();
        let urls: Vec<&str> = back.feeds.iter().map(|f| f.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://www.reddit.com/r/linux/.rss",
                "https://www.reddit.com/r/rust/.rss",
                "https://example.com/r/odd/.rss",
            ]
        );
        // They come back as plain RSS feeds.
        assert!(back.feeds.iter().all(|f| f.source_type == SourceType::Rss));
    }

    /// An empty list still writes a valid file that reads back as no feeds.
    #[test]
    fn an_empty_list_writes_a_valid_file() {
        let back = parse_opml(&write_opml(&[])).unwrap();
        assert!(back.feeds.is_empty());
    }

    /// Control characters in a name are dropped, not written into the file.
    #[test]
    fn control_characters_are_not_written() {
        let text = write_opml(&[source(
            "a\u{1}b\nc",
            "https://a.example/f",
            "T",
            SourceType::Rss,
        )]);
        assert!(!text.contains('\u{1}'));
        assert_eq!(parse_opml(&text).unwrap().feeds[0].name, "ab c");
    }

    /// A file source (a feed saved on this computer) is left out of an export:
    /// OPML holds web addresses, and a path on one computer means nothing on
    /// another. The web feed next to it is still written.
    #[test]
    fn file_sources_are_not_exported() {
        let text = write_opml(&[
            source("Web", "https://web.example/feed", "Tech", SourceType::Rss),
            source(
                "Saved",
                "/home/me/Downloads/index.rss",
                "Tech",
                SourceType::File,
            ),
        ]);
        assert!(text.contains("https://web.example/feed"));
        assert!(
            !text.contains("index.rss"),
            "the file path must not be exported"
        );
        assert!(!text.contains("Saved"));
    }
}
