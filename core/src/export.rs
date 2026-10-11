//! The Markdown text of the "saved articles and notes" export.
//!
//! This module only *builds the text*. It does not know where the file goes:
//! the terminal app decides that from `export.toml`, and the Android app lets
//! the person pick a place with the phone's own file picker. Keeping the
//! formatting here means both apps write exactly the same file.
//!
//! The file has two sections, in this order:
//!
//! 1. **Saved articles**: articles saved *without* a note, as a short list
//!    (title as a link, source, the day it was saved). Left out when
//!    [`ExportOptions::include_unnoted`] is `false`.
//! 2. **Notes**: articles that have a note, each with its title, a link back
//!    to the article, its dates, and the note in full.
//!
//! Within each section the newest comes first. Building the text never
//! changes the database, so exporting never deletes or alters a note.

// Has clippy flag any undocumented item here, the same as in `theme.rs`.
#![warn(clippy::missing_docs_in_private_items)]

use crate::models::Article;
use chrono::{DateTime, Utc};

/// What to put in the export.
///
/// A struct (rather than a bare `bool` argument) so that more choices can be
/// added later without every caller having to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportOptions {
    /// Whether to list saved articles that have no note. `true` exports
    /// everything that is saved; `false` exports only the notes.
    pub include_unnoted: bool,
}

impl Default for ExportOptions {
    /// Everything saved is exported unless the person turns that off.
    fn default() -> Self {
        ExportOptions {
            include_unnoted: true,
        }
    }
}

/// Whether the article has a note with something in it. A note that is only
/// spaces counts as no note, the same way the app treats it elsewhere.
fn has_note(article: &Article) -> bool {
    article
        .note
        .as_deref()
        .is_some_and(|note| !note.trim().is_empty())
}

/// The day the article was saved, as `2026-08-12`. Articles saved before the
/// app tracked save times fall back to their publish day, so the date is
/// never missing.
fn saved_day(article: &Article) -> String {
    article
        .saved_at
        .unwrap_or(article.timestamp)
        .format("%Y-%m-%d")
        .to_string()
}

/// The moment used to put articles newest-first: when the note was last
/// written if there is one, otherwise when the article was saved, otherwise
/// when it was published. `Option::or` picks the first of two options that
/// holds a value.
fn sort_moment(article: &Article) -> DateTime<Utc> {
    article
        .noted_at
        .or(article.saved_at)
        .unwrap_or(article.timestamp)
}

/// Makes `text` safe to use as the visible text of a Markdown link and keeps
/// it on one line. A title containing `[` or `]` would otherwise end the link
/// early, and a line break would split the list item in two.
fn link_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            // A backslash in front of a character tells Markdown to show it
            // as itself instead of treating it as syntax.
            '[' | ']' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            // Line breaks and tabs become spaces.
            '\n' | '\r' | '\t' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// Makes `url` safe inside the `(...)` part of a Markdown link: a closing
/// bracket or a space would end it early, so those are written in their
/// percent-encoded form, which web addresses use for exactly this.
fn link_target(url: &str) -> String {
    url.replace(' ', "%20").replace(')', "%29")
}

/// One saved article without a note, as a list item:
/// `- [Title](https://...) — Source, saved 2026-08-12`.
fn unnoted_line(article: &Article) -> String {
    format!(
        "- [{}]({}) — {}, saved {}\n",
        link_text(&article.title),
        link_target(&article.url),
        link_text(&article.source),
        saved_day(article)
    )
}

/// Formats one article that has a note as the block shown in the "Notes"
/// section:
/// ```markdown
/// ### <title>
/// Source: [<source name>](<url>) — saved <day>, noted <day>
///
/// > <note>
/// ```
/// The "noted" day is only shown when it differs from the saved day, because
/// a note written when the article was saved (the usual case) would otherwise
/// repeat the same date twice. Every line of a multi-line note starts with
/// `> `, since Markdown only keeps a quote going across line breaks when each
/// line carries its own marker.
///
/// An article with no note gets just the heading and the source line, with
/// no empty quote.
pub fn format_article_markdown(article: &Article) -> String {
    // Step 1: the source line, with the dates.
    let saved = saved_day(article);
    let mut header = format!(
        "Source: [{}]({}) — saved {saved}",
        link_text(&article.source),
        link_target(&article.url)
    );
    // `filter` keeps the "noted" day only when it differs from the saved day.
    if let Some(noted) = article
        .noted_at
        .map(|moment| moment.format("%Y-%m-%d").to_string())
        .filter(|day| *day != saved)
    {
        header.push_str(&format!(", noted {noted}"));
    }

    // Step 2: the heading and source line. The title is kept on one line.
    let mut out = format!("### {}\n{header}\n", link_text(&article.title));

    // Step 3: the note, quoted line by line, when there is one.
    if has_note(article) {
        let note = article.note.as_deref().unwrap_or_default();
        // `lines()` splits the note into its lines, `map` puts "> " in front
        // of each one, and `join` glues them back together with line breaks.
        let quoted: Vec<String> = note.lines().map(|line| format!("> {line}")).collect();
        out.push('\n');
        out.push_str(&quoted.join("\n"));
        out.push('\n');
    }
    out
}

