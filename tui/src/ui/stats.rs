//! The terminal side of **Stats**: the `g` screen with summary cards, a chart
//! of reads per week, and the topics and sources you read most.
//!
//! Every number comes from `tuxwire_core::stats::compute_stats`, which the
//! Android app uses too, so the two never disagree. This file only works out
//! two inputs the core crate must not look up for itself (the time zone and the
//! total number of feeds), and draws the answer.
//!
//! Like Read Later, it is a popup screen, not a new view of the article list. It
//! shows a snapshot taken when it opens; closing and reopening it refreshes the
//! numbers. Its state is the whole [`Stats`] value, handed to the handler by
//! value and handed back in the returned `Mode` (the same ownership shape as
//! `read_later.rs`).

#![warn(clippy::missing_docs_in_private_items)]

use super::{Mode, centered_rect};
use crate::feedlists::FeedLists;
use crate::fetchers;
use crate::theme::Theme;
use chrono::{Local, Offset, Utc};
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Bar, BarChart, BarGroup, Block, Borders, Clear, Paragraph};
use std::collections::HashSet;
use tuxwire_core::stats::{NameCount, Stats, compute_stats};
use tuxwire_core::storage::Storage;

/// The footer hint line while the Stats screen is open.
pub(super) const HINTS: &str = "Esc close · g close";

/// How many feeds there are across **all** feed lists, each address counted
/// once even if two lists share it. A list file that cannot be read is skipped:
/// the number is then a little low rather than the whole screen failing.
///
/// `&FeedLists` is borrowed because this only reads the list of lists.
fn total_feeds(lists: &FeedLists) -> u32 {
    // A `HashSet` keeps each address once, however many lists contain it.
    let mut addresses: HashSet<String> = HashSet::new();
    for list in lists.lists() {
        // `if let Ok(..)` ignores a list that fails to load.
        if let Ok(sources) = fetchers::configured_sources_at(&list.path) {
            for source in sources {
                addresses.insert(source.url().to_string());
            }
        }
    }
    addresses.len() as u32
}

/// Opens the Stats screen: works out the numbers now and returns the mode that
/// shows them.
///
/// The person's time zone is read here (the core crate never reads the
/// environment): `Local::now().offset()` is the clock's current distance from
/// UTC, and `local_minus_utc` gives it in seconds.
pub(super) fn open(storage: &Storage, lists: &FeedLists) -> anyhow::Result<Mode> {
    let offset_secs = Local::now().offset().fix().local_minus_utc();
    let stats = compute_stats(storage, Utc::now(), offset_secs, total_feeds(lists))?;
    Ok(Mode::Stats {
        stats: Box::new(stats),
    })
}

/// Handles a key while the Stats screen is open: `Esc`, `q` or `g` close it;
/// anything else leaves it as it is. Takes the state by value and returns the
/// next mode, like the Read Later handlers.
pub(super) fn handle_key(key: KeyCode, stats: Box<Stats>) -> Mode {
    match key {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('g') => Mode::Normal,
        _ => Mode::Stats { stats },
    }
}

/// One summary card: its title, the big number, and a small line under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Card {
    /// The title in the card's border.
    pub title: &'static str,
    /// The big number or short phrase.
    pub big: String,
    /// The small line under it.
    pub small: String,
}

/// The six summary cards, in reading order (two rows of three). Pure text, so
/// it can be tested without drawing anything.
pub(super) fn cards(stats: &Stats) -> [Card; 6] {
    // "1 day" / "5 days".
    let days = |n: u32| format!("{n} {}", if n == 1 { "day" } else { "days" });
    [
        Card {
            title: "Read",
            big: stats.read_total.to_string(),
            small: format!(
                "week {} · month {}",
                stats.read_this_week, stats.read_this_month
            ),
        },
        Card {
            title: "Unread",
            big: stats.unread.to_string(),
            small: "on this device".to_string(),
        },
        Card {
            title: "Feeds",
            big: stats.feeds.to_string(),
            small: "across all lists".to_string(),
        },
        Card {
            title: "Notes",
            big: stats.notes.to_string(),
            small: "articles with a note".to_string(),
        },
        Card {
            title: "Saved",
            big: stats.saved.to_string(),
            small: "kept for good".to_string(),
        },
        Card {
            title: "Streak",
            big: days(stats.streak_current),
            small: format!("best {}", days(stats.streak_best)),
        },
    ]
}

/// Draws the Stats screen over `area`.
///
/// Layout, top to bottom: two rows of three cards, the weekly chart, then the
/// top topics and top sources side by side. Colors come from the theme only.
pub(super) fn render(frame: &mut Frame, theme: &Theme, area: Rect, stats: &Stats) {
    // Step 1: a big centered panel that blanks what is behind it (`Clear`).
    let panel = centered_rect(area, 96, area.height);
    frame.render_widget(Clear, panel);
    let outer = Block::new()
        .title(" Stats ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));
    let inner = outer.inner(panel);
    frame.render_widget(outer, panel);

    // Step 2: split the inside into the card rows, the chart and the lists.
    let [cards_top, cards_bottom, chart_area, lists_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Length(4),
            Constraint::Min(7),
            Constraint::Length(8),
        ])
        .areas(inner);

    // Step 3: the cards, three across in each row.
    let all = cards(stats);
    for (row_area, row_cards) in [(cards_top, &all[0..3]), (cards_bottom, &all[3..6])] {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Ratio(1, 3); 3])
            .split(row_area);
        for (column, card) in columns.iter().zip(row_cards) {
            render_card(frame, theme, *column, card);
        }
    }

    // Step 4: the weekly chart and the two top lists.
    render_chart(frame, theme, chart_area, stats);
    let [topics_area, sources_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 2), Constraint::Ratio(1, 2)])
        .areas(lists_area);
    render_top(
        frame,
        theme,
        topics_area,
        "Top topics (read)",
        &stats.top_topics,
    );
    render_top(
        frame,
        theme,
        sources_area,
        "Top sources (read)",
        &stats.top_sources,
    );
}

