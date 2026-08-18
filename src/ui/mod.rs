//! ratatui views/widgets (`docs/ARCHITECTURE.md` § 3. TUI).
//!
//! Second milestone: navigation and opening links. `j`/`k` move the
//! article-list selection, `Up`/`Down`/`Tab` move the topic-sidebar
//! selection (reloading the article list from `storage` whenever the
//! topic changes), and `Enter` opens the selected article's URL in
//! `$BROWSER`. State-changing keybinds -- `x` skip, `s` save, `n` note,
//! `r` refresh, `S` saved view, `a` add source -- are all still to come,
//! and none of them run yet, including the "opening an article marks it
//! read" behavior ARCHITECTURE.md describes; `Enter` here only opens the
//! link. This module owns the terminal setup/teardown, the draw loop, and
//! (starting with this milestone) the small bit of navigation state that
//! selection requires -- which topic and which article are highlighted,
//! and the current topic's article list, since that has to be reloaded
//! from `storage` on every topic change rather than loaded once upfront.

use crate::models::Article;
use crate::storage::Storage;
use crate::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

/// Keybind hints for the footer. Only the keys actually wired up so far --
/// the rest of ARCHITECTURE.md's v1 keybind table (`x` skip, `s` save, `n`
/// note, `r` refresh, `S` saved view, `a` add source) isn't implemented
/// yet, so it doesn't belong in the footer yet either; showing a hint for
/// a key that does nothing would be worse than showing no hint at all.
/// Kept as one `const` (rather than inlined into `footer` below) so
/// there's exactly one place this has to stay in sync with the match in
/// `draw_until_quit` below.
const KEYBIND_HINTS: &str = "j/k move · ↑/↓/Tab topic · Enter open · q quit";

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

    loop {
        let selected_topic = topics.get(topic_index).map(String::as_str);
        let selected_article = if articles.is_empty() { None } else { Some(article_index) };

        terminal.draw(|frame| render(frame, theme, topics, selected_topic, &articles, selected_article))?;

        // `event::read()` blocks until the next terminal event -- no
        // polling loop or timer needed, since nothing here animates or
        // refreshes on its own. `KeyEventKind::Press` matters because some
        // terminals (with the right protocol enabled) report *both* a key
        // press and its later release as separate events; without this
        // check, releasing one key after pressing another first could be
        // misread as a second, unrelated keystroke.
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }

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
                topic_index = if topic_index == 0 { topics.len() - 1 } else { topic_index - 1 };
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
    let Ok(browser) = std::env::var("BROWSER") else { return };

    let _ = std::process::Command::new(browser)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Draws one frame: the topic sidebar + article list side by side, with
/// the keybind footer beneath them -- the layout ARCHITECTURE.md
/// describes ("Left pane: topic list... Right pane: article list...
/// Footer: keybind hints").
fn render(
    frame: &mut Frame,
    theme: &Theme,
    topics: &[String],
    selected_topic: Option<&str>,
    articles: &[Article],
    selected_article: Option<usize>,
) {
    // Painting a plain background-colored block across the whole frame
    // first means every gap between/around the panes below (e.g. if the
    // terminal is wider than 100% + 100%) still shows the theme's
    // background instead of whatever the terminal's own default color is.
    frame.render_widget(Block::new().style(Style::new().bg(theme.background)), frame.area());

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
    render_articles(frame, theme, articles_area, selected_topic, articles, selected_article);
    render_footer(frame, theme, footer_area);
}

/// The left pane: every topic in `storage`, with `selected_topic`
/// highlighted using `theme.accent_selected`.
fn render_sidebar(frame: &mut Frame, theme: &Theme, area: Rect, topics: &[String], selected_topic: Option<&str>) {
    let block = Block::new()
        .title(" Topics ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    let items: Vec<ListItem> = topics.iter().map(|topic| ListItem::new(topic.as_str())).collect();

    // `List` is a `StatefulWidget`: rendering it takes a `&mut ListState`
    // that records which row (if any) is highlighted. `state` is rebuilt
    // fresh every frame from whatever topic index `draw_until_quit`
    // currently has selected -- there's no need to persist a `ListState`
    // across frames when the source of truth (`selected_topic`) already
    // lives in the caller's loop state.
    let mut state = ListState::default();
    state.select(selected_topic.and_then(|selected| topics.iter().position(|topic| topic == selected)));

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(theme.accent_selected).fg(theme.background).add_modifier(Modifier::BOLD));

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

    let items: Vec<ListItem> = articles.iter().map(|article| article_item(theme, article)).collect();

    // Each `ListItem` here is two lines (title + source/timestamp, see
    // `article_item` below), so highlighting needs to cover both rather
    // than just the title line -- `highlight_spacing` isn't enough on its
    // own for that, but the default `HighlightSpacing::WhenSelected`
    // behavior already highlights every line of the selected item, which
    // is what's wanted here.
    let mut state = ListState::default();
    state.select(selected_article);

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(theme.accent_selected).fg(theme.background).add_modifier(Modifier::BOLD));

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

    let title_line = Line::from(Span::styled(article.title.as_str(), Style::new().fg(title_color)));

    let meta_line = Line::from(Span::styled(
        format!("  {} · {}", article.source, article.timestamp.format("%Y-%m-%d %H:%M")),
        Style::new().fg(theme.text_muted),
    ));

    ListItem::new(vec![title_line, meta_line])
}

/// The footer: the keybind hint line, covering exactly the keys wired up
/// so far -- see `KEYBIND_HINTS` and this module's top-level doc comment
/// for which parts of ARCHITECTURE.md's full keybind table that excludes.
fn render_footer(frame: &mut Frame, theme: &Theme, area: Rect) {
    let footer = Paragraph::new(KEYBIND_HINTS).style(Style::new().bg(theme.background).fg(theme.text_muted));

    frame.render_widget(footer, area);
}
