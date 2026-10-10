//! Turning the text found in feeds into plain text that is pleasant to read.
//!
//! Feed summaries are usually HTML (`<p>`, `<a href=...>`, `&amp;`, and so on),
//! but the apps show plain text. This module has two jobs:
//!
//! 1. [`html_to_text`] removes the markup and keeps the words, with paragraph
//!    breaks where the HTML had them.
//! 2. [`limit_to_sentences`] shortens long text, but only at the end of a
//!    sentence, so a summary never stops halfway through one.
//!
//! It is written by hand, rather than with an HTML library, because a feed
//! summary needs only a small part of what HTML can express, and the project
//! avoids new dependencies where a few dozen lines will do. The cost is that it
//! is not a full HTML parser: it is a careful tag stripper, which is all a
//! summary needs.

// Has clippy flag any undocumented item here, the same as in
// `core/src/icons/mod.rs`.
#![warn(clippy::missing_docs_in_private_items)]

/// The most characters a stored summary may have, before the cut described on
/// [`limit_to_sentences`]. Long enough for a few paragraphs, short enough that a
/// phone screen shows the lot without a long scroll.
pub const SUMMARY_MAX_CHARS: usize = 2000;

/// HTML tags that start a new line (or paragraph) when they open or close.
///
/// `&[&str]` is a "slice of string slices": a fixed list of text values baked
/// into the program. `const` means it is built at compile time, not each call.
const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "br",
    "li",
    "ul",
    "ol",
    "tr",
    "table",
    "blockquote",
    "pre",
    "section",
    "article",
    "header",
    "footer",
    "figure",
    "figcaption",
    "hr",
    "dl",
    "dt",
    "dd",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
];

/// Converts HTML into plain text.
///
/// What it does, in order:
/// 1. Drops tags, keeping the text between them.
/// 2. Drops `<script>` and `<style>` blocks and comments entirely, including
///    what is inside them.
/// 3. Turns block tags (paragraphs, line breaks, list items, headings) into
///    line breaks, and list items into lines starting with `- `.
/// 4. Decodes entities such as `&amp;`, `&#8217;` and `&nbsp;`.
/// 5. Tidies whitespace: runs of spaces become one, and at most one blank line
///    is kept between paragraphs.
///
/// A `<` that does not start a real tag (as in `a < b`) is kept as text.
///
/// `html: &str` borrows the input and the result is a new owned `String`, so
/// the caller keeps its original text.
pub fn html_to_text(html: &str) -> String {
    // Step 1: build the text without markup. `out` collects the result and
    // `rest` is the part of the input not looked at yet. Each time round the
    // loop we find the next `<`, copy the plain text before it, and decide what
    // the tag was.
    let mut out = String::with_capacity(html.len());
    let mut rest = html;

    while let Some(start) = rest.find('<') {
        // Everything before the `<` is ordinary text: keep it.
        out.push_str(&rest[..start]);
        rest = &rest[start..];

        // A comment (`<!-- ... -->`) is skipped whole. If it is never closed,
        // the rest of the input is part of it.
        if rest.starts_with("<!--") {
            rest = match rest.find("-->") {
                Some(end) => &rest[end + 3..],
                None => "",
            };
            continue;
        }

        // `<!DOCTYPE ...>` and `<?xml ...?>` are skipped up to their `>`.
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = match rest.find('>') {
                Some(end) => &rest[end + 1..],
                None => "",
            };
            continue;
        }

        // A real tag's name starts with a letter (after an optional `/`). If
        // not, this `<` is just a less-than sign in the text, as in `a < b`:
        // keep it and carry on after it.
        let after_open = rest[1..].trim_start_matches('/');
        if !after_open.starts_with(|c: char| c.is_ascii_alphabetic()) {
            out.push('<');
            rest = &rest[1..];
            continue;
        }

        // A tag with no closing `>` cannot be parsed: keep it as text and stop.
        let Some(end) = rest.find('>') else {
            out.push_str(rest);
            rest = "";
            break;
        };

        // Split out the inside of the tag, and move on past it.
        let inside = &rest[1..end];
        rest = &rest[end + 1..];

        // The tag's name in lower case: the letters and digits after the `<`
        // (and after a `/` for a closing tag), up to the first space.
        let closing = inside.starts_with('/');
        let name: String = inside
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();

        // `<script>` and `<style>` hold code, not words. Skip everything up to
        // the matching closing tag. `to_ascii_lowercase` keeps every character
        // at the same byte position, so an index found in the lower-cased copy
        // is valid in the original.
        if !closing && (name == "script" || name == "style") {
            let needle = format!("</{name}");
            rest = match rest.to_ascii_lowercase().find(&needle) {
                Some(pos) => {
                    let from_close = &rest[pos..];
                    match from_close.find('>') {
                        Some(e) => &from_close[e + 1..],
                        None => "",
                    }
                }
                None => "",
            };
            continue;
        }

        // Block tags become line breaks. An opening `<li>` also starts a
        // bulleted line, and a closing `</p>` adds a second break so
        // paragraphs are separated by a blank line.
        //
        // A closing `</li>` adds nothing: the *next* `<li>` already starts its
        // own line, so breaking here too would leave a blank line between
        // every pair of list items.
        if BLOCK_TAGS.contains(&name.as_str()) && !(name == "li" && closing) {
            out.push('\n');
            if name == "li" && !closing {
                out.push_str("- ");
            }
            if name == "p" && closing {
                out.push('\n');
            }
        }
        // Any other tag (`<a>`, `<b>`, `<span>`, `<img>`...) is dropped and its
        // inner text stays, because that text is already in `out`.
    }

    // Whatever is left has no more tags in it.
    out.push_str(rest);

    // Steps 4 and 5. Entities are decoded *after* the tags are gone, so a
    // literal `&lt;b&gt;` in the source correctly ends up as the visible text
    // `<b>` instead of being mistaken for a tag.
    tidy_whitespace(&decode_entities(&out))
}

