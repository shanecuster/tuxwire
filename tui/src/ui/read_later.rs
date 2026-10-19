//! The terminal side of **Read Later**: the `L` list, the `p` paste-a-link
//! prompt and its "Read later / Save / Cancel" question, and the `x` discard
//! confirmation.
//!
//! The list itself, its rules and its sync live in `tuxwire-core`
//! (`Storage::add_read_later` and friends). This file only draws it and turns
//! key presses into those calls.
//!
//! Read Later is a popup screen (like the `f` and `t` pickers), not a new view
//! of the article list: its items are links, not articles, and keeping it
//! separate leaves the Topic, Saved and History views exactly as they were.
//!
//! The handlers here follow the same shape as `handle_add_source_key` in the
//! parent module: they take the popup's state **by value** (the caller gives it
//! up), and hand back the `Mode` the screen should be in next. That is how
//! ownership works out without fighting the borrow checker: the old state is
//! consumed, the new state is returned.

#![warn(clippy::missing_docs_in_private_items)]

use super::{Mode, Pending, centered_rect, open_in_browser, open_in_w3m, w3m_available};
use crate::icons::{Icon, IconSettings};
use crate::theme::Theme;
use crossterm::event::KeyCode;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use tuxwire_core::fetchers::link_title::fetch_page_title;
use tuxwire_core::storage::{ReadLaterItem, Storage};
use tuxwire_core::sync::feeds::is_syncable_url;

/// The footer hint line while the Read Later list is open.
pub(super) const LIST_HINTS: &str =
    "j/k move · Enter open · s save · x discard · p paste a link · Esc close";

/// The footer hint line while the paste-a-link prompt is open.
pub(super) const PASTE_HINTS: &str = "Enter continue · Esc cancel";

/// The footer hint line while the "what to do with this link" question is open.
pub(super) const CHOICE_HINTS: &str = "l read later · s save · Esc cancel";

/// The most list rows shown at once; a longer list scrolls to keep the
/// highlighted row in view.
const MAX_VISIBLE_ROWS: usize = 18;

/// Turns what was typed or pasted into an address to use: surrounding space is
/// removed, and `https://` is put in front when no scheme was typed (the same
/// courtesy the add-source prompt gives). Fails, with a message fit for the
/// popup, when the result is not an ordinary web address.
///
/// A plain function with no network, so it is easy to test. `&str` is
/// borrowed; the `String` returned is a new value the caller owns.
pub(super) fn normalise_pasted(text: &str) -> Result<String, String> {
    // Step 1: nothing typed yet.
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("paste a web address first".to_string());
    }

    // Step 2: add `https://` when there is no scheme. `contains("://")` is a
    // simple test that is right for every address a person pastes; anything
    // odd (`javascript:alert(1)`, which has no `://`) gets a prefix and then
    // fails the check below, because `https://javascript:...` is not a usable
    // address either.
    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    // Step 3: only ordinary web addresses are accepted.
    if is_syncable_url(&candidate) {
        Ok(candidate)
    } else {
        Err("that is not a web address (it must start with http:// or https://)".to_string())
    }
}

/// The part of an address between the scheme and the first `/`, for showing
/// where a link points. Returns the whole address if it has no scheme.
pub(super) fn host_of(url: &str) -> &str {
    // `split_once("://")` gives the text after the scheme, or `None`.
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    // Everything up to the first `/`, `?` or `#`.
    after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme)
}

/// Loads the list from the database and returns the popup showing it, with row
/// `selected` highlighted (kept within the list).
pub(super) fn open_list(storage: &Storage, selected: usize) -> anyhow::Result<Mode> {
    let items = storage.list_read_later()?;
    // `saturating_sub(1)` is "one less, but never below zero", which a
    // `usize` cannot go.
    let selected = selected.min(items.len().saturating_sub(1));
    Ok(Mode::ReadLater { items, selected })
}

