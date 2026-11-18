//! The terminal side of **Help**: the `?` popup that lists every key.
//!
//! The sidebar used to keep a permanent "Keys" box on screen. That box used
//! about twenty rows that the feed list needs, so the full reference now
//! lives here instead, one keypress away, while the footer shows only the
//! keys used most often.
//!
//! Like Stats, it is a popup screen drawn over the article area. Its only
//! state is how far it is scrolled (so it still works in a short terminal),
//! which lives inside `Mode::Help` and is handed in and back by value.

#![warn(clippy::missing_docs_in_private_items)]

use super::{Mode, centered_rect};
use crate::theme::Theme;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// The footer hint line while the Help popup is open.
pub(super) const HINTS: &str = "j/k scroll · Esc or ? close";

/// One group of keys in the popup: a heading and its `(key, action)` pairs.
/// `&'static` means the text is baked into the program and lives for its
/// whole run, so no copying or cleanup is needed.
type Section = (&'static str, &'static [(&'static str, &'static str)]);

/// Every key the app understands, grouped the way you use them. This is the
/// one place the full list is kept, so the popup can never disagree with
/// itself. (The footer's short list is separate, see `KEYBIND_HINTS`.)
const SECTIONS: [Section; 5] = [
    (
        "Moving around",
        &[
            ("j/k", "move in the article list"),
            ("↑/↓", "move in the sidebar"),
            ("Tab", "jump to the next category"),
            ("→/l", "open a category (show its feeds)"),
            ("←/h", "close a category"),
            ("Enter", "open the article"),
            ("Esc", "go back"),
            ("q", "quit"),
        ],
    ),
    (
        "Articles",
        &[
            ("s", "save"),
            ("u", "un-save"),
            ("x", "close (skip)"),
            ("n", "note"),
            ("p", "paste a link"),
            ("R", "add to read later"),
        ],
    ),
    (
        "Views",
        &[
            ("S", "saved articles"),
            ("L", "read later"),
            ("y", "history"),
            ("/", "search"),
            ("g", "stats"),
        ],
    ),
    (
        "Feeds",
        &[
            ("r", "refresh this category or feed"),
            ("N", "refresh everything"),
            ("a", "add a feed"),
            ("c", "sync (optional)"),
        ],
    ),
    (
        "Look and files",
        &[
            ("i", "icons: Nerd / Unicode / ASCII"),
            ("t", "theme"),
            ("E", "export saved articles (in the S view)"),
        ],
    ),
];

/// The extra row for `f`, listed only when there is more than one feed list
/// (with one list the key has nothing to pick, so it is left out).
const FEED_LIST_ROW: (&str, &str) = ("f", "switch feed list");

/// Builds every line of the popup: for each section a heading, then one
/// `key  action` row per entry, then a blank line. `show_list_key` adds the
/// `f` row to the "Feeds" section.
///
/// Returns owned `Line`s (they borrow nothing from `theme`; styles are copied
/// in) so the caller can count them for scrolling and then hand them on.
fn build_lines(theme: &Theme, show_list_key: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (heading, entries) in SECTIONS {
        // The heading, bold and in the accent color.
        lines.push(Line::from(Span::styled(
            heading,
            Style::new()
                .fg(theme.accent_unread)
                .add_modifier(Modifier::BOLD),
        )));

        // `chain` glues the optional `f` row onto the end of "Feeds" only.
        // `.then_some` is `Some(row)` when the test is true, else `None`.
        let extra = (heading == "Feeds" && show_list_key).then_some(FEED_LIST_ROW);
        for (key, action) in entries.iter().copied().chain(extra) {
            lines.push(Line::from(vec![
                // `{key:<6}` pads the key to 6 cells so actions line up.
                Span::styled(
                    format!("  {key:<6}"),
                    Style::new()
                        .fg(theme.accent_selected)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(action, Style::new().fg(theme.text_primary)),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines
}

/// How many lines the popup has, used to stop scrolling past the end.
fn line_count(show_list_key: bool) -> usize {
    // Counted by arithmetic (no theme needed): each section is one heading,
    // its rows, the optional `f` row, and one blank line. A unit test checks
    // this against `build_lines`.
    SECTIONS
        .iter()
        .map(|(heading, entries)| {
            let extra = usize::from(*heading == "Feeds" && show_list_key);
            1 + entries.len() + extra + 1
        })
        .sum()
}

/// Draws the popup over `area`, scrolled down by `scroll` lines.
///
/// `Clear` first wipes whatever is underneath, otherwise the article list
/// would show through the gaps in the text.
pub(super) fn render(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    scroll: u16,
    show_list_key: bool,
) {
    // Tall enough for everything when the terminal allows (`centered_rect`
    // shrinks it to fit otherwise), with two rows for the border.
    let height = line_count(show_list_key) as u16 + 2;
    let popup = centered_rect(area, 60, height);
    frame.render_widget(Clear, popup);

    let block = Block::new()
        .title(" Help ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    // `scroll((rows, columns))` skips that many lines from the top.
    let paragraph = Paragraph::new(build_lines(theme, show_list_key))
        .block(block)
        .scroll((scroll, 0));
    frame.render_widget(paragraph, popup);
}

/// Handles one keypress while Help is open and returns the next `Mode`.
///
/// `scroll` is taken by value and handed back inside the returned `Mode`
/// (the same ownership shape as `stats.rs`). `visible` is how many text rows
/// the popup can show right now, used to stop scrolling once the last line is
/// on screen.
pub(super) fn handle_key(key: KeyCode, scroll: u16, show_list_key: bool, visible: u16) -> Mode {
    // The furthest the text can be pushed up: total lines minus what fits.
    // `saturating_sub` stops at 0 instead of going below zero (`u16` has no
    // negative numbers).
    let max_scroll = (line_count(show_list_key) as u16).saturating_sub(visible);
    match key {
        // Close.
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => Mode::Normal,
        // Scroll one line, never past either end.
        KeyCode::Down | KeyCode::Char('j') => Mode::Help {
            scroll: (scroll + 1).min(max_scroll),
        },
        KeyCode::Up | KeyCode::Char('k') => Mode::Help {
            scroll: scroll.saturating_sub(1),
        },
        // Anything else leaves the popup as it is.
        _ => Mode::Help { scroll },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hand-counted `line_count` must match the lines actually built,
    /// with and without the optional `f` row, or scrolling would stop short.
    #[test]
    fn line_count_matches_the_built_lines() {
        let theme = crate::theme::ThemeSettings::default().resolve();
        for show in [false, true] {
            assert_eq!(build_lines(&theme, show).len(), line_count(show));
        }
    }

    /// `?` and `Esc` both close the popup; `j` scrolls but never past the end.
    #[test]
    fn keys_close_and_scroll_within_bounds() {
        assert!(matches!(
            handle_key(KeyCode::Char('?'), 0, false, 10),
            Mode::Normal
        ));
        assert!(matches!(
            handle_key(KeyCode::Esc, 3, false, 10),
            Mode::Normal
        ));
        // With everything visible there is nothing to scroll.
        assert!(matches!(
            handle_key(KeyCode::Char('j'), 0, false, 200),
            Mode::Help { scroll: 0 }
        ));
        // `k` at the top stays at the top.
        assert!(matches!(
            handle_key(KeyCode::Char('k'), 0, false, 10),
            Mode::Help { scroll: 0 }
        ));
    }
}