/// Replaces entities such as `&amp;`, `&#8217;` and `&#x2019;` with the
/// characters they stand for. An entity it does not know is left exactly as
/// written, so nothing is lost.
fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    // Same pattern as `html_to_text`: copy the plain text up to the next `&`,
    // then try to decode what follows it.
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];

        // An entity looks like `&name;` with a short name, so only look for the
        // `;` in the next few bytes. `filter` keeps the position only if it is
        // close enough; `and_then` then tries to decode the name between.
        let decoded = rest[1..]
            .find(';')
            .filter(|&semi| semi <= 10)
            .and_then(|semi| decode_entity(&rest[1..1 + semi]).map(|ch| (ch, semi)));

        match decoded {
            // Success: emit the character and skip `&`, the name and `;`.
            Some((ch, semi)) => {
                out.push(ch);
                rest = &rest[semi + 2..];
            }
            // Not a known entity: keep the `&` as plain text and move on.
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The character for one entity name (the part between `&` and `;`), or `None`
/// if the name is not one we know. Handles the handful of named entities feeds
/// commonly use, plus any number written as `#8217` or `#x2019`.
fn decode_entity(name: &str) -> Option<char> {
    // Numeric entities: `#` then decimal digits, or `x` then hex digits.
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse::<u32>().ok()?,
        };
        // `from_u32` is `None` for numbers that are not valid characters.
        // Control characters (other than whitespace) are refused too.
        return char::from_u32(code).filter(|c| !c.is_control() || c.is_whitespace());
    }

    // Named entities. `return None` inside the `match` handles unknown names.
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "hellip" => '\u{2026}',
        "mdash" => '\u{2014}',
        "ndash" => '\u{2013}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "copy" => '\u{00A9}',
        "reg" => '\u{00AE}',
        "trade" => '\u{2122}',
        _ => return None,
    })
}

