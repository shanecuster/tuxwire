//! ratatui views/widgets (`docs/ARCHITECTURE.md` § 3. TUI).
//!
//! This is the *read-only* first milestone: a two-pane layout (topic
//! sidebar on the left, article list on the right) plus a footer showing
//! the full keybind set, drawn from real rows in `storage` and styled from
//! a real `Theme`. Nothing here reacts to a keypress except `q` (quit) --
//! `j`/`k` navigation, opening articles, saving, skipping, etc. are all
//! still to come. This module owns the terminal setup/teardown and the
//! draw loop; it doesn't own any app *state* yet beyond what's loaded once
//! at startup, since there's no interaction that would change it.

use crate::models::Article;
use crate::storage::Storage;
use crate::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

/// Keybind hints for the footer. This is the *full* v1 keybind table from
/// ARCHITECTURE.md, even though only `q` actually does anything yet --
/// the footer is meant to match the mockup, and the mockup shows the
/// finished set. Kept as one `const` (rather than inlined into `footer`
/// below) so the string that has to stay in sync with ARCHITECTURE.md's
/// keybind table lives in exactly one place.
const KEYBIND_HINTS: &str =
    "j/k move · Enter open · x skip · s save · n note · r refresh · S saved · a add source · q quit";

/// Runs the read-only TUI shell until the user presses `q`.
///
/// Loads `topics()` and the first topic's `articles_by_topic(...)` from
/// `storage` *once*, before entering the draw loop -- there's no refresh or
/// topic-switching keybind yet, so there's nothing that would ever make
/// this data go stale mid-run.
///
/// `storage: &Storage` and `theme: &Theme` are both borrowed rather than
/// owned: this function only ever *reads* through them, and taking
/// ownership would force whoever calls `ui::run` to give up their own
/// `Storage`/`Theme` (or clone them) just to display something once.
pub fn run(storage: &Storage, theme: &Theme) -> anyhow::Result<()> {
    let topics = storage.topics()?;

    // No topic-selection keybind exists yet, so "selected" just means
    // "the first one alphabetically" (that's the order `Storage::topics`
    // already returns them in). `.first()` hands back `Option<&String>` --
    // `None` for a database with no articles in it at all yet, which the
    // rendering code below has to handle without panicking.
    let selected_topic = topics.first().cloned();

    let articles = match &selected_topic {
        Some(topic) => storage.articles_by_topic(topic)?,
        None => Vec::new(),
    };

    // `ratatui::run` (see the doc comment on it in the `ratatui` crate)
    // is the "simplest path" helper: it puts the terminal into raw mode +
    // the alternate screen, hands a `&mut DefaultTerminal` to this
    // closure, and -- critically -- restores the terminal afterwards
    // *no matter how the closure returns*, including on an `Err`. That's
    // exactly the guarantee this function needs: if `draw_until_quit`
    // below hits an error partway through, the user's shell must not be
    // left in raw mode / the alternate screen.
    ratatui::run(|terminal| draw_until_quit(terminal, theme, &topics, selected_topic.as_deref(), &articles))
}

/// The actual draw loop: redraw the frame, block for the next terminal
/// event, and quit on `q` -- otherwise loop forever. Split out from `run`
/// so `run` itself stays focused on "load the data, then hand off to
/// ratatui," rather than mixing that with the loop's control flow.
fn draw_until_quit(
    terminal: &mut ratatui::DefaultTerminal,
    theme: &Theme,
    topics: &[String],
    selected_topic: Option<&str>,
    articles: &[Article],
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|frame| render(frame, theme, topics, selected_topic, articles))?;

        // `event::read()` blocks until the next terminal event -- no
        // polling loop or timer needed, since this UI has nothing to
        // animate or refresh on its own yet. `KeyEventKind::Press` matters
        // because some terminals (with the right protocol enabled) report
        // *both* a key press and its later release as separate events;
        // without this check, releasing `q` after pressing some other key
        // first could be misread as a second, unrelated keystroke.
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && key.code == KeyCode::Char('q')
        {
            return Ok(());
        }
    }
}

/// Draws one frame: the topic sidebar + article list side by side, with
/// the keybind footer beneath them -- the layout ARCHITECTURE.md
/// describes ("Left pane: topic list... Right pane: article list...
/// Footer: keybind hints").
fn render(frame: &mut Frame, theme: &Theme, topics: &[String], selected_topic: Option<&str>, articles: &[Article]) {
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
    render_articles(frame, theme, articles_area, selected_topic, articles);
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
    // that records which row (if any) is highlighted, which is what lets
    // the same widget later grow into `j`/`k` navigation without changing
    // shape -- only how `state` gets built will change, not this
    // rendering code. For now `state` is rebuilt fresh every frame with
    // whatever topic index matches `selected_topic`, since nothing yet
    // lets the user move it.
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
fn render_articles(frame: &mut Frame, theme: &Theme, area: Rect, selected_topic: Option<&str>, articles: &[Article]) {
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
    let list = List::new(items).block(block);

    frame.render_widget(list, area);
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

/// The footer: the full keybind hint line, matching ARCHITECTURE.md's
/// keybind table (only `q` is actually wired up yet -- see this module's
/// top-level doc comment).
fn render_footer(frame: &mut Frame, theme: &Theme, area: Rect) {
    let footer = Paragraph::new(KEYBIND_HINTS).style(Style::new().bg(theme.background).fg(theme.text_muted));

    frame.render_widget(footer, area);
}
