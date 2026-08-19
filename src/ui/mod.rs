//! ratatui views/widgets (`docs/ARCHITECTURE.md` § 3. TUI).
//!
//! Fourth milestone: `s` save and `n` note. `j`/`k` move the article-list
//! selection, `Up`/`Down`/`Tab` move the topic-sidebar selection (reloading
//! the article list from `storage` whenever the topic changes), `Enter`
//! opens the selected article's URL in `$BROWSER`, `x` marks the selected
//! article skipped, `r` re-fetches the current topic's sources (via
//! `fetchers::configured_sources`), inserts whatever's new into `storage`,
//! and reloads the article list. `s` now saves the selected article via
//! `Storage::save_article`, and `n` opens a small inline popup (see `Mode`
//! below) for editing that article's note via `Storage::update_note`.
//! `S` saved view and `a` add source are still to come, and none of them
//! run yet -- same for the "opening an article marks it read" behavior
//! ARCHITECTURE.md describes; `Enter` here only opens the link. This
//! module owns the terminal setup/teardown, the draw loop, and the small
//! bit of navigation state that selection requires -- which topic and
//! which article are highlighted, the current topic's article list (since
//! that has to be reloaded from `storage` on every topic change and every
//! `r` refresh, rather than loaded once upfront), and now `Mode`, which
//! tracks whether the note popup is open.

use crate::fetchers::{self, Fetcher};
use crate::models::Article;
use crate::storage::Storage;
use crate::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};

/// Keybind hints for the footer, for the normal (not editing-a-note)
/// state -- see `EDITING_NOTE_HINTS` below for the popup's own hint line.
/// `S` saved view and `a` add source, the rest of ARCHITECTURE.md's v1
/// keybind table, aren't implemented yet, so they don't belong in the
/// footer yet either; showing a hint for a key that does nothing would be
/// worse than showing no hint at all. Kept as one `const` (rather than
/// inlined into `render_footer` below) so there's exactly one place this
/// has to stay in sync with the `Mode::Normal` match arm in
/// `draw_until_quit` below.
const KEYBIND_HINTS: &str =
    "j/k move · ↑/↓/Tab topic · Enter open · x skip · s save · n note · r refresh · q quit";

/// The footer hint line shown while the note popup (`Mode::EditingNote`)
/// is open -- every other keybind is suspended while typing a note (see
/// the `Mode::EditingNote` match arm in `draw_until_quit`), so the footer
/// swaps to just these two.
const EDITING_NOTE_HINTS: &str = "Enter save note · Esc cancel";

/// Which "screen" the draw loop is currently in. `Normal` is the ordinary
/// two-pane browsing view every other keybind operates on; `EditingNote`
/// means the `n` popup is open and every keypress instead edits `text`
/// (or saves/cancels it) rather than being interpreted as a navigation
/// keybind -- see the two match arms in `draw_until_quit`.
///
/// This is an `enum` rather than a `bool` flag (`editing_note: bool`) plus
/// a separate `note_text: String` because those two pieces of state are
/// only ever meaningful *together*: there's no valid moment where
/// `editing_note` is true but there's no text buffer, or vice versa. An
/// `enum` where the buffer lives *inside* the `EditingNote` variant makes
/// that invalid combination unrepresentable, instead of relying on both
/// fields happening to be kept in sync by convention.
enum Mode {
    Normal,
    EditingNote { text: String },
}

/// Runs the TUI shell until the user presses `q`.
///
/// Loads `topics()` from `storage` once -- the topic list itself only
/// changes when a new topic first appears in `sources.toml` and gets
/// fetched, which can't happen while the TUI is running (no `r` refresh
/// keybind yet) -- but each topic's article list is loaded lazily, inside
/// `draw_until_quit`, since switching topics has to re-query `storage` for
/// the newly selected topic's articles.
///
/// `storage: &Storage` and `theme: &Theme` are both borrowed rather than
/// owned: this function only ever *reads* through them, and taking
/// ownership would force whoever calls `ui::run` to give up their own
/// `Storage`/`Theme` (or clone them) just to display something once.
pub fn run(storage: &Storage, theme: &Theme) -> anyhow::Result<()> {
    let topics = storage.topics()?;

    // `ratatui::run` (see the doc comment on it in the `ratatui` crate)
    // is the "simplest path" helper: it puts the terminal into raw mode +
    // the alternate screen, hands a `&mut DefaultTerminal` to this
    // closure, and -- critically -- restores the terminal afterwards
    // *no matter how the closure returns*, including on an `Err`. That's
    // exactly the guarantee this function needs: if `draw_until_quit`
    // below hits an error partway through, the user's shell must not be
    // left in raw mode / the alternate screen.
    ratatui::run(|terminal| draw_until_quit(terminal, theme, storage, &topics))
}