/// Handles one key press while the list is open and returns the mode to go to.
///
/// - `j`/`k`/arrows move the highlight (wrapping).
/// - `Enter` opens the link (in `w3m`, or the browser if there is no `w3m`),
///   marks it read and leaves it on the list.
/// - `s` saves it permanently (it becomes an ordinary saved article) and takes
///   it off the list.
/// - `x` asks before discarding it.
/// - `p` opens the paste-a-link prompt.
/// - `Esc`, `q` or `L` close the list.
///
/// `notice` is the footer's one-line message; handlers set it to report what
/// happened.
pub(super) fn handle_list_key(
    key: KeyCode,
    items: Vec<ReadLaterItem>,
    selected: usize,
    storage: &Storage,
    terminal: &mut ratatui::DefaultTerminal,
    notice: &mut Option<String>,
) -> anyhow::Result<Mode> {
    let count = items.len();
    match key {
        // Close.
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('L') => Ok(Mode::Normal),

        // Move, wrapping at either end. Adding `count - 1` and taking `% count`
        // is "minus one, wrapping" without ever going below zero.
        KeyCode::Up | KeyCode::Char('k') if count > 0 => Ok(Mode::ReadLater {
            items,
            selected: (selected + count - 1) % count,
        }),
        KeyCode::Down | KeyCode::Char('j') if count > 0 => Ok(Mode::ReadLater {
            items,
            selected: (selected + 1) % count,
        }),

        // Open the highlighted link.
        KeyCode::Enter if count > 0 => {
            let url = items[selected].url.clone();
            // Try `w3m` first, as everywhere else; fall back to the browser.
            let opened = if w3m_available() {
                open_in_w3m(terminal, &url)?;
                true
            } else {
                open_in_browser(&url)
            };
            if opened {
                // Mark it read in the database, then reload the list so what
                // is shown is what is stored.
                storage.mark_read_later_read(&url)?;
            } else {
                *notice = Some(
                    "could not open the link -- w3m isn't installed, and no browser \
                     could be launched via $BROWSER or xdg-open"
                        .to_string(),
                );
            }
            open_list(storage, selected)
        }

        // Save permanently: it becomes a saved article (which syncs like any
        // other) and leaves the list.
        KeyCode::Char('s') if count > 0 => {
            let item = &items[selected];
            *notice = Some(match storage.promote_read_later(&item.url) {
                Ok(_) => format!("Saved: {}", item.title),
                Err(err) => format!("Could not save it: {err:#}"),
            });
            open_list(storage, selected)
        }

        // Discard, but only after a yes/no question.
        KeyCode::Char('x') if count > 0 => {
            let item = &items[selected];
            Ok(Mode::Confirm(Pending::DiscardReadLater {
                url: item.url.clone(),
                title: item.title.clone(),
                selected,
            }))
        }

        // Paste a link from inside the list; afterwards come back to the list.
        KeyCode::Char('p') => Ok(Mode::PasteLink {
            text: String::new(),
            error: None,
            from_list: true,
        }),

        // Anything else: keep the list as it is.
        _ => Ok(Mode::ReadLater { items, selected }),
    }
}