/// Cleans up spacing: each line's runs of spaces become a single space, blank
/// lines at the start and end are dropped, and any run of blank lines in the
/// middle becomes exactly one (so paragraphs are one blank line apart).
fn tidy_whitespace(text: &str) -> String {
    let mut out = String::new();
    // How many blank lines were seen since the last line with words on it.
    let mut blank_run = 0;

    for line in text.lines() {
        // `split_whitespace` splits on any run of whitespace and drops the
        // empty pieces; joining with one space collapses the run. This also
        // counts a non-breaking space as whitespace.
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");

        // A blank line is only remembered; it is written later, if more words
        // follow, so none are left dangling at the end.
        if collapsed.is_empty() {
            blank_run += 1;
            continue;
        }

        // Start a new line, with a blank line first if there was one before.
        if !out.is_empty() {
            out.push('\n');
            if blank_run > 0 {
                out.push('\n');
            }
        }
        blank_run = 0;
        out.push_str(&collapsed);
    }
    out
}

/// Shortens `text` to at most about `max_chars` characters, cutting only at the
/// end of a sentence, so the result never stops halfway through one.
///
/// - If the whole text fits, it is returned unchanged.
/// - Otherwise it keeps as many complete sentences as fit within `max_chars`.
///   That may be well under the limit: a cleanly ended shorter text is better
///   than a longer one cut off in the middle.
/// - A sentence ends at `.`, `!` or `?` (optionally followed by closing quotes
///   or brackets) that is followed by a space or the end of the text, so the
///   dot in `3.5` or in a web address does not count. Abbreviations such as
///   "e.g." can still be mistaken for the end of a sentence; the cost is only a
///   slightly shorter summary.
/// - If the first sentence alone is longer than the limit (no sentence end
///   within it), it falls back to cutting at a word boundary and adding `...`,
///   so the reader can see it was shortened.
///
/// Lengths are counted in characters, not bytes, so accented letters and emoji
/// are never split.
pub fn limit_to_sentences(text: &str, max_chars: usize) -> String {
    // Step 1: if it all fits, there is nothing to do. `chars().count()` counts
    // characters; `len()` would count bytes, which is not the same.
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    // Step 2: find the byte position where the first `max_chars` characters end.
    // `nth(max_chars)` is the character just past the limit; its byte position
    // is where to cut. `candidate` is the text we are allowed to keep.
    let cut = text
        .char_indices()
        .nth(max_chars)
        .map_or(text.len(), |(index, _)| index);
    let candidate = &text[..cut];

    // Step 3: look for the last sentence end inside the candidate.
    let mut last_end = None;
    for (index, ch) in candidate.char_indices() {
        // Only `.`, `!` and `?` can end a sentence.
        if !matches!(ch, '.' | '!' | '?') {
            continue;
        }

        // Include any closing quote or bracket right after it, as in `"Stop."`.
        // This reads from the full `text`, then checks it stays within the cap.
        let mut end = index + ch.len_utf8();
        for next in text[end..].chars() {
            if matches!(next, '"' | '\'' | ')' | ']' | '\u{201D}' | '\u{2019}') {
                end += next.len_utf8();
            } else {
                break;
            }
        }
        if end > cut {
            continue;
        }

        // It is a real sentence end only if what follows is whitespace or the
        // end of the text (not the `5` in `3.5`, or a letter in a web address).
        if text[end..].chars().next().is_none_or(char::is_whitespace) {
            last_end = Some(end);
        }
    }

    // Step 4: found one, so cut there.
    if let Some(end) = last_end {
        return text[..end].trim_end().to_string();
    }

    // Step 5: no sentence ends within the limit. Cut at the last space (or
    // hard, if there is none) and add `...` so it is clear it was shortened.
    let cut_at = candidate
        .rfind(char::is_whitespace)
        .unwrap_or(candidate.len());
    format!("{}...", candidate[..cut_at].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tags are removed and the words between them are kept, including
    /// uppercase tags and tags with attributes.
    #[test]
    fn strips_tags_and_keeps_words() {
        assert_eq!(html_to_text("<p>Hello <b>world</b></p>"), "Hello world");
        assert_eq!(
            html_to_text(r#"See <A HREF="https://x.test/a">the link</A> now"#),
            "See the link now"
        );
    }

    /// Paragraphs are one blank line apart, line breaks become new lines, and
    /// list items become `- ` lines.
    #[test]
    fn block_tags_become_line_breaks() {
        assert_eq!(html_to_text("<p>One</p><p>Two</p>"), "One\n\nTwo");
        assert_eq!(html_to_text("a<br>b<br/>c"), "a\nb\nc");
        assert_eq!(html_to_text("<ul><li>x</li><li>y</li></ul>"), "- x\n- y");
    }

    /// Script and style contents and comments never reach the text.
    #[test]
    fn scripts_styles_and_comments_are_dropped() {
        assert_eq!(
            html_to_text("a<script>alert('x')</script>b<style>p{color:red}</style>c<!-- hi -->d"),
            "abcd"
        );
        // An unclosed script swallows the rest rather than leaking code.
        assert_eq!(html_to_text("ok<script>never closed"), "ok");
    }

    /// Named and numeric entities are decoded; unknown ones are left alone;
    /// and decoded markup is shown as text, not treated as a tag.
    #[test]
    fn entities_are_decoded() {
        assert_eq!(
            html_to_text("Tom &amp; Jerry &lt;3 &#8217; &#x2019; &nbsp;x &bogus; end"),
            "Tom & Jerry <3 \u{2019} \u{2019} x &bogus; end"
        );
        assert_eq!(html_to_text("&lt;b&gt;bold&lt;/b&gt;"), "<b>bold</b>");
        // A bare ampersand with no `;` stays as it is.
        assert_eq!(html_to_text("R&D team"), "R&D team");
    }

    /// A `<` that is not a tag is kept, and so is a tag that never closes.
    #[test]
    fn stray_angle_brackets_are_kept() {
        assert_eq!(html_to_text("a < b and c > d"), "a < b and c > d");
        assert_eq!(html_to_text("abc <b"), "abc <b");
    }

    /// Spaces and blank lines are tidied; leading and trailing space is gone.
    #[test]
    fn whitespace_is_tidied() {
        assert_eq!(tidy_whitespace("  a   b \n\n\n\n c  \n"), "a b\n\nc");
        assert_eq!(html_to_text("   "), "");
    }

    /// Text that fits is returned unchanged.
    #[test]
    fn short_text_is_not_changed() {
        assert_eq!(
            limit_to_sentences("Short one. Two.", 100),
            "Short one. Two."
        );
    }

    /// Too-long text is cut after the last complete sentence that fits, never
    /// in the middle of one.
    #[test]
    fn long_text_is_cut_at_a_sentence_end() {
        let text = "First sentence. Second sentence. Third sentence is long.";
        // The first 40 characters end inside the third sentence.
        assert_eq!(
            limit_to_sentences(text, 40),
            "First sentence. Second sentence."
        );
    }

    /// A dot inside a number does not count as the end of a sentence.
    #[test]
    fn decimal_points_are_not_sentence_ends() {
        assert_eq!(
            limit_to_sentences("Version 3.5 is out and it is quite long indeed", 20),
            "Version 3.5 is out..."
        );
    }

    /// A closing quote belongs to the sentence it closes.
    #[test]
    fn closing_quotes_stay_with_their_sentence() {
        assert_eq!(
            limit_to_sentences("He said \"Stop.\" Then left and so on and on", 20),
            "He said \"Stop.\""
        );
    }

    /// One very long sentence falls back to a word boundary plus `...`, and
    /// never ends in half a word.
    #[test]
    fn a_single_long_sentence_is_cut_at_a_word_with_dots() {
        let text = "word ".repeat(100);
        let cut = limit_to_sentences(&text, 50);
        assert!(cut.ends_with("word..."), "{cut}");
        assert!(cut.chars().count() <= 53);
    }

    /// Accented characters are never split, and the length is counted in
    /// characters, not bytes.
    #[test]
    fn multibyte_text_is_cut_safely() {
        let text = "\u{e9}".repeat(3000);
        let cut = limit_to_sentences(&text, SUMMARY_MAX_CHARS);
        assert_eq!(cut.chars().count(), SUMMARY_MAX_CHARS + 3);
        assert!(cut.ends_with("..."));
    }
}