/// The actual draw loop: redraw the frame, block for the next terminal
/// event, act on it, and quit on `q` -- otherwise loop forever. Split out
/// from `run` so `run` itself stays focused on "load the data, then hand
/// off to ratatui," rather than mixing that with the loop's control flow.
///
/// Owns the navigation state that didn't exist before this milestone:
/// `topic_index` (which row of the sidebar is selected) and
/// `article_index` (which row of the article list is selected, meaningless
/// when `articles` is empty). `articles` itself is reloaded from `storage`
/// every time `topic_index` changes -- it's *not* one big upfront list, so
/// switching topics always reflects whatever's actually in the database
/// for that topic.
fn draw_until_quit(
    terminal: &mut ratatui::DefaultTerminal,
    theme: &Theme,
    storage: &Storage,
    topics: &[String],
) -> anyhow::Result<()> {
    let mut topic_index: usize = 0;
    let mut article_index: usize = 0;
    let mut articles: Vec<Article> = match topics.first() {
        Some(topic) => storage.articles_by_topic(topic)?,
        None => Vec::new(),
    };
    let mut mode = Mode::Normal;

    loop {
        let selected_topic = topics.get(topic_index).map(String::as_str);
        let selected_article = if articles.is_empty() {
            None
        } else {
            Some(article_index)
        };

        terminal.draw(|frame| {
            render(
                frame,
                theme,
                topics,
                selected_topic,
                &articles,
                selected_article,
                &mode,
            )
        })?;

        // `event::read()` blocks until the next terminal event -- no
        // polling loop or timer needed, since nothing here animates or
        // refreshes on its own. `KeyEventKind::Press` matters because some
        // terminals (with the right protocol enabled) report *both* a key
        // press and its later release as separate events; without this
        // check, releasing one key after pressing another first could be
        // misread as a second, unrelated keystroke.
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        // While the note popup is open, every keypress belongs to it --
        // typing "q" or "j" into a note must type that character, not quit
        // or move the article selection. Splitting on `mode` first (rather
        // than adding an `if let Mode::EditingNote { .. } = mode` guard to
        // every arm below) is what makes that isolation total rather than
        // something that has to be remembered arm-by-arm.
        let Mode::EditingNote { text } = &mut mode else {
            match key.code {
                KeyCode::Char('q') => return Ok(()),

                // `j`/`k` move the article-list selection, clamped to the
                // current list's bounds (not wrapping) -- the same convention
                // as most line-oriented list UIs (vim's own motions included).
                // `saturating_sub` is what makes `k` at row 0 a no-op instead
                // of underflowing `usize` (which has no negative values, so
                // `0 - 1` would otherwise panic).
                KeyCode::Char('j') if !articles.is_empty() => {
                    article_index = (article_index + 1).min(articles.len() - 1);
                }
                KeyCode::Char('k') if !articles.is_empty() => {
                    article_index = article_index.saturating_sub(1);
                }

                // `Up`/`Down`/`Tab` move the topic-sidebar selection instead,
                // wrapping around at either end (unlike `j`/`k` above) since a
                // sidebar of a handful of topics is small enough that cycling
                // through it is more convenient than getting stuck at an edge.
                // Every topic switch reloads `articles` for the newly selected
                // topic and resets `article_index` back to the top -- carrying
                // over an index from a different topic's list makes no sense,
                // and could even be out of bounds for a shorter one.
                KeyCode::Up if !topics.is_empty() => {
                    topic_index = if topic_index == 0 {
                        topics.len() - 1
                    } else {
                        topic_index - 1
                    };
                    articles = storage.articles_by_topic(&topics[topic_index])?;
                    article_index = 0;
                }
                KeyCode::Down | KeyCode::Tab if !topics.is_empty() => {
                    topic_index = (topic_index + 1) % topics.len();
                    articles = storage.articles_by_topic(&topics[topic_index])?;
                    article_index = 0;
                }

                // `Enter` opens the selected article's URL in `$BROWSER` --
                // see `open_in_browser` below for why this can't fail this
                // loop even if it doesn't work.
                KeyCode::Enter => {
                    if let Some(article) = articles.get(article_index) {
                        open_in_browser(&article.url);
                    }
                }

                // `x` marks the selected article skipped, both on disk and in
                // the in-memory `articles` list -- mutating the list entry
                // directly (rather than reloading the whole topic from
                // `storage`, as the topic-switch arms above do) is enough
                // here, since skipping doesn't change *which* rows belong in
                // the list, only the `skipped` flag `article_item` reads to
                // color this one. This only records the skip itself; deriving
                // keyword(s) from the article to feed into
                // `increment_skip_weight` is skip-*weighting* logic that
                // belongs in `scoring.rs` (not built yet -- see its module doc
                // comment), so that part is deliberately not done here.
                KeyCode::Char('x') => {
                    if let Some(article) = articles.get_mut(article_index) {
                        storage.mark_skipped(article.id)?;
                        article.skipped = true;
                    }
                }

                // `s` saves the selected article via `Storage::save_article` --
                // which, per its own doc comment, sets `saved = true` *and*
                // `read = true` in one `UPDATE` statement. ARCHITECTURE.md is
                // explicit that this pairing ("saving auto-marks as read") is
                // `save_article`'s job, not something to re-implement here;
                // this arm only calls it and mirrors the two flags into the
                // in-memory `article` so the list re-colors immediately without
                // a full reload from `storage`. The existing note (if any) is
                // passed straight through unchanged -- pressing `s` should
                // never silently clear a note `n` already saved.
                KeyCode::Char('s') => {
                    if let Some(article) = articles.get_mut(article_index) {
                        storage.save_article(article.id, article.note.clone())?;
                        article.saved = true;
                        article.read = true;
                    }
                }

                // `n` opens the note popup (switches `mode` to
                // `Mode::EditingNote`), pre-filled with whatever note (if any)
                // the selected article already has -- `Option<String>`'s
                // `.clone().unwrap_or_default()` turns `Some(existing)` into a
                // copy of `existing` and `None` into `String::new()` in one
                // expression, so the popup always starts from a real (possibly
                // empty) `String` rather than having to special-case "no note
                // yet" separately from "editing an existing note". The actual
                // keystrokes that follow are handled by the `Mode::EditingNote`
                // arm below, once this same `loop` iterates back around and
                // reads the next key.
                KeyCode::Char('n') => {
                    if let Some(article) = articles.get(article_index) {
                        mode = Mode::EditingNote {
                            text: article.note.clone().unwrap_or_default(),
                        };
                    }
                }

                // `r` re-fetches the current topic's sources and reloads its
                // article list -- see `refresh_topic` below. Clamping
                // `article_index` afterwards matters because a refresh can
                // only ever grow or hold steady the list (nothing here
                // removes rows), but doing it unconditionally is simpler than
                // reasoning about which case applies, and is a no-op when the
                // list grew or stayed the same length.
                KeyCode::Char('r') if !topics.is_empty() => {
                    articles = refresh_topic(storage, &topics[topic_index])?;
                    article_index = article_index.min(articles.len().saturating_sub(1));
                }

                _ => {}
            }
            continue;
        };

        // Reaching here means `mode` matched `Mode::EditingNote` above, so
        // `text` is the popup's live text buffer -- every key from here
        // down edits, commits, or discards it instead of doing anything
        // navigation-related.
        match key.code {
            // `Enter` commits the buffer as the article's note and returns
            // to `Mode::Normal`. An empty buffer becomes `None` (not
            // `Some(String::new())`) -- clearing all the text in the popup
            // and hitting `Enter` is how a note gets *removed*, matching
            // `Article::note`/`Storage::update_note`'s existing use of
            // `Option<String>` to mean "no note" via `None`, never an empty
            // string sitting inside `Some`.
            KeyCode::Enter => {
                if let Some(article) = articles.get_mut(article_index) {
                    let note = if text.is_empty() {
                        None
                    } else {
                        Some(text.clone())
                    };
                    storage.update_note(article.id, note.clone())?;
                    article.note = note;
                }
                mode = Mode::Normal;
            }

            // `Esc` discards the buffer and returns to `Mode::Normal`
            // without touching `storage` at all -- "Esc cancels without
            // saving".
            KeyCode::Esc => mode = Mode::Normal,

            // `Backspace` deletes the last *character*, not byte --
            // `String::pop` is UTF-8-aware, so this can't split a
            // multi-byte character (an accented letter, an emoji) in half
            // the way popping a raw byte off the end could.
            KeyCode::Backspace => {
                text.pop();
            }

            // Any other plain character gets appended to the buffer. This
            // intentionally ignores cursor movement (`Left`/`Right`) and
            // everything else -- "basic text input" (append-only, cursor
            // always at the end) is what this first pass calls for; a full
            // line editor or the `$EDITOR` shell-out is the documented
            // future path for anything more capable (see ARCHITECTURE.md's
            // Roadmap).
            KeyCode::Char(c) => text.push(c),

            _ => {}
        }
    }
}