/// Handles one key press in the paste-a-link prompt and returns the mode to go
/// to.
///
/// Typed (or pasted) characters build `text`; `Backspace` removes one; `Esc`
/// cancels. `Enter` checks the address, looks up the page's title (a
/// courtesy: if that fails the address itself is used) and moves on to the
/// "Read later / Save / Cancel" question. A bad address keeps the prompt open
/// with the reason shown.
///
/// `from_list` remembers whether the prompt was opened from the Read Later
/// list, so cancelling or finishing returns there.
pub(super) fn handle_paste_key(
    key: KeyCode,
    mut text: String,
    from_list: bool,
    storage: &Storage,
) -> anyhow::Result<Mode> {
    match key {
        // Cancel.
        KeyCode::Esc => back(storage, from_list),

        // Edit the text.
        KeyCode::Backspace => {
            text.pop();
            Ok(Mode::PasteLink {
                text,
                error: None,
                from_list,
            })
        }
        KeyCode::Char(c) => {
            text.push(c);
            Ok(Mode::PasteLink {
                text,
                error: None,
                from_list,
            })
        }

        // Check the address, find the title, ask what to do.
        KeyCode::Enter => match normalise_pasted(&text) {
            Err(message) => Ok(Mode::PasteLink {
                text,
                error: Some(message),
                from_list,
            }),
            Ok(url) => {
                // `fetch_page_title` is `async`, but this code is a plain
                // (blocking) function. `block_in_place` tells tokio "this
                // thread is about to wait", and `block_on` runs the async call
                // to completion on the current runtime -- the same trick the
                // add-source prompt uses for feed discovery.
                let found = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(fetch_page_title(&url))
                });
                // `ok()` turns an error into `None`; `flatten()` merges the
                // two layers of `Option`; `unwrap_or_default()` gives an empty
                // title when there is none, which core turns into the address.
                let title = found.ok().flatten().unwrap_or_default();
                Ok(Mode::LinkChoice {
                    url,
                    title,
                    from_list,
                })
            }
        },

        // Anything else: keep the prompt as it is.
        _ => Ok(Mode::PasteLink {
            text,
            error: None,
            from_list,
        }),
    }
}

/// Handles one key press in the "Read later / Save / Cancel" question and
/// returns the mode to go to. Any other key leaves the question open.
pub(super) fn handle_choice_key(
    key: KeyCode,
    url: String,
    title: String,
    from_list: bool,
    storage: &Storage,
    notice: &mut Option<String>,
) -> anyhow::Result<Mode> {
    match key {
        // Put it on the Read Later list.
        KeyCode::Char('l') => {
            *notice = Some(match storage.add_read_later(&title, &url) {
                Ok(true) => "Added to Read Later".to_string(),
                Ok(false) => "Already in Read Later".to_string(),
                Err(err) => format!("Read Later: {err:#}"),
            });
            back(storage, from_list)
        }

        // Save it permanently.
        KeyCode::Char('s') => {
            *notice = Some(match storage.save_link(&title, &url) {
                Ok(_) => "Saved the link".to_string(),
                Err(err) => format!("Could not save it: {err:#}"),
            });
            back(storage, from_list)
        }

        // Do neither.
        KeyCode::Esc => {
            *notice = Some("Nothing added".to_string());
            back(storage, from_list)
        }

        // Anything else: keep asking.
        _ => Ok(Mode::LinkChoice {
            url,
            title,
            from_list,
        }),
    }
}

/// Carries out the answer to the discard question and returns the list again
/// (reloaded, with the highlight kept near where it was). Only `confirmed`
/// (a plain `y`) discards; anything else keeps the link.
pub(super) fn resolve_discard(
    confirmed: bool,
    url: &str,
    title: &str,
    selected: usize,
    storage: &Storage,
    notice: &mut Option<String>,
) -> anyhow::Result<Mode> {
    if confirmed {
        storage.discard_read_later(url)?;
        *notice = Some(format!("Discarded: {title}"));
    } else {
        *notice = Some("Kept on the Read Later list".to_string());
    }
    open_list(storage, selected)
}

/// Cuts `text` to at most `max_chars` characters, ending with `ellipsis` (from
/// the icon settings, so it follows the Nerd/Unicode/ASCII choice) when
/// something was cut. Counts characters, not bytes, so accented letters and
/// other multi-byte text are cut safely.
fn fit_text(text: &str, max_chars: usize, ellipsis: &str) -> String {
    // Step 1: short enough already.
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    // Step 2: keep room for the ellipsis, then take that many characters.
    // `take(n)` stops after `n` characters; `collect` gathers them.
    let keep = max_chars.saturating_sub(ellipsis.chars().count());
    let mut cut: String = text.chars().take(keep).collect();
    cut.push_str(ellipsis);
    cut
}

