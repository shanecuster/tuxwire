//! ratatui views/widgets (`docs/ARCHITECTURE.md` § 3. TUI).
//!
//! Fifth milestone: `S` saved view. `j`/`k` move the article-list
//! selection, `Up`/`Down`/`Tab` move the topic-sidebar selection (reloading
//! the article list from `storage` whenever the topic changes), `Enter`
//! suspends tuxwire's screen and opens the selected article's URL in `w3m`
//! (see `open_in_w3m`), falling back to `$BROWSER`/`xdg-open` (see
//! `open_in_browser`) when `w3m` isn't installed, `x` marks the selected
//! article skipped, `r` re-fetches the current topic's sources (via
//! `fetchers::configured_sources`), inserts whatever's new into `storage`,
//! and reloads the article list. `s` saves the selected article via
//! `Storage::save_article`, and `n` opens a small inline popup (see `Mode`
//! below) for editing that article's note via `Storage::update_note`.
//! `S` now switches the whole main view (see `View` below) to every saved
//! article across every topic, via `Storage::saved_articles` -- pressing it
//! again (or `Esc`, or `Tab`) switches back. `j`/`k`, `Enter`, and `n` all
//! keep working unmodified in that view since they already just operate on
//! whatever's in `articles`/`article_index`, regardless of which query
//! populated them. `a` add source is still to come, and neither it nor the
//! "opening an article marks it read" behavior ARCHITECTURE.md describes
//! run yet; `Enter` here only opens the link. This module owns the
//! terminal setup/teardown, the draw loop, and the small bit of navigation
//! state that selection requires -- which topic and which article are
//! highlighted, the current article list (since that has to be reloaded
//! from `storage` on every topic change, every `r` refresh, and every `S`
//! toggle, rather than loaded once upfront), `Mode`, which tracks whether
//! the note popup is open, and now `View`, which tracks whether that list
//! is a topic's articles or the saved-articles view.

use crate::fetchers::{self, Fetcher};
use crate::models::Article;
use crate::storage::Storage;
use crate::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

/// Keybind hints for the footer, for the normal (not editing-a-note,
/// not adding-a-source) state while `View::Topic` is showing -- see
/// `SAVED_HINTS` below for the saved view's own hint line,
/// `EDITING_NOTE_HINTS` for the note popup's, and
/// `ADD_SOURCE_URL_HINTS`/`ADD_SOURCE_CONFIRM_HINTS` for the add-source
/// flow's. Kept as one `const` (rather than inlined into `render_footer`
/// below) so there's exactly one place this has to stay in sync with the
/// `Mode::Normal` match arm in `draw_until_quit` below.
///
/// Deliberately drops `x`/`s`/`n`/`r`/`S`/`a` -- those six now have a
/// permanent home in the sidebar's own "Keys" section (see
/// `render_keys_section`), which is on screen at all times rather than
/// only in the footer, so repeating them here would just be the same
/// reference living in two places. What's left is exactly the keys the
/// Keys section *doesn't* cover: raw navigation (`j`/`k`, `↑`/`↓`/`Tab`,
/// `Enter`) and `q` to quit.
const KEYBIND_HINTS: &str = "j/k move · ↑/↓/Tab topic · Enter open · q quit";

/// The footer hint line shown while `View::Saved` is showing (and no
/// popup is open). Deliberately drops `↑/↓ topic` and `r refresh` --
/// both are disabled in this view (see the guards on their match arms in
/// `draw_until_quit`), since neither a topic-sidebar selection nor "refetch
/// this topic's sources" means anything once the list is "every saved
/// article across every topic" instead of one topic's.
///
/// Also drops `x`/`s`/`n`/`a`, same reasoning as `KEYBIND_HINTS` above --
/// covered by the sidebar's always-visible Keys section instead. `S` stays
/// off this line for the same reason (it's in Keys too), but `Esc`/`Tab`
/// remain: they're this view's *own* way back to `View::Topic`, which the
/// Keys section (a flat list of the six main-flow keys) doesn't document.
const SAVED_HINTS: &str = "j/k move · Enter open · Esc/Tab back · q quit";

/// The footer hint line shown while the note popup (`Mode::EditingNote`)
/// is open -- every other keybind is suspended while typing a note (see
/// the `Mode::EditingNote` match arm in `draw_until_quit`), so the footer
/// swaps to just these two regardless of which `View` was showing
/// underneath.
const EDITING_NOTE_HINTS: &str = "Enter save note · Esc cancel";

/// The footer hint line shown while the add-source popup is open and on
/// its first step -- prompting for the feed URL itself (see
/// `AddSourceStep::Url`).
const ADD_SOURCE_URL_HINTS: &str = "Enter fetch & validate · Esc cancel";

/// The footer hint line shown on the add-source popup's second step -- the
/// name/topic confirm screen (see `AddSourceStep::Confirm`).
const ADD_SOURCE_CONFIRM_HINTS: &str =
    "Tab switch field · ↑/↓ pick existing topic · Enter save · Esc cancel";

/// The footer hint line shown while the `Mode::Error` popup is open --
/// there's only one way out of it, unlike the other popups' `Enter`/`Esc`
/// split, since there's no "confirm" action for a plain error message.
const ERROR_HINTS: &str = "any key to dismiss";

/// How many characters of a saved article's note to show in the saved
/// view's list before truncating with `…` -- see `truncate_preview` and
/// its use in `article_item` below. The saved view is a list of many
/// articles at once, each getting at most a couple of terminal rows, so a
/// long note would either wrap unpredictably or crowd out the rows around
/// it; the full text is always one `n` press away in the same edit popup
/// already built for editing it.
const NOTE_PREVIEW_CHARS: usize = 45;

/// The sidebar's "Keys" reference section (`docs/ARCHITECTURE.md` § 3. TUI)
/// -- the six main-flow keybinds, as `(key, action)` pairs, rendered one per
/// line by `render_keys_section` below. A `const` array (rather than
/// building this `Vec` fresh inside `render_keys_section` every frame) since
/// it never changes at runtime -- there's no reason to reallocate the same
/// six pairs on every single draw.
///
/// This exists specifically because the person building tuxwire has no
/// prior Rust/TUI-app experience and shouldn't have to memorize the keybind
/// table before the app is usable -- the reference lives on screen instead,
/// in both `View::Topic` and `View::Saved` (see `render_sidebar`, which
/// doesn't take a `View` at all -- these two sections render identically
/// regardless of which one is showing).
const KEY_HINTS: [(&str, &str); 6] = [
    ("s", "save"),
    ("x", "close"),
    ("n", "note"),
    ("S", "saved view"),
    ("r", "refresh"),
    ("a", "add source"),
];

/// The `tuxwire` wordmark, straight out of `figlet -f standard tuxwire` --
/// see `render_banner` and the "Banner bar" bullet in `docs/ARCHITECTURE.md`'s
/// TUI section. Kept as a fixed array of lines rather than pulling in a
/// figlet-rendering crate to regenerate it at runtime: it's five characters
/// of static ASCII art that never changes, so there's nothing dynamic here
/// worth a dependency for. Each line is exactly 37 characters wide -- the
/// `standard` font's natural width for this word -- and there are six rows,
/// not five: figlet reserves a row for descenders on every character's grid
/// cell even though none of `t`/`u`/`x`/`w`/`i`/`r`/`e` actually dip below
/// the baseline in this font, so the last row here is blank padding, not a
/// mistake.
const BANNER: [&str; 6] = [
    r" _                        _          ",
    r"| |_ _   ___  ____      _(_)_ __ ___ ",
    r"| __| | | \ \/ /\ \ /\ / / | '__/ _ \",
    r"| |_| |_| |>  <  \ V  V /| | | |  __/",
    r" \__|\__,_/_/\_\  \_/\_/ |_|_|  \___|",
    r"                                     ",
];