/// Builds the whole export file from every saved article.
///
/// `articles` is the complete current list of saved articles (from
/// `Storage::saved_articles`). The file is rebuilt from scratch each time
/// rather than added to, so it can never disagree with what is saved right
/// now: an article un-saved or re-noted since the last export simply looks
/// different next time.
///
/// Returns an empty string when there is nothing to export (nothing saved, or
/// `include_unnoted` is off and nothing has a note), so the caller can write
/// an empty file, or tell the person there is nothing to save, as it prefers.
pub fn render_saved_markdown(articles: &[Article], options: &ExportOptions) -> String {
    // Step 1: split the articles into the two groups. `partition` walks the
    // list once and returns two lists: those where the test is true, and the
    // rest. `iter()` borrows each article instead of moving it, so the
    // originals stay with the caller.
    let (mut noted, mut unnoted): (Vec<&Article>, Vec<&Article>) =
        articles.iter().partition(|article| has_note(article));

    // Step 2: newest first in each group. `sort_by_key` with `Reverse` sorts
    // from the biggest value (the latest moment) down. The sort is "stable":
    // articles with the same moment keep the order they came in.
    noted.sort_by_key(|article| std::cmp::Reverse(sort_moment(article)));
    unnoted.sort_by_key(|article| std::cmp::Reverse(sort_moment(article)));

    // Step 3: with the toggle off, the un-noted group is simply not written.
    if !options.include_unnoted {
        unnoted.clear();
    }
    if noted.is_empty() && unnoted.is_empty() {
        return String::new();
    }

    // Step 4: write the title, then each section that has anything in it.
    let mut out = String::from("# Saved articles and notes\n");
    if !unnoted.is_empty() {
        out.push_str(&format!("\n## Saved articles ({})\n\n", unnoted.len()));
        for article in &unnoted {
            out.push_str(&unnoted_line(article));
        }
    }
    if !noted.is_empty() {
        out.push_str(&format!("\n## Notes ({})\n", noted.len()));
        // A `---` line (a "thematic break") separates one note from the next.
        for (position, article) in noted.iter().enumerate() {
            if position > 0 {
                out.push_str("\n---\n");
            }
            out.push('\n');
            out.push_str(&format_article_markdown(article));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// A saved article with a note, saved on 2026-08-12 and noted on
    /// 2026-08-14, published on 2026-08-10.
    fn noted_article() -> Article {
        let mut article = Article::new(
            "Btrfs send/receive got noticeably faster in 6.18".to_string(),
            "https://reddit.com/r/linux/comments/example".to_string(),
            "r/linux".to_string(),
            "kernel".to_string(),
            Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap(),
        );
        article.saved = true;
        article.saved_at = Some(Utc.with_ymd_and_hms(2026, 8, 12, 9, 0, 0).unwrap());
        article.note = Some("worth trying on the homelab NAS".to_string());
        article.noted_at = Some(Utc.with_ymd_and_hms(2026, 8, 14, 20, 0, 0).unwrap());
        article
    }

    /// A saved article with no note, saved on the given day of August 2026.
    fn plain_article(title: &str, day: u32) -> Article {
        let mut article = noted_article();
        article.title = title.to_string();
        article.note = None;
        article.noted_at = None;
        article.saved_at = Some(Utc.with_ymd_and_hms(2026, 8, day, 9, 0, 0).unwrap());
        article
    }

    /// The block for one noted article has the documented shape: a heading,
    /// the source line with both dates, and the note as a quote.
    #[test]
    fn formats_the_documented_block() {
        assert_eq!(
            format_article_markdown(&noted_article()),
            "### Btrfs send/receive got noticeably faster in 6.18\n\
             Source: [r/linux](https://reddit.com/r/linux/comments/example) — saved 2026-08-12, noted 2026-08-14\n\
             \n\
             > worth trying on the homelab NAS\n"
        );
    }

    /// A note written on the day of saving does not repeat the date.
    #[test]
    fn omits_the_noted_day_when_it_matches() {
        let mut article = noted_article();
        article.noted_at = article.saved_at;
        let text = format_article_markdown(&article);
        assert!(text.contains("saved 2026-08-12"));
        assert!(!text.contains("noted"));
    }

    /// An article saved before save times were tracked uses its publish day.
    #[test]
    fn falls_back_to_the_publish_day() {
        let mut article = noted_article();
        article.saved_at = None;
        assert!(format_article_markdown(&article).contains("saved 2026-08-10"));
    }

    /// Every line of a multi-line note is quoted, and a note of only spaces
    /// is treated as no note (no empty quote).
    #[test]
    fn notes_are_quoted_line_by_line() {
        let mut article = noted_article();
        article.note = Some("first line\nsecond line".to_string());
        assert!(format_article_markdown(&article).contains("> first line\n> second line"));

        article.note = Some("   \n ".to_string());
        assert!(!format_article_markdown(&article).contains('>'));
    }

    /// Saved articles without a note come first, as a list, and the notes
    /// follow under their own heading with counts in both headings.
    #[test]
    fn plain_saved_articles_come_first_then_the_notes() {
        let articles = vec![
            noted_article(),
            plain_article("Plain one", 11),
            plain_article("Plain two", 13),
        ];
        let text = render_saved_markdown(&articles, &ExportOptions::default());

        let saved_at = text.find("## Saved articles (2)").unwrap();
        let notes_at = text.find("## Notes (1)").unwrap();
        assert!(saved_at < notes_at, "saved articles come before notes");
        // Newest first: day 13 is listed before day 11.
        assert!(text.find("Plain two").unwrap() < text.find("Plain one").unwrap());
        assert!(text.contains(
            "- [Plain two](https://reddit.com/r/linux/comments/example) — r/linux, saved 2026-08-13"
        ));
        // The note appears in full under the Notes heading.
        assert!(text[notes_at..].contains("> worth trying on the homelab NAS"));
    }

    /// With the toggle off only the notes are written.
    #[test]
    fn turning_the_toggle_off_leaves_only_notes() {
        let articles = vec![noted_article(), plain_article("Plain one", 11)];
        let text = render_saved_markdown(
            &articles,
            &ExportOptions {
                include_unnoted: false,
            },
        );
        assert!(text.contains("## Notes (1)"));
        assert!(!text.contains("Saved articles ("));
        assert!(!text.contains("Plain one"));
    }

    /// Several notes are separated by a `---` line and listed newest first
    /// (by when the note was written).
    #[test]
    fn notes_are_separated_and_newest_first() {
        let mut older = noted_article();
        older.title = "Older".to_string();
        older.noted_at = Some(Utc.with_ymd_and_hms(2026, 8, 13, 1, 0, 0).unwrap());
        let mut newer = noted_article();
        newer.title = "Newer".to_string();
        newer.noted_at = Some(Utc.with_ymd_and_hms(2026, 8, 20, 1, 0, 0).unwrap());

        let text = render_saved_markdown(&[older, newer], &ExportOptions::default());
        assert!(text.find("### Newer").unwrap() < text.find("### Older").unwrap());
        assert_eq!(text.matches("\n---\n").count(), 1);
    }

    /// Nothing to export gives an empty string, whether nothing is saved or
    /// the toggle hides every article that has no note.
    #[test]
    fn nothing_to_export_is_empty() {
        assert_eq!(
            render_saved_markdown(&[], &ExportOptions::default()),
            String::new()
        );
        let only_plain = vec![plain_article("Plain", 11)];
        assert_eq!(
            render_saved_markdown(
                &only_plain,
                &ExportOptions {
                    include_unnoted: false
                }
            ),
            String::new()
        );
    }

    /// Brackets and line breaks in a title cannot break the link, and a
    /// closing bracket or space in an address is encoded.
    #[test]
    fn awkward_titles_and_addresses_are_made_safe() {
        let mut article = plain_article("A [tricky]\ntitle", 11);
        article.url = "https://example.com/a b(c)".to_string();
        let text = render_saved_markdown(&[article], &ExportOptions::default());
        assert!(text.contains("- [A \\[tricky\\] title](https://example.com/a%20b(c%29)"));
    }

    /// Exporting does not change the articles it is given, so a note can
    /// never be lost by exporting.
    #[test]
    fn rendering_leaves_the_articles_untouched() {
        let articles = vec![noted_article(), plain_article("Plain", 11)];
        let before = articles.clone();
        let _ = render_saved_markdown(&articles, &ExportOptions::default());
        assert_eq!(articles.len(), before.len());
        assert_eq!(articles[0].note, before[0].note);
        assert_eq!(articles[1].title, before[1].title);
    }
}