/// One card: a bordered box with the big number (in the unread accent, bold)
/// and the small line (muted) under it.
fn render_card(frame: &mut Frame, theme: &Theme, area: Rect, card: &Card) {
    let block = Block::new()
        .title(format!(" {} ", card.title))
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme.panel_border));
    let lines = vec![
        Line::from(Span::styled(
            card.big.clone(),
            Style::new()
                .fg(theme.accent_unread)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            card.small.clone(),
            Style::new().fg(theme.text_muted),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// The reads-per-week bar chart, oldest week on the left. Each bar is labelled
/// with its week's Monday as `MM-DD`. The title says where the history starts,
/// or that no times have been recorded yet.
fn render_chart(frame: &mut Frame, theme: &Theme, area: Rect, stats: &Stats) {
    let title = match &stats.tracking_since {
        Some(date) => format!(" Reads per week (since {date}) "),
        None => " Reads per week (nothing recorded yet) ".to_string(),
    };
    let block = Block::new()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme.panel_border));

    // Turn each week into a `Bar`: a value, a short label, and the value shown
    // above the bar. The label is the part of "YYYY-MM-DD" after the year.
    let bars: Vec<Bar> = stats
        .weeks
        .iter()
        .map(|week| {
            let label = week.week_start.get(5..).unwrap_or(&week.week_start);
            Bar::default()
                .value(u64::from(week.reads))
                .label(Line::from(label.to_string()))
        })
        .collect();

    let chart = BarChart::default()
        .block(block)
        .data(BarGroup::default().bars(&bars))
        .bar_width(5)
        .bar_gap(1)
        .bar_style(Style::new().fg(theme.accent_unread))
        .value_style(
            Style::new()
                .fg(theme.background)
                .bg(theme.accent_unread)
                .add_modifier(Modifier::BOLD),
        )
        .label_style(Style::new().fg(theme.text_muted));
    frame.render_widget(chart, area);
}

/// A ranked list of names with their read counts, or a short hint when nothing
/// has been read yet.
fn render_top(frame: &mut Frame, theme: &Theme, area: Rect, title: &str, rows: &[NameCount]) {
    let block = Block::new()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(Style::new().fg(theme.panel_border));
    let lines: Vec<Line> = if rows.is_empty() {
        vec![Line::from(Span::styled(
            "Nothing read yet.",
            Style::new().fg(theme.text_muted),
        ))]
    } else {
        rows.iter()
            .enumerate()
            .map(|(index, row)| {
                Line::from(vec![
                    Span::styled(
                        format!("{}. ", index + 1),
                        Style::new().fg(theme.text_muted),
                    ),
                    Span::raw(row.name.clone()),
                    Span::styled(
                        format!("  {}", row.reads),
                        Style::new().fg(theme.accent_unread),
                    ),
                ])
            })
            .collect()
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuxwire_core::stats::WeekCount;

    /// A small, fixed `Stats` for the tests below.
    fn sample() -> Stats {
        Stats {
            read_total: 120,
            read_this_week: 7,
            read_this_month: 31,
            unread: 42,
            feeds: 37,
            notes: 3,
            saved: 9,
            streak_current: 1,
            streak_best: 5,
            weeks: vec![
                WeekCount {
                    week_start: "2026-10-12".into(),
                    reads: 4,
                },
                WeekCount {
                    week_start: "2026-10-19".into(),
                    reads: 7,
                },
            ],
            tracking_since: Some("2026-10-12".into()),
            top_topics: vec![NameCount {
                name: "linux".into(),
                reads: 60,
            }],
            top_sources: vec![NameCount {
                name: "Arch Linux News".into(),
                reads: 12,
            }],
        }
    }

    /// The cards show the numbers in the order Read, Unread, Feeds, Notes,
    /// Saved, Streak, with "1 day" in the singular.
    #[test]
    fn cards_show_the_numbers() {
        let all = cards(&sample());
        assert_eq!(all[0].big, "120");
        assert_eq!(all[0].small, "week 7 · month 31");
        assert_eq!(all[1].big, "42");
        assert_eq!(all[2].big, "37");
        assert_eq!(all[3].big, "3");
        assert_eq!(all[4].big, "9");
        assert_eq!(all[5].big, "1 day");
        assert_eq!(all[5].small, "best 5 days");
    }

    /// Esc, q and g close the screen; other keys leave it open.
    #[test]
    fn keys_close_or_keep() {
        for key in [KeyCode::Esc, KeyCode::Char('q'), KeyCode::Char('g')] {
            assert!(matches!(handle_key(key, Box::new(sample())), Mode::Normal));
        }
        assert!(matches!(
            handle_key(KeyCode::Char('x'), Box::new(sample())),
            Mode::Stats { .. }
        ));
    }

    /// Drawing into an in-memory terminal puts the card titles and numbers,
    /// the chart title and the top names on screen.
    #[test]
    fn the_screen_draws_its_parts() {
        let backend = ratatui::backend::TestBackend::new(100, 40);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        let theme = crate::theme::ThemeSettings::default().resolve();
        terminal
            .draw(|frame| render(frame, &theme, frame.area(), &sample()))
            .unwrap();
        // Join every cell's symbol into one string to search.
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for expected in [
            "Read",
            "120",
            "Feeds",
            "37",
            "Streak",
            "Reads per week (since 2026-10-12)",
            "10-19",
            "Arch Linux News",
            "linux",
        ] {
            assert!(screen.contains(expected), "missing {expected:?}");
        }
    }
}