/// Which main view is currently showing -- the ordinary per-topic article
/// list (`Topic`, the default) or every saved article across every topic
/// (`Saved`, entered/exited with `S`). This is a *separate* piece of state
/// from `Mode` below rather than another `Mode` variant: `Mode` tracks
/// whether the note popup is open, which can happen while looking at
/// *either* view, so folding them into one enum would need a variant for
/// every combination (`Mode::EditingNote` while saved, while not, ...)
/// instead of the two independent axes this actually is.
///
/// `PartialEq` is derived so match guards elsewhere in this file can write
/// plain `view == View::Topic` / `view == View::Saved` comparisons; `Clone,
/// Copy` because a `View` is just a two-variant tag with no data of its
/// own, cheap to copy by value rather than worth ever borrowing.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Topic,
    Saved,
}

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
    /// The `a` "add a new source" flow is open -- see `AddSourceStep` for
    /// which of its two steps is currently showing. Wrapping the step in
    /// its own type (rather than adding more `Mode` variants directly, one
    /// per step) keeps `Mode` itself a flat "which popup, if any" tag, and
    /// lets `AddSourceStep` evolve its own fields independently.
    AddSource(AddSourceStep),

    /// A blocking problem `Enter` hit that there's no other popup to show it
    /// through -- right now that's only "neither `w3m` nor a `$BROWSER`/
    /// `xdg-open` fallback could open the article" (see `w3m_available`,
    /// `open_in_w3m`, and `open_in_browser`), checked *before* tuxwire's
    /// screen is ever suspended, so there's nothing to resume from and this
    /// is a plain in-place popup like the other two `Mode` variants.
    /// Dismissed by any keypress, same as pressing `Esc` on the others,
    /// since there's no follow-up action to take on an error besides
    /// acknowledging it.
    Error { message: String },
}

/// Which step of the `a` add-source flow is currently showing (`ARCHITECTURE.md`
/// § Adding Sources): first a bare feed URL prompt, then -- once that URL
/// has actually been fetched and parsed successfully, which *is* the
/// validation (see `fetchers::rss::fetch_feed`) -- a confirm screen for the
/// name and topic before anything is written to `sources.toml`.
enum AddSourceStep {
    /// Prompting for the feed URL itself. `error`, when `Some`, is the
    /// message from the most recent failed fetch/parse attempt -- kept
    /// alongside `text` (not cleared) so the user can see *why* it failed
    /// while they edit `text` and retry, per ARCHITECTURE.md's "show a
    /// clear error and let the user retry or cancel."
    Url { text: String, error: Option<String> },

    /// The URL in `url` parsed successfully; confirming `name` (guessed
    /// from the feed's own `<title>`, but editable) and `topic` (required,
    /// either picked from `topic_options` -- the topics that existed when
    /// this screen opened, via `Storage::topics()` -- or freely typed as a
    /// brand new one) before writing a `[[source]]` block. `error`, when
    /// `Some`, is a validation or write failure to show inline (e.g. an
    /// empty topic on `Enter`), same idea as `Url`'s.
    Confirm {
        url: String,
        name: String,
        topic: String,
        field: ConfirmField,
        topic_options: Vec<String>,
        error: Option<String>,
    },
}

/// Which field of the `AddSourceStep::Confirm` screen currently has focus
/// -- `Tab` toggles this, and it decides both which field typed characters
/// go into and (for `Topic`) whether `Up`/`Down` cycle through
/// `topic_options`. `Clone, Copy, PartialEq` for the same reason as `View`
/// above: a two-variant tag with no data of its own, compared with plain
/// `==` (see `render_add_source_popup`'s `focus_style`).
#[derive(Clone, Copy, PartialEq)]
enum ConfirmField {
    Name,
    Topic,
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
    // `mut` (and passed to `draw_until_quit` as `&mut` below) because the
    // `a` add-source flow can grow this list: a brand-new topic typed on
    // the confirm screen needs to show up in the sidebar immediately (per
    // ARCHITECTURE.md's "usable immediately without restart"), which means
    // mutating this same `Vec` in place rather than only ever reading it
    // once here at startup.
    let mut topics = storage.topics()?;