/// Opens `url` in the user's `$BROWSER`, per ARCHITECTURE.md's keybind
/// table. Best-effort and silent: if `$BROWSER` isn't set, or the command
/// it names doesn't exist, or launching it fails for any other reason,
/// this simply does nothing rather than propagating an `Err` up through
/// `draw_until_quit` -- there's no status line yet for surfacing that kind
/// of message to the user (see ARCHITECTURE.md's footer spec, "keybind
/// hints + last-refresh timestamp" -- no error slot), and a failed launch
/// attempt is not a reason to tear down the whole TUI session.
///
/// `.spawn()` (rather than `.status()` or `.output()`) starts the browser
/// process and immediately returns without waiting for it to exit --
/// blocking the draw loop until the user closes their browser would make
/// `Enter` freeze the whole TUI, which is exactly what opening a link
/// should *not* do. Stdio is redirected to `/dev/null` so a browser that's
/// actually a terminal program (some `$BROWSER` values are, e.g. `lynx`)
/// can't fight with tuxwire over control of the same terminal, which is
/// still in raw mode / the alternate screen at this point.
fn open_in_browser(url: &str) {
    let Ok(browser) = std::env::var("BROWSER") else {
        return;
    };

    let _ = std::process::Command::new(browser)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Re-fetches every source in `fetchers::configured_sources` filed under
/// `topic`, inserts whatever comes back into `storage` (idempotently, on
/// `url` -- see `Storage::insert_article`), and returns the reloaded
/// article list for `topic` -- the `r` keybind.
///
/// `draw_until_quit` runs synchronously (it's the body of a plain,
/// blocking draw loop, not an `async fn`), but `Fetcher::fetch` is async
/// -- it does real network I/O. `main` already put us inside a `tokio`
/// runtime (`#[tokio::main]`), so `tokio::task::block_in_place` +
/// `Handle::current().block_on(...)` is how a synchronous call site
/// inside that runtime drives an `async` call to completion: rather than
/// panicking (which is what trying to enter a *second*, independent
/// runtime from here would do), `block_in_place` hands this task's worker
/// thread over to `tokio`'s blocking-task pool for the duration of the
/// call, so the runtime's other workers are free to keep making progress
/// while this one fetch is in flight. This only works because
/// `#[tokio::main]`'s default multi-threaded runtime has more than one
/// worker thread to hand off to.
fn refresh_topic(storage: &Storage, topic: &str) -> anyhow::Result<Vec<Article>> {
    for source in fetchers::configured_sources()
        .into_iter()
        .filter(|source| source.topic == topic)
    {
        let fetched = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(source.fetch())
        })?;

        for article in &fetched {
            storage.insert_article(article)?;
        }
    }

    storage.articles_by_topic(topic)
}