/// Where to go once the paste flow is over: back to the list if it was opened
/// from there, otherwise to the ordinary screen.
fn back(storage: &Storage, from_list: bool) -> anyhow::Result<Mode> {
    if from_list {
        open_list(storage, 0)
    } else {
        Ok(Mode::Normal)
    }
}

/// Draws the Read Later list popup: a header line, one row per link, and a
/// line of help. Each row is `<pointer> <read marker> <title> · <site> ·
/// <date>`. Unread titles use the unread colour and opened ones the read
/// colour, the same as articles everywhere else; the highlighted row uses the
/// selected colour. A long list scrolls so the highlighted row stays visible.
///
/// All glyphs come from `icons` and all colours from `theme`.
pub(super) fn render_list_popup(
    frame: &mut Frame,
    theme: &Theme,
    icons: &IconSettings,
    area: Rect,
    selected: usize,
    items: &[ReadLaterItem],
) {
    let muted = Style::new().fg(theme.text_muted);

    // Step 1: the rows to show. An empty list says how to fill it.
    let mut lines: Vec<Line> = Vec::new();
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            "Nothing here yet. Press p to paste a link, or R on an article.",
            muted,
        )));
    } else {
        // Step 2: pick the window of rows around the highlight. `start` is
        // where the window begins; it keeps the highlighted row about in the
        // middle, never starting before 0 or running past the end.
        let visible = MAX_VISIBLE_ROWS
            .min(usize::from(area.height).saturating_sub(6))
            .max(3);
        let start = selected
            .saturating_sub(visible / 2)
            .min(items.len().saturating_sub(visible));
        let end = (start + visible).min(items.len());

        // Step 3: how much room a title has. The popup is 85% of the width;
        // the border takes two cells; the rest of each row is the pointer,
        // marker, site and date.
        let inner = (usize::from(area.width) * 85 / 100).saturating_sub(2);

        // Step 4: build one line per visible row.
        for (offset, item) in items[start..end].iter().enumerate() {
            let is_selected = start + offset == selected;
            let pointer = if is_selected {
                icons.glyph(Icon::Collapsed)
            } else {
                " "
            };
            // Opened items get the read marker and colour, the rest the
            // unread ones, like everywhere else in the app.
            let (marker, colour) = if item.read {
                (icons.glyph(Icon::Read), theme.accent_read)
            } else {
                (icons.glyph(Icon::Unread), theme.accent_unread)
            };
            let title_style = if is_selected {
                Style::new()
                    .fg(theme.accent_selected)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(colour)
            };
            let meta = format!(
                "  {} · {}",
                host_of(&item.url),
                item.added_at.format("%Y-%m-%d")
            );
            let room = inner.saturating_sub(4 + meta.chars().count()).max(10);
            lines.push(Line::from(vec![
                Span::styled(format!("{pointer} "), title_style),
                Span::styled(format!("{marker} "), Style::new().fg(colour)),
                Span::styled(fit_text(&item.title, room, icons.ellipsis()), title_style),
                Span::styled(meta, muted),
            ]));
        }

        // Step 5: where in the list the window is, when it is not all shown.
        if items.len() > visible {
            lines.push(Line::from(Span::styled(
                format!("{} of {}", selected + 1, items.len()),
                muted,
            )));
        }
    }

    // Step 6: draw the box. `+ 2` for the border, like every popup here;
    // `Clear` blanks what is underneath first.
    let popup_area = centered_rect(area, 85, lines.len() as u16 + 2);
    frame.render_widget(Clear, popup_area);
    let block = Block::new()
        .title(format!(" {} Read Later ", icons.glyph(Icon::Unread)))
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.accent_selected));
    frame.render_widget(Paragraph::new(lines).block(block), popup_area);
}