    // `ratatui::run` (see the doc comment on it in the `ratatui` crate)
    // is the "simplest path" helper: it puts the terminal into raw mode +
    // the alternate screen, hands a `&mut DefaultTerminal` to this
    // closure, and -- critically -- restores the terminal afterwards
    // *no matter how the closure returns*, including on an `Err`. That's
    // exactly the guarantee this function needs: if `draw_until_quit`
    // below hits an error partway through, the user's shell must not be
    // left in raw mode / the alternate screen.
    ratatui::run(|terminal| draw_until_quit(terminal, theme, storage, &mut topics))
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
    topics: &mut Vec<String>,
) -> anyhow::Result<()> {
    let mut topic_index: usize = 0;
    let mut article_index: usize = 0;
    let mut articles: Vec<Article> = match topics.first() {
        Some(topic) => storage.articles_by_topic(topic)?,
        None => Vec::new(),
    };
    let mut mode = Mode::Normal;
    let mut view = View::Topic;

    loop {
        // The sidebar only ever reflects the current *topic* selection,
        // even while `view` is `View::Saved` -- there's no per-row
        // selection in the saved view that maps onto it, so leaving this
        // as-is just means the sidebar keeps showing whichever topic was
        // selected before `S` was pressed, ready to resume from once the
        // user switches back.
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
                topics.as_slice(),
                selected_topic,
                &articles,
                selected_article,
                &mode,
                view,
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

        // While the add-source popup is open, every keypress belongs to
        // it, same reasoning as the note-popup split just below -- and
        // for the same reason, this has to be checked *first*, before
        // that split, so a keypress meant for the add-source flow never
        // falls through into ordinary navigation.
        //
        // `matches!` here only inspects `mode`'s discriminant (the `_`
        // doesn't bind the `AddSourceStep` payload by value), so it
        // doesn't move `mode` -- unlike the `let Mode::AddSource(step) =
        // mode else { unreachable!() };` right after, which does move it
        // (that's *why* it's guarded by this check: the pattern is
        // guaranteed to match, so `unreachable!()` really is unreachable).
        // `handle_add_source_key` takes `step` (and the old `mode`, via
        // that move) by value and hands back whatever `mode` should become
        // next -- there's no way to pass `&mut mode` in here directly
        // instead, since `step` would then be a live borrow *out of*
        // `mode` at the same time something tries to reassign `mode`
        // itself, which the borrow checker rejects.
        if matches!(mode, Mode::AddSource(_)) {
            // Captured *before* `handle_add_source_key` runs, since a
            // successful confirm can insert a brand-new topic into
            // `topics` and re-sort it -- `topic_index` (a plain number)
            // would then silently point at the wrong topic afterward
            // unless it's re-derived from the topic's *name* instead.
            let previously_selected_topic = topics.get(topic_index).cloned();

            let Mode::AddSource(step) = mode else {
                unreachable!("just checked mode is Mode::AddSource above")
            };
            mode = handle_add_source_key(key.code, step, topics)?;

            // `mode` is back to `Mode::Normal` once the flow ends, whether
            // that's by successfully confirming or by cancelling with
            // `Esc` -- both cases need the same cleanup: re-find the
            // sidebar selection by name (see the comment above) and
            // reload `articles` for it, so a source added under a
            // brand-new topic is immediately reflected in the sidebar
            // without restarting tuxwire, and a first-ever topic (added
            // when `topics` was completely empty) becomes browsable
            // right away instead of staying on the "no articles yet"
            // placeholder against a still-empty topic list.
            if matches!(mode, Mode::Normal) {
                topic_index = previously_selected_topic
                    .and_then(|name| topics.iter().position(|t| *t == name))
                    .unwrap_or(0);

                articles = match topics.get(topic_index) {
                    Some(topic) => storage.articles_by_topic(topic)?,
                    None => Vec::new(),
                };
                article_index = article_index.min(articles.len().saturating_sub(1));
            }
            continue;
        }

        // While the error popup is open, every keypress dismisses it back
        // to `Mode::Normal` -- there's no text to type or field to navigate,
        // so unlike the note/add-source popups this doesn't need to inspect
        // `key.code` at all before deciding what to do with it.
        if matches!(mode, Mode::Error { .. }) {
            mode = Mode::Normal;
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

                // `Up`/`Down` move the topic-sidebar selection instead,
                // clamping at either end (same as `j`/`k` above) rather than
                // wrapping around, so hitting the top or bottom topic just
                // stays put instead of jumping to the other end.
                // Every topic switch reloads `articles` for the newly selected
                // topic and resets `article_index` back to the top -- carrying
                // over an index from a different topic's list makes no sense,
                // and could even be out of bounds for a shorter one. Guarded
                // to `View::Topic` only -- there's no topic-sidebar selection
                // to move while `View::Saved` is showing, and reloading
                // `articles` here would silently clobber the saved list with
                // a topic's instead.
                KeyCode::Up if view == View::Topic && !topics.is_empty() => {
                    topic_index = topic_index.saturating_sub(1);
                    articles = storage.articles_by_topic(&topics[topic_index])?;
                    article_index = 0;
                }
                KeyCode::Down if view == View::Topic && !topics.is_empty() => {
                    topic_index = (topic_index + 1).min(topics.len() - 1);
                    articles = storage.articles_by_topic(&topics[topic_index])?;
                    article_index = 0;
                }

                // `Tab` does double duty depending on `view`: in `View::Topic`
                // it's just another way to move the topic sidebar forward
                // (same as `Down` above, clamping at the last topic rather
                // than wrapping); in `View::Saved` it instead exits back to
                // the topic view, per ARCHITECTURE.md's "pressing S again (or
                // Esc, or Tab) returns to the normal topic view." Splitting
                // this into its own arm (rather than folding it into the
                // `Down` arm above the way it used to be) is what makes that
                // second behavior possible -- the two keys now genuinely do
                // different things depending on `view`.
                KeyCode::Tab => {
                    if view == View::Saved {
                        view = View::Topic;
                        if let Some(topic) = topics.get(topic_index) {
                            articles = storage.articles_by_topic(topic)?;
                        }
                        article_index = 0;
                    } else if !topics.is_empty() {
                        topic_index = (topic_index + 1).min(topics.len() - 1);
                        articles = storage.articles_by_topic(&topics[topic_index])?;
                        article_index = 0;
                    }
                }

                // `Esc` outside the note popup only means something in
                // `View::Saved`: exit back to the topic view, same as `Tab`
                // above. In `View::Topic` there's nothing for a bare `Esc` to
                // cancel, so it falls through to the `_ => {}` catch-all
                // instead.
                KeyCode::Esc if view == View::Saved => {
                    view = View::Topic;
                    if let Some(topic) = topics.get(topic_index) {
                        articles = storage.articles_by_topic(topic)?;
                    }
                    article_index = 0;
                }

                // `Enter` suspends tuxwire's screen and spawns `w3m`
                // full-screen against the selected article's URL, resuming
                // tuxwire exactly where it left off once the user quits w3m
                // (its own `q` key) -- Roadmap Option A's in-terminal
                // article reading. `w3m_available` is checked first so a
                // missing `w3m` doesn't suspend the screen for nothing; see
                // it and `open_in_w3m` below for why each exists as its own
                // function. When `w3m` isn't available, this falls back to
                // `open_in_browser` (the old "open in `$BROWSER`" behavior,
                // now also trying `xdg-open`) rather than immediately
                // showing `Mode::Error` -- a missing `w3m` shouldn't stop
                // someone who still has a working `$BROWSER`/`xdg-open`
                // from reading the article. `Mode::Error` is reserved for
                // when *both* paths fail to open anything. Marks the
                // article read the same way regardless of which path
                // succeeded -- both on disk via `Storage::mark_read` and in
                // the in-memory `articles` list, mirroring how `x` and `s`
                // update their entry directly -- but only once something
                // has actually opened; the `Mode::Error` branch leaves
                // `read` untouched, since nothing was.
                KeyCode::Enter => {
                    if let Some(article) = articles.get_mut(article_index) {
                        if w3m_available() {
                            open_in_w3m(terminal, &article.url)?;
                            storage.mark_read(article.id)?;
                            article.read = true;
                        } else if open_in_browser(&article.url) {
                            storage.mark_read(article.id)?;
                            article.read = true;
                        } else {
                            mode = Mode::Error {
                                message: "could not open article -- w3m \
                                    isn't installed, and no browser could \
                                    be launched via $BROWSER or xdg-open."
                                    .to_string(),
                            };
                        }
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
                // list grew or stayed the same length. Guarded to
                // `View::Topic` -- "refetch this topic's sources" has no
                // meaning against "every saved article across every topic",
                // and would otherwise silently swap the saved view out for a
                // single topic's list without actually leaving `View::Saved`.
                KeyCode::Char('r') if view == View::Topic && !topics.is_empty() => {
                    articles = refresh_topic(storage, &topics[topic_index])?;
                    article_index = article_index.min(articles.len().saturating_sub(1));
                }

                // `S` toggles between the two main views: `View::Topic`
                // (the ordinary per-topic list) and `View::Saved` (every
                // saved article across every topic, via
                // `Storage::saved_articles` -- ARCHITECTURE.md's dedicated
                // "Saved" view, deliberately independent of the topic
                // sidebar rather than a pseudo-topic inside it). `j`/`k`,
                // `Enter`, `x`, `s`, and `n` all keep working unmodified
                // afterwards since none of them care *how* `articles` got
                // populated, only that it's a `Vec<Article>` with a valid
                // `article_index` into it -- which resetting to `0` here
                // guarantees, the same as every other list reload above.
                KeyCode::Char('S') => {
                    view = match view {
                        View::Topic => View::Saved,
                        View::Saved => View::Topic,
                    };
                    articles = match view {
                        View::Saved => storage.saved_articles()?,
                        View::Topic => match topics.get(topic_index) {
                            Some(topic) => storage.articles_by_topic(topic)?,
                            None => Vec::new(),
                        },
                    };
                    article_index = 0;
                }

                // `a` opens the add-source popup (switches `mode` to
                // `Mode::AddSource`), starting on its first step -- an
                // empty feed-URL prompt with no error showing yet. The
                // keystrokes that follow are handled by the
                // `Mode::AddSource` branch above, once this same `loop`
                // iterates back around and reads the next key -- same
                // pattern as `n` opening the note popup above. Available
                // in both `View::Topic` and `View::Saved` (no `view ==
                // ...` guard) since adding a source has nothing to do with
                // which view happens to be showing.
                KeyCode::Char('a') => {
                    mode = Mode::AddSource(AddSourceStep::Url { text: String::new(), error: None });
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

/// Whether `w3m` exists on `$PATH` -- checked before `Enter` ever suspends
/// tuxwire's screen, so a missing `w3m` shows `Mode::Error` instead of
/// leaving/re-entering the alternate screen for nothing. Spawns `w3m
/// -version` directly (immediately waited on, output discarded) rather than
/// shelling out to a separate `which`/`command -v` -- that's one real spawn
/// attempt of the exact program `open_in_w3m` is about to run again, so it
/// can't disagree with what happens a moment later, and it works the same
/// on every platform without assuming a `which` binary is present at all.
///
/// `io::ErrorKind::NotFound` is the specific failure that means "no such
/// program to exec"; any other error (permission denied, say) is treated as
/// "available" here, since this check only exists to catch the common "not
/// installed at all" case -- the real launch attempt in `open_in_w3m` right
/// after is what actually matters for anything subtler.
fn w3m_available() -> bool {
    match std::process::Command::new("w3m")
        .arg("-version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(_) => true,
        Err(err) => err.kind() != std::io::ErrorKind::NotFound,
    }
}

/// Suspends tuxwire's screen, runs `w3m` full-screen against `url`, and
/// resumes tuxwire exactly where it left off once the user quits w3m (its
/// own `q` key) -- Roadmap Option A's in-terminal article reading. Same
/// suspend/resume shape as a future `$EDITOR` shell-out for notes would use:
/// leave the alternate screen, hand the real terminal to the child program,
/// and restore the screen on return. `ratatui::try_restore`/`try_init` are
/// the same pair `ratatui::run` itself calls around the whole draw loop
/// (see `run` above) -- reusing them here is genuinely adapting existing
/// plumbing to a new target program, not new architecture.
///
/// Unlike `open_in_browser` (the fallback below), this calls `.status()`
/// -- blocking -- instead of `.spawn()`: there's nothing else for
/// `draw_until_quit` to do while w3m owns the whole terminal, so blocking
/// until the user quits it is exactly right here, not something to work
/// around. Caller is expected to have already checked
/// `w3m_available()`; this doesn't re-check, so a `w3m` that vanishes
/// between that check and this call surfaces as a plain `Err` propagated up
/// through `draw_until_quit`'s `?` rather than a friendly `Mode::Error` --
/// an acceptably narrow gap for that race.
///
/// `*terminal = ratatui::try_init()?` (rather than reusing the old
/// `Terminal`) is what makes the redraw after w3m exits correct: a
/// `Terminal` diffs each frame against its own record of what's already on
/// screen, and that record still says "tuxwire's last frame" even after the
/// alternate screen itself has been left and re-entered (now genuinely
/// blank) out from under it. A fresh `Terminal` starts with a matching
/// blank record, so the very next `terminal.draw` call repaints every cell
/// instead of wrongly believing some are already correct. Re-init happens
/// *before* `status?` propagates any error from running w3m itself, so a
/// failed launch still leaves tuxwire's own screen correctly restored
/// rather than stuck off the alternate screen.
fn open_in_w3m(terminal: &mut ratatui::DefaultTerminal, url: &str) -> anyhow::Result<()> {
    ratatui::try_restore()?;
    let status = std::process::Command::new("w3m").arg(url).status();
    *terminal = ratatui::try_init()?;
    status?;
    Ok(())
}

/// Falls back to the user's `$BROWSER`, or `xdg-open` if `$BROWSER` isn't
/// set (or fails to launch), when `w3m_available()` is `false` -- the
/// pre-`w3m` behavior this restores rather than leaving a missing `w3m` as
/// a dead end. Returns whether something was actually launched, so the
/// `Enter` handler above only falls through to `Mode::Error` once *this*
/// has failed too, not just `w3m`.
///
/// `.spawn()` (rather than `.status()`, which `open_in_w3m` uses) starts
/// the browser process and immediately returns without waiting for it to
/// exit -- unlike w3m, a `$BROWSER`/`xdg-open` launch doesn't take over
/// this terminal, so there's nothing to block the draw loop on. Stdio is
/// redirected to `/dev/null` so a browser that's actually a terminal
/// program (some `$BROWSER` values are, e.g. `lynx`) can't fight with
/// tuxwire over control of the same terminal, which is still in raw mode /
/// the alternate screen at this point.
fn open_in_browser(url: &str) -> bool {
    let spawn = |program: &str| {
        std::process::Command::new(program)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
    };

    if let Ok(browser) = std::env::var("BROWSER") {
        if spawn(&browser) {
            return true;
        }
    }

    spawn("xdg-open")
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
    for source in fetchers::configured_sources()?.into_iter().filter(|source| source.topic() == topic) {
        let fetch_result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(source.fetch())
        });

        // Same reasoning as `main.rs`'s initial fetch loop: one source
        // failing (a dead feed, or a `type = "reddit"` source that isn't
        // implemented yet) must not make `r` fail to refresh every *other*
        // source filed under this topic. `eprintln!` rather than `?`
        // records the failure without aborting this loop -- there's no
        // status line yet to surface it inside the TUI itself (see
        // ARCHITECTURE.md's footer spec), so this is only visible if
        // stderr is redirected somewhere, but it's still strictly better
        // than losing the rest of this topic's refresh over it.
        let fetched = match fetch_result {
            Ok(fetched) => fetched,
            Err(err) => {
                eprintln!("skipped {}: {err:#}", source.name());
                continue;
            }
        };

        for article in &fetched {
            storage.insert_article(article)?;
        }
    }

    storage.articles_by_topic(topic)
}

/// Advances the `a` add-source flow by one keypress: `step` is the state
/// `Mode::AddSource` was carrying when `key` was pressed, and the returned
/// `Mode` is what it should become next -- either still `Mode::AddSource`
/// (with an updated `AddSourceStep`, mid-flow) or `Mode::Normal` (the flow
/// finished, by cancelling or by successfully confirming).
///
/// `topics` doubles as both "the existing topics to offer on the confirm
/// screen" (per ARCHITECTURE.md, "existing topics -- queried live via
/// `Storage::topics()`") and the write target for a brand-new one: it's
/// the very same `Vec` `run` originally populated from
/// `Storage::topics()`, kept live in `draw_until_quit`'s loop state rather
/// than re-queried here, so a successful confirm can push a new topic
/// directly onto it (re-sorted) and have the sidebar reflect it right
/// away -- see the comment on `run`'s own `let mut topics` for why that
/// has to be a real, in-place mutation rather than something the caller
/// re-derives afterward.
///
/// This function only ever touches `sources.toml` on a successful
/// confirm (via `fetchers::config::add_source`, see its own doc comment
/// for why nothing here needs to separately "reload" the in-memory
/// fetcher list) -- every other keypress just edits `step`'s fields in
/// memory.
fn handle_add_source_key(key: KeyCode, step: AddSourceStep, topics: &mut Vec<String>) -> anyhow::Result<Mode> {
    match step {
        AddSourceStep::Url { mut text, error } => match key {
            KeyCode::Esc => Ok(Mode::Normal),

            KeyCode::Backspace => {
                text.pop();
                Ok(Mode::AddSource(AddSourceStep::Url { text, error: None }))
            }

            KeyCode::Char(c) => {
                text.push(c);
                Ok(Mode::AddSource(AddSourceStep::Url { text, error: None }))
            }

            // Per ARCHITECTURE.md's Adding Sources section: "tuxwire
            // validates it by trying to parse it... If it parses
            // successfully, that *is* the proof it's a valid feed." This
            // arm is that validation, end to end -- a real network fetch
            // plus a real `feed-rs` parse, the exact same path
            // `RssFetcher::fetch` uses for every regular refresh (see
            // `fetch_feed`'s own doc comment), just not yet wrapped in an
            // `RssFetcher` since there's no confirmed name/topic for one
            // yet.
            //
            // `draw_until_quit` runs synchronously, but `fetch_feed` is
            // `async` (real I/O) -- `tokio::task::block_in_place` +
            // `Handle::current().block_on(...)` is the same "drive an
            // async call from inside a sync call site, without blocking
            // the whole runtime" trick `refresh_topic` above already uses
            // for the `r` keybind; see its doc comment for the full
            // explanation of why this works.
            KeyCode::Enter => {
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    return Ok(Mode::AddSource(AddSourceStep::Url {
                        text,
                        error: Some("enter a feed URL first".to_string()),
                    }));
                }

                let fetch_result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(fetchers::rss::fetch_feed(trimmed))
                });

                match fetch_result {
                    Ok(feed) => Ok(Mode::AddSource(AddSourceStep::Confirm {
                        name: guess_source_name(&feed, trimmed),
                        url: trimmed.to_string(),
                        topic: String::new(),
                        field: ConfirmField::Name,
                        topic_options: topics.clone(),
                        error: None,
                    })),
                    // Per ARCHITECTURE.md: "show a clear error ('couldn't
                    // parse this as a feed -- check the URL') and let the
                    // user retry or cancel." `text` (not `trimmed`) is
                    // kept so the popup still shows exactly what they
                    // typed, whitespace included, ready to edit rather
                    // than retype from scratch.
                    Err(err) => Ok(Mode::AddSource(AddSourceStep::Url {
                        text,
                        error: Some(format!("couldn't parse this as a feed -- check the URL ({err:#})")),
                    })),
                }
            }

            _ => Ok(Mode::AddSource(AddSourceStep::Url { text, error })),
        },

        AddSourceStep::Confirm { url, mut name, mut topic, mut field, topic_options, error } => match key {
            KeyCode::Esc => Ok(Mode::Normal),

            KeyCode::Tab => {
                field = match field {
                    ConfirmField::Name => ConfirmField::Topic,
                    ConfirmField::Topic => ConfirmField::Name,
                };
                Ok(Mode::AddSource(AddSourceStep::Confirm {
                    url,
                    name,
                    topic,
                    field,
                    topic_options,
                    error: None,
                }))
            }

            // Only meaningful while `Topic` has focus -- cycling "pick an
            // existing topic" makes no sense while typing the source's
            // `name`. Guarded here (rather than in `render`) so `Up`/`Down`
            // simply do nothing while `Name` is focused, instead of
            // needing a separate "is this key even valid right now" check
            // anywhere else.
            KeyCode::Up if matches!(field, ConfirmField::Topic) && !topic_options.is_empty() => {
                topic = cycle_topic(&topic_options, &topic, -1);
                Ok(Mode::AddSource(AddSourceStep::Confirm {
                    url,
                    name,
                    topic,
                    field,
                    topic_options,
                    error: None,
                }))
            }
            KeyCode::Down if matches!(field, ConfirmField::Topic) && !topic_options.is_empty() => {
                topic = cycle_topic(&topic_options, &topic, 1);
                Ok(Mode::AddSource(AddSourceStep::Confirm {
                    url,
                    name,
                    topic,
                    field,
                    topic_options,
                    error: None,
                }))
            }

            KeyCode::Backspace => {
                match field {
                    ConfirmField::Name => {
                        name.pop();
                    }
                    ConfirmField::Topic => {
                        topic.pop();
                    }
                }
                Ok(Mode::AddSource(AddSourceStep::Confirm {
                    url,
                    name,
                    topic,
                    field,
                    topic_options,
                    error: None,
                }))
            }

            // Typing while `Topic` is focused is how a brand new topic
            // gets created -- there's no separate "new topic" mode to
            // switch into first; starting to type just overwrites
            // whatever existing topic `Up`/`Down` had most recently
            // selected, per ARCHITECTURE.md's "pick from existing topics
            // ... or type a new one."
            KeyCode::Char(c) => {
                match field {
                    ConfirmField::Name => name.push(c),
                    ConfirmField::Topic => topic.push(c),
                }
                Ok(Mode::AddSource(AddSourceStep::Confirm {
                    url,
                    name,
                    topic,
                    field,
                    topic_options,
                    error: None,
                }))
            }

            // Confirm: validate both fields are non-empty (ARCHITECTURE.md:
            // "No source can be left without a topic" -- `name` gets the
            // same treatment, since an empty source name would be equally
            // useless), then write the `[[source]]` block. `topics` only
            // gets the new topic pushed onto it *after* `add_source`
            // actually succeeds -- the in-memory sidebar list must never
            // show a topic that didn't really make it into `sources.toml`.
            KeyCode::Enter => {
                let trimmed_name = name.trim();
                let trimmed_topic = topic.trim();

                if trimmed_name.is_empty() {
                    return Ok(Mode::AddSource(AddSourceStep::Confirm {
                        url,
                        name,
                        topic,
                        field,
                        topic_options,
                        error: Some("name can't be empty".to_string()),
                    }));
                }
                if trimmed_topic.is_empty() {
                    return Ok(Mode::AddSource(AddSourceStep::Confirm {
                        url,
                        name,
                        topic,
                        field,
                        topic_options,
                        error: Some("every source needs a topic".to_string()),
                    }));
                }

                match fetchers::config::add_source(
                    trimmed_name,
                    fetchers::config::SourceType::Rss,
                    &url,
                    trimmed_topic,
                ) {
                    Ok(()) => {
                        if !topics.iter().any(|existing| existing == trimmed_topic) {
                            topics.push(trimmed_topic.to_string());
                            topics.sort();
                        }
                        Ok(Mode::Normal)
                    }
                    Err(err) => Ok(Mode::AddSource(AddSourceStep::Confirm {
                        url,
                        name,
                        topic,
                        field,
                        topic_options,
                        error: Some(format!("failed to save source: {err:#}")),
                    })),
                }
            }

            _ => Ok(Mode::AddSource(AddSourceStep::Confirm {
                url,
                name,
                topic,
                field,
                topic_options,
                error,
            })),
        },
    }
}

/// Picks the next (`direction > 0`) or previous (`direction < 0`) topic in
/// `options`, wrapping around at either end -- the `Confirm` screen's
/// `Up`/`Down` handling. If `current` doesn't match any entry in
/// `options` (the user had been free-typing a new topic, or this is the
/// very first press), starts from the first entry going forward or the
/// last one going backward, rather than requiring `current` to already be
/// a real selection.
fn cycle_topic(options: &[String], current: &str, direction: i32) -> String {
    let len = options.len() as i32;
    let next_index = match options.iter().position(|topic| topic == current) {
        Some(index) => (index as i32 + direction).rem_euclid(len),
        None if direction >= 0 => 0,
        None => len - 1,
    };
    options[next_index as usize].clone()
}

/// Guesses a source name from the feed's own `<title>`, per
/// ARCHITECTURE.md's "guessed name (from the feed's own `<title>`
/// metadata, editable)". Falls back to the URL itself for the rare feed
/// that omits a title entirely (or has only whitespace in it) -- still a
/// reasonable, editable starting point, rather than leaving the name
/// field blank.
fn guess_source_name(feed: &feed_rs::model::Feed, url: &str) -> String {
    feed.title
        .as_ref()
        .map(|title| title.content.trim().to_string())
        .filter(|content| !content.is_empty())
        .unwrap_or_else(|| url.to_string())
}

/// Draws one frame: the topic sidebar + article list side by side, with
/// the keybind footer beneath them -- the layout ARCHITECTURE.md
/// describes ("Left pane: topic list... Right pane: article list...
/// Footer: keybind hints") -- plus the note popup on top of everything
/// else when `mode` is `Mode::EditingNote`. `view` decides what the right
/// pane actually shows (see `render_articles`) and which footer hint line
/// applies (see `render_footer`); the sidebar itself doesn't change shape
/// based on `view` -- see the note on `selected_topic` in `draw_until_quit`
/// for why it keeps showing the last topic selection either way.
///
/// `view` pushes this past clippy's default 7-argument threshold for
/// `too_many_arguments`. Bundling these into a params struct just to quiet
/// the lint would be an abstraction with no other caller and no reuse to
/// justify it -- `render` has exactly one call site (`draw_until_quit`'s
/// `terminal.draw` closure), so the allow below is the more honest fix.
#[allow(clippy::too_many_arguments)]
fn render(
    frame: &mut Frame,
    theme: &Theme,
    topics: &[String],
    selected_topic: Option<&str>,
    articles: &[Article],
    selected_article: Option<usize>,
    mode: &Mode,
    view: View,
) {
    // Painting a plain background-colored block across the whole frame
    // first means every gap between/around the panes below (e.g. if the
    // terminal is wider than 100% + 100%) still shows the theme's
    // background instead of whatever the terminal's own default color is.
    frame.render_widget(
        Block::new().style(Style::new().bg(theme.background)),
        frame.area(),
    );

    // Split the frame vertically into the banner bar (fixed height, exactly
    // `BANNER.len()` rows), the body, and the footer -- `Constraint::Min(0)`
    // on the body claims whatever's left over after the other two fixed-size
    // constraints are satisfied, which is what makes both the banner and the
    // footer pinned to a constant height regardless of terminal size. Per
    // `docs/ARCHITECTURE.md`'s "Banner bar" bullet, this row never changes
    // shape based on topic, view, or selection -- `render_banner` below
    // takes no such state as a parameter at all, so there's nothing here
    // that *could* make it vary from frame to frame.
    let [banner_area, body, footer_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(BANNER.len() as u16),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(frame.area());

    // Then split that body horizontally into the sidebar and the article
    // list. A quarter of the width is plenty for topic names, which tend
    // to be short (`linux-news`, `gaming`, ...) compared to article
    // titles.
    let [sidebar_area, articles_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .areas(body);

    render_banner(frame, theme, banner_area);
    render_sidebar(frame, theme, sidebar_area, topics, selected_topic);
    render_articles(
        frame,
        theme,
        articles_area,
        selected_topic,
        articles,
        selected_article,
        view,
    );
    render_footer(frame, theme, footer_area, mode, view);

    // The note popup, if open, paints on top of everything drawn above --
    // see `render_note_popup` for why it has to come last (after the panes
    // it's meant to sit over) and why it lives inside `body` rather than
    // the full frame. The add-source and error popups follow the same
    // reasoning; `mode` can only ever be *one* of these three at a time (see
    // `Mode` itself), so at most one of the three `if let`s below actually
    // draws anything.
    if let Mode::EditingNote { text } = mode {
        render_note_popup(frame, theme, body, text);
    }
    if let Mode::AddSource(step) = mode {
        render_add_source_popup(frame, theme, body, step);
    }
    if let Mode::Error { message } = mode {
        render_error_popup(frame, theme, body, message);
    }
}

/// The top banner bar (`docs/ARCHITECTURE.md` § 3. TUI): the `tuxwire`
/// figlet wordmark (`BANNER`), spanning the full terminal width, styled in
/// `theme.accent_unread`. Same pattern as Claude Code's own terminal header
/// -- a reserved top row that never scrolls, resizes, or changes regardless
/// of topic, view, or selection state, which is why this function -- unlike
/// `render_sidebar`/`render_articles` right below it -- takes no such state
/// as a parameter at all: there's nothing here that varies frame to frame
/// except the theme color.
///
/// `Alignment::Center` (rather than left-flush at column 0) is what gives
/// the bar "room to spare" either side of the wordmark per
/// `docs/ARCHITECTURE.md`'s note that full width means the 37-char banner
/// "fits comfortably" -- centering is what actually uses that spare room
/// instead of leaving it all bunched up on the right.
fn render_banner(frame: &mut Frame, theme: &Theme, area: Rect) {
    let lines: Vec<Line> = BANNER
        .iter()
        .map(|line| Line::styled(*line, Style::new().fg(theme.accent_unread)))
        .collect();

    let banner = Paragraph::new(lines)
        .style(Style::new().bg(theme.background))
        .alignment(Alignment::Center);

    frame.render_widget(banner, area);
}

/// The left pane, three stacked sections top to bottom (`docs/ARCHITECTURE.md`
/// § 3. TUI): the topic list itself (interactive, sized to fill whatever
/// space the other two don't need), then the static "Keys" and "Colors"
/// reference sections. This function takes no `View` -- unlike
/// `render_articles`, which shows a genuinely different list/title
/// depending on `View::Topic` vs `View::Saved`, every part of the sidebar
/// is identical in both: the topic list keeps showing the last topic
/// selection either way (see the comment on `selected_topic` in
/// `draw_until_quit`), and the Keys/Colors sections below don't depend on
/// `View` at all.
///
/// `Constraint::Length` for the bottom two sections (rather than
/// `Constraint::Min`/`Percentage`) is what makes them a fixed height
/// regardless of terminal size -- six key rows and four color rows, plus
/// two border rows apiece, is always enough content to show in full, so
/// there's no reason to let them grow. `Constraint::Min(0)` on the topics
/// list is what then makes *it* absorb every leftover row instead.
fn render_sidebar(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    topics: &[String],
    selected_topic: Option<&str>,
) {
    let [topics_area, keys_area, colors_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(KEY_HINTS.len() as u16 + 2),
            Constraint::Length(4 + 2),
        ])
        .areas(area);

    render_topics_section(frame, theme, topics_area, topics, selected_topic);
    render_keys_section(frame, theme, keys_area);
    render_colors_section(frame, theme, colors_area);
}

/// The sidebar's top section: every topic in `storage`, with
/// `selected_topic` highlighted using `theme.accent_selected`. Split out of
/// `render_sidebar` itself once that function grew two more sections below
/// this one -- keeps each section's rendering logic self-contained instead
/// of one long function juggling three `Block`s at once.
fn render_topics_section(
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

/// The sidebar's middle section: a static, non-interactive readout of
/// `KEY_HINTS`, one `"<key> <action>"` line per row -- see `KEY_HINTS`'s own
/// doc comment for why this exists. A plain `Paragraph`, not a `List`: there's
/// nothing here to select or scroll, just fixed reference text, so a `List`'s
/// selection/highlight machinery would be pure overhead with no behavior
/// behind it.
fn render_keys_section(frame: &mut Frame, theme: &Theme, area: Rect) {
    let block = Block::new()
        .title(" Keys ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    let lines: Vec<Line> = KEY_HINTS
        .iter()
        .map(|(key, action)| {
            Line::from(vec![
                Span::styled(*key, Style::new().fg(theme.accent_selected).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {action}"), Style::new().fg(theme.text_primary)),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// The sidebar's bottom section: a small colored square next to each article
/// state's label (unread/read/saved/skipped, `docs/ARCHITECTURE.md`'s
/// "Article States & Behavior" table order), so the legend is always right
/// there without having to remember which color means what.
///
/// The colors themselves come straight from `theme.accent_*` -- the very
/// same fields `article_item` above reads to color the article list -- never
/// a hardcoded value here. That's the whole point of this section: if
/// someone swaps `theme.toml` for a different palette (Mocha, Frappé, a
/// fully custom one), this legend updates automatically along with the
/// article list it's explaining, instead of silently going stale.
fn render_colors_section(frame: &mut Frame, theme: &Theme, area: Rect) {
    let block = Block::new()
        .title(" Colors ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    let states: [(&str, Color); 4] = [
        ("unread", theme.accent_unread),
        ("read", theme.accent_read),
        ("saved", theme.accent_saved),
        ("skipped", theme.accent_skipped),
    ];

    let lines: Vec<Line> = states
        .iter()
        .map(|(label, color)| {
            Line::from(vec![
                Span::styled("■ ", Style::new().fg(*color)),
                Span::styled(*label, Style::new().fg(theme.text_primary)),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// The right pane: every article in `articles` (either
/// `storage.articles_by_topic(selected_topic)` or, while `view` is
/// `View::Saved`, `storage.saved_articles()` -- see the `S` keybind's arm
/// in `draw_until_quit` for which), most recent first, styled per its
/// read/skipped/saved state using the matching `theme.accent_*` color --
/// ARCHITECTURE.md's "Article States & Behavior" table. `view` decides the
/// title and whether each item also gets a truncated note preview (see
/// `article_item`) -- the saved view is the one place a note is worth
/// surfacing without pressing `n` first, since "which of these did I leave
/// a note on, and roughly what did it say" is the whole point of browsing
/// it.
fn render_articles(
    frame: &mut Frame,
    theme: &Theme,
    area: Rect,
    selected_topic: Option<&str>,
    articles: &[Article],
    selected_article: Option<usize>,
    view: View,
) {
    let title = match view {
        View::Saved => " Saved Articles ".to_string(),
        View::Topic => match selected_topic {
            Some(topic) => format!(" Articles — {topic} "),
            None => " Articles ".to_string(),
        },
    };

    let block = Block::new()
        .title(title)
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    if articles.is_empty() {
        // An empty topic (or no topics at all) shouldn't render as a
        // blank pane with no explanation -- that looks indistinguishable
        // from a bug. Same logic applies to an empty saved view: "nothing
        // saved yet" is a real, expected state (nobody's pressed `s` yet),
        // not something to leave the user guessing about.
        let message = match view {
            View::Saved => "No saved articles yet -- press s on an article to save it.",
            View::Topic => "No articles yet -- run a fetcher first.",
        };
        let empty = Paragraph::new(message)
            .style(Style::new().fg(theme.text_muted))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let show_note_preview = view == View::Saved;
    let items: Vec<ListItem> = articles
        .iter()
        .map(|article| article_item(theme, article, show_note_preview))
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

/// One article's `ListItem`: the title (colored by state) above a dimmer
/// "source · timestamp" line, plus -- when `show_note_preview` is true and
/// the article actually has a note -- a third line with a truncated
/// preview of it (see `truncate_preview`). `show_note_preview` is a plain
/// `bool` parameter rather than this function reaching for a `View`
/// itself: whether to show the preview is entirely `render_articles`'s
/// call (only `View::Saved` wants it), and `article_item` doesn't need to
/// know *why*, just *whether*.
fn article_item<'a>(theme: &Theme, article: &'a Article, show_note_preview: bool) -> ListItem<'a> {
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

    let mut lines = vec![title_line, meta_line];

    // `article.note.as_deref()` turns `&Option<String>` into
    // `Option<&str>` without cloning the note just to look at it --
    // `if let Some(note) = ...` then only runs at all when
    // `show_note_preview` is set *and* a note actually exists, so an
    // article saved without one still renders as the plain two-line item
    // above instead of a blank or missing third line.
    if show_note_preview && let Some(note) = article.note.as_deref() {
        let preview_line = Line::from(Span::styled(
            format!(
                "  \u{201c}{}\u{201d}",
                truncate_preview(note, NOTE_PREVIEW_CHARS)
            ),
            Style::new().fg(theme.accent_saved),
        ));
        lines.push(preview_line);
    }

    ListItem::new(lines)
}

/// Truncates `text` to at most `max_chars` characters, appending `…` if
/// anything was actually cut off -- the short note preview shown next to
/// each title in the saved view (see `article_item`), since the full note
/// is always one `n` press away in the same edit popup already built for
/// it, and terminal width doesn't allow rendering a full note inline next
/// to every row.
///
/// Counts *characters*, not bytes, via `str::chars()` -- `String`/`&str`
/// in Rust are UTF-8 under the hood, where one character (an accented
/// letter, an emoji) can take more than one byte. Slicing by byte index
/// instead (e.g. `&text[..max_chars]`) risks cutting a multi-byte
/// character in half, which panics at runtime rather than producing
/// garbled output the way it might in a language that allows it silently.
///
/// The `chars.next().is_some()` check after collecting the first
/// `max_chars` is what tells apart "this note was exactly `max_chars`
/// characters, nothing to truncate" from "there was more text after
/// this" -- `Iterator::by_ref()` is what makes that possible: it borrows
/// the iterator instead of consuming it outright, so `.take(max_chars)`
/// only advances it that far, leaving whatever's left over still
/// reachable via `chars.next()` on the next line.
fn truncate_preview(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let preview: String = chars.by_ref().take(max_chars).collect();

    if chars.next().is_some() {
        format!("{preview}…")
    } else {
        preview
    }
}

/// The footer: the keybind hint line, covering exactly the keys wired up
/// so far -- see `KEYBIND_HINTS`/`SAVED_HINTS`/`EDITING_NOTE_HINTS` and
/// this module's top-level doc comment for which parts of ARCHITECTURE.md's
/// full keybind table that excludes. `mode` takes priority over `view`:
/// while the note popup or the add-source popup is open, every other
/// keybind (navigation included) is suspended for the duration (see the
/// `Mode::EditingNote` match arm and the `Mode::AddSource` check in
/// `draw_until_quit`), so one of `EDITING_NOTE_HINTS` /
/// `ADD_SOURCE_URL_HINTS` / `ADD_SOURCE_CONFIRM_HINTS` shows regardless of
/// which view sits underneath it. Otherwise, `view` picks between the two
/// normal-mode hint lines.
fn render_footer(frame: &mut Frame, theme: &Theme, area: Rect, mode: &Mode, view: View) {
    let hints = match mode {
        Mode::EditingNote { .. } => EDITING_NOTE_HINTS,
        Mode::AddSource(AddSourceStep::Url { .. }) => ADD_SOURCE_URL_HINTS,
        Mode::AddSource(AddSourceStep::Confirm { .. }) => ADD_SOURCE_CONFIRM_HINTS,
        Mode::Error { .. } => ERROR_HINTS,
        Mode::Normal => match view {
            View::Topic => KEYBIND_HINTS,
            View::Saved => SAVED_HINTS,
        },
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

/// The error popup: same "bordered box centered over `area`" idea as
/// `render_note_popup` above, but read-only -- no cursor, dismissed by any
/// keypress (see the `Mode::Error` check in `draw_until_quit`) rather than
/// an `Enter`/`Esc` split. `Wrap { trim: true }` lets a longer message (e.g.
/// `open_in_w3m`'s "not installed" text) flow across multiple lines instead
/// of running off the popup's fixed width.
fn render_error_popup(frame: &mut Frame, theme: &Theme, area: Rect, message: &str) {
    let popup_area = centered_rect(area, 60, 5);

    // Same reasoning as `render_note_popup`'s own `Clear` -- without it the
    // article list underneath would show through around the popup's text.
    frame.render_widget(Clear, popup_area);

    let block = Block::new()
        .title(" Error ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.accent_selected));

    let paragraph = Paragraph::new(message)
        .style(Style::new().fg(theme.text_primary))
        .wrap(Wrap { trim: true })
        .block(block);
    frame.render_widget(paragraph, popup_area);
}

/// The add-source popup: same "bordered box centered over `area`" idea as
/// `render_note_popup` above, but with two different layouts depending on
/// `step` -- a single line for the URL prompt, or four lines (plus an
/// optional error line) for the name/topic confirm screen. Both branches
/// place the real terminal cursor at the end of whichever field is
/// currently being typed into, same reasoning as `render_note_popup`'s own
/// cursor placement.
fn render_add_source_popup(frame: &mut Frame, theme: &Theme, area: Rect, step: &AddSourceStep) {
    match step {
        AddSourceStep::Url { text, error } => {
            // One line for the typed URL, plus one more if there's an
            // error to show beneath it; `+ 2` for the box's own top/bottom
            // border, same accounting `centered_rect`'s caller always does.
            let height = if error.is_some() { 4 } else { 3 };
            let popup_area = centered_rect(area, 70, height);

            // See `render_note_popup`'s comment on `Clear` -- same reason
            // it's needed here: without it, the article list underneath
            // would show through anywhere this popup's own background
            // doesn't happen to overwrite.
            frame.render_widget(Clear, popup_area);

            let block = Block::new()
                .title(" Add Source — Feed URL ")
                .borders(Borders::ALL)
                .style(Style::new().bg(theme.background).fg(theme.text_primary))
                .border_style(Style::new().fg(theme.accent_selected));

            let mut lines = vec![Line::from(text.as_str())];
            if let Some(message) = error {
                lines.push(Line::from(Span::styled(
                    message.as_str(),
                    Style::new().fg(theme.accent_breaking),
                )));
            }

            let paragraph = Paragraph::new(lines)
                .style(Style::new().fg(theme.text_primary))
                .block(block);
            frame.render_widget(paragraph, popup_area);

            let cursor_x = popup_area.x + 1 + text.chars().count() as u16;
            let cursor_y = popup_area.y + 1;
            frame.set_cursor_position((cursor_x, cursor_y));
        }

        AddSourceStep::Confirm { url, name, topic, field, topic_options, error } => {
            // Four content lines (URL, Name, Topic, the "existing topics"
            // hint) always show; a fifth appears only when there's a
            // validation/write error to surface. `+ 2` for the border,
            // same as the `Url` branch above.
            let height = if error.is_some() { 7 } else { 6 };
            let popup_area = centered_rect(area, 70, height);

            frame.render_widget(Clear, popup_area);

            let block = Block::new()
                .title(" Add Source — Confirm ")
                .borders(Borders::ALL)
                .style(Style::new().bg(theme.background).fg(theme.text_primary))
                .border_style(Style::new().fg(theme.accent_selected));

            // The focused field (per `field`) is highlighted in
            // `accent_selected` so it's visually obvious which one `Tab`
            // last landed on and which one typed characters/`Backspace`
            // will affect -- everything else stays `text_primary`.
            let focus_style = |this_field: ConfirmField| {
                if *field == this_field {
                    Style::new().fg(theme.accent_selected)
                } else {
                    Style::new().fg(theme.text_primary)
                }
            };

            let mut lines = vec![
                Line::from(Span::styled(format!("URL:   {url}"), Style::new().fg(theme.text_muted))),
                Line::from(Span::styled(format!("Name:  {name}"), focus_style(ConfirmField::Name))),
                Line::from(Span::styled(format!("Topic: {topic}"), focus_style(ConfirmField::Topic))),
                Line::from(Span::styled(
                    format!("       existing: {}", topic_options.join(", ")),
                    Style::new().fg(theme.text_muted),
                )),
            ];
            if let Some(message) = error {
                lines.push(Line::from(Span::styled(
                    message.as_str(),
                    Style::new().fg(theme.accent_breaking),
                )));
            }

            let paragraph = Paragraph::new(lines)
                .style(Style::new().fg(theme.text_primary))
                .block(block);
            frame.render_widget(paragraph, popup_area);

            // "Name:  " and "Topic: " are both exactly 7 characters wide
            // (see the `format!`s above), so the same `label_width`
            // positions the cursor correctly after either label -- the
            // cursor's *row* is what actually depends on `field`.
            let label_width: u16 = 7;
            let (field_text, row) = match field {
                ConfirmField::Name => (name.as_str(), 2),
                ConfirmField::Topic => (topic.as_str(), 3),
            };
            let cursor_x = popup_area.x + 1 + label_width + field_text.chars().count() as u16;
            let cursor_y = popup_area.y + row;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
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

// `#[cfg(test)]` -- see the same note on `storage/mod.rs`'s `tests` module
// for why this doesn't add anything to a normal `cargo build`. Only
// `truncate_preview` gets unit tests here: it's the one piece of this
// module's new logic that's a plain, pure function (`&str` in, `String`
// out, no `Storage`/terminal/event dependency) -- exactly the shape that's
// cheap to test directly, unlike `draw_until_quit`'s event loop, which
// this project instead verifies with a pty-driven run (see the project's
// own notes on that, not a `#[test]`).
#[cfg(test)]
mod tests {
    use super::*;

    /// Text shorter than the limit comes back untouched, with no `…`
    /// appended -- there was nothing to cut off.
    #[test]
    fn truncate_preview_leaves_short_text_untouched() {
        assert_eq!(truncate_preview("short note", 45), "short note");
    }

    /// Text exactly at the limit also comes back untouched -- the
    /// `chars.next().is_some()` check in `truncate_preview` is what tells
    /// "exactly max_chars, nothing left over" apart from "there was more,"
    /// and this is the boundary that distinguishes them.
    #[test]
    fn truncate_preview_leaves_exact_length_text_untouched() {
        let text = "12345";
        assert_eq!(truncate_preview(text, 5), "12345");
    }

    /// Text longer than the limit is cut to exactly `max_chars` characters
    /// with `…` appended.
    #[test]
    fn truncate_preview_truncates_long_text() {
        let text = "this note is definitely longer than the preview limit allows";
        let truncated = truncate_preview(text, 10);
        assert_eq!(truncated, "this note …");
    }

    /// Truncation counts *characters*, not bytes -- a multi-byte character
    /// (here, an accented "é") must count as one unit, and the cut must
    /// never land in the middle of one. Slicing by byte index instead would
    /// either panic (Rust refuses to slice a `&str` mid-character) or, in a
    /// language that allowed it, produce garbled output.
    #[test]
    fn truncate_preview_counts_chars_not_bytes() {
        let text = "café résumé";
        let truncated = truncate_preview(text, 4);
        assert_eq!(truncated, "café…");
    }
}