/// Draws one frame: the topic sidebar + article list side by side, with
/// the keybind footer beneath them -- the layout ARCHITECTURE.md
/// describes ("Left pane: topic list... Right pane: article list...
/// Footer: keybind hints") -- plus the note popup on top of everything
/// else when `mode` is `Mode::EditingNote`.
fn render(
    frame: &mut Frame,
    theme: &Theme,
    topics: &[String],
    selected_topic: Option<&str>,
    articles: &[Article],
    selected_article: Option<usize>,
    mode: &Mode,
) {
    // Painting a plain background-colored block across the whole frame
    // first means every gap between/around the panes below (e.g. if the
    // terminal is wider than 100% + 100%) still shows the theme's
    // background instead of whatever the terminal's own default color is.
    frame.render_widget(
        Block::new().style(Style::new().bg(theme.background)),
        frame.area(),
    );

    // Split the frame vertically into "everything except the last row"
    // and "the last row" -- `Constraint::Min(0)` claims as much space as
    // is left over after the other constraints in the same `Layout` are
    // satisfied, which is what makes the footer pinned to exactly one row
    // regardless of terminal height.
    let [body, footer_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .areas(frame.area());

    // Then split that body horizontally into the sidebar and the article
    // list. A quarter of the width is plenty for topic names, which tend
    // to be short (`linux-news`, `gaming`, ...) compared to article
    // titles.
    let [sidebar_area, articles_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .areas(body);

    render_sidebar(frame, theme, sidebar_area, topics, selected_topic);
    render_articles(
        frame,
        theme,
        articles_area,
        selected_topic,
        articles,
        selected_article,
    );
    render_footer(frame, theme, footer_area, mode);

    // The note popup, if open, paints on top of everything drawn above --
    // see `render_note_popup` for why it has to come last (after the panes
    // it's meant to sit over) and why it lives inside `body` rather than
    // the full frame.
    if let Mode::EditingNote { text } = mode {
        render_note_popup(frame, theme, body, text);
    }
}

/// The left pane: every topic in `storage`, with `selected_topic`
/// highlighted using `theme.accent_selected`.
fn render_sidebar(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    topics: &[String],
    selected_topic: Option<&str>,
) {
    let block = Block::new()
        .title(" Topics ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    let items: Vec<ListItem> = topics
        .iter()
        .map(|topic| ListItem::new(topic.as_str()))
        .collect();

    // `List` is a `StatefulWidget`: rendering it takes a `&mut ListState`
    // that records which row (if any) is highlighted. `state` is rebuilt
    // fresh every frame from whatever topic index `draw_until_quit`
    // currently has selected -- there's no need to persist a `ListState`
    // across frames when the source of truth (`selected_topic`) already
    // lives in the caller's loop state.
    let mut state = ListState::default();
    state.select(
        selected_topic.and_then(|selected| topics.iter().position(|topic| topic == selected)),
    );

    let list = List::new(items).block(block).highlight_style(
        Style::new()
            .bg(theme.accent_selected)
            .fg(theme.background)
            .add_modifier(Modifier::BOLD),
    );

    frame.render_stateful_widget(list, area, &mut state);
}

/// The right pane: every article in `articles` (already the result of
/// `storage.articles_by_topic(selected_topic)`, most recent first), styled
/// per its read/skipped/saved state using the matching `theme.accent_*`
/// color -- ARCHITECTURE.md's "Article States & Behavior" table.
fn render_articles(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    selected_topic: Option<&str>,
    articles: &[Article],
    selected_article: Option<usize>,
) {
    let title = match selected_topic {
        Some(topic) => format!(" Articles — {topic} "),
        None => " Articles ".to_string(),
    };

    let block = Block::new()
        .title(title)
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    if articles.is_empty() {
        // An empty topic (or no topics at all) shouldn't render as a
        // blank pane with no explanation -- that looks indistinguishable
        // from a bug.
        let empty = Paragraph::new("No articles yet -- run a fetcher first.")
            .style(Style::new().fg(theme.text_muted))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = articles
        .iter()
        .map(|article| article_item(theme, article))
        .collect();

    // Each `ListItem` here is two lines (title + source/timestamp, see
    // `article_item` below), so highlighting needs to cover both rather
    // than just the title line -- `highlight_spacing` isn't enough on its
    // own for that, but the default `HighlightSpacing::WhenSelected`
    // behavior already highlights every line of the selected item, which
    // is what's wanted here.
    let mut state = ListState::default();
    state.select(selected_article);

    let list = List::new(items).block(block).highlight_style(
        Style::new()
            .bg(theme.accent_selected)
            .fg(theme.background)
            .add_modifier(Modifier::BOLD),
    );

    frame.render_stateful_widget(list, area, &mut state);
}

/// One article's two-line `ListItem`: the title (colored by state) above a
/// dimmer "source · timestamp" line.
fn article_item<'a>(theme: &Theme, article: &'a Article) -> ListItem<'a> {
    // Priority order matters here: an article can technically be both
    // `read` and `saved` (saving implies read, per ARCHITECTURE.md), so
    // `saved`/`skipped` are checked first -- they're the more specific,
    // more deliberate states, and should win visually over the more
    // general "read" fade-out.
    let title_color = if article.skipped {
        theme.accent_skipped
    } else if article.saved {
        theme.accent_saved
    } else if article.read {
        theme.accent_read
    } else {
        theme.accent_unread
    };

    let title_line = Line::from(Span::styled(
        article.title.as_str(),
        Style::new().fg(title_color),
    ));

    let meta_line = Line::from(Span::styled(
        format!(
            "  {} · {}",
            article.source,
            article.timestamp.format("%Y-%m-%d %H:%M")
        ),
        Style::new().fg(theme.text_muted),
    ));

    ListItem::new(vec![title_line, meta_line])
}

/// The footer: the keybind hint line, covering exactly the keys wired up
/// so far -- see `KEYBIND_HINTS`/`EDITING_NOTE_HINTS` and this module's
/// top-level doc comment for which parts of ARCHITECTURE.md's full keybind
/// table that excludes. Swaps to `EDITING_NOTE_HINTS` while the note popup
/// is open, since every other keybind is suspended for the duration (see
/// the `Mode::EditingNote` match arm in `draw_until_quit`).
fn render_footer(frame: &mut Frame, theme: &Theme, area: Rect, mode: &Mode) {
    let hints = match mode {
        Mode::Normal => KEYBIND_HINTS,
        Mode::EditingNote { .. } => EDITING_NOTE_HINTS,
    };

    let footer =
        Paragraph::new(hints).style(Style::new().bg(theme.background).fg(theme.text_muted));

    frame.render_widget(footer, area);
}

/// The note popup: a bordered box centered over `area` (the sidebar +
/// article-list region, *not* the whole frame -- staying off the footer
/// row means the "Enter save · Esc cancel" hint stays visible while the
/// popup is open), showing `text` -- the live buffer `Mode::EditingNote`
/// carries -- with a blinking cursor placed right after it.
///
/// This is deliberately simple: no scrolling, no wrapping, no multi-line
/// support. `docs/ARCHITECTURE.md`'s Roadmap lists "multi-line vs.
/// single-line notes UI" as still an open question, with the `$EDITOR`
/// shell-out as the other half of that -- both are explicitly future work,
/// not this first pass.
fn render_note_popup(frame: &mut Frame, theme: &Theme, area: Rect, text: &str) {
    let popup_area = centered_rect(area, 60, 5);

    // `Clear` is a widget whose only job is to blank out whatever cells it
    // covers before something else draws over them. Without it, the
    // article list rendered underneath would show through around any
    // characters/whitespace the popup's own `Block` background doesn't
    // happen to overwrite -- borders included, since a `Block`'s border
    // only touches the very edge cells, not necessarily every cell a
    // non-rectangular terminal font/glyph might otherwise bleed through on.
    frame.render_widget(Clear, popup_area);

    let block = Block::new()
        .title(" Note ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.accent_selected));

    let paragraph = Paragraph::new(text)
        .style(Style::new().fg(theme.text_primary))
        .block(block);
    frame.render_widget(paragraph, popup_area);

    // Places the real terminal cursor right after the last character of
    // `text`, inside the popup's border (hence the `+ 1` on both axes).
    // `ratatui::run`'s draw loop shows this as the terminal's own native
    // blinking cursor -- exactly what a text-input field needs to signal
    // where typing lands, and something a plain `Paragraph` alone can't
    // convey on its own.
    let cursor_x = popup_area.x + 1 + text.chars().count() as u16;
    let cursor_y = popup_area.y + 1;
    frame.set_cursor_position((cursor_x, cursor_y));
}

/// A `width_percent`-wide, `height`-tall `Rect` centered inside `area` --
/// the standard "popup over the rest of the UI" layout trick: split `area`
/// into thirds/whatever-the-percentage-implies twice, horizontally then
/// vertically, and keep only the middle slice of each split.
/// `Constraint::Percentage` (rather than a fixed cell count) for the
/// horizontal split is what makes the popup's *width* scale with the
/// terminal instead of overflowing a narrow one; `height` is a fixed row
/// count instead, since a note's input box doesn't need to grow with
/// terminal height the way its width should grow with terminal width.
fn centered_rect(area: Rect, width_percent: u16, height: u16) -> Rect {
    let [_, vertical, _] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height.min(area.height)),
            Constraint::Fill(1),
        ])
        .areas(area);

    let [_, horizontal, _] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Percentage(width_percent),
            Constraint::Fill(1),
        ])
        .areas(vertical);

    horizontal
}