/// Draws the paste-a-link prompt: one input line (with the cursor after the
/// text) and, when the last attempt failed, the reason beneath it.
pub(super) fn render_paste_popup(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    text: &str,
    error: &Option<String>,
) {
    // One line for the text, one more for an error; `+ 2` for the border.
    let height = if error.is_some() { 4 } else { 3 };
    let popup_area = centered_rect(area, 70, height);
    frame.render_widget(Clear, popup_area);

    let block = Block::new()
        .title(" Paste a link ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.accent_selected));

    let mut lines = vec![Line::from(text)];
    if let Some(message) = error {
        lines.push(Line::from(Span::styled(
            message.as_str(),
            Style::new().fg(theme.accent_breaking),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::new().fg(theme.text_primary))
            .block(block),
        popup_area,
    );

    // The real terminal cursor goes right after the last typed character,
    // inside the border (hence `+ 1` on both axes).
    frame.set_cursor_position((
        popup_area.x + 1 + text.chars().count() as u16,
        popup_area.y + 1,
    ));
}

/// Draws the "what should I do with this link?" question: the title, the
/// address and the three answers.
pub(super) fn render_choice_popup(
    frame: &mut Frame,
    theme: &Theme,
    icons: &IconSettings,
    area: Rect,
    url: &str,
    title: &str,
) {
    let popup_area = centered_rect(area, 70, 7);
    frame.render_widget(Clear, popup_area);

    let block = Block::new()
        .title(" What should tuxwire do with this link? ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.accent_selected));

    // The title falls back to the address when the page had none, so showing
    // it twice would be noise.
    let shown_title = if title.trim().is_empty() { url } else { title };
    let inner = (usize::from(popup_area.width)).saturating_sub(4);
    let key = Style::new()
        .fg(theme.accent_selected)
        .add_modifier(Modifier::BOLD);
    let lines = vec![
        Line::from(Span::styled(
            fit_text(shown_title, inner, icons.ellipsis()),
            Style::new().fg(theme.text_primary),
        )),
        Line::from(Span::styled(
            fit_text(url, inner, icons.ellipsis()),
            Style::new().fg(theme.text_muted),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("l", key),
            Span::raw("  read it later (the Read Later list)"),
        ]),
        Line::from(vec![
            Span::styled("s", key),
            Span::raw("  save it (permanent, like a saved article)"),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), popup_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare address gets `https://`; an address with a scheme is kept.
    #[test]
    fn pasted_addresses_are_normalised() {
        assert_eq!(
            normalise_pasted("  example.com/post ").unwrap(),
            "https://example.com/post"
        );
        assert_eq!(
            normalise_pasted("http://example.com/a").unwrap(),
            "http://example.com/a"
        );
        assert_eq!(
            normalise_pasted("https://Example.com/Path?q=1").unwrap(),
            "https://Example.com/Path?q=1"
        );
    }

    /// Empty input and things that are not web addresses are refused with a
    /// message.
    #[test]
    fn bad_pasted_text_is_refused() {
        assert!(normalise_pasted("   ").is_err());
        assert!(normalise_pasted("javascript:alert(1)").is_err());
        assert!(normalise_pasted("file:///etc/passwd").is_err());
        assert!(normalise_pasted("two words").is_err());
        assert!(normalise_pasted("ftp://example.com/x").is_err());
    }

    /// Long text is cut with the ellipsis, short text is left alone.
    #[test]
    fn long_text_is_cut_with_the_ellipsis() {
        assert_eq!(fit_text("short", 10, "..."), "short");
        assert_eq!(fit_text("exactly10!", 10, "..."), "exactly10!");
        assert_eq!(fit_text("a much longer title", 10, "..."), "a much ...");
        // Multi-byte characters are counted as characters.
        assert_eq!(fit_text("ééééééééééé", 5, "…"), "éééé…");
    }

    /// The site shown for a link is the part between the scheme and the path.
    #[test]
    fn host_is_the_part_before_the_path() {
        assert_eq!(host_of("https://example.com/a/b"), "example.com");
        assert_eq!(host_of("http://example.com?x=1"), "example.com");
        assert_eq!(host_of("https://example.com#top"), "example.com");
        assert_eq!(host_of("example.com/a"), "example.com");
        assert_eq!(host_of(""), "");
    }
}
