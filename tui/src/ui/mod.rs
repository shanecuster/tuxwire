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
//!
//! The sidebar is a flat feed list, laid out like the Android app's home
//! screen (`docs/ARCHITECTURE.md`'s "Left pane" bullet): an "All feeds" row,
//! then every feed A-Z, each drawn as its name, its unread count and its
//! `[category]` (`render_feeds_section`). Feed names come from
//! `sources_by_topic`, grouped once from the `ConfiguredSource` list `main`
//! already loaded, no new `sources.toml` read. `Up`/`Down` walk the rows
//! (`sidebar_rows`, `move_sidebar_selection`); `Tab`/`Shift+Tab` cycle a
//! category filter (`next_category_filter`) that keeps only one category's
//! feeds and turns the first row into "All [category]". The row the cursor
//! is on decides the article list: every article, a category's, or one
//! feed's (`load_articles_for_selection`, `selection_topic`).
//!
//! Notes retrieval & search (`docs/ARCHITECTURE.md`'s "Notes Retrieval &
//! Search"): `View::Saved`'s meta line now shows `saved_at` (falling back
//! to the article's publish `timestamp` for a pre-migration-003 row that
//! predates tuxwire tracking `saved_at` at all) plus `noted_at` when it
//! differs meaningfully from `saved_at` -- see `saved_meta_line`. `y` adds
//! a third main view, `View::History`, over `Storage::all_articles` --
//! every article ever fetched, regardless of read/skipped/saved state --
//! reusing the exact same list rendering and navigation `View::Saved`
//! already has (`articles_for_view` is now the one place every
//! view-switching keybind goes to load a view's own list). `/` opens
//! `Mode::Searching`, a live substring filter (`Storage::search_articles`,
//! plain `LIKE`) across every article's title and note -- unscoped by
//! `view`, since the point is finding something regardless of which list
//! happens to be open -- re-run on every keystroke and rendered as an
//! overlay on the article pane's own title/border (`SearchStatus`) rather
//! than a popup, so the live-filtered results stay visible underneath
//! while typing; `Enter` commits the query and returns to ordinary
//! navigation over the (still-filtered) results, `Esc` drops it and
//! restores whichever view's own unfiltered list was showing.
//!
//! Themes (`crate::theme`): `draw_until_quit` owns the `ThemeSettings`
//! (selected built-in + `theme.toml` overrides) and the `Theme` they resolve
//! to. Every frame borrows that `Theme`, so nothing here hard-codes a color.
//! `t` opens `Mode::ThemePicker`, which previews by re-resolving the theme
//! as the selection moves; `Enter` saves `base` to `theme.toml`, and `Esc`
//! restores the original.
//!
//! Icons (`crate::icons`): every symbol this module draws comes from an
//! `IconSettings` by name (`Icon::Unread`, `Icon::New`, a topic's
//! category icon, ...), never a hard-coded glyph. Article rows get a state
//! icon (`article_state_icon`), topic rows get a category icon, the
//! "Colors" legend shows each state's icon, and refresh/error/offline/new
//! results appear as a one-shot footer `notice` (`fetch_notice`). `i`
//! cycles the icon mode live, and the first launch opens on
//! `Mode::IconCheck` ("Do you see an icon here?").

use crate::export::{self, ExportConfig};
use crate::feedlists::{self, FeedLists, Switch};
use crate::fetchers::{self, ConfiguredSource};
use crate::icons::{self, Icon, IconMode, IconSettings};
use crate::sync_cli;
use crate::theme::{self, BUILTIN_THEMES, Theme, ThemeSettings, picker_order};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use tuxwire_core::fetch_report::FetchReport;
use tuxwire_core::models::Article;
use tuxwire_core::storage::{FeedUnread, ReadLaterItem, Scope, Storage};

mod help;
mod read_later;
mod stats;

/// Everything else `main.rs` hands `ui::run` from startup: the theme and
/// icon settings, whether to open with the first-run icon check, and what
/// the startup fetch found. Bundled into one struct so `ui::run` gains one
/// parameter for all of it instead of four.
pub struct SessionStart {
    /// The selected built-in theme plus the user's color overrides, from
    /// `theme.toml`. Moved into `draw_until_quit`, which owns it so the
    /// `t` theme picker can switch the base theme in place.
    pub themes: ThemeSettings,
    /// The icon mode and category icons from `icons.toml` (or the
    /// defaults). Moved into `draw_until_quit`, which owns it from then
    /// on so the `i` key can change the mode in place.
    pub icons: IconSettings,
    /// `true` when `icons.toml` doesn't exist yet: open on
    /// `Mode::IconCheck` ("Do you see an icon here?") instead of
    /// `Mode::Normal`.
    pub show_icon_check: bool,
    /// The startup fetch's results: which articles are new this session
    /// (they get `Icon::New` while unread), plus the failure counts for
    /// the first footer message (see `fetch_notice`).
    pub report: FetchReport,
    /// Every feed list from `lists.toml` (or the single implicit one).
    /// Moved into `draw_until_quit`, which uses it for the `f` picker, the
    /// footer path, and `r`/`a` on the active list's file.
    pub feed_lists: FeedLists,
    /// Which of `feed_lists` is active: an index into its lists. `main`
    /// already loaded and fetched this list's sources.
    pub active_list: usize,
    /// Why the remembered list wasn't used at startup (a missing or broken
    /// file, or all lists failing), shown as the first footer notice. `None`
    /// when everything loaded as expected.
    pub list_notice: Option<String>,
}

/// Below this terminal width (in columns) the footer never shows the feed
/// list path, however short the hint line is. tuxwire has no other
/// "narrow mode"; this is the footer's own, matching zerohour's 60-column
/// narrow threshold. At that width the hint line alone nearly fills the
/// row.
const NARROW_FOOTER_WIDTH: u16 = 60;

/// Blank cells kept between the footer's hint/notice text on the left and
/// the feed list path on the right, so the two never run together.
const FOOTER_GAP: u16 = 2;

/// The footer hint line shown while the `f` feed list picker
/// (`Mode::ListPicker`) is open.
const LIST_PICKER_HINTS: &str = "j/k or ↑/↓ choose · Enter switch · Esc cancel";

/// The footer hint line shown while the `t` theme picker
/// (`Mode::ThemePicker`) is open.
const THEME_PICKER_HINTS: &str = "j/k or ↑/↓ preview · Enter keep · Esc cancel";

/// The footer hint line shown while the first-run icon check
/// (`Mode::IconCheck`) is open.
const ICON_CHECK_HINTS: &str =
    "y yes, use Nerd Font icons · n no, use plain symbols · Esc ask next time";

/// Keybind hints for the footer's upper line, for the normal (not
/// editing-a-note, not adding-a-source) state while `View::Topic` is
/// showing. Kept as one `const` (rather than inlined into `footer_hints`)
/// so there is exactly one place to keep in sync with the `Mode::Normal`
/// match arms in `draw_until_quit`.
///
/// This is the short list of everyday keys. The full list (including rarer
/// ones like `i` for icon mode and `t` for the theme) is in the `?` Help
/// popup (`help.rs`), which is why `? help` and `q quit` come first: if the
/// terminal is too narrow for the whole line, the end is cut off, and the way
/// to the full list (and the way out) must never be the part that disappears.
const KEYBIND_HINTS: &str = "? help · q quit · Tab category · Enter open · s save · x close · n note · r refresh · a add · p paste · / search · S saved · c sync";

/// The footer hint line shown while `View::Saved` or `View::History` is
/// showing (and no popup is open) -- both non-`Topic` views share this one
/// line, since both disable the same keys for the same reason (the sidebar
/// cursor and `r` refresh mean nothing once the list is "every saved
/// article" or "every article ever fetched"). `E` is listed because
/// exporting is done from the saved view; `Esc`/`Tab` are these views' own
/// way back to `View::Topic`.
const SAVED_HINTS: &str =
    "? help · j/k move · Enter open · u un-save · n note · E export · Esc/Tab back · q quit";

/// The footer hint line shown while the note popup (`Mode::EditingNote`)
/// is open -- every other keybind is suspended while typing a note (see
/// the `Mode::EditingNote` match arm in `draw_until_quit`), so the footer
/// swaps to just these two regardless of which `View` was showing
/// underneath.
const EDITING_NOTE_HINTS: &str = "Enter save note · Esc cancel";

/// The footer hint line shown while a yes/no confirmation popup
/// (`Mode::Confirm`) is open. Only a plain `y` confirms; every other key
/// cancels, so a stray key press can never delete anything.
const CONFIRM_HINTS: &str = "y yes, do it · any other key cancels";

/// The footer hint line shown while the add-source popup is open and on
/// its first step -- prompting for the feed URL itself (see
/// `AddSourceStep::Url`).
const ADD_SOURCE_URL_HINTS: &str = "Enter find the feed & validate · Esc cancel";

/// The footer hint line shown on the add-source popup's second step -- the
/// name/topic confirm screen (see `AddSourceStep::Confirm`).
const ADD_SOURCE_CONFIRM_HINTS: &str =
    "Tab switch field · ↑/↓ pick existing topic · Enter save · Esc cancel";

/// The footer hint line shown while the `Mode::Error` popup is open --
/// there's only one way out of it, unlike the other popups' `Enter`/`Esc`
/// split, since there's no "confirm" action for a plain error message.
/// Also covers the export result popup (see `Mode::Error`'s `title`
/// field) -- both are dismiss-on-any-key, so they share this line too.
const ERROR_HINTS: &str = "any key to dismiss";

/// The footer hint line shown while `Mode::Searching` is open -- every
/// other keybind is suspended while typing a search query, same reasoning
/// as `EDITING_NOTE_HINTS`. "Enter confirm" leaves the filtered results
/// showing and returns to ordinary navigation over them (so `j`/`k`/
/// `Enter`/`s`/`x`/`n` all work against the matches, same as any other
/// view); "Esc cancel" drops the query entirely and restores whichever
/// view's own unfiltered list was showing before `/` was pressed.
const SEARCH_HINTS: &str = "type to filter · Enter confirm · Esc cancel";

/// How many characters of a saved article's note to show in the saved
/// view's list before truncating with `…` -- see `truncate_preview` and
/// its use in `article_item` below. The saved view is a list of many
/// articles at once, each getting at most a couple of terminal rows, so a
/// long note would either wrap unpredictably or crowd out the rows around
/// it; the full text is always one `n` press away in the same edit popup
/// already built for editing it.
const NOTE_PREVIEW_CHARS: usize = 45;

/// How many rows the sidebar's "Colors" legend has (see
/// `render_colors_section`). A named constant because two places must
/// agree on it: the legend's array length and the section's height in
/// `render_sidebar`.
const LEGEND_ROWS: usize = 5;

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
/// list (`Topic`, the default), every saved article across every topic
/// (`Saved`, entered/exited with `S`), or every article tuxwire has ever
/// fetched regardless of state (`History`, entered/exited with `y`). This
/// is a *separate* piece of state from `Mode` below rather than another
/// `Mode` variant: `Mode` tracks whether the note popup (or the search
/// input, or the add-source flow) is open, which can happen while looking
/// at *any* of these views, so folding them into one enum would need a
/// variant for every combination instead of the two independent axes this
/// actually is.
///
/// `PartialEq` is derived so match guards elsewhere in this file can write
/// plain `view == View::Topic` / `view != View::Topic` comparisons; `Clone,
/// Copy` because a `View` is just a three-variant tag with no data of its
/// own, cheap to copy by value rather than worth ever borrowing.
#[derive(Clone, Copy, PartialEq)]
enum View {
    Topic,
    Saved,
    History,
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
    EditingNote {
        text: String,
    },
    /// The first-run icon check: a popup showing one Nerd Font glyph and
    /// asking "Do you see an icon here?". `y` keeps Nerd mode, `n`
    /// switches to Unicode, and either answer is saved to `icons.toml` so
    /// the check never appears again. `Esc` closes it without saving, so it
    /// comes back next launch. Only shown when `icons.toml` doesn't exist
    /// yet (see `SessionStart::show_icon_check`).
    IconCheck,
    /// The `t` theme picker is open. `selected` is the highlighted *row*,
    /// a position in `theme::picker_order()` (alphabetical), not a
    /// `BUILTIN_THEMES` index. The theme on that row is the one being
    /// previewed: the whole screen is already drawn in it, because moving
    /// the selection re-resolves `current_theme` right away. `original` is
    /// the `BUILTIN_THEMES` index to go back to on `Esc`.
    /// `overrides` is how many `theme.toml` `[theme]` slots apply on top of
    /// every built-in, captured when the picker opens so the popup can say
    /// so (see `render_theme_picker_popup`).
    ThemePicker {
        selected: usize,
        original: usize,
        overrides: usize,
    },
    /// The `f` feed list picker is open (only possible with two or more
    /// lists). `selected` is the highlighted row, in `lists.toml` order, the
    /// same order as `rows`. `rows` (name, path, unread count) is built once
    /// when the picker opens (see `list_picker_rows`) rather than on every
    /// frame, since counting unread articles queries the database.
    ListPicker {
        selected: usize,
        rows: Vec<ListRow>,
    },

    /// The `?` Help popup is open, listing every key. `scroll` is how many
    /// lines it is scrolled down (for short terminals). Its keys are handled
    /// in `help.rs`.
    Help {
        scroll: u16,
    },

    /// The `g` Stats screen is open. `stats` is the snapshot taken when it
    /// opened (see `stats.rs`); boxed because it is a large value and `Mode`
    /// is moved around often.
    Stats {
        stats: Box<tuxwire_core::stats::Stats>,
    },

    /// The `L` Read Later list is open. `items` is the list as loaded from the
    /// database when it opened (and reloaded after every change), `selected`
    /// the highlighted row. Its keys are handled in `read_later.rs`.
    ReadLater {
        items: Vec<ReadLaterItem>,
        selected: usize,
    },

    /// The `p` paste-a-link prompt is open: `text` is what has been typed or
    /// pasted, `error` why the last attempt was refused (if it was), and
    /// `from_list` whether it was opened from the Read Later list, so
    /// finishing returns there.
    PasteLink {
        text: String,
        error: Option<String>,
        from_list: bool,
    },

    /// A pasted link was accepted and the question "read it later, or save
    /// it?" is open. `title` is the page's title, or empty if it had none (the
    /// address is then used). `from_list` is as for `PasteLink`.
    LinkChoice {
        url: String,
        title: String,
        from_list: bool,
    },
    /// The `a` "add a new source" flow is open -- see `AddSourceStep` for
    /// which of its two steps is currently showing. Wrapping the step in
    /// its own type (rather than adding more `Mode` variants directly, one
    /// per step) keeps `Mode` itself a flat "which popup, if any" tag, and
    /// lets `AddSourceStep` evolve its own fields independently.
    AddSource(AddSourceStep),

    /// A blocking problem, or a one-off result, that there's no other popup
    /// to show through -- right now that covers "neither `w3m` nor a
    /// `$BROWSER`/`xdg-open` fallback could open the article" (see
    /// `w3m_available`, `open_in_w3m`, and `open_in_browser`) *and* the
    /// outcome of an `E` export (success or failure -- see the
    /// `KeyCode::Char('E')` arm in `draw_until_quit`). Dismissed by any
    /// keypress, same as pressing `Esc` on the others, since there's no
    /// follow-up action to take on either an error or a one-shot result
    /// besides acknowledging it.
    ///
    /// `title` distinguishes the two uses in the popup's own border (`
    /// Error `/` Export `) without needing a second, near-identical `Mode`
    /// variant just to say "same read-only dismiss-on-any-key popup, but
    /// with different heading text" -- a `&'static str` rather than
    /// `String` since every title used is a fixed literal chosen at the
    /// call site, never built at runtime.
    Error {
        title: &'static str,
        message: String,
    },

    /// The `/` search input is open -- `text` is the live query buffer,
    /// re-run against `Storage::search_articles` on every keystroke so the
    /// list underneath updates as the person types (ARCHITECTURE.md's
    /// "Live-filters whichever view is currently open... as the person
    /// types"). Same shape as `EditingNote` above and for the same reason:
    /// there's no valid moment with a live query buffer but no search
    /// active, or vice versa, so the buffer lives *inside* this variant
    /// rather than as a separate `bool` + `String` pair that could drift
    /// out of sync.
    ///
    /// Distinct from the *committed* search filter (`draw_until_quit`'s own
    /// `search: Option<String>`, set once `Enter` confirms this): that's
    /// what stays active while browsing the filtered results afterward,
    /// this is only the moment of actively typing the query.
    Searching {
        text: String,
    },

    /// A yes/no question is open: something would be deleted or replaced,
    /// and nothing happens until the person answers. Only a plain `y`
    /// confirms; any other key cancels (see the `Mode::Confirm` branch in
    /// `draw_until_quit`). `Pending` says *what* is being confirmed and
    /// carries everything needed to carry it out, so the question and the
    /// action cannot drift apart.
    Confirm(Pending),
}

/// What a `Mode::Confirm` popup is asking about. Each variant holds just
/// enough to show the question and to do the action if the answer is yes.
enum Pending {
    /// `u`: remove an article from Saved. If it has a note, the note goes
    /// too (a note and a save are locked together), and the question says so.
    Unsave {
        /// Database id of the article to un-save.
        article_id: i64,
        /// Its title, shown in the question so there is no doubt which one.
        title: String,
        /// Whether it has a note, which changes what the question warns about.
        has_note: bool,
    },
    /// `n` then `Enter` on an emptied box: delete the note but keep the
    /// article saved.
    DeleteNote {
        /// Database id of the article whose note would be deleted.
        article_id: i64,
        /// Its title, shown in the question.
        title: String,
    },
    /// `x` in the Read Later list: take a link off the list. `selected` is
    /// the highlighted row, so the list can be reopened near it afterwards.
    DiscardReadLater {
        /// The address of the link to discard.
        url: String,
        /// Its title, shown in the question.
        title: String,
        /// The row that was highlighted in the list.
        selected: usize,
    },
    /// `E` when the export file already exists: replacing it overwrites
    /// whatever is in the file, including anything edited by hand.
    ReplaceExport {
        /// The file that would be replaced, shown in the question.
        path: std::path::PathBuf,
    },
}

/// One row of the `f` feed list picker, everything it shows for one list.
struct ListRow {
    /// The list's name from `lists.toml`.
    name: String,
    /// Its file, with the home directory shown as `~`.
    path: String,
    /// Unread articles from that list's feeds, or `None` if its file
    /// couldn't be read (the row then says "unavailable").
    unread: Option<i64>,
    /// Whether this is the list currently being viewed.
    active: bool,
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
        /// A short line saying how the feed was found when the address the
        /// person typed was not itself the feed (`fetchers::discover`), shown
        /// in the muted colour under the URL. `None` when they typed the feed
        /// address directly, so nothing needs explaining.
        note: Option<String>,
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
/// `storage: &Storage` is borrowed rather than owned: this function only
/// ever *reads* through it, and taking ownership would force whoever calls
/// `ui::run` to give up their own `Storage` just to display something.
///
/// `session` (see `SessionStart`) is moved in, not borrowed, because
/// `draw_until_quit` changes the theme and icon settings it carries.
///
/// `sources: &[ConfiguredSource]` is `main`'s own already-loaded,
/// already-fetched-from list (the same one it built via
/// `fetchers::configured_sources()` before ever opening the TUI) -- passed
/// in and grouped by topic once, right here, so the sidebar's feed list
/// (see `SidebarRow`/`sidebar_rows`) can list every feed with no new
/// `sources.toml` read and no new database query.
pub fn run(
    storage: &Storage,
    sources: &[ConfiguredSource],
    session: SessionStart,
) -> anyhow::Result<()> {
    // `mut` (and passed to `draw_until_quit` as `&mut` below) because the
    // `a` add-source flow can grow this list: a brand-new topic typed on
    // the confirm screen needs to show up in the sidebar immediately (per
    // ARCHITECTURE.md's "usable immediately without restart"), which means
    // mutating this same `Vec` in place rather than only ever reading it
    // once here at startup.
    // What the topic views cover: everything with one feed list (exactly
    // as before lists existed), or just the active list's feeds.
    let scope = list_scope(&session.feed_lists, sources);
    let mut topics = list_topics(storage, &scope, sources)?;
    // `mut`: switching lists (and adding a source to a list) replaces
    // this with the new list's sources.
    let mut sources_by_topic = group_sources_by_topic(sources);
    let mut feed_addresses = feed_addresses_of(sources);

    // `ratatui::run` (see the doc comment on it in the `ratatui` crate)
    // is the "simplest path" helper: it puts the terminal into raw mode +
    // the alternate screen, hands a `&mut DefaultTerminal` to this
    // closure, and -- critically -- restores the terminal afterwards
    // *no matter how the closure returns*, including on an `Err`. That's
    // exactly the guarantee this function needs: if `draw_until_quit`
    // below hits an error partway through, the user's shell must not be
    // left in raw mode / the alternate screen.
    ratatui::run(|terminal| {
        draw_until_quit(
            terminal,
            storage,
            &mut topics,
            &mut sources_by_topic,
            &mut feed_addresses,
            scope,
            session,
        )
    })
}

/// Which articles the topic views cover for the list whose sources are
/// `sources`: everything (`Scope::All`) when there's only one feed list, so
/// behavior is exactly as before lists existed (including articles from
/// feeds since removed from `sources.toml`). With several lists, only the
/// articles whose `feed_url` is one of these sources' URLs.
fn list_scope(lists: &FeedLists, sources: &[ConfiguredSource]) -> Scope {
    if lists.is_single() {
        Scope::All
    } else {
        Scope::Feeds(
            sources
                .iter()
                .map(|source| source.url().to_string())
                .collect(),
        )
    }
}

/// The sidebar's topics for the active list: every topic that has articles
/// (`Storage::topics`) plus every topic the list's sources are configured
/// with, even ones with no articles yet.
///
/// The configured ones must be included because the feed list shows every
/// configured feed (like the Android home screen). Otherwise a feed that has
/// never fetched anything, or keeps failing, would be missing and could not
/// be selected to refresh. A feed with no articles shows "0 unread" and an
/// empty article list.
///
/// `extend` adds each configured topic, then `sort_names` + `dedup` (which
/// removes *adjacent* duplicates, hence sorting first) keep the list
/// alphabetical (ignoring case) with no repeats.
fn list_topics(
    storage: &Storage,
    scope: &Scope,
    sources: &[ConfiguredSource],
) -> anyhow::Result<Vec<String>> {
    let mut topics = storage.topics(scope)?;
    topics.extend(sources.iter().map(|source| source.topic().to_string()));
    // Always re-sort here, even with one list: the database sorts by raw
    // bytes (every capital letter before any lowercase one), but the
    // sidebar wants a plain A-Z order that ignores case.
    sort_names(&mut topics);
    topics.dedup();
    Ok(topics)
}

/// Sorts `names` A-Z ignoring upper/lower case, so "gaming" comes before
/// "Linux" instead of after it (a plain `sort()` puts every capital letter
/// before every lowercase one). Names that differ only by case fall back to
/// a plain comparison, which keeps equal names next to each other so a
/// following `dedup()` (which only removes *adjacent* repeats) works.
///
/// `names` is `&mut`: the caller's `Vec` is reordered in place.
fn sort_names(names: &mut [String]) {
    // `sort_by` takes a closure that compares two items. `then` only runs
    // its second comparison when the first says they're equal.
    names.sort_by(|a, b| {
        a.to_lowercase()
            .cmp(&b.to_lowercase())
            .then_with(|| a.cmp(b))
    });
}

/// The `f` picker's rows, one per feed list in `lists.toml` order: name,
/// `~`-abbreviated path, and unread count.
///
/// The active list's count uses `active_scope`, which is already known.
/// Every other list's file is *read* to learn its feed URLs (TOML parsing
/// only: nothing is fetched or refreshed), then counted with one SQL
/// query. A file that can't be read gives `unread: None`, so the picker
/// still opens and shows which list is broken.
fn list_picker_rows(
    storage: &Storage,
    lists: &FeedLists,
    active: usize,
    active_scope: &Scope,
    home: Option<&Path>,
) -> Vec<ListRow> {
    lists
        .lists()
        .iter()
        .enumerate()
        .map(|(index, list)| {
            // `.ok()` turns each failure into `None`; `and_then` only
            // continues while there's a value.
            let unread = if index == active {
                storage.unread_count(active_scope).ok()
            } else {
                fetchers::configured_sources_at(&list.path)
                    .ok()
                    .and_then(|sources| {
                        let urls = sources
                            .iter()
                            .map(|source| source.url().to_string())
                            .collect();
                        storage.unread_count(&Scope::Feeds(urls)).ok()
                    })
            };
            ListRow {
                name: list.name.clone(),
                path: crate::paths::abbreviate_home(&list.path, home),
                unread,
                active: index == active,
            }
        })
        .collect()
}

/// Groups every configured source's name under its one topic
/// (`sources.toml`'s "exactly one topic per source" rule -- see
/// ARCHITECTURE.md's Configuration section), built once from the
/// `ConfiguredSource` list `main` already loaded at startup. Source names
/// within a topic are sorted A-Z ignoring case (`sort_names`), so the
/// sidebar is easy to scan. This only changes what the sidebar shows;
/// `sources.toml` itself keeps whatever order the person wrote.
fn group_sources_by_topic(sources: &[ConfiguredSource]) -> HashMap<String, Vec<String>> {
    let mut grouped: HashMap<String, Vec<String>> = HashMap::new();
    for source in sources {
        grouped
            .entry(source.topic().to_string())
            .or_default()
            .push(source.name().to_string());
    }
    // Sort each topic's feed names A-Z. `values_mut` hands out a mutable
    // reference to every `Vec` in the map, so each is reordered in place.
    for names in grouped.values_mut() {
        sort_names(names);
    }
    grouped
}

/// Each configured feed's address, keyed by its topic and name -- the pair a
/// sidebar feed row is known by. Looked up when a feed row is selected so its
/// articles can be found by address (see `load_articles_for_selection`).
type FeedAddresses = HashMap<(String, String), String>;

/// Builds the [`FeedAddresses`] lookup from the loaded sources. Rebuilt
/// wherever `group_sources_by_topic` is, because both describe the same
/// sidebar and must change together (a list switch, a source added).
fn feed_addresses_of(sources: &[ConfiguredSource]) -> FeedAddresses {
    sources
        .iter()
        .map(|source| {
            (
                (source.topic().to_string(), source.name().to_string()),
                source.url().to_string(),
            )
        })
        .collect()
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
///
/// Also owns the sidebar's selection state, which is three values:
/// `category_filter` (`None` lists every feed; `Some(name)` only that
/// category's), `selected_source` (`Some(feed name)` when the cursor is on a
/// feed row, `None` when it is on the first "All" row) and `topic_index`
/// (which topic that feed, or the filtered category, belongs to). Together
/// they say which of three article lists is showing: every article ("All
/// feeds": no filter and no feed), one category's ("All [category]"), or one
/// feed's. `category_filter` is a topic *name*, not an index -- the `a`
/// add-source flow can insert a brand-new topic into `topics` and re-sort
/// the whole `Vec` (see the block handling `Mode::AddSource` finishing,
/// below), which would silently invalidate an index; a name stays correct no
/// matter how `topics` gets reordered.
fn draw_until_quit(
    terminal: &mut ratatui::DefaultTerminal,
    storage: &Storage,
    topics: &mut Vec<String>,
    // `&mut` so switching lists can replace it with the new list's
    // sources (see the `Mode::ListPicker` handling).
    sources_by_topic: &mut HashMap<String, Vec<String>>,
    // Each feed's address by (topic, name); kept in step with
    // `sources_by_topic` and replaced alongside it.
    feed_addresses: &mut FeedAddresses,
    // What the topic views cover (see `list_scope`). Owned and `mut` for
    // the same reason.
    mut scope: Scope,
    session: SessionStart,
) -> anyhow::Result<()> {
    let mut topic_index: usize = 0;
    let mut article_index: usize = 0;
    // The sidebar opens on its "All feeds" row, so the list starts as every
    // article in scope, whatever its topic.
    let mut articles: Vec<Article> = storage.articles_in_scope(&scope)?;
    // Open on the first-run icon check if `icons.toml` doesn't exist yet,
    // otherwise straight into ordinary browsing.
    let mut mode = if session.show_icon_check {
        Mode::IconCheck
    } else {
        Mode::Normal
    };
    let mut view = View::Topic;

    // The icon settings, owned here and `mut` so the `i` key and the
    // first-run check can change `icons.mode` in place. The very next
    // frame then draws with the new mode, so a change applies immediately.
    let mut icons = session.icons;

    // The theme settings (selected built-in + overrides), and the colors
    // they currently resolve to. Both `mut` so the `t` picker can change the
    // base and re-resolve, which repaints the whole screen on the next frame
    // (the picker's live preview). `current_theme` is cached here rather
    // than calling `themes.resolve()` every frame, since it only changes
    // when the picker moves.
    let mut themes = session.themes;
    let mut current_theme = themes.resolve();

    // A one-shot footer message (refresh results, icon mode changes) that
    // replaces the keybind hints until the next keypress clears it, so it
    // never hides them for long. Starts with the startup fetch's own
    // summary when there's something worth saying (see `fetch_notice`).
    //
    // A startup problem with the feed lists (see `SessionStart::list_notice`)
    // comes first, since it explains why a different list is showing.
    // `Option::or_else` only builds the fetch summary when there isn't one.
    let mut notice: Option<String> = session
        .list_notice
        .map(|message| format!("{} {message}", icons.glyph(Icon::Error)))
        .or_else(|| fetch_notice(&icons, &session.report, false));

    // IDs of articles that arrived this session (startup fetch, plus every
    // `r` refresh), drawn with `Icon::New` while still unread. Grows as
    // refreshes add more; never shrinks, since "new this session" stays
    // true until tuxwire quits. Moved out of `session.report` (no copy)
    // because the startup report isn't needed again after `notice` above
    // was built from it.
    let mut new_article_ids = session.report.new_article_ids;

    // The feeds (as `(topic, source name)`) that got new articles and have not
    // been looked at yet; the sidebar draws `Icon::New` beside them and beside
    // their topic. Unlike `new_article_ids` this *shrinks*: a feed leaves the
    // set once the person acts in its article list (see `clear_fresh_feeds`).
    // Kept in memory only; nothing is stored, so a restart starts clean.
    let mut fresh_feeds: HashSet<(String, String)> = HashSet::new();
    note_fresh_feeds(storage, &new_article_ids, &mut fresh_feeds);

    // The feed lists, and which one is active. `active_list` changes when
    // the `f` picker switches; `feed_lists` itself never changes while
    // running (lists are added by editing `lists.toml`).
    let feed_lists = session.feed_lists;
    let mut active_list = session.active_list;

    // The home directory, used to show paths with `~`.
    let home = crate::paths::home_dir();

    // The active list's path for the footer, with the home directory shown
    // as `~`. Converted to text once here (and again on each switch) rather
    // than on every frame.
    let mut feed_list_display =
        crate::paths::abbreviate_home(&feed_lists.lists()[active_list].path, home.as_deref());
    // The sidebar's category filter: `None` lists every feed, `Some(name)`
    // only that category's. `Tab` / `Shift+Tab` cycle it. Kept by name, not
    // index, so it survives the topic list being re-sorted. While it is
    // `Some`, `topic_index` always points at that category.
    let mut category_filter: Option<String> = None;
    let mut selected_source: Option<String> = None;

    // The *committed* search filter, if any -- `Some(query)` once `/` +
    // typing + `Enter` has confirmed one, `None` otherwise. Distinct from
    // `Mode::Searching`'s own live buffer (see that variant's doc comment):
    // this is what stays active while browsing the filtered results
    // afterward, not the moment of typing itself. Read by `render` (via
    // `SearchStatus`) purely for display, and by the `S`/`y`/`Tab`/`Esc`
    // view-switching arms below, which all drop it back to `None` --
    // switching views means "go look at that view's own list," not "keep
    // filtering it."
    let mut search: Option<String> = None;

    // Set by `N` and acted on right after the next frame is drawn, so the
    // footer can say "Refreshing..." *before* the screen goes quiet for the
    // download (the draw loop is plain blocking code, so nothing is painted
    // while a fetch runs). See the `N` arm and the check after `terminal.draw`.
    let mut refresh_all_pending = false;

    loop {
        // A short-lived borrow of this frame's colors, named `theme` so the
        // `render(frame, theme, ...)` call below reads as it always has.
        // Re-taken at the top of each pass, so after the picker assigns a
        // new `current_theme` the next frame borrows that one.
        let theme = &current_theme;

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

        // Every feed's unread count for the sidebar, from one grouped query.
        // A database error just means the counts show as 0 for this frame
        // (`unwrap_or_default` gives an empty list on `Err`).
        let unread_by_feed = feed_unread_map(
            &storage.unread_by_feed(&scope).unwrap_or_default(),
            &*feed_addresses,
        );

        terminal.draw(|frame| {
            render(
                frame,
                theme,
                &icons,
                &new_article_ids,
                &fresh_feeds,
                &unread_by_feed,
                topics.as_slice(),
                sources_by_topic,
                category_filter.as_deref(),
                selected_topic,
                selected_source.as_deref(),
                &articles,
                selected_article,
                &mode,
                view,
                search.as_deref(),
                notice.as_deref(),
                &feed_list_display,
                !feed_lists.is_single(),
            )
        })?;

        // `N` was pressed on the previous pass: the frame just drawn shows
        // "Refreshing...", so the (slow) download can start now. When it ends,
        // `continue` goes round again to draw the result before waiting for a
        // key. `view == View::Topic` and a non-empty `topics` were checked when
        // `N` was pressed, and nothing changes them in between.
        if refresh_all_pending {
            refresh_all_pending = false;
            let (refreshed, report) = refresh_all(
                storage,
                &*feed_addresses,
                &scope,
                &feed_lists.lists()[active_list].path,
                selection_topic(
                    topics,
                    topic_index,
                    selected_source.as_deref(),
                    category_filter.as_deref(),
                ),
                selected_source.as_deref(),
            )?;
            articles = refreshed;
            article_index = article_index.min(articles.len().saturating_sub(1));
            new_article_ids.extend(report.new_article_ids.iter().copied());
            note_fresh_feeds(storage, &report.new_article_ids, &mut fresh_feeds);
            notice = refresh_all_notice(&icons, &report);
            continue;
        }

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

        // Any keypress clears the previous one-shot notice. It has been on
        // screen for at least one frame, and whatever this key does may set
        // a fresh one below.
        notice = None;

        // While the feed list picker is open, every keypress belongs to it.
        // Moving the highlight only moves it (no preview); `Enter` switches.
        // `&mut mode` borrows the variant's fields so `selected` can be
        // changed in place. Once `Enter` has copied `selected` out, that
        // borrow is over, so `mode` itself can be reassigned.
        if let Mode::ListPicker { selected, rows } = &mut mode {
            let count = rows.len();
            match key.code {
                // Wrap around at either end, same as the theme picker.
                KeyCode::Up | KeyCode::Char('k') => *selected = (*selected + count - 1) % count,
                KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1) % count,
                KeyCode::Esc | KeyCode::Char('q') => mode = Mode::Normal,
                KeyCode::Enter => {
                    let target = *selected;
                    mode = Mode::Normal;
                    // Load only the target list's file. On success swap in
                    // its sources and reload the views from the database,
                    // which is instant and fetches nothing (press `r` to
                    // refresh). On failure stay on the current list.
                    match feedlists::try_switch(
                        &feed_lists,
                        active_list,
                        target,
                        home.as_deref(),
                        |list| fetchers::configured_sources_at(&list.path),
                    ) {
                        Switch::Switched { index, loaded } => {
                            active_list = index;
                            *sources_by_topic = group_sources_by_topic(&loaded);
                            *feed_addresses = feed_addresses_of(&loaded);
                            scope = list_scope(&feed_lists, &loaded);
                            *topics = list_topics(storage, &scope, &loaded)?;

                            // A different list means a different sidebar, so
                            // start from its first topic. Saved, History, and
                            // search are global, but returning to the topic
                            // view shows the switch happened.
                            topic_index = 0;
                            selected_source = None;
                            category_filter = None;
                            view = View::Topic;
                            search = None;
                            articles = articles_for_view(
                                storage,
                                &*feed_addresses,
                                &scope,
                                view,
                                topics,
                                topic_index,
                                None,
                                category_filter.as_deref(),
                            )?;
                            article_index = 0;

                            // Footer path, and remember the choice in
                            // `lists.toml`. The switch has already happened
                            // either way; a failed save only means it won't
                            // be remembered next time, which the notice says.
                            let list = &feed_lists.lists()[active_list];
                            feed_list_display =
                                crate::paths::abbreviate_home(&list.path, home.as_deref());
                            notice = Some(match feedlists::config::save_active(&list.name) {
                                Ok(()) => format!(
                                    "{} Feed list: {} (press f to change)",
                                    icons.glyph(Icon::FeedFile),
                                    list.name
                                ),
                                Err(err) => format!(
                                    "{} Switched to {} for now, but saving lists.toml failed: {err:#}",
                                    icons.glyph(Icon::Error),
                                    list.name
                                ),
                            });
                        }
                        Switch::Unchanged => {}
                        Switch::Failed { message } => {
                            notice = Some(format!("{} {message}", icons.glyph(Icon::Error)));
                        }
                    }
                }
                _ => {}
            }
            continue;
        }

        // While the Read Later list, the paste-a-link prompt or the "read
        // later or save?" question is open, every keypress belongs to it. Each
        // handler takes the popup's state by value and returns the next mode
        // (see `read_later.rs`). `std::mem::replace` swaps `Mode::Normal` in
        // and hands back the old value, so the state can be moved out of
        // `mode` without leaving it empty. Afterwards, if the screen is back
        // to normal, the article list is reloaded, because saving a link can
        // have added a saved article.
        // While the Stats screen is open it owns the keyboard: `Esc`, `q` and `g`
        // close it, and the handler hands back the next mode (see `stats.rs`).
        if matches!(mode, Mode::Stats { .. }) {
            mode = match std::mem::replace(&mut mode, Mode::Normal) {
                Mode::Stats { stats } => stats::handle_key(key.code, stats),
                other => other,
            };
            continue;
        }

        if matches!(
            mode,
            Mode::ReadLater { .. } | Mode::PasteLink { .. } | Mode::LinkChoice { .. }
        ) {
            mode = match std::mem::replace(&mut mode, Mode::Normal) {
                Mode::ReadLater { items, selected } => read_later::handle_list_key(
                    key.code,
                    items,
                    selected,
                    storage,
                    terminal,
                    &mut notice,
                )?,
                Mode::PasteLink {
                    text, from_list, ..
                } => read_later::handle_paste_key(key.code, text, from_list, storage)?,
                Mode::LinkChoice {
                    url,
                    title,
                    from_list,
                } => read_later::handle_choice_key(
                    key.code,
                    url,
                    title,
                    from_list,
                    storage,
                    &mut notice,
                )?,
                other => other,
            };
            if matches!(mode, Mode::Normal) {
                articles = articles_for_view(
                    storage,
                    &*feed_addresses,
                    &scope,
                    view,
                    topics,
                    topic_index,
                    selected_source.as_deref(),
                    category_filter.as_deref(),
                )?;
                article_index = article_index.min(articles.len().saturating_sub(1));
            }
            continue;
        }

        // While the theme picker is open, every keypress belongs to it, the
        // same isolation as the other popups. Moving the selection changes
        // `themes.base` and re-resolves `current_theme` immediately, which is
        // the live preview: the next frame paints the whole app in that
        // theme. `if let ... = mode` copies the three `usize` fields out
        // (they're `Copy`), so `mode` itself is free to be reassigned below.
        if let Mode::ThemePicker {
            selected,
            original,
            overrides,
        } = mode
        {
            // The rows in display order (alphabetical), as `BUILTIN_THEMES`
            // indices. `selected` is a position in this list.
            let order = picker_order();
            let count = order.len();
            match key.code {
                // Up/down wrap around the list. Adding `count - 1` and then
                // taking `% count` is "minus one, wrapping" without ever
                // going below zero, which a `usize` can't represent.
                KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                    let next = if matches!(key.code, KeyCode::Up | KeyCode::Char('k')) {
                        (selected + count - 1) % count
                    } else {
                        (selected + 1) % count
                    };
                    // Row → theme: the theme shown on row `next`.
                    themes.base = order[next];
                    current_theme = themes.resolve();
                    mode = Mode::ThemePicker {
                        selected: next,
                        original,
                        overrides,
                    };
                }
                // `Enter` keeps the previewed theme and saves it as `base`
                // in `theme.toml`. Even if saving fails, the theme stays
                // for this session, and the notice says so.
                KeyCode::Enter => {
                    notice = Some(match theme::save_base(themes.base_name()) {
                        Ok(()) => format!(
                            "{} Theme: {} (press t to change)",
                            icons.glyph(Icon::Theme),
                            themes.base_name()
                        ),
                        Err(err) => format!(
                            "{} Theme is {} for now, but saving theme.toml failed: {err:#}",
                            icons.glyph(Icon::Error),
                            themes.base_name()
                        ),
                    });
                    mode = Mode::Normal;
                }
                // `Esc` (or `q`, since quitting from inside a popup would be
                // surprising) puts back the theme that was active when the
                // picker opened, and saves nothing.
                KeyCode::Esc | KeyCode::Char('q') => {
                    themes.base = original;
                    current_theme = themes.resolve();
                    mode = Mode::Normal;
                }
                _ => {}
            }
            continue;
        }

        // While the first-run icon check is open, every keypress belongs to
        // it, the same isolation as the popups below. Checked first because
        // it's the first thing a new user sees.
        if matches!(mode, Mode::IconCheck) {
            // `Some(mode)` for an answer, `None` for "not answered yet".
            let answer = match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => Some(IconMode::Nerd),
                KeyCode::Char('n') | KeyCode::Char('N') => Some(IconMode::Unicode),
                // `Esc` closes the popup without writing `icons.toml`, so
                // the check is asked again next launch.
                KeyCode::Esc => {
                    mode = Mode::Normal;
                    None
                }
                // Anything else is ignored; the popup stays open.
                _ => None,
            };
            if let Some(answer) = answer {
                icons.mode = answer;
                mode = Mode::Normal;
                notice = Some(save_icon_mode_notice(&icons));
            }
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
            // Filled in by `handle_add_source_key` with the new feed's address
            // when (and only when) a source was just saved.
            let mut saved_url: Option<String> = None;
            mode = handle_add_source_key(
                key.code,
                step,
                topics,
                &feed_lists.lists()[active_list].path,
                &mut saved_url,
            )?;

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
                // Re-read the active list's file so a newly added source
                // joins the sidebar's feed list (and, with several lists, its
                // `scope`, otherwise its articles wouldn't appear in this
                // list after the next `r`). `reread` keeps the sources for
                // rebuilding the topic list below.
                let mut reread: Option<Vec<ConfiguredSource>> = None;
                match fetchers::configured_sources_at(&feed_lists.lists()[active_list].path) {
                    Ok(sources) => {
                        *sources_by_topic = group_sources_by_topic(&sources);
                        *feed_addresses = feed_addresses_of(&sources);
                        scope = list_scope(&feed_lists, &sources);
                        reread = Some(sources);
                    }
                    Err(err) => {
                        notice = Some(format!(
                            "{} Couldn't re-read the feed list: {err:#}",
                            icons.glyph(Icon::Error)
                        ));
                    }
                }

                // A source was just saved: fetch its articles now, so the person
                // is not left looking at a feed with nothing in it and thinking
                // the app is broken. Done after the list above was re-read (so
                // the feed is in `scope`) and before `articles` is reloaded
                // below (so the new articles are in that list). A feed that
                // fails to download stays saved; the footer says what happened.
                if let Some(url) = saved_url {
                    match fetch_added_source(storage, &feed_lists.lists()[active_list].path, &url) {
                        Ok(report) => {
                            new_article_ids.extend(report.new_article_ids.iter().copied());
                            note_fresh_feeds(storage, &report.new_article_ids, &mut fresh_feeds);
                            notice = fetch_notice(&icons, &report, true);
                        }
                        Err(err) => {
                            notice = Some(format!(
                                "{} Saved, but couldn't fetch it yet: {err:#}",
                                icons.glyph(Icon::Error)
                            ));
                        }
                    }
                }

                // Rebuild the topic list now that the file was re-read and the
                // new feed's first articles are stored, so a brand-new category
                // appears at once. If the file couldn't be re-read, keep the
                // list `handle_add_source_key` already updated.
                if let Some(sources) = &reread {
                    *topics = list_topics(storage, &scope, sources)?;
                }

                // Topics may have been re-sorted or added, so find the
                // selection again by *name*: the category filter's if there is
                // one, otherwise the previously selected topic's.
                let wanted = category_filter.clone().or(previously_selected_topic);
                topic_index = wanted
                    .and_then(|name| topics.iter().position(|t| *t == name))
                    .unwrap_or(0);

                // The add-source flow has nothing to do with a feed-row
                // selection that may have been active before it opened --
                // always land back on the "All" row (of the filter, if any)
                // rather than trying to re-derive whether that feed selection
                // still makes sense.
                selected_source = None;
                articles = articles_for_view(
                    storage,
                    &*feed_addresses,
                    &scope,
                    view,
                    topics,
                    topic_index,
                    None,
                    category_filter.as_deref(),
                )?;
                article_index = article_index.min(articles.len().saturating_sub(1));
            }
            continue;
        }

        // While a yes/no question is open, the next key is the answer: a
        // plain `y` does the action, anything else cancels it. Checked before
        // every other mode so that no navigation key can ever slip through
        // while a deletion is waiting for its answer.
        if matches!(mode, Mode::Confirm(_)) {
            // `std::mem::replace` swaps `Mode::Normal` in and hands back the
            // old value, so we own the `Pending` and the popup is closed
            // whichever way the answer goes.
            let Mode::Confirm(pending) = std::mem::replace(&mut mode, Mode::Normal) else {
                continue;
            };
            let confirmed = key.code == KeyCode::Char('y');
            match (pending, confirmed) {
                // Yes: remove the save (and the note with it) in the
                // database, then make the on-screen list match.
                (
                    Pending::Unsave {
                        article_id,
                        title,
                        has_note,
                    },
                    true,
                ) => {
                    storage.unsave_article(article_id)?;
                    if view == View::Saved {
                        // The Saved view lists only saved articles, so the
                        // row disappears; keep the selection on the list.
                        articles.retain(|a| a.id != article_id);
                        article_index = article_index.min(articles.len().saturating_sub(1));
                    } else if let Some(a) = articles.iter_mut().find(|a| a.id == article_id) {
                        // Other views keep the row, now drawn as not saved.
                        a.saved = false;
                        a.saved_at = None;
                        a.note = None;
                        a.noted_at = None;
                    }
                    notice = Some(if has_note {
                        format!("Un-saved and deleted its note: {title}")
                    } else {
                        format!("Un-saved: {title}")
                    });
                }
                // No: explain, especially why an article with a note stays.
                (Pending::Unsave { has_note, .. }, false) => {
                    notice = Some(if has_note {
                        "Kept saved: an article with a note stays in Saved. Delete the note first, or press u and confirm to remove both".to_string()
                    } else {
                        "Kept: the article is still saved".to_string()
                    });
                }
                // Yes: clear the note only; the article stays saved.
                (Pending::DeleteNote { article_id, .. }, true) => {
                    storage.update_note(article_id, None)?;
                    if let Some(a) = articles.iter_mut().find(|a| a.id == article_id) {
                        a.note = None;
                        a.noted_at = None;
                    }
                    notice = Some("Note deleted; the article stays saved".to_string());
                }
                (Pending::DeleteNote { .. }, false) => {
                    notice = Some("Note kept".to_string());
                }
                // The Read Later discard question: yes discards, anything else
                // keeps; either way the list is shown again.
                (
                    Pending::DiscardReadLater {
                        url,
                        title,
                        selected,
                    },
                    answer,
                ) => {
                    mode = read_later::resolve_discard(
                        answer,
                        &url,
                        &title,
                        selected,
                        storage,
                        &mut notice,
                    )?;
                }
                // Yes: write the export over the existing file.
                (Pending::ReplaceExport { .. }, true) => {
                    mode = run_export(storage);
                }
                (Pending::ReplaceExport { .. }, false) => {
                    notice = Some("Export cancelled; the file was not changed".to_string());
                }
            }
            continue;
        }

        // While the Help popup is open, its own handler decides what each key
        // does (scroll or close) and hands back the next mode. `terminal.size()`
        // gives the screen size so scrolling stops when the last line shows;
        // the 2 border rows and the banner/footer rows are subtracted.
        if let Mode::Help { scroll } = mode {
            let screen_rows = terminal.size()?.height;
            let visible = screen_rows.saturating_sub(BANNER.len() as u16 + 2 + 2);
            mode = help::handle_key(key.code, scroll, !feed_lists.is_single(), visible);
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

        // While the search input is open, every keypress belongs to it,
        // same isolation reasoning as the note popup just below -- typing
        // "q" or "j" into a search query must type that character, not
        // quit or move the article selection. Checked (and `continue`d)
        // before the `Mode::EditingNote` let-else right after, so a
        // keypress meant for search never falls through into either that
        // popup's handling or ordinary navigation.
        if let Mode::Searching { text } = &mut mode {
            match key.code {
                // `Esc` drops the query entirely (not just clears the
                // buffer) and restores whichever view's own unfiltered
                // list was showing before `/` was pressed --
                // ARCHITECTURE.md's "Esc clears the search and restores
                // the full list."
                KeyCode::Esc => {
                    search = None;
                    mode = Mode::Normal;
                    articles = articles_for_view(
                        storage,
                        &*feed_addresses,
                        &scope,
                        view,
                        topics,
                        topic_index,
                        selected_source.as_deref(),
                        category_filter.as_deref(),
                    )?;
                    article_index = 0;
                }

                // `Enter` confirms the query and returns to ordinary
                // navigation over whatever `articles` currently holds (already
                // live-filtered by the keystrokes below) -- an empty query is
                // treated the same as `Esc`, restoring the view's own
                // unfiltered list, rather than committing to a "search for
                // nothing" that would trivially match everything via `LIKE
                // '%%'`.
                KeyCode::Enter => {
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        search = None;
                        articles = articles_for_view(
                            storage,
                            &*feed_addresses,
                            &scope,
                            view,
                            topics,
                            topic_index,
                            selected_source.as_deref(),
                            category_filter.as_deref(),
                        )?;
                    } else {
                        search = Some(trimmed.to_string());
                        articles = storage.search_articles(trimmed)?;
                    }
                    mode = Mode::Normal;
                    article_index = 0;
                }

                // `Backspace`/`Char` both re-run the search on every
                // keystroke -- ARCHITECTURE.md's "Live-filters... as the
                // person types" -- rather than waiting for `Enter`.
                // `String::pop` is UTF-8-aware, same reasoning as the note
                // popup's own `Backspace` handling below.
                KeyCode::Backspace => {
                    text.pop();
                    articles = storage.search_articles(text)?;
                    article_index = 0;
                }
                KeyCode::Char(c) => {
                    text.push(c);
                    articles = storage.search_articles(text)?;
                    article_index = 0;
                }

                _ => {}
            }
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
                    clear_fresh_for_selection(
                        &mut fresh_feeds,
                        topics,
                        topic_index,
                        selected_source.as_deref(),
                        category_filter.as_deref(),
                    );
                }
                KeyCode::Char('k') if !articles.is_empty() => {
                    article_index = article_index.saturating_sub(1);
                    clear_fresh_for_selection(
                        &mut fresh_feeds,
                        topics,
                        topic_index,
                        selected_source.as_deref(),
                        category_filter.as_deref(),
                    );
                }

                // `Up`/`Down` move the sidebar cursor through the feed list --
                // the "All" row first, then every feed A-Z (see
                // `sidebar_rows`) -- clamping at either end (same as `j`/`k`
                // above) rather than wrapping. `move_sidebar_selection`
                // rewrites `topic_index`/`selected_source` in place to
                // wherever the cursor lands, and `articles_for_view` then
                // reloads `articles` from *that*: every article ("All"), a
                // category's, or one feed's. Guarded to `View::Topic` only --
                // there's no sidebar cursor to move while `View::Saved` is
                // showing, and reloading `articles` here would silently
                // clobber the saved list with a feed's instead.
                KeyCode::Up | KeyCode::Down if view == View::Topic && !topics.is_empty() => {
                    // `-1` moves up, `1` moves down.
                    let direction = if key.code == KeyCode::Up { -1 } else { 1 };
                    move_sidebar_selection(
                        direction,
                        topics,
                        sources_by_topic,
                        category_filter.as_deref(),
                        &mut topic_index,
                        &mut selected_source,
                    );
                    articles = articles_for_view(
                        storage,
                        &*feed_addresses,
                        &scope,
                        view,
                        topics,
                        topic_index,
                        selected_source.as_deref(),
                        category_filter.as_deref(),
                    )?;
                    article_index = 0;
                }

                // `Tab` and `Shift+Tab` (`BackTab`) do double duty depending
                // on `view`. In `View::Topic` they cycle the sidebar's
                // category filter: all feeds, then each category A-Z, then
                // back to all feeds (`next_category_filter`), landing on the
                // "All" row of the new filter. In `View::Saved` or
                // `View::History` they instead exit back to the topic view,
                // per ARCHITECTURE.md's "pressing S again (or Esc, or Tab)
                // returns to the normal topic view" (the same holds for
                // `y`/History).
                KeyCode::Tab | KeyCode::BackTab => {
                    if view != View::Topic {
                        view = View::Topic;
                        selected_source = None;
                        search = None;
                        articles = articles_for_view(
                            storage,
                            &*feed_addresses,
                            &scope,
                            view,
                            topics,
                            topic_index,
                            None,
                            category_filter.as_deref(),
                        )?;
                        article_index = 0;
                    } else if !topics.is_empty() {
                        // `Tab` goes forward, `Shift+Tab` backward.
                        category_filter = next_category_filter(
                            topics,
                            category_filter.as_deref(),
                            key.code == KeyCode::Tab,
                        );
                        // Back on the "All" row of whatever the filter is now.
                        selected_source = None;
                        // With a filter, `topic_index` must point at that
                        // category so "All [category]" loads its articles.
                        if let Some(name) = &category_filter
                            && let Some(index) = topics.iter().position(|topic| topic == name)
                        {
                            topic_index = index;
                        }
                        articles = articles_for_view(
                            storage,
                            &*feed_addresses,
                            &scope,
                            view,
                            topics,
                            topic_index,
                            None,
                            category_filter.as_deref(),
                        )?;
                        article_index = 0;
                    }
                }

                // `Esc` outside the note popup only means something in
                // `View::Saved`/`View::History`: exit back to the topic
                // view, same as `Tab` above. In `View::Topic` there's
                // nothing for a bare `Esc` to cancel, so it falls through
                // to the `_ => {}` catch-all instead.
                KeyCode::Esc if view != View::Topic => {
                    view = View::Topic;
                    selected_source = None;
                    search = None;
                    articles = articles_for_view(
                        storage,
                        &*feed_addresses,
                        &scope,
                        view,
                        topics,
                        topic_index,
                        None,
                        category_filter.as_deref(),
                    )?;
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
                    clear_fresh_for_selection(
                        &mut fresh_feeds,
                        topics,
                        topic_index,
                        selected_source.as_deref(),
                        category_filter.as_deref(),
                    );
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
                                title: "Error",
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

                // `u` un-saves the selected article -- but never straight
                // away. It opens a yes/no question (`Mode::Confirm`), and
                // nothing changes until `y` is pressed. A separate key from
                // `s` on purpose, so saving and un-saving can never be
                // mixed up by one slip of the finger. If the article has a
                // note the question says so, because a note and a save are
                // locked together: un-saving deletes the note.
                KeyCode::Char('u') => match articles.get(article_index) {
                    Some(article) if article.saved => {
                        mode = Mode::Confirm(Pending::Unsave {
                            article_id: article.id,
                            title: article.title.clone(),
                            has_note: article.note.is_some(),
                        });
                    }
                    Some(_) => {
                        notice = Some("Not saved, so there is nothing to un-save".to_string());
                    }
                    None => {}
                },

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
                // article list -- see `refresh_topic` below. Passing
                // `selected_source` through is what makes a refresh
                // preserve whichever sidebar row (the topic itself, or one
                // of its source rows) was selected beforehand, instead of
                // silently dropping back to the unfiltered topic list every
                // time `r` is pressed. Clamping `article_index` afterwards
                // matters because a refresh can only ever grow or hold
                // steady the list (nothing here removes rows), but doing it
                // unconditionally is simpler than reasoning about which
                // case applies, and is a no-op when the list grew or stayed
                // the same length. Guarded to `View::Topic` -- "refetch this
                // topic's sources" has no meaning against "every saved
                // article across every topic", and would otherwise silently
                // swap the saved view out for a single topic's list without
                // actually leaving `View::Saved`.
                //
                // What `r` refreshes depends on the sidebar row: one feed row
                // refreshes just that feed (handy for a feed that fails and
                // needs another try), "All [category]" refreshes that whole
                // category, and the "All feeds" row refreshes everything,
                // exactly like `N`.
                KeyCode::Char('r')
                    if view == View::Topic
                        && !topics.is_empty()
                        && category_filter.is_none()
                        && selected_source.is_none() =>
                {
                    // Same as `N`: say what is about to happen and let the
                    // check after the next frame run the download.
                    let feeds: usize = sources_by_topic.values().map(Vec::len).sum();
                    notice = Some(format!(
                        "{} Refreshing {feeds} feed(s)...",
                        icons.glyph(Icon::Refresh)
                    ));
                    refresh_all_pending = true;
                }
                KeyCode::Char('r') if view == View::Topic && !topics.is_empty() => {
                    // `refresh_topic` returns two things as a tuple, and
                    // `let (a, b) = ...` unpacks it into two variables.
                    let (refreshed, report) = refresh_topic(
                        storage,
                        &*feed_addresses,
                        &scope,
                        &feed_lists.lists()[active_list].path,
                        &topics[topic_index],
                        selected_source.as_deref(),
                    )?;
                    articles = refreshed;
                    article_index = article_index.min(articles.len().saturating_sub(1));
                    // Remember this refresh's new articles for the
                    // `Icon::New` marker. `extend` adds every item from
                    // another collection; `.iter().copied()` reads the
                    // `i64`s out of `report` without moving the set.
                    new_article_ids.extend(report.new_article_ids.iter().copied());
                    note_fresh_feeds(storage, &report.new_article_ids, &mut fresh_feeds);
                    notice = fetch_notice(&icons, &report, true);
                }

                // `N` refreshes every feed in the active list at once (`r` only
                // does the selected topic). It works like `r` otherwise: the
                // new articles get the `Icon::New` marker and their feeds the
                // fresh-feed marker, and the footer says how it went.
                KeyCode::Char('N') if view == View::Topic && !topics.is_empty() => {
                    // Say what is about to happen, and ask for the refresh to
                    // run after this message has been drawn (see the check
                    // after `terminal.draw`). The count is every source in the
                    // active list, from the sidebar's own grouping.
                    let feeds: usize = sources_by_topic.values().map(Vec::len).sum();
                    notice = Some(format!(
                        "{} Refreshing {feeds} feed(s)...",
                        icons.glyph(Icon::Refresh)
                    ));
                    refresh_all_pending = true;
                }

                // `S` toggles `View::Saved` on/off: pressing it while
                // already in `View::Saved` returns to `View::Topic`;
                // pressing it from `View::Topic` *or* `View::History`
                // switches straight to `View::Saved` (there's no reason to
                // require bouncing back through `View::Topic` first to get
                // from one dedicated view to the other). `View::Saved` is
                // every saved article across every topic, via
                // `Storage::saved_articles` -- ARCHITECTURE.md's dedicated
                // "Saved" view, deliberately independent of the topic
                // sidebar rather than a pseudo-topic inside it. `j`/`k`,
                // `Enter`, `x`, `s`, and `n` all keep working unmodified
                // afterwards since none of them care *how* `articles` got
                // populated, only that it's a `Vec<Article>` with a valid
                // `article_index` into it -- which resetting to `0` here
                // guarantees, the same as every other list reload above.
                //
                // `selected_source` is cleared either way, same reasoning
                // as `Tab`/`Esc` above: `S` always lands back on a bare
                // topic row (when returning to `View::Topic`) rather than
                // trying to re-derive whether a source-row selection from
                // before the toggle still makes sense. Any committed
                // search is dropped too (`search = None`) -- switching
                // views is "go look at this view's own list," not "keep
                // filtering it," so `/` has to be pressed again from
                // scratch in the new view if that's still wanted.
                KeyCode::Char('S') => {
                    view = if view == View::Saved {
                        View::Topic
                    } else {
                        View::Saved
                    };
                    selected_source = None;
                    search = None;
                    articles = articles_for_view(
                        storage,
                        &*feed_addresses,
                        &scope,
                        view,
                        topics,
                        topic_index,
                        None,
                        category_filter.as_deref(),
                    )?;
                    article_index = 0;
                }

                // `y` toggles `View::History` on/off -- every article
                // tuxwire has ever fetched, regardless of read/skipped/
                // saved state, via `Storage::all_articles`
                // (`docs/ARCHITECTURE.md`'s "Notes Retrieval & Search":
                // "solves 'I read something a few days ago, didn't save
                // it, and want it back' without requiring the person to
                // have remembered to save it in the moment"). Same
                // toggle-on/switch-to shape as `S` just above, and for the
                // same reason: pressing `y` from `View::Saved` jumps
                // straight to `View::History` rather than requiring a
                // detour back through `View::Topic` first.
                //
                // `j`/`k`, `Enter`, `x`, `s`, and `n` all keep working here
                // unmodified for the same reason they do in `View::Saved`
                // -- see `S`'s own comment just above, which applies
                // identically to this arm.
                KeyCode::Char('y') => {
                    view = if view == View::History {
                        View::Topic
                    } else {
                        View::History
                    };
                    selected_source = None;
                    search = None;
                    articles = articles_for_view(
                        storage,
                        &*feed_addresses,
                        &scope,
                        view,
                        topics,
                        topic_index,
                        None,
                        category_filter.as_deref(),
                    )?;
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
                    mode = Mode::AddSource(AddSourceStep::Url {
                        text: String::new(),
                        error: None,
                    });
                }

                // `L` opens the Read Later list: links to come back to. It is a
                // popup over whichever view is showing (see `read_later.rs`).
                KeyCode::Char('L') => {
                    mode = read_later::open_list(storage, 0)?;
                }

                // `R` puts the selected article on the Read Later list. The
                // list ignores a link it already has, and the footer says
                // which happened.
                KeyCode::Char('R') => {
                    if let Some(article) = articles.get(article_index) {
                        notice = Some(match storage.add_read_later(&article.title, &article.url) {
                            Ok(true) => "Added to Read Later".to_string(),
                            Ok(false) => "Already in Read Later".to_string(),
                            Err(err) => format!("Read Later: {err:#}"),
                        });
                    }
                }

                // `p` opens the paste-a-link prompt: any web address can be put
                // on the Read Later list or saved, with the choice asked each time.
                KeyCode::Char('p') => {
                    mode = Mode::PasteLink {
                        text: String::new(),
                        error: None,
                        from_list: false,
                    };
                }

                // `/` opens the search input (switches `mode` to
                // `Mode::Searching`), pre-filled with whatever query is
                // already committed (if any) -- same "pre-fill from
                // existing state" pattern `n` uses for the note popup,
                // letting a search be refined rather than always
                // retyped from scratch. The actual keystrokes that follow
                // are handled by the `Mode::Searching` branch below, once
                // this same `loop` iterates back around and reads the next
                // key. Available regardless of `view`, same reasoning as
                // `a` just above: which view happens to be showing has no
                // bearing on whether search should be reachable.
                KeyCode::Char('/') => {
                    mode = Mode::Searching {
                        text: search.clone().unwrap_or_default(),
                    };
                }

                // `i` cycles the icon mode (Nerd → Unicode → ASCII → Nerd).
                // Changing `icons.mode` here is all it takes for the next
                // frame (drawn at the top of this loop) to use the new
                // glyphs, so the change shows immediately. It's also saved
                // to `icons.toml` so it sticks across restarts.
                KeyCode::Char('i') => {
                    icons.mode = icons.mode.next();
                    notice = Some(save_icon_mode_notice(&icons));
                }

                // `g` opens the Stats screen (see `stats.rs`). The numbers are
                // worked out once, here; if that fails the footer says why
                // instead of the app stopping.
                // `?` opens the Help popup with every key (see `help.rs`).
                KeyCode::Char('?') => mode = Mode::Help { scroll: 0 },

                KeyCode::Char('g') => match stats::open(storage, &feed_lists) {
                    Ok(next) => mode = next,
                    Err(err) => notice = Some(format!("Stats failed: {err}")),
                },

                // `f` opens the feed list picker, with the active list
                // highlighted. With only one list there's nothing to pick,
                // so instead of a one-row picker the footer says how to add
                // lists. The rows' unread counts are computed once, here.
                KeyCode::Char('f') => {
                    if feed_lists.is_single() {
                        notice = Some(format!(
                            "{} Only one feed list. To add more, define them in {}",
                            icons.glyph(Icon::FeedFile),
                            lists_toml_display(home.as_deref())
                        ));
                    } else {
                        mode = Mode::ListPicker {
                            selected: active_list,
                            rows: list_picker_rows(
                                storage,
                                &feed_lists,
                                active_list,
                                &scope,
                                home.as_deref(),
                            ),
                        };
                    }
                }

                // `t` opens the theme picker with the current theme's row
                // highlighted. `selected` is that theme's position in the
                // alphabetical `picker_order()`, found with `.position()`
                // (`unwrap_or(0)` is unreachable in practice, since every
                // theme is in the order). `original` is the theme itself,
                // so `Esc` with no movement changes nothing.
                KeyCode::Char('t') => {
                    mode = Mode::ThemePicker {
                        selected: picker_order()
                            .iter()
                            .position(|&index| index == themes.base)
                            .unwrap_or(0),
                        original: themes.base,
                        overrides: themes.override_count(),
                    };
                }

                // `E` exports every saved article at once into one
                // combined `saved-articles.md` (`docs/ARCHITECTURE.md`'s
                // "Markdown Export") -- no popup, no per-article choice:
                // there's nothing left to ask once "export" always means
                // "regenerate the whole file from the current saved set."
                // Guarded to `View::Saved` -- purely a "this is where the
                // export action lives" placement, matching where `s`/`n`
                // operate on saved-article state, even though the export
                // itself (via `Storage::saved_articles`) doesn't depend on
                // whatever `articles`/`article_index` currently holds.
                //
                // `ExportConfig::load` re-reads `export.toml` fresh on
                // every press rather than being loaded once up front,
                // matching `Theme::load`/`load_sources`'s own "cheap
                // enough to just re-read" reasoning -- an edit to
                // `export.toml` while tuxwire is already running takes
                // effect on the very next `E` with no restart needed.
                //
                // If the export file already exists, `E` asks before
                // replacing it (`Pending::ReplaceExport`): the file is
                // rewritten from scratch, so any edits made to it by hand
                // would be lost.
                KeyCode::Char('E') if view == View::Saved => {
                    match ExportConfig::load() {
                        // Config loaded: ask first if there is a file to replace.
                        Ok(config) => {
                            let path = export::export_file_path(&config);
                            mode = if path.exists() {
                                Mode::Confirm(Pending::ReplaceExport { path })
                            } else {
                                run_export(storage)
                            };
                        }
                        Err(err) => {
                            mode = Mode::Error {
                                title: "Error",
                                message: format!("export failed: {err:#}"),
                            };
                        }
                    }
                }

                // `c` runs the optional CouchDB sync once (saved articles and
                // notes only). It does nothing unless the person set sync up
                // and switched it on in `sync.toml`; otherwise the footer says
                // why. The screen waits while it runs (at most about half a
                // minute), then the lists are reloaded so articles that
                // arrived from another device show up straight away.
                KeyCode::Char('c') => {
                    notice = Some(match sync_cli::sync_for_reader() {
                        Ok(summary) => format!("Synced: {summary}"),
                        Err(err) => format!("Sync: {err:#}"),
                    });
                    articles = articles_for_view(
                        storage,
                        &*feed_addresses,
                        &scope,
                        view,
                        topics,
                        topic_index,
                        selected_source.as_deref(),
                        category_filter.as_deref(),
                    )?;
                    article_index = article_index.min(articles.len().saturating_sub(1));
                }

                // `E` anywhere except the Saved view does nothing useful, so
                // say so instead of leaving the person wondering why the
                // key seems dead. (The arm above handles the Saved view
                // first; `match` takes the first arm that fits.)
                KeyCode::Char('E') => {
                    notice = Some("Export works from the Saved view: press S, then E".to_string());
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
            //
            // Removing an *existing* note is a deletion, so it asks first:
            // an emptied box on an article that has a note opens a yes/no
            // question instead of deleting.
            KeyCode::Enter => {
                // Is this Enter about to wipe out a note that exists?
                let deleting = text.is_empty()
                    && articles
                        .get(article_index)
                        .is_some_and(|a| a.note.is_some());
                if deleting {
                    if let Some(article) = articles.get(article_index) {
                        mode = Mode::Confirm(Pending::DeleteNote {
                            article_id: article.id,
                            title: article.title.clone(),
                        });
                    }
                } else {
                    if let Some(article) = articles.get_mut(article_index) {
                        let note = if text.is_empty() {
                            None
                        } else {
                            Some(text.clone())
                        };
                        storage.update_note(article.id, note.clone())?;
                        // A real note also saves the article (they are
                        // locked together in `Storage::update_note`), so
                        // mirror that onto the on-screen copy.
                        if note.is_some() {
                            article.saved = true;
                            article.read = true;
                        }
                        article.note = note;
                    }
                    mode = Mode::Normal;
                }
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

    // A `let`-chain (`if let ... && ...`): only runs the body when
    // `$BROWSER` is set *and* spawning it succeeded.
    if let Ok(browser) = std::env::var("BROWSER")
        && spawn(&browser)
    {
        return true;
    }

    spawn("xdg-open")
}

/// Re-fetches **every** source in the active feed list, all topics at once --
/// the `N` keybind, the terminal's version of the Android app's "refresh all".
/// Where [`refresh_topic`] only fetches the sources filed under one topic, this
/// fetches the whole list, then reloads the article list for the current
/// selection (`topic` and `source`, exactly as `refresh_topic` does) and returns
/// a [`FetchReport`] covering the whole refresh.
///
/// The downloads run at the same time, a few at once (`fetchers::fetch_all`,
/// the same function the Android app uses), so a long list is much quicker than
/// fetching one feed after another. `fetch_all` hands the results back in the
/// order of the sources, so what is stored and marked new does not depend on
/// which download finished first.
///
/// `list_path` is the active feed list's file, re-read each time like in
/// `refresh_topic`. `addresses` and `scope` are used only to reload the list.
fn refresh_all(
    storage: &Storage,
    addresses: &FeedAddresses,
    scope: &Scope,
    list_path: &Path,
    topic: Option<&str>,
    source: Option<&str>,
) -> anyhow::Result<(Vec<Article>, FetchReport)> {
    let mut report = FetchReport::default();

    // Step 1: every source in the list. `configured_sources_at` already leaves
    // out feeds imported from a file, which have nothing to download.
    let sources = fetchers::configured_sources_at(list_path)?;

    // Step 2: fetch them all. This function is not `async` (the draw loop is
    // plain blocking code), so, as in `refresh_topic`, `block_in_place` lets
    // this thread wait while tokio's other workers keep running, and `block_on`
    // drives the `async` fetch to its end.
    let outcomes = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(fetchers::fetch_all(&sources))
    });

    // Step 3: store each outcome. A failed feed is counted in the report, not
    // raised, so one dead feed never stops the rest (same rule as `r`).
    for outcome in outcomes {
        if let Err(err) = &outcome.result {
            eprintln!("skipped {}: {err:#}", outcome.name);
        }
        report.record_outcome(storage, outcome)?;
    }

    // Step 4: show the list the person was looking at, now with the new articles.
    Ok((
        load_articles_for_selection(storage, addresses, scope, topic, source)?,
        report,
    ))
}

/// Re-fetches the sources in `fetchers::configured_sources` filed under
/// `topic` (just the one named `source`, when a feed row is selected),
/// inserts whatever comes back into `storage` (idempotently, on
/// `url` -- see `Storage::insert_article`), and returns the reloaded
/// article list for `topic` -- the `r` keybind. `source`, when `Some`, is
/// forwarded straight to `load_articles_for_selection` so a refresh
/// preserves whichever source-row filter (if any) was active beforehand,
/// rather than always handing back every article in the topic.
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
///
/// Also returns a `FetchReport` for this refresh (sources tried and failed,
/// and which articles were new), which the `r` arm turns into a footer
/// message with `fetch_notice` and uses for the `Icon::New` markers.
///
/// `list_path` is the active feed list's file, re-read on every `r` (so an
/// edit to it takes effect without restarting), and `scope` is its feeds,
/// for reloading the article list afterwards. Other lists are never
/// fetched here.
fn refresh_topic(
    storage: &Storage,
    addresses: &FeedAddresses,
    scope: &Scope,
    list_path: &Path,
    topic: &str,
    source: Option<&str>,
) -> anyhow::Result<(Vec<Article>, FetchReport)> {
    let mut report = FetchReport::default();

    // `source`, when `Some`, narrows the refresh to that one feed (a feed row
    // was selected); otherwise every feed in the topic is fetched.
    let wanted_feed = source;
    for configured_source in fetchers::configured_sources_at(list_path)?
        .into_iter()
        .filter(|source| source.topic() == topic)
        .filter(|source| wanted_feed.is_none_or(|name| source.name() == name))
    {
        // `fetch_one` (shared with the startup fetch and the Android app)
        // does the network call and returns the answer as a `FetchOutcome`.
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(fetchers::fetch_one(&configured_source))
        });

        // One source failing (a dead feed, or a `type = "reddit"` source that
        // isn't implemented yet) must not make `r` fail to refresh every
        // *other* source filed under this topic, so a failure is only printed
        // here, not propagated. The full error text is only visible if stderr
        // is redirected somewhere, but `record_outcome` also counts it, so the
        // footer can say "N of M sources failed" (or "offline") inside the TUI.
        if let Err(err) = &outcome.result {
            eprintln!("skipped {}: {err:#}", outcome.name);
        }

        // Stores the articles (noting which were new) or counts the failure.
        report.record_outcome(storage, outcome)?;
    }

    Ok((
        load_articles_for_selection(storage, addresses, scope, Some(topic), source)?,
        report,
    ))
}

/// Fetches just the one source whose feed address is `url`, from the active
/// list's file, and stores whatever comes back. Used right after a source is
/// added with `a`, so its articles show up at once instead of waiting for the
/// next `r` (a feed that is saved but empty looks like a broken app).
///
/// The source is looked up in the file again (rather than built from what was
/// just typed) so it is fetched exactly like every later refresh will fetch it.
/// A source that is not found (the file changed in the meantime) simply gives
/// an empty report, which `fetch_notice` words as "no local sources".
///
/// Like [`refresh_topic`], a dead feed does not make this an `Err`: it is
/// counted in the returned `FetchReport`, so the footer can say what happened,
/// while the source stays saved. `Err` is only for real problems (the file
/// could not be read, the database could not be written).
///
/// `block_in_place` + `block_on` is the same "drive an async call from this
/// synchronous code" trick `refresh_topic` explains.
fn fetch_added_source(
    storage: &Storage,
    list_path: &Path,
    url: &str,
) -> anyhow::Result<FetchReport> {
    let mut report = FetchReport::default();

    // `into_iter().find(..)` walks the sources and stops at the first match.
    let found = fetchers::configured_sources_at(list_path)?
        .into_iter()
        .find(|source| source.url() == url);

    // `if let Some(..)` runs the fetch only when the source was found.
    if let Some(source) = found {
        let outcome = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(fetchers::fetch_one(&source))
        });
        // Same as a refresh: print a failure's reason, then store the articles
        // (noting which are new) or count the failure.
        if let Err(err) = &outcome.result {
            eprintln!("skipped {}: {err:#}", outcome.name);
        }
        report.record_outcome(storage, outcome)?;
    }

    Ok(report)
}

/// The one-shot footer message summarizing a fetch round, with an icon in
/// front, or `None` when there's nothing worth interrupting the keybind
/// hints for.
///
/// `after_refresh` is `true` for an explicit `r` press, which always gets
/// an answer (even "up to date"), and `false` for the startup fetch, which
/// only speaks up when something is new or wrong.
///
/// Checked in order, first match wins:
/// - offline (every source unreachable): `Icon::Offline`;
/// - some sources failed: `Icon::Error`, plus the new count if any;
/// - new articles: `Icon::New` with the count;
/// - a refresh with no sources: `Icon::Refresh` "no local sources";
/// - a refresh with nothing new: `Icon::Refresh` "up to date".
fn fetch_notice(icons: &IconSettings, report: &FetchReport, after_refresh: bool) -> Option<String> {
    let new = report.new_article_ids.len();
    if report.is_offline() {
        return Some(format!(
            "{} Offline: couldn't reach any source. Showing articles already downloaded.",
            icons.glyph(Icon::Offline)
        ));
    }
    if report.failed > 0 {
        let mut message = format!(
            "{} {} of {} source(s) failed to fetch",
            icons.glyph(Icon::Error),
            report.failed,
            report.attempted
        );
        if new > 0 {
            message.push_str(&format!(" · {} {new} new", icons.glyph(Icon::New)));
        }
        return Some(message);
    }
    if new > 0 {
        return Some(format!("{} {new} new article(s)", icons.glyph(Icon::New)));
    }
    if !after_refresh {
        return None;
    }
    if report.attempted == 0 {
        return Some(format!(
            "{} No sources in this topic to refresh",
            icons.glyph(Icon::Refresh)
        ));
    }
    Some(format!(
        "{} Up to date, nothing new",
        icons.glyph(Icon::Refresh)
    ))
}

/// The footer message when `N` (refresh all) has finished: it names how many
/// feeds were refreshed, so a quiet result is still clearly "done". Failures
/// and being offline keep [`fetch_notice`]'s wording, which already says what
/// went wrong.
fn refresh_all_notice(icons: &IconSettings, report: &FetchReport) -> Option<String> {
    let new = report.new_article_ids.len();
    // Offline, or some feeds failed: the shared wording covers both.
    if report.is_offline() || report.failed > 0 {
        return fetch_notice(icons, report, true);
    }
    // No feeds in the list at all.
    if report.attempted == 0 {
        return Some(format!(
            "{} No feeds to refresh",
            icons.glyph(Icon::Refresh)
        ));
    }
    // Everything fetched: say how many, and what was new.
    if new > 0 {
        Some(format!(
            "{} Refreshed {} feed(s) \u{b7} {new} new article(s)",
            icons.glyph(Icon::New),
            report.attempted
        ))
    } else {
        Some(format!(
            "{} Refreshed {} feed(s), nothing new",
            icons.glyph(Icon::Refresh),
            report.attempted
        ))
    }
}

/// Saves `icons.mode` to `icons.toml` and returns the footer message to
/// show: a confirmation naming the new mode, or the save error. The mode
/// is already in effect on screen either way. A failed save means it only
/// lasts until tuxwire quits, which is worth telling the user but no reason
/// to undo the change.
fn save_icon_mode_notice(icons: &IconSettings) -> String {
    match icons::config::save_mode(icons.mode) {
        Ok(()) => format!(
            "{} Icon mode: {} (press i to change)",
            icons.glyph(Icon::Starred),
            icons.mode.as_str()
        ),
        Err(err) => format!(
            "{} Icon mode is {} for now, but saving icons.toml failed: {err:#}",
            icons.glyph(Icon::Error),
            icons.mode.as_str()
        ),
    }
}

/// Loads the article list a sidebar row's selection actually implies:
///
/// - `topic` is `None`: the "All feeds" row, every article in `scope`
///   whatever its topic (`Storage::articles_in_scope`);
/// - `topic` is `Some` and `source` is `None`: the "All [topic]" row, every
///   article in that topic (`Storage::articles_by_topic`);
/// - both are `Some`: one feed row, that topic's articles further filtered
///   to just that feed (`Storage::articles_by_feed_or_name`).
///
/// Every call site that needs to reload `articles` from the current sidebar
/// selection goes through this one function instead of re-deriving the
/// three-way split itself.
///
/// `scope` limits all three queries to the active feed list's feeds when
/// there are several lists (see `storage::Scope`).
fn load_articles_for_selection(
    storage: &Storage,
    addresses: &FeedAddresses,
    scope: &Scope,
    topic: Option<&str>,
    source: Option<&str>,
) -> anyhow::Result<Vec<Article>> {
    // No topic at all means "All feeds". (A feed row always has a topic.)
    let Some(topic) = topic else {
        return storage.articles_in_scope(scope);
    };
    match source {
        // A feed row is found by the feed's address when `sources.toml` gives
        // one, so renaming the feed does not strand its older articles (see
        // `Storage::articles_by_feed_or_name`). A name with no known address
        // falls back to matching by name alone.
        Some(name) => {
            let address = addresses
                .get(&(topic.to_string(), name.to_string()))
                .map(String::as_str);
            storage.articles_by_feed_or_name(scope, topic, name, address)
        }
        None => storage.articles_by_topic(scope, topic),
    }
}

/// Loads whichever article list `view`'s *own* ordinary (unfiltered by
/// search) list actually is -- `storage.saved_articles()` for
/// `View::Saved`, `storage.all_articles()` for `View::History`, or the
/// current sidebar selection's list (via `load_articles_for_selection`,
/// same as any other row change) for `View::Topic`.
///
/// This is the one place every "switch to/return to this view" call site
/// goes through -- the `S`/`y`/`Tab`/`Esc` handlers in `draw_until_quit`,
/// the initial load in `draw_until_quit` itself, and the `Mode::Searching`
/// handlers' "restore the full list" path -- instead of each one
/// re-deriving its own `match view { ... }`.
///
/// `category_filter` is the sidebar's category filter (`None` = all
/// feeds); together with `selected_source` it decides which of the three
/// sidebar selections applies (see `selection_topic`).
///
/// `scope` only applies to `View::Topic`: Saved and History always cover
/// every feed list.
///
/// Eight parameters, one past clippy's default limit of seven: the sidebar
/// selection is three values (`topic_index`, `selected_source`,
/// `category_filter`) that every caller already holds as loose variables, so
/// a params struct would only move the same eight values one layer.
#[allow(clippy::too_many_arguments)]
fn articles_for_view(
    storage: &Storage,
    addresses: &FeedAddresses,
    scope: &Scope,
    view: View,
    topics: &[String],
    topic_index: usize,
    selected_source: Option<&str>,
    category_filter: Option<&str>,
) -> anyhow::Result<Vec<Article>> {
    match view {
        View::Saved => storage.saved_articles(),
        View::History => storage.all_articles(),
        View::Topic => {
            let topic = selection_topic(topics, topic_index, selected_source, category_filter);
            load_articles_for_selection(storage, addresses, scope, topic, selected_source)
        }
    }
}

/// The topic the sidebar selection means for loading articles, or `None`
/// for the "All feeds" row.
///
/// "All feeds" is the cursor on the first row while no category filter is
/// set: `category_filter` is `None` and `selected_source` is `None`. In
/// every other case the selection lives inside one topic, `topics[topic_index]`
/// (the filtered category, or the topic of the selected feed).
///
/// The returned `&str` borrows from `topics`, which is why it carries the
/// lifetime `'a`: it can't outlive that slice.
fn selection_topic<'a>(
    topics: &'a [String],
    topic_index: usize,
    selected_source: Option<&str>,
    category_filter: Option<&str>,
) -> Option<&'a str> {
    if category_filter.is_none() && selected_source.is_none() {
        None
    } else {
        topics.get(topic_index).map(String::as_str)
    }
}

/// Clears the new-feed markers for what the article list is showing, the
/// same moment `clear_fresh_feeds` always did (the person acts in the list).
/// On the "All feeds" row the list shows every feed together, so every
/// marker is cleared; otherwise it defers to `clear_fresh_feeds`.
fn clear_fresh_for_selection(
    fresh: &mut HashSet<(String, String)>,
    topics: &[String],
    topic_index: usize,
    selected_source: Option<&str>,
    category_filter: Option<&str>,
) {
    if category_filter.is_none() && selected_source.is_none() {
        fresh.clear();
    } else {
        clear_fresh_feeds(fresh, topics.get(topic_index), selected_source);
    }
}

/// One row of the sidebar's feed list, in the order it is drawn and the
/// order `Up`/`Down` step through (`move_sidebar_selection`). Both are built
/// from `sidebar_rows` so what is on screen and what the cursor lands on can
/// never disagree.
///
/// Holds an index and a borrowed name rather than owned `String`s: the list
/// is rebuilt on every keypress and every frame, which is cheap for a few
/// dozen feeds and means it can never go stale.
enum SidebarRow<'a> {
    /// The first row: "All feeds", or "All [category]" while a category
    /// filter is set. Shows every article in that set.
    All,
    /// `(index into topics, the feed's name)`; the name is borrowed out of
    /// `sources_by_topic`, hence the lifetime.
    Feed(usize, &'a str),
}

/// Builds the sidebar's rows: the `All` row, then every feed sorted A-Z
/// ignoring case (ties broken by category name, then exact spelling), like
/// the Android home screen. With a `category_filter`, only that category's
/// feeds are included.
///
/// Feeds come from `sources_by_topic`, grouped from the config, so a feed
/// that has never fetched an article still appears. A category in
/// `topics` with no configured feeds simply contributes none.
fn sidebar_rows<'a>(
    topics: &[String],
    sources_by_topic: &'a HashMap<String, Vec<String>>,
    category_filter: Option<&str>,
) -> Vec<SidebarRow<'a>> {
    // Collect `(topic index, feed name)` for every feed that passes the filter.
    let mut feeds: Vec<(usize, &str)> = Vec::new();
    for (index, topic) in topics.iter().enumerate() {
        // `is_some_and` is false for `None`, so with no filter nothing is skipped.
        if category_filter.is_some_and(|wanted| wanted != topic) {
            continue;
        }
        if let Some(names) = sources_by_topic.get(topic) {
            feeds.extend(names.iter().map(|name| (index, name.as_str())));
        }
    }

    // Sort by name ignoring case. `then_with` only runs the next comparison
    // when the previous one says "equal", so the same name in two categories
    // stays in a fixed order.
    feeds.sort_by(|(topic_a, name_a), (topic_b, name_b)| {
        name_a
            .to_lowercase()
            .cmp(&name_b.to_lowercase())
            .then_with(|| {
                topics[*topic_a]
                    .to_lowercase()
                    .cmp(&topics[*topic_b].to_lowercase())
            })
            .then_with(|| name_a.cmp(name_b))
    });

    let mut rows = vec![SidebarRow::All];
    rows.extend(
        feeds
            .into_iter()
            .map(|(index, name)| SidebarRow::Feed(index, name)),
    );
    rows
}

/// Moves the sidebar cursor one row up (`direction < 0`) or down
/// (`direction > 0`) through `sidebar_rows`, clamping at either end rather
/// than wrapping -- same convention as every other list here.
/// `topic_index`/`selected_source` are the two pieces of state that together
/// *are* "which row is selected"; this is the one place that decides where
/// they point next, rewriting both in place.
///
/// The current row is re-derived from `topic_index`/`selected_source`
/// (rather than tracking a raw row number) so changing the category filter
/// elsewhere, which changes how many rows exist, can never leave a stale
/// position pointing at the wrong row.
///
/// Landing on the `All` row clears `selected_source`; with a category filter
/// it also points `topic_index` back at that category, so "All [linux]" loads
/// linux's articles.
fn move_sidebar_selection(
    direction: i32,
    topics: &[String],
    sources_by_topic: &HashMap<String, Vec<String>>,
    category_filter: Option<&str>,
    topic_index: &mut usize,
    selected_source: &mut Option<String>,
) {
    let rows = sidebar_rows(topics, sources_by_topic, category_filter);

    // Which row is the cursor on now? No feed selected means the `All` row.
    let current = rows
        .iter()
        .position(|row| match row {
            SidebarRow::All => selected_source.is_none(),
            SidebarRow::Feed(index, name) => {
                *index == *topic_index && selected_source.as_deref() == Some(*name)
            }
        })
        .unwrap_or(0);

    let next = if direction < 0 {
        current.saturating_sub(1)
    } else {
        (current + 1).min(rows.len() - 1)
    };

    match rows[next] {
        SidebarRow::All => {
            *selected_source = None;
            // With a filter, "All [category]" belongs to that category.
            if let Some(wanted) = category_filter
                && let Some(index) = topics.iter().position(|topic| topic == wanted)
            {
                *topic_index = index;
            }
        }
        SidebarRow::Feed(index, name) => {
            *topic_index = index;
            *selected_source = Some(name.to_string());
        }
    }
}

/// The next category filter when `Tab` (`forward`) or `Shift+Tab` is pressed:
/// the cycle is `All feeds`, then each category A-Z, then back to `All feeds`.
/// `None` stands for "no filter" and sits at the start of the cycle.
fn next_category_filter(topics: &[String], current: Option<&str>, forward: bool) -> Option<String> {
    // Position in the cycle: 0 is "no filter", 1.. are the categories.
    let position = match current {
        None => 0,
        Some(name) => topics
            .iter()
            .position(|topic| topic == name)
            .map_or(0, |index| index + 1),
    };
    let length = topics.len() + 1;
    let next = if forward {
        (position + 1) % length
    } else {
        (position + length - 1) % length
    };
    // 0 means back to "no filter"; otherwise the category at `next - 1`.
    next.checked_sub(1).map(|index| topics[index].clone())
}

/// How many unread articles each configured feed has, keyed like the
/// sidebar: `(category, feed name)`. Built from the grouped counts
/// `Storage::unread_by_feed` returns.
///
/// A count is matched to its feed by address first (so a renamed feed keeps
/// its count), and otherwise by the category and name stored on the
/// articles (older articles have no address).
fn feed_unread_map(
    counts: &[FeedUnread],
    addresses: &FeedAddresses,
) -> HashMap<(String, String), u32> {
    // Flip `(topic, name) -> address` around into `address -> (topic, name)`.
    let by_address: HashMap<&str, &(String, String)> = addresses
        .iter()
        .map(|(key, address)| (address.as_str(), key))
        .collect();

    let mut map: HashMap<(String, String), u32> = HashMap::new();
    for count in counts {
        // Prefer the configured feed this address belongs to; fall back to
        // the names stored on the articles themselves.
        let key = count
            .feed_url
            .as_deref()
            .and_then(|url| by_address.get(url))
            .map(|key| (*key).clone())
            .unwrap_or_else(|| (count.topic.clone(), count.source.clone()));
        *map.entry(key).or_insert(0) += count.unread;
    }
    map
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
/// This function only ever touches the active feed list's file
/// (`list_path`, normally `sources.toml`) on a successful confirm (via
/// `fetchers::config::add_source_at`, see its own doc comment for why
/// nothing here needs to separately "reload" the in-memory fetcher list)
/// -- every other keypress just edits `step`'s fields in memory.
///
/// `saved_url` is an "out-parameter": a `&mut` the caller lends so this
/// function can hand a second answer back besides the returned `Mode`. It is
/// set to the new feed's address only when a source was actually saved.
fn handle_add_source_key(
    key: KeyCode,
    step: AddSourceStep,
    topics: &mut Vec<String>,
    list_path: &Path,
    saved_url: &mut Option<String>,
) -> anyhow::Result<Mode> {
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
                        error: Some(
                            "enter a feed address, a page address, or a file path first"
                                .to_string(),
                        ),
                    }));
                }

                // A local path (`~/Downloads/index.rss`) is a feed saved from
                // a browser, for sites that block programs. It is read and
                // parsed right here (no network, so nothing to await); the
                // parse succeeding is the same proof of a valid feed. The
                // confirm screen then carries the full path as its `url`,
                // which is how the save step below knows it is a file.
                if fetchers::config::looks_like_file_path(trimmed) {
                    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
                    let loaded = fetchers::config::resolve_file_path(trimmed, home.as_deref())
                        .map_err(anyhow::Error::msg)
                        .and_then(|path| {
                            let feed =
                                tuxwire_core::fetchers::file::read_feed_file(Path::new(&path))?;
                            Ok((path, feed))
                        });
                    return match loaded {
                        Ok((path, feed)) => Ok(Mode::AddSource(AddSourceStep::Confirm {
                            name: guess_source_name(&feed, &path),
                            note: Some("read from a file on this computer".to_string()),
                            url: path,
                            topic: String::new(),
                            field: ConfirmField::Name,
                            topic_options: topics.clone(),
                            error: None,
                        })),
                        Err(err) => Ok(Mode::AddSource(AddSourceStep::Url {
                            text,
                            error: Some(format!("{err:#}")),
                        })),
                    };
                }

                // `discover_feed` accepts a feed address (used as it is) or a
                // web page (its feed is looked up), see `fetchers::discover`.
                // It is `async`, so it is driven the same way as before.
                let fetch_result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current()
                        .block_on(fetchers::discover::discover_feed(trimmed))
                });

                match fetch_result {
                    // `found.url` is the address of the feed itself, which may
                    // differ from what was typed: that is the address saved.
                    Ok(found) => Ok(Mode::AddSource(AddSourceStep::Confirm {
                        name: guess_source_name(&found.feed, &found.url),
                        note: found_note(found.found_by),
                        url: found.url,
                        topic: String::new(),
                        field: ConfirmField::Name,
                        topic_options: topics.clone(),
                        error: None,
                    })),
                    // The messages from discovery already say what went wrong
                    // ("answered HTTP 404", "no feed found at ...", "only
                    // offers a comments feed"), so they are shown as they
                    // are. `text` (not `trimmed`) is kept so the popup still
                    // shows exactly what they typed, ready to edit rather
                    // than retype.
                    Err(err) => Ok(Mode::AddSource(AddSourceStep::Url {
                        text,
                        error: Some(format!("{err:#}")),
                    })),
                }
            }

            _ => Ok(Mode::AddSource(AddSourceStep::Url { text, error })),
        },

        AddSourceStep::Confirm {
            url,
            mut name,
            mut topic,
            mut field,
            topic_options,
            note,
            error,
        } => match key {
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
                    note,
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
                    note,
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
                    note,
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
                    note,
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
                    note,
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
                        note,
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
                        note,
                        error: Some("every source needs a topic".to_string()),
                    }));
                }

                // A full path means a feed file; anything else is a web feed.
                let source_type = if fetchers::config::looks_like_file_path(&url) {
                    fetchers::config::SourceType::File
                } else {
                    fetchers::config::SourceType::Rss
                };
                match fetchers::config::add_source_at(
                    list_path,
                    trimmed_name,
                    source_type,
                    &url,
                    trimmed_topic,
                ) {
                    Ok(()) => {
                        if !topics.iter().any(|existing| existing == trimmed_topic) {
                            topics.push(trimmed_topic.to_string());
                            sort_names(topics);
                        }
                        // Tell the caller which feed was just saved (through
                        // the `&mut` out-parameter), so it can fetch that
                        // feed's articles right away. Cancelling with `Esc`
                        // leaves this `None`.
                        *saved_url = Some(url.clone());
                        Ok(Mode::Normal)
                    }
                    Err(err) => Ok(Mode::AddSource(AddSourceStep::Confirm {
                        url,
                        name,
                        topic,
                        field,
                        topic_options,
                        note,
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
                note,
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

/// The line the confirm screen shows when the feed was *found* rather than
/// typed: `None` when the person gave the feed's own address (nothing to
/// explain), otherwise a short sentence on how discovery got there, so a
/// changed address is never a surprise.
///
/// `FoundBy` is `Copy`, so it is passed by value (no `&`).
fn found_note(found_by: fetchers::discover::FoundBy) -> Option<String> {
    use fetchers::discover::FoundBy;
    match found_by {
        FoundBy::Direct => None,
        FoundBy::Advertised => Some("found through the page's own feed link".to_string()),
        FoundBy::Guessed => {
            Some("the page lists no feed; found one at a common address".to_string())
        }
    }
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
/// `view` (and now the sidebar's `sources_by_topic`/`category_filter`/
/// `selected_source`) pushes this well past clippy's default 7-argument
/// threshold for `too_many_arguments`. Bundling these into a params struct
/// just to quiet the lint would be an abstraction with no other caller and
/// no reuse to justify it -- `render` has exactly one call site
/// (`draw_until_quit`'s `terminal.draw` closure), so the allow below is the
/// more honest fix.
#[allow(clippy::too_many_arguments)]
fn render(
    frame: &mut Frame,
    theme: &Theme,
    // Which glyphs to draw (see `crate::icons`). Every icon below comes
    // from this, and none are hard-coded.
    icons: &IconSettings,
    // Articles new this session, which get `Icon::New` while unread. See
    // `draw_until_quit`'s `new_article_ids`.
    new_article_ids: &HashSet<i64>,
    // Feeds with new articles not yet looked at; the sidebar marks them. See
    // `draw_until_quit`'s `fresh_feeds`.
    fresh_feeds: &HashSet<(String, String)>,
    // How many unread articles each feed has, keyed `(category, feed name)`;
    // shown under each feed's name in the sidebar.
    unread_by_feed: &HashMap<(String, String), u32>,
    topics: &[String],
    sources_by_topic: &HashMap<String, Vec<String>>,
    // The sidebar's category filter (`None` = all feeds).
    category_filter: Option<&str>,
    selected_topic: Option<&str>,
    selected_source: Option<&str>,
    articles: &[Article],
    selected_article: Option<usize>,
    mode: &Mode,
    view: View,
    search: Option<&str>,
    // `draw_until_quit`'s one-shot `notice` (refresh results, icon mode
    // changes), shown in the footer in place of the keybind hints.
    notice: Option<&str>,
    // The feed list path (home already shown as `~`), drawn at the right
    // of the footer when there's room.
    feed_list: &str,
    // Whether the Help popup lists `f` (feed lists): only when there's more
    // than one list.
    show_list_key: bool,
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
            // Two rows: key hints on top, status line (notice + feed list
            // path) underneath. See `render_footer`.
            Constraint::Length(2),
        ])
        .areas(frame.area());

    // Then split that body horizontally into the sidebar and the article
    // list. Thirty percent of the width: the sidebar now shows every feed
    // name (indented under its category) all the time, so it gets a little
    // more room than the old quarter. Longer names are clipped at the edge.
    let [sidebar_area, articles_area] = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .areas(body);

    render_banner(frame, theme, banner_area);
    render_sidebar(
        frame,
        theme,
        icons,
        sidebar_area,
        topics,
        sources_by_topic,
        category_filter,
        selected_topic,
        selected_source,
        fresh_feeds,
        unread_by_feed,
    );
    // The article pane's title names what is listed: "all feeds" on the
    // "All feeds" row, otherwise the topic of the selection.
    let articles_label = if category_filter.is_none() && selected_source.is_none() {
        Some("all feeds")
    } else {
        selected_topic
    };
    // What (if anything) `render_articles` should show about search --
    // computed once, here, rather than each of its three call sites (there
    // is only one, but the same reasoning as `articles_for_view` applies:
    // one place that knows how `mode`/`search` map onto a `SearchStatus`).
    // `Mode::Searching` always wins over a previously committed `search`:
    // while actively typing a *new* query, the live buffer is what's
    // driving `articles` (see `draw_until_quit`'s `Mode::Searching` key
    // handling), so that's what the title/cursor should reflect, not
    // whatever query was committed before `/` was pressed again.
    let search_status = match mode {
        Mode::Searching { text } => SearchStatus::Typing(text),
        _ => match search {
            Some(query) => SearchStatus::Committed(query),
            None => SearchStatus::None,
        },
    };

    render_articles(
        frame,
        theme,
        icons,
        new_article_ids,
        articles_area,
        articles_label,
        articles,
        selected_article,
        view,
        search_status,
    );
    render_footer(
        frame,
        theme,
        icons,
        footer_area,
        mode,
        view,
        notice,
        feed_list,
    );

    // The note popup, if open, paints on top of everything drawn above --
    // see `render_note_popup` for why it has to come last (after the panes
    // it's meant to sit over) and why it lives inside `body` rather than
    // the full frame. The add-source, error, and icon-check popups follow
    // the same reasoning; `mode` can only ever be *one* of these at a time
    // (see `Mode` itself), so at most one of the `if let`s below actually
    // draws anything.
    if let Mode::EditingNote { text } = mode {
        render_note_popup(frame, theme, body, text);
    }
    if let Mode::AddSource(step) = mode {
        render_add_source_popup(frame, theme, body, step);
    }
    if let Mode::Error { title, message } = mode {
        render_error_popup(frame, theme, icons, body, title, message);
    }
    if let Mode::Confirm(pending) = mode {
        render_confirm_popup(frame, theme, body, pending);
    }
    if let Mode::IconCheck = mode {
        render_icon_check_popup(frame, theme, body);
    }
    // The theme picker, also last so it sits on top. Everything behind it is
    // already drawn in the previewed theme (`theme` *is* the preview).
    if let Mode::ThemePicker {
        selected,
        overrides,
        ..
    } = mode
    {
        render_theme_picker_popup(frame, theme, icons, body, *selected, *overrides);
    }
    if let Mode::ListPicker { selected, rows } = mode {
        render_list_picker_popup(frame, theme, icons, body, *selected, rows);
    }
    // The Stats screen (see `stats.rs`).
    if let Mode::Stats { stats } = mode {
        stats::render(frame, theme, body, stats);
    }
    // The Help popup (see `help.rs`).
    if let Mode::Help { scroll } = mode {
        help::render(frame, theme, body, *scroll, show_list_key);
    }
    // The Read Later screens (see `read_later.rs`).
    if let Mode::ReadLater { items, selected } = mode {
        read_later::render_list_popup(frame, theme, icons, body, *selected, items);
    }
    if let Mode::PasteLink { text, error, .. } = mode {
        read_later::render_paste_popup(frame, theme, body, text, error);
    }
    if let Mode::LinkChoice { url, title, .. } = mode {
        read_later::render_choice_popup(frame, theme, icons, body, url, title);
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

/// The left pane, two stacked sections top to bottom (`docs/ARCHITECTURE.md`
/// § 3. TUI): the feed list (interactive, sized to fill all the space the
/// legend doesn't need), then the static "Colors" legend. There is no
/// permanent Keys box: the full key list lives in the `?` Help popup and the
/// everyday keys in the footer, so the feeds get the whole column. This
/// function takes no `View`: the sidebar is identical in every view (the
/// list keeps showing the last selection, see the comment on
/// `selected_topic` in `draw_until_quit`).
///
/// `Constraint::Length` for the legend makes it a fixed height regardless
/// of terminal size; `Constraint::Min(0)` on the feed list then makes *it*
/// absorb every leftover row. When there are more feeds than rows, the
/// `List` scrolls to keep the selected row in view.
///
/// Too many arguments for clippy's default threshold, for the same reason
/// as `render` itself: one call site, with everything already in the
/// caller's hands.
#[allow(clippy::too_many_arguments)]
fn render_sidebar(
    frame: &mut Frame,
    theme: &Theme,
    icons: &IconSettings,
    area: Rect,
    topics: &[String],
    sources_by_topic: &HashMap<String, Vec<String>>,
    category_filter: Option<&str>,
    selected_topic: Option<&str>,
    selected_source: Option<&str>,
    fresh_feeds: &HashSet<(String, String)>,
    unread_by_feed: &HashMap<(String, String), u32>,
) {
    let [feeds_area, colors_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(LEGEND_ROWS as u16 + 2),
        ])
        .areas(area);

    render_feeds_section(
        frame,
        theme,
        icons,
        feeds_area,
        topics,
        sources_by_topic,
        category_filter,
        selected_topic,
        selected_source,
        fresh_feeds,
        unread_by_feed,
    );
    render_colors_section(frame, theme, icons, colors_area);
}

/// Records which feeds the just-added articles belong to, so the sidebar can
/// mark them. Looks the IDs up with `Storage::article_origins` and adds each
/// `(topic, source name)` to `fresh`. A database error only means no marker is
/// drawn, so it is ignored rather than interrupting a refresh.
///
/// `fresh` is a `&mut` out-parameter: the caller's set is changed in place.
fn note_fresh_feeds(
    storage: &Storage,
    new_ids: &HashSet<i64>,
    fresh: &mut HashSet<(String, String)>,
) {
    // `if let Ok(..)` runs the body only when the lookup succeeded.
    if let Ok(origins) = storage.article_origins(new_ids) {
        for origin in origins {
            fresh.insert((origin.topic, origin.source));
        }
    }
}

/// Removes the marker for what the person is now looking at, once they act in
/// its article list (`j`, `k` or `Enter`), not merely move the cursor past it:
/// moving the sidebar cursor shows a feed's articles at once, so clearing on
/// that would erase the markers while scanning the list for them.
///
/// With a feed row selected, only that feed is cleared. With an "All
/// [category]" row selected, the list shows every feed in the category
/// together, so all of that category's feeds are cleared. `topic` is `None`
/// only when there are no topics. (`clear_fresh_for_selection` handles the
/// "All feeds" row, which clears everything.)
fn clear_fresh_feeds(
    fresh: &mut HashSet<(String, String)>,
    topic: Option<&String>,
    source: Option<&str>,
) {
    let Some(topic) = topic else { return };
    match source {
        // `retain` keeps only the entries for which the closure is true, so
        // this drops exactly the one `(topic, source)` pair.
        Some(source) => fresh.retain(|(t, s)| !(t == topic && s == source)),
        None => fresh.retain(|(t, _)| t != topic),
    }
}

/// The sidebar's top section: the feed list, laid out like the Android app's
/// home screen. First an "All feeds" row (or "All [category]" while a
/// category filter is set), then every feed A-Z, each one drawn as
///
/// ```text
/// Feed name              <- bold; orange if it has unread articles, green if not
/// 3 unread ✦             <- muted; the new-article icon in blue after a refresh found some
/// [category]             <- the selection color
/// (blank line)
/// ```
///
/// The selected row is painted as one block (background `accent_selected`,
/// text in the background color), so its `[category]` stays readable even
/// though that text is normally the selection color. Colors and the icon all
/// come from `theme` and `icons`; nothing is hard-coded.
///
/// `unread_by_feed` has each feed's unread count (a missing entry means 0).
/// The "All" row's count is the sum over the feeds currently listed.
/// `fresh_feeds` holds the feeds a refresh found new articles for.
///
/// Walks the same `sidebar_rows` order `Up`/`Down` use, so what is drawn and
/// what the cursor lands on cannot drift apart. Same many-arguments `#[allow]`
/// as `render_sidebar`.
#[allow(clippy::too_many_arguments)]
fn render_feeds_section(
    frame: &mut Frame,
    theme: &Theme,
    icons: &IconSettings,
    area: Rect,
    topics: &[String],
    sources_by_topic: &HashMap<String, Vec<String>>,
    category_filter: Option<&str>,
    selected_topic: Option<&str>,
    selected_source: Option<&str>,
    fresh_feeds: &HashSet<(String, String)>,
    unread_by_feed: &HashMap<(String, String), u32>,
) {
    // The box title says which set of feeds is listed.
    let title = match category_filter {
        Some(category) => format!(" Feeds — [{category}] "),
        None => " Feeds — all ".to_string(),
    };
    let block = Block::new()
        .title(title)
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    let rows = sidebar_rows(topics, sources_by_topic, category_filter);

    // The unread count of one feed: a missing entry means none are unread.
    let unread_of = |topic: &str, name: &str| -> u32 {
        unread_by_feed
            .get(&(topic.to_string(), name.to_string()))
            .copied()
            .unwrap_or(0)
    };

    // The "All" row's totals, summed over every feed currently listed
    // (`rows` already respects the category filter). `skip(1)` leaves out the
    // `All` row itself, which is always first.
    let mut total_unread = 0;
    let mut any_fresh = false;
    for row in rows.iter().skip(1) {
        if let SidebarRow::Feed(index, name) = row {
            total_unread += unread_of(&topics[*index], name);
            any_fresh |= fresh_feeds.contains(&(topics[*index].clone(), name.to_string()));
        }
    }

    // Built alongside `items` below: each row knows whether it is the
    // selected one as it is turned into a `ListItem`, so its position is
    // recorded then instead of searched for afterwards.
    let mut selected_row_index = None;
    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(row_index, row)| {
            // What this row shows: its name, unread count, new marker, and
            // (feeds only) its category tag.
            let (name, unread, fresh, tag, selected) = match *row {
                SidebarRow::All => {
                    let label = match category_filter {
                        Some(category) => format!("All [{category}]"),
                        None => "All feeds".to_string(),
                    };
                    (
                        label,
                        total_unread,
                        any_fresh,
                        None,
                        selected_source.is_none(),
                    )
                }
                SidebarRow::Feed(index, name) => (
                    name.to_string(),
                    unread_of(&topics[index], name),
                    fresh_feeds.contains(&(topics[index].clone(), name.to_string())),
                    Some(format!("[{}]", topics[index])),
                    Some(topics[index].as_str()) == selected_topic && selected_source == Some(name),
                ),
            };
            if selected {
                selected_row_index = Some(row_index);
            }

            // On the selected row every piece of text uses the background
            // color (it sits on a `accent_selected` block); otherwise each
            // piece has its own color.
            let color = |normal| if selected { theme.background } else { normal };

            // Line 1: the name, bold; unread feeds in the unread color,
            // fully read ones in the read color (as on Android).
            let name_color = if unread > 0 {
                theme.accent_unread
            } else {
                theme.accent_read
            };
            let mut lines = vec![Line::from(Span::styled(
                name,
                Style::new()
                    .fg(color(name_color))
                    .add_modifier(Modifier::BOLD),
            ))];

            // Line 2: "N unread", plus the new-article icon in blue when a
            // refresh found something here.
            let mut count_line = vec![Span::styled(
                format!("{unread} unread"),
                Style::new().fg(color(theme.text_muted)),
            )];
            if fresh {
                count_line.push(Span::styled(
                    format!(" {}", icons.glyph(Icon::New)),
                    Style::new().fg(color(theme.accent_new)),
                ));
            }
            lines.push(Line::from(count_line));

            // Line 3 (feeds only): the category tag.
            if let Some(tag) = tag {
                lines.push(Line::from(Span::styled(
                    tag,
                    Style::new().fg(color(theme.accent_selected)),
                )));
            }

            // A blank spacer line, explicitly in the normal background so a
            // selected row's block does not run on into the gap.
            lines.push(Line::styled("", Style::new().bg(theme.background)));

            // The selected row's block color goes on the whole item.
            let item = ListItem::new(lines);
            if selected {
                item.style(Style::new().bg(theme.accent_selected))
            } else {
                item
            }
        })
        .collect();

    // `List` is a `StatefulWidget`: rendering it takes a `&mut ListState`
    // that records which row is selected (so the list scrolls to keep it in
    // view). It is rebuilt every frame from the caller's own selection state.
    let mut state = ListState::default();
    state.select(selected_row_index);

    frame.render_stateful_widget(List::new(items).block(block), area, &mut state);
}

/// The sidebar's bottom section: each article state's icon, in that state's
/// color, next to its label (unread/new/read/saved/skipped, following
/// `docs/ARCHITECTURE.md`'s "Article States & Behavior" table order, with
/// "new" next to "unread" since it's a kind of unread). The legend is
/// always on screen, so there's no need to remember which icon or color
/// means what.
///
/// The colors themselves come straight from `theme.accent_*` -- the very
/// same fields `article_item` above reads to color the article list -- never
/// a hardcoded value here. That's the whole point of this section: if
/// someone swaps `theme.toml` for a different palette (Mocha, Frappé, a
/// fully custom one), this legend updates automatically along with the
/// article list it's explaining, instead of silently going stale.
fn render_colors_section(frame: &mut Frame, theme: &Theme, icons: &IconSettings, area: Rect) {
    let block = Block::new()
        .title(" Colors ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    // One row per article state: the same icon and color `article_item`
    // draws for that state, so this legend explains both at once. The
    // array's length is written as `LEGEND_ROWS`, the same constant
    // `render_sidebar` sizes this section with, so the compiler rejects a
    // row added here without the height growing to fit it.
    let states: [(&str, Icon, Color); LEGEND_ROWS] = [
        ("unread", Icon::Unread, theme.accent_unread),
        ("new", Icon::New, theme.accent_unread),
        ("read", Icon::Read, theme.accent_read),
        ("saved", Icon::Starred, theme.accent_saved),
        ("skipped", Icon::Skipped, theme.accent_skipped),
    ];

    let lines: Vec<Line> = states
        .iter()
        .map(|(label, icon, color)| {
            Line::from(vec![
                Span::styled(format!("{} ", icons.glyph(*icon)), Style::new().fg(*color)),
                Span::styled(*label, Style::new().fg(theme.text_primary)),
            ])
        })
        .collect();

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// What (if anything) `render_articles` should show about an active
/// search, layered on top of whatever `view`'s ordinary title would
/// otherwise be. Computed once in `render` (see its own doc comment on
/// `search_status`) from `mode` and `draw_until_quit`'s committed `search`,
/// so `render_articles` itself doesn't need to know about either.
///
/// - `None`: no search active -- `view`'s own title shows, unchanged.
/// - `Typing`: `Mode::Searching` is open and this is its live buffer.
///   Deliberately rendered *inside the block's own title* (see
///   `render_articles`'s cursor placement below) rather than as a separate
///   popup: a popup would cover the very results it's supposed to be
///   live-filtering, defeating "as the person types."
/// - `Committed`: a query was confirmed with `Enter` and is still
///   narrowing whichever list is showing; no live cursor, since typing
///   has stopped and ordinary navigation (`j`/`k`/`Enter`/`s`/`x`/`n`) is
///   what's running now.
enum SearchStatus<'a> {
    None,
    Typing(&'a str),
    Committed(&'a str),
}

/// The right pane: every article in `articles` -- populated by whichever
/// query `view` implies (`storage.articles_by_topic(selected_topic)` for
/// `View::Topic`, `storage.saved_articles()` for `View::Saved`,
/// `storage.all_articles()` for `View::History` -- see `articles_for_view`,
/// which every view-switching keybind in `draw_until_quit` goes through),
/// most recent first, styled per its read/skipped/saved state using the
/// matching `theme.accent_*` color -- ARCHITECTURE.md's "Article States &
/// Behavior" table. `view` decides the title and, via `article_item`,
/// which meta line and whether a truncated note preview render for each
/// item -- the saved view is the one place a note is worth surfacing
/// without pressing `n` first, since "which of these did I leave a note
/// on, and roughly what did it say" is the whole point of browsing it.
///
/// `search` overrides that title (see `SearchStatus`) whenever a search is
/// active -- `articles` itself, by the time this runs, already *is* the
/// filtered list (`draw_until_quit`'s `Mode::Searching` handling and
/// `storage.search_articles` calls build it before `render` ever sees it),
/// so this function's own job re: search is purely presentational: what to
/// show in the title, and where to put the terminal cursor while typing.
///
/// `search` pushes this past clippy's default 7-argument threshold for
/// `too_many_arguments` -- same reasoning as the `#[allow]` on `render`
/// itself: `render_articles` has exactly one call site (`render`, which
/// already computed `search_status` from its own state), so a params
/// struct here would just be `render`'s params one layer down, with no
/// other caller to justify it.
#[allow(clippy::too_many_arguments)]
fn render_articles(
    frame: &mut Frame,
    theme: &Theme,
    icons: &IconSettings,
    new_article_ids: &HashSet<i64>,
    area: Rect,
    selected_topic: Option<&str>,
    articles: &[Article],
    selected_article: Option<usize>,
    view: View,
    search: SearchStatus,
) {
    let title = match search {
        SearchStatus::Typing(text) => format!(" Search: {text} "),
        SearchStatus::Committed(query) => {
            format!(
                " Search: \u{201c}{query}\u{201d} ({} result{}) ",
                articles.len(),
                if articles.len() == 1 { "" } else { "s" }
            )
        }
        SearchStatus::None => match view {
            View::Saved => " Saved Articles ".to_string(),
            View::History => " History ".to_string(),
            View::Topic => match selected_topic {
                Some(topic) => format!(" Articles — {topic} "),
                None => " Articles ".to_string(),
            },
        },
    };

    let block = Block::new()
        .title(title)
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.panel_border));

    // While actively typing, the block's own top border/title row doubles
    // as the input line -- placing the real terminal cursor right after
    // the live query text is what actually signals "you're typing here,"
    // the same reasoning as `render_note_popup`'s cursor placement. `+ 9`
    // is `" Search: "`'s own length (the literal prefix in the `title`
    // `format!` above), so this has to stay in sync with that string if it
    // ever changes.
    if let SearchStatus::Typing(text) = search {
        let cursor_x = area.x + 1 + 9 + text.chars().count() as u16;
        let cursor_y = area.y;
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    if articles.is_empty() {
        // An empty topic (or no topics at all) shouldn't render as a
        // blank pane with no explanation -- that looks indistinguishable
        // from a bug. Same logic applies to an empty saved view ("nothing
        // saved yet" is a real, expected state, not something to leave the
        // user guessing about), an empty history view (in practice, only
        // ever happens on a completely fresh install before the very first
        // fetch has run), and a search with no matches.
        let message = match search {
            SearchStatus::Typing(_) | SearchStatus::Committed(_) => "No matches.",
            SearchStatus::None => match view {
                View::Saved => "No saved articles yet -- press s on an article to save it.",
                View::History => "No articles fetched yet.",
                View::Topic => "No articles yet -- run a fetcher first.",
            },
        };
        let empty = Paragraph::new(message)
            .style(Style::new().fg(theme.text_muted))
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = articles
        .iter()
        .map(|article| article_item(theme, icons, new_article_ids, article, view))
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
/// meta line, plus -- in `View::Saved` specifically, when the article
/// actually has a note -- a third line with a truncated preview of it (see
/// `truncate_preview`). `view` is a plain value parameter rather than this
/// function reaching for the caller's other state: whether to show the
/// note preview, and which meta line to build, is entirely
/// `render_articles`'s call (only `View::Saved` wants either), and
/// `article_item` doesn't need to know anything else about *why*, just
/// *which view*.
///
/// The meta line itself differs by view: `View::Saved` shows the source
/// name plus `saved_at` (falling back to `article.timestamp`'s publish
/// date only when this article was saved before migration 003 started
/// tracking `saved_at` -- see `saved_meta_line`), since "when did I save
/// this" is what's actually useful while browsing saved articles.
/// Everywhere else shows the source name plus the article's own publish
/// `timestamp`, unchanged from before this view got its own meta line.
fn article_item<'a>(
    theme: &Theme,
    icons: &IconSettings,
    new_article_ids: &HashSet<i64>,
    article: &'a Article,
    view: View,
) -> ListItem<'a> {
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

    // The state icon in front of the title, in the same color. It's one
    // cell plus a space, so the meta line's two-space indent below still
    // lines up under the title text. `Line::from(vec![...])` builds one
    // line out of several differently-styled pieces (`Span`s).
    let state_icon = icons.glyph(article_state_icon(article, new_article_ids));
    let title_line = Line::from(vec![
        Span::styled(format!("{state_icon} "), Style::new().fg(title_color)),
        Span::styled(article.title.as_str(), Style::new().fg(title_color)),
    ]);

    // `article.saved` is checked alongside `view == View::Saved`, not just
    // `view` alone: search (see `SearchStatus`) runs unscoped across every
    // article regardless of `saved`, so a search result surfaced while
    // `View::Saved` is showing can genuinely be an article that was never
    // saved at all -- claiming "saved <date>" about one (even via
    // `saved_meta_line`'s pre-migration fallback to `timestamp`) would be
    // outright wrong, not just an honest gap in the data. Those fall back
    // to the same plain source/publish-timestamp line every other view
    // uses.
    let meta_line = if view == View::Saved && article.saved {
        saved_meta_line(theme, article)
    } else {
        Line::from(Span::styled(
            format!(
                "  {} · {}",
                article.source,
                article.timestamp.format("%Y-%m-%d %H:%M")
            ),
            Style::new().fg(theme.text_muted),
        ))
    };

    let mut lines = vec![title_line, meta_line];

    // `article.note.as_deref()` turns `&Option<String>` into
    // `Option<&str>` without cloning the note just to look at it --
    // `if let Some(note) = ...` then only runs at all in `View::Saved`
    // *and* when a note actually exists, so an article saved without one
    // still renders as the plain two-line item above instead of a blank or
    // missing third line.
    if view == View::Saved
        && let Some(note) = article.note.as_deref()
    {
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

/// Which state icon an article gets in the list. Same priority order as
/// `article_item`'s title color (skipped, then saved, then read), with one
/// addition: an unread article that arrived this session (its ID is in
/// `new_article_ids`) gets `Icon::New` instead of `Icon::Unread`. Once it's
/// read, saved, or skipped, it shows that state like any other article.
fn article_state_icon(article: &Article, new_article_ids: &HashSet<i64>) -> Icon {
    if article.skipped {
        Icon::Skipped
    } else if article.saved {
        Icon::Starred
    } else if article.read {
        Icon::Read
    } else if new_article_ids.contains(&article.id) {
        Icon::New
    } else {
        Icon::Unread
    }
}

/// `View::Saved`'s own meta line: `"  {source} · saved {date}"`, plus a
/// trailing `", noted {date}"` when `noted_at` is meaningfully different
/// from `saved_at` -- ARCHITECTURE.md's "if `noted_at` differs meaningfully
/// from `saved_at` (note added/edited after the initial save), show that
/// too."
///
/// "Meaningfully differs" is defined here as "renders to a different
/// day": both dates are formatted to `%Y-%m-%d` first, and the `noted`
/// clause is only appended if that formatted string actually differs from
/// `saved`'s. This deliberately ignores same-day differences down to the
/// second (writing a note minutes after saving, say) -- the two dates
/// would print identically either way, so appending a second, redundant
/// "noted <same date>" would tell the reader nothing they didn't already
/// see in the "saved" clause right next to it.
///
/// Returns `Line<'static>` rather than borrowing from `article`: unlike
/// the source name (a `&str` already living inside `article`), the
/// formatted date strings are freshly built `String`s with nothing to
/// borrow from, so there's no lifetime to tie this to `article` in the
/// first place.
fn saved_meta_line(theme: &Theme, article: &Article) -> Line<'static> {
    // `saved_at` is `None` only for a saved article that predates
    // migration 003 (see the migration's own doc comment) -- an honest gap
    // in the data, not a bug, so this falls back to the article's publish
    // `timestamp` rather than showing a blank or a placeholder like "?".
    let saved_day = article
        .saved_at
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| article.timestamp.format("%Y-%m-%d").to_string());

    let noted_day = article.noted_at.map(|d| d.format("%Y-%m-%d").to_string());

    let mut text = format!("  {} · saved {saved_day}", article.source);
    if let Some(noted_day) = noted_day
        && noted_day != saved_day
    {
        text.push_str(&format!(", noted {noted_day}"));
    }

    Line::from(Span::styled(text, Style::new().fg(theme.text_muted)))
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
/// so far -- see `KEYBIND_HINTS`/`SAVED_HINTS`/`EDITING_NOTE_HINTS`/
/// `SEARCH_HINTS` and this module's top-level doc comment for which parts
/// of ARCHITECTURE.md's full keybind table that excludes. `mode` takes
/// priority over `view`: while the note popup, the add-source popup, or
/// the search input is open, every other keybind (navigation included) is
/// suspended for the duration (see the `Mode::EditingNote` match arm, the
/// `Mode::Searching` check, and the `Mode::AddSource` check in
/// `draw_until_quit`), so one of `EDITING_NOTE_HINTS` /
/// `ADD_SOURCE_URL_HINTS` / `ADD_SOURCE_CONFIRM_HINTS` / `SEARCH_HINTS`
/// shows regardless of which view sits underneath it. Otherwise, `view`
/// picks between the normal-mode hint lines -- `View::Saved` and
/// `View::History` share `SAVED_HINTS` (see its own doc comment for why:
/// both disable the same keys for the same reason).
///
/// The footer is two rows. The upper row is always the hint line for the
/// current mode. The lower row is the status line: `notice` (a one-shot
/// message from `draw_until_quit`, such as the refresh result) on the left,
/// empty when there is none, so a message never hides the hints.
///
/// `feed_list` (the active feed list's path, home as `~`) is drawn
/// right-aligned on the status row, as "Feed List: <path>" (or the file icon
/// in Nerd mode), in `text_muted`. The notice on the left always
/// keeps priority: the path only gets the cells left over after it plus
/// `FOOTER_GAP`, is shortened from the left to fit (see `fit_feed_list`),
/// and is hidden when even the file name won't fit, or when the terminal
/// is narrower than `NARROW_FOOTER_WIDTH`.
///
/// Eight parameters, past clippy's default limit of seven. Same reasoning
/// as `render`'s own `#[allow]`: one call site, and every argument is
/// already in the caller's hands.
#[allow(clippy::too_many_arguments)]
fn render_footer(
    frame: &mut Frame,
    theme: &Theme,
    icons: &IconSettings,
    area: Rect,
    mode: &Mode,
    view: View,
    notice: Option<&str>,
    feed_list: &str,
) {
    // Split the two footer rows apart: hints on top, status underneath.
    let [hint_area, status_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .areas(area);

    // The upper row: the hint line for the current mode.
    let hints = Paragraph::new(footer_hints(mode, view))
        .style(Style::new().bg(theme.background).fg(theme.text_muted));
    frame.render_widget(hints, hint_area);

    // The lower row, left side: the notice, or nothing. `unwrap_or("")`
    // turns "no notice" into an empty string.
    let left = notice.unwrap_or("");
    let status = Paragraph::new(left).style(Style::new().bg(theme.background).fg(theme.text_muted));
    frame.render_widget(status, status_area);

    // The right side, only on a wide-enough terminal. `cell_width` counts
    // display cells, not characters, so wide characters in the hint line
    // reserve the space they really take.
    if area.width < NARROW_FOOTER_WIDTH {
        return;
    }
    let used = cell_width(left) as u16 + FOOTER_GAP;
    let available = status_area.width.saturating_sub(used) as usize;
    if let Some(text) = fit_feed_list(
        icons.feed_list_label(),
        feed_list,
        icons.ellipsis(),
        available,
    ) {
        // A one-row `Rect` exactly as wide as the text, pushed against the
        // right edge of the footer. `area.x + area.width - width` is the
        // first column of that strip; `fit_feed_list` already guaranteed
        // `width <= available < area.width`, so this can't underflow.
        let width = cell_width(&text) as u16;
        let strip = Rect {
            x: status_area.x + status_area.width - width,
            y: status_area.y,
            width,
            height: 1,
        };
        let path =
            Paragraph::new(text).style(Style::new().bg(theme.background).fg(theme.text_muted));
        frame.render_widget(path, strip);
    }
}

/// How many terminal cells `text` takes up: one for most characters, two
/// for wide ones like CJK and most emoji. Uses ratatui's own measurement
/// (`Span::width`), so it always agrees with how ratatui lays the text
/// out, without adding a dependency.
fn cell_width(text: &str) -> usize {
    Span::raw(text).width()
}

/// The footer's feed list text, made to fit in `available` cells, or
/// `None` if it can't: `"<label> <path>"`, with the path shortened from the
/// left by `truncate_path_left` when needed (e.g. `"Feed List: …/casual.toml"`).
/// `None` means not even the label plus the file name fit, so the footer
/// shows no path at all rather than a meaningless fragment.
fn fit_feed_list(label: &str, path: &str, ellipsis: &str, available: usize) -> Option<String> {
    // The cells left for the path once the label and its space are placed.
    // `checked_sub` returns `None` instead of wrapping around below zero,
    // and `?` then returns `None` from this function: no room for the label.
    let budget = available.checked_sub(cell_width(label) + 1)?;
    truncate_path_left(path, ellipsis, budget).map(|path| format!("{label} {path}"))
}

/// `path`, made to fit in `available` cells by cutting from the *left*, so
/// the file name, the most useful part, stays visible:
///
/// 1. the whole path, if it fits;
/// 2. otherwise `<ellipsis><end of path>`, keeping as much of the end as
///    fits. The kept part must include the last `/` and the whole file
///    name (`…/casual.toml`);
/// 3. otherwise `None`.
///
/// Widths are display cells (`cell_width`), so a wide character counts as
/// two, and the path is cut on character boundaries, never mid-character.
/// Used by the footer (`fit_feed_list`) and the feed list picker.
fn truncate_path_left(path: &str, ellipsis: &str, available: usize) -> Option<String> {
    // Step 1: the whole path fits.
    if cell_width(path) <= available {
        return Some(path.to_string());
    }

    // The byte position where the part we must keep starts: the last `/`
    // (so the kept text reads `…/name`), or the very start when the path
    // has no `/` at all, in which case nothing can be dropped.
    // `rfind` searches from the right and returns a byte index.
    let must_keep_from = path.rfind('/').unwrap_or(0);

    // Step 2: the cells left for the end of the path once the ellipsis is
    // placed (`?` returns `None` if not even that fits).
    let budget = available.checked_sub(cell_width(ellipsis))?;

    // Walk the path backwards one character at a time, adding each
    // character's width, and stop before the first one that doesn't fit.
    // `char_indices().rev()` yields `(byte_index, char)` pairs from the
    // end. `keep_from` ends up as the byte index where the kept text
    // starts.
    let mut keep_from = path.len();
    let mut used = 0;
    for (index, ch) in path.char_indices().rev() {
        // `encode_utf8` writes the char into a small stack buffer and
        // returns it as a `&str`, so `cell_width` can measure it without
        // allocating a `String`.
        let width = cell_width(ch.encode_utf8(&mut [0u8; 4]));
        if used + width > budget {
            break;
        }
        used += width;
        keep_from = index;
    }

    // Step 3: only show it if the file name (and its `/`) made it in.
    if keep_from > must_keep_from {
        return None;
    }
    Some(format!("{ellipsis}{}", &path[keep_from..]))
}

/// Where lists are defined, for messages: `lists.toml`'s path with `~` for
/// the home directory, or just `lists.toml` if the path can't be worked
/// out (no `$HOME`).
fn lists_toml_display(home: Option<&Path>) -> String {
    match feedlists::config::lists_path() {
        Ok(path) => crate::paths::abbreviate_home(&path, home),
        Err(_) => "lists.toml".to_string(),
    }
}

/// The footer's hint line for `mode` and `view`, used whenever there's no
/// notice to show instead (see `render_footer`).
fn footer_hints(mode: &Mode, view: View) -> &'static str {
    match mode {
        Mode::EditingNote { .. } => EDITING_NOTE_HINTS,
        Mode::AddSource(AddSourceStep::Url { .. }) => ADD_SOURCE_URL_HINTS,
        Mode::AddSource(AddSourceStep::Confirm { .. }) => ADD_SOURCE_CONFIRM_HINTS,
        Mode::Error { .. } => ERROR_HINTS,
        Mode::Confirm(_) => CONFIRM_HINTS,
        Mode::Searching { .. } => SEARCH_HINTS,
        Mode::IconCheck => ICON_CHECK_HINTS,
        Mode::ThemePicker { .. } => THEME_PICKER_HINTS,
        Mode::ListPicker { .. } => LIST_PICKER_HINTS,
        Mode::Stats { .. } => stats::HINTS,
        Mode::Help { .. } => help::HINTS,
        Mode::ReadLater { .. } => read_later::LIST_HINTS,
        Mode::PasteLink { .. } => read_later::PASTE_HINTS,
        Mode::LinkChoice { .. } => read_later::CHOICE_HINTS,
        Mode::Normal => match view {
            View::Topic => KEYBIND_HINTS,
            View::Saved | View::History => SAVED_HINTS,
        },
    }
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

/// Carries out an export for real: reads `export.toml`, writes the Markdown
/// file, and returns the popup that reports the result (a success message,
/// or the error). Called straight from `E` when there is no file to replace,
/// and again from the confirm answer when there is.
fn run_export(storage: &Storage) -> Mode {
    // Gather everything that can fail into one `Result`: `and_then` runs the
    // next step only if the previous one worked, and `?` passes any error up.
    match storage.saved_articles().and_then(|saved| {
        let config = ExportConfig::load()?;
        let path = export::export_saved_articles(&config, &saved)?;
        Ok((saved.len(), path))
    }) {
        Ok((count, path)) => Mode::Error {
            title: "Export",
            message: format!("Exported {count} saved article(s) to {}", path.display()),
        },
        Err(err) => Mode::Error {
            title: "Error",
            message: format!("export failed: {err:#}"),
        },
    }
}

/// The yes/no popup for `Mode::Confirm`: a bordered box centered over `area`
/// saying exactly what will happen, so the answer is an informed one. Same
/// `Clear` + `Block` approach as `render_error_popup`.
fn render_confirm_popup(frame: &mut Frame, theme: &Theme, area: Rect, pending: &Pending) {
    // The heading and the message for each kind of question. `format!`
    // builds an owned `String` because the messages include the title.
    let (heading, message) = match pending {
        Pending::Unsave {
            title, has_note, ..
        } => {
            if *has_note {
                (
                    " Un-save? ",
                    format!(
                        "\"{title}\"\n\nThis article has a note. A note and a save go together, so un-saving also DELETES the note.\n\nRemove the save and delete the note? (y = yes, any other key keeps both)"
                    ),
                )
            } else {
                (
                    " Un-save? ",
                    format!(
                        "\"{title}\"\n\nRemove this article from Saved? (y = yes, any other key cancels)"
                    ),
                )
            }
        }
        Pending::DeleteNote { title, .. } => (
            " Delete note? ",
            format!(
                "\"{title}\"\n\nDelete this note? The article stays saved. (y = yes, any other key keeps the note)"
            ),
        ),
        Pending::DiscardReadLater { title, .. } => (
            " Discard? ",
            format!(
                "\"{title}\"\n\nTake this link off the Read Later list? (y = yes, any other key keeps it)"
            ),
        ),
        Pending::ReplaceExport { path } => (
            " Replace export? ",
            format!(
                "{} already exists.\n\nReplace it with a fresh export? Anything you changed in that file by hand will be lost. (y = yes, any other key cancels)",
                path.display()
            ),
        ),
    };

    // Tall enough for the longest message once it wraps, capped by
    // `centered_rect` to the room that is actually there.
    let popup_area = centered_rect(area, 70, 10);
    frame.render_widget(Clear, popup_area);

    let block = Block::new()
        .title(heading)
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.accent_selected));

    let paragraph = Paragraph::new(message)
        .style(Style::new().fg(theme.text_primary))
        .wrap(Wrap { trim: true })
        .block(block);
    frame.render_widget(paragraph, popup_area);
}

/// The error/result popup: same "bordered box centered over `area`" idea as
/// `render_note_popup` above, but read-only -- no cursor, dismissed by any
/// keypress (see the `Mode::Error` check in `draw_until_quit`) rather than
/// an `Enter`/`Esc` split. `Wrap { trim: true }` lets a longer message (e.g.
/// `open_in_w3m`'s "not installed" text, or an exported file's full path)
/// flow across multiple lines instead of running off the popup's fixed
/// width. `title` is `Mode::Error`'s own field, passed straight through --
/// `" Error "` for a genuine failure, `" Export "` for a successful `E`
/// export, so the border itself tells the two apart at a glance.
fn render_error_popup(
    frame: &mut Frame,
    theme: &Theme,
    icons: &IconSettings,
    area: Rect,
    title: &str,
    message: &str,
) {
    let popup_area = centered_rect(area, 60, 5);

    // Same reasoning as `render_note_popup`'s own `Clear` -- without it the
    // article list underneath would show through around the popup's text.
    frame.render_widget(Clear, popup_area);

    // A real error gets `Icon::Error` in its title. The `E` export result
    // shares this popup (with the title "Export") and gets no icon,
    // because it's good news. Matching on the title text works because
    // `Mode::Error`'s titles are fixed literals chosen at each call site.
    let heading = if title == "Error" {
        format!(" {} {title} ", icons.glyph(Icon::Error))
    } else {
        format!(" {title} ")
    };
    let block = Block::new()
        .title(heading)
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
                .title(" Add Source — Feed or page address ")
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

        AddSourceStep::Confirm {
            url,
            name,
            topic,
            field,
            topic_options,
            note,
            error,
        } => {
            // Four content lines (URL, Name, Topic, the "existing topics"
            // hint) always show; a fifth appears only when there's a
            // validation/write error to surface. `+ 2` for the border,
            // same as the `Url` branch above.
            // `+ 1` more when a note about how the feed was found is shown.
            let height = 6 + u16::from(error.is_some()) + u16::from(note.is_some());
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
                Line::from(Span::styled(
                    format!("URL:   {url}"),
                    Style::new().fg(theme.text_muted),
                )),
                Line::from(Span::styled(
                    format!("Name:  {name}"),
                    focus_style(ConfirmField::Name),
                )),
                Line::from(Span::styled(
                    format!("Topic: {topic}"),
                    focus_style(ConfirmField::Topic),
                )),
                Line::from(Span::styled(
                    format!("       existing: {}", topic_options.join(", ")),
                    Style::new().fg(theme.text_muted),
                )),
            ];
            // When the feed was found rather than typed, say how, in the muted
            // colour, on its own line right under the URL (`insert(1, ..)`
            // puts it at position 1, pushing Name and Topic down one row).
            if let Some(text) = note {
                lines.insert(
                    1,
                    Line::from(Span::styled(
                        format!("       {text}"),
                        Style::new().fg(theme.text_muted),
                    )),
                );
            }
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
            // The note line, when shown, pushes both fields down one row.
            let shift = u16::from(note.is_some());
            let (field_text, row) = match field {
                ConfirmField::Name => (name.as_str(), 2 + shift),
                ConfirmField::Topic => (topic.as_str(), 3 + shift),
            };
            let cursor_x = popup_area.x + 1 + label_width + field_text.chars().count() as u16;
            let cursor_y = popup_area.y + row;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

/// The `f` feed list picker popup (`Mode::ListPicker`): one row per list,
/// in `lists.toml` order, each showing
/// `<pointer> <name> <unread icon> <count>  <path>`. The pointer
/// (`Icon::Collapsed`) marks the highlighted row, the active list's name is
/// followed by "(active)", and a list whose file can't be read shows the
/// error icon and "unavailable" instead of a count. Paths are cut from the
/// left to fit (`truncate_path_left`), so file names stay visible. A last
/// line says where lists are defined, since lists are added by editing
/// `lists.toml`, not from here.
///
/// All glyphs come from `icons` and all colors from `theme`.
fn render_list_picker_popup(
    frame: &mut Frame,
    theme: &Theme,
    icons: &IconSettings,
    area: Rect,
    selected: usize,
    rows: &[ListRow],
) {
    // The popup takes 80% of the body's width; the border uses one cell on
    // each side.
    let popup_width = (area.width as u32 * 80 / 100) as u16;
    let inner = popup_width.saturating_sub(2) as usize;

    // Each row's name text, with "(active)" for the active list, padded to
    // the widest so the counts line up. `max()` is `None` only for no rows,
    // which the picker never has.
    let names: Vec<String> = rows
        .iter()
        .map(|row| {
            if row.active {
                format!("{} (active)", row.name)
            } else {
                row.name.clone()
            }
        })
        .collect();
    let name_width = names.iter().map(|name| cell_width(name)).max().unwrap_or(0);

    let muted = Style::new().fg(theme.text_muted);
    let mut lines = Vec::new();
    for (index, (row, name)) in rows.iter().zip(&names).enumerate() {
        let is_selected = index == selected;
        let name_style = if is_selected {
            Style::new()
                .fg(theme.accent_selected)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.text_primary)
        };
        let pointer = if is_selected {
            icons.glyph(Icon::Collapsed)
        } else {
            " "
        };

        // The count column: `{:>5}` right-aligns the number in 5 columns.
        let (count_text, count_style) = match row.unread {
            Some(count) => (
                format!("{} {count:>5} unread", icons.glyph(Icon::Unread)),
                Style::new().fg(theme.accent_unread),
            ),
            None => (
                format!("{} unavailable  ", icons.glyph(Icon::Error)),
                Style::new().fg(theme.accent_breaking),
            ),
        };

        // `{:<width$}` pads to a width held in a variable. Padding is by
        // characters, so wide characters in a name are padded by hand from
        // `cell_width` instead.
        let padding = " ".repeat(name_width - cell_width(name));
        let used = cell_width(pointer) + 1 + name_width + 2 + cell_width(&count_text) + 2;
        let path = truncate_path_left(&row.path, icons.ellipsis(), inner.saturating_sub(used))
            .unwrap_or_default();

        lines.push(Line::from(vec![
            Span::styled(format!("{pointer} "), name_style),
            Span::styled(format!("{name}{padding}  "), name_style),
            Span::styled(count_text, count_style),
            Span::styled(format!("  {path}"), muted),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!(
            "Add or edit lists in {}",
            lists_toml_display(crate::paths::home_dir().as_deref())
        ),
        muted,
    )));

    // `+ 2` for the border, same as every other popup in this module.
    let popup_area = centered_rect(area, 80, lines.len() as u16 + 2);

    // Same `Clear` reasoning as `render_note_popup`.
    frame.render_widget(Clear, popup_area);

    let block = Block::new()
        .title(format!(" {} Feed Lists ", icons.glyph(Icon::FeedFile)))
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.accent_selected));

    frame.render_widget(Paragraph::new(lines).block(block), popup_area);
}

/// The `t` theme picker popup (`Mode::ThemePicker`): one row per built-in
/// theme, in alphabetical order (`theme::picker_order`), with the
/// highlighted row (`selected`, a row position) marked, and a few color swatches drawn
/// in *that theme's own* colors (background, text, unread, read, saved,
/// skipped, selected). You can compare themes before moving to them, while
/// the screen behind shows the highlighted one in full.
///
/// When `overrides > 0`, a note explains that `theme.toml` is replacing some
/// of every theme's colors. Otherwise an old full `[theme]` table makes
/// the picker look broken, since nothing would visibly change.
fn render_theme_picker_popup(
    frame: &mut Frame,
    theme: &Theme,
    icons: &IconSettings,
    area: Rect,
    selected: usize,
    overrides: usize,
) {
    let mut lines = Vec::new();

    // One line per built-in, in alphabetical order. `.enumerate()` pairs
    // each theme with its row number, so the row being previewed can be
    // marked and bolded.
    for (row, &index) in picker_order().iter().enumerate() {
        let builtin = &BUILTIN_THEMES[index];
        let is_selected = row == selected;
        let name_style = if is_selected {
            Style::new()
                .fg(theme.accent_selected)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(theme.text_primary)
        };
        // The pointer on the highlighted row, or a blank of the same
        // one-cell width, so the names line up.
        let pointer = if is_selected {
            icons.glyph(Icon::Collapsed)
        } else {
            " "
        };

        // `{:<22}` pads the name with spaces to 22 characters (left-aligned),
        // so every row's swatches start in the same column. 22 fits the
        // longest name, "Catppuccin Macchiato", plus a gap.
        let mut spans = vec![
            Span::styled(format!("{pointer} "), name_style),
            Span::styled(format!("{:<22}", builtin.name), name_style),
        ];
        // The swatches. `c` is that theme's palette, not the current one.
        // The built-in table holds plain core RGB values, so convert it to
        // the ratatui-coloured `Theme` first (`Theme::from`, see theme.rs).
        let c = Theme::from(builtin.colors);
        for color in [
            c.background,
            c.text_primary,
            c.accent_unread,
            c.accent_read,
            c.accent_saved,
            c.accent_skipped,
            c.accent_selected,
        ] {
            spans.push(Span::styled(
                format!("{} ", icons.glyph(Icon::Swatch)),
                Style::new().fg(color),
            ));
        }
        lines.push(Line::from(spans));
    }

    // The overrides note, only when it applies.
    if overrides > 0 {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("theme.toml overrides {overrides} of 11 colors in every theme"),
            Style::new().fg(theme.accent_saved),
        )));
    }

    // `+ 2` for the border, same as every other popup in this module.
    let popup_area = centered_rect(area, 60, lines.len() as u16 + 2);

    // Same `Clear` reasoning as `render_note_popup`.
    frame.render_widget(Clear, popup_area);

    let block = Block::new()
        .title(format!(" {} Theme ", icons.glyph(Icon::Theme)))
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.accent_selected));

    frame.render_widget(Paragraph::new(lines).block(block), popup_area);
}

/// The first-run icon check popup (`Mode::IconCheck`): one Nerd Font
/// glyph and the question "Do you see an icon here?".
///
/// The glyph is always `Icon::Linux`'s *Nerd* version (Tux), whatever
/// the current mode, because the question is specifically whether the
/// terminal's font can draw Nerd Font glyphs. It's shown in
/// `accent_unread` and bold so it's easy to spot. With a non-Nerd font it
/// appears as an empty box, a `?`, or nothing at all, which is exactly
/// what the user is being asked to notice.
fn render_icon_check_popup(frame: &mut Frame, theme: &Theme, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            Icon::Linux.get(IconMode::Nerd),
            Style::new()
                .fg(theme.accent_unread)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Do you see an icon here?"),
        Line::from(Span::styled(
            "y = yes, keep icons · n = no, use plain symbols",
            Style::new().fg(theme.text_muted),
        )),
    ];

    // `+ 2` for the border, same as every other popup in this module.
    let popup_area = centered_rect(area, 60, lines.len() as u16 + 2);

    // Same `Clear` reasoning as `render_note_popup`.
    frame.render_widget(Clear, popup_area);

    let block = Block::new()
        .title(" Icons ")
        .borders(Borders::ALL)
        .style(Style::new().bg(theme.background).fg(theme.text_primary))
        .border_style(Style::new().fg(theme.accent_selected));

    let paragraph = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .block(block);
    frame.render_widget(paragraph, popup_area);
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
// `truncate_preview`, `article_state_icon`, and `fetch_notice` get unit
// tests here: they're the pieces of this module's logic that are plain,
// pure functions (plain values in, plain values out, no
// `Storage`/terminal/event dependency) -- exactly the shape that's
// cheap to test directly, unlike `draw_until_quit`'s event loop, which
// this project instead verifies with a pty-driven run (see the project's
// own notes on that, not a `#[test]`).
#[cfg(test)]
mod tests {
    use super::*;

    /// Names sort A-Z ignoring case, so a lowercase name is not pushed
    /// behind every capitalized one, and names that differ only by case
    /// stay next to each other (so `dedup` can remove exact repeats).
    #[test]
    fn sort_names_ignores_case_and_keeps_repeats_adjacent() {
        // `to_string` turns each `&str` literal into an owned `String`.
        let mut names: Vec<String> = ["Zed", "apple", "Banana", "apple", "Apple"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        sort_names(&mut names);
        assert_eq!(names, ["Apple", "apple", "apple", "Banana", "Zed"]);
        names.dedup();
        assert_eq!(names, ["Apple", "apple", "Banana", "Zed"]);
    }

    /// The sidebar rows are the "All" row, then every feed A-Z ignoring case
    /// across categories; a category filter keeps just that category's feeds.
    #[test]
    fn sidebar_rows_are_all_then_feeds_sorted_az() {
        let topics = vec!["gaming".to_string(), "linux".to_string()];
        let mut sources = HashMap::new();
        sources.insert(
            "linux".to_string(),
            vec!["Zeta".to_string(), "alpha".to_string()],
        );
        sources.insert("gaming".to_string(), vec!["Beta".to_string()]);

        // Turn rows into readable text: "All", or "topic/feed".
        let describe = |rows: Vec<SidebarRow>| -> Vec<String> {
            rows.iter()
                .map(|row| match row {
                    SidebarRow::All => "All".to_string(),
                    SidebarRow::Feed(index, name) => format!("{}/{name}", topics[*index]),
                })
                .collect()
        };

        assert_eq!(
            describe(sidebar_rows(&topics, &sources, None)),
            ["All", "linux/alpha", "gaming/Beta", "linux/Zeta"]
        );
        assert_eq!(
            describe(sidebar_rows(&topics, &sources, Some("linux"))),
            ["All", "linux/alpha", "linux/Zeta"]
        );
    }

    /// `Tab` cycles all feeds -> each category -> all feeds; `Shift+Tab`
    /// goes the other way round.
    #[test]
    fn category_filter_cycles_both_ways() {
        let topics = vec!["gaming".to_string(), "linux".to_string()];
        assert_eq!(
            next_category_filter(&topics, None, true).as_deref(),
            Some("gaming")
        );
        assert_eq!(
            next_category_filter(&topics, Some("gaming"), true).as_deref(),
            Some("linux")
        );
        assert_eq!(next_category_filter(&topics, Some("linux"), true), None);
        // Backward from "all feeds" lands on the last category.
        assert_eq!(
            next_category_filter(&topics, None, false).as_deref(),
            Some("linux")
        );
        assert_eq!(next_category_filter(&topics, Some("gaming"), false), None);
    }

    /// Moving down then up walks the rows and clamps at both ends; landing
    /// on "All" clears the feed selection and, with a filter, re-points
    /// `topic_index` at that category.
    #[test]
    fn moving_the_sidebar_cursor_clamps_and_returns_to_all() {
        let topics = vec!["gaming".to_string(), "linux".to_string()];
        let mut sources = HashMap::new();
        sources.insert("linux".to_string(), vec!["alpha".to_string()]);
        sources.insert("gaming".to_string(), vec!["Beta".to_string()]);

        let mut topic_index = 0;
        let mut selected: Option<String> = None;
        // Down: first feed A-Z is "alpha" (linux).
        move_sidebar_selection(1, &topics, &sources, None, &mut topic_index, &mut selected);
        assert_eq!((topic_index, selected.as_deref()), (1, Some("alpha")));
        move_sidebar_selection(1, &topics, &sources, None, &mut topic_index, &mut selected);
        assert_eq!((topic_index, selected.as_deref()), (0, Some("Beta")));
        // Past the last row stays put.
        move_sidebar_selection(1, &topics, &sources, None, &mut topic_index, &mut selected);
        assert_eq!(selected.as_deref(), Some("Beta"));

        // Filter to linux, standing on its only feed, then up to "All [linux]".
        topic_index = 1;
        selected = Some("alpha".to_string());
        move_sidebar_selection(
            -1,
            &topics,
            &sources,
            Some("linux"),
            &mut topic_index,
            &mut selected,
        );
        assert_eq!((topic_index, selected), (1, None));
    }

    /// Unread counts are matched to a configured feed by address first (so a
    /// rename keeps its count), then by the stored category and name.
    #[test]
    fn unread_counts_follow_the_feed_address() {
        let mut addresses: FeedAddresses = HashMap::new();
        addresses.insert(
            ("linux".to_string(), "New Name".to_string()),
            "https://e/feed".to_string(),
        );
        let counts = vec![
            // Stored under the feed's old name, but with its address.
            FeedUnread {
                topic: "linux".to_string(),
                source: "Old Name".to_string(),
                feed_url: Some("https://e/feed".to_string()),
                unread: 3,
            },
            // No address: matched by category and name.
            FeedUnread {
                topic: "news".to_string(),
                source: "Other".to_string(),
                feed_url: None,
                unread: 2,
            },
        ];
        let map = feed_unread_map(&counts, &addresses);
        assert_eq!(map[&("linux".to_string(), "New Name".to_string())], 3);
        assert_eq!(map[&("news".to_string(), "Other".to_string())], 2);
    }

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

    /// A plain unread test article with the given database ID.
    fn test_article(id: i64) -> Article {
        let mut article = Article::new(
            "title".to_string(),
            format!("https://example.com/{id}"),
            "source".to_string(),
            "linux".to_string(),
            chrono::Utc::now(),
        );
        article.id = id;
        article
    }

    /// The state icon follows the same priority as the title color
    /// (skipped, then saved, then read), and "new" only applies to an
    /// article that is still unread.
    #[test]
    fn article_state_icon_priority() {
        let new_ids = HashSet::from([1]);

        let mut article = test_article(1);
        assert_eq!(article_state_icon(&article, &new_ids), Icon::New);
        assert_eq!(article_state_icon(&test_article(2), &new_ids), Icon::Unread);

        article.read = true;
        assert_eq!(article_state_icon(&article, &new_ids), Icon::Read);
        article.saved = true;
        assert_eq!(article_state_icon(&article, &new_ids), Icon::Starred);
        article.skipped = true;
        assert_eq!(article_state_icon(&article, &new_ids), Icon::Skipped);
    }

    /// `fetch_notice` picks the right icon and message for each outcome,
    /// and stays quiet at startup when nothing happened.
    #[test]
    fn fetch_notice_messages() {
        let icons = IconSettings::new(IconMode::Ascii, HashMap::new());

        // Startup, nothing new, nothing failed: no message.
        let quiet = FetchReport {
            attempted: 2,
            ..FetchReport::default()
        };
        assert_eq!(fetch_notice(&icons, &quiet, false), None);
        // The same result after an `r` press does get an answer.
        assert_eq!(
            fetch_notice(&icons, &quiet, true).as_deref(),
            Some("@ Up to date, nothing new")
        );

        let fresh = FetchReport {
            attempted: 1,
            new_article_ids: HashSet::from([7, 8]),
            ..FetchReport::default()
        };
        assert_eq!(
            fetch_notice(&icons, &fresh, false).as_deref(),
            Some("+ 2 new article(s)")
        );

        let partial = FetchReport {
            attempted: 3,
            failed: 1,
            new_article_ids: HashSet::from([7]),
            ..FetchReport::default()
        };
        assert_eq!(
            fetch_notice(&icons, &partial, true).as_deref(),
            Some("! 1 of 3 source(s) failed to fetch · + 1 new")
        );

        let offline = FetchReport {
            attempted: 2,
            failed: 2,
            unreachable: 2,
            ..FetchReport::default()
        };
        assert!(
            fetch_notice(&icons, &offline, false)
                .unwrap()
                .starts_with("~ Offline")
        );

        let empty_topic = FetchReport::default();
        assert!(
            fetch_notice(&icons, &empty_topic, true)
                .unwrap()
                .starts_with("@ No sources")
        );
    }

    /// A short path fits in full, with the label and one space.
    #[test]
    fn feed_list_short_path_fits_whole() {
        assert_eq!(
            fit_feed_list("Feed List:", "~/.config/tuxwire/sources.toml", "…", 80).as_deref(),
            Some("Feed List: ~/.config/tuxwire/sources.toml")
        );
        // Exactly the needed width still fits (11 label+space + 30 path = 41).
        assert!(fit_feed_list("Feed List:", "~/.config/tuxwire/sources.toml", "…", 41).is_some());
    }

    /// The home abbreviation and the footer text work together: a real
    /// path under home shows as `~/...` in the footer.
    #[test]
    fn feed_list_home_abbreviation() {
        let shown = crate::paths::abbreviate_home(
            std::path::Path::new("/home/shane/.config/tuxwire/casual.toml"),
            Some(std::path::Path::new("/home/shane")),
        );
        assert_eq!(
            fit_feed_list("Feed List:", &shown, "…", 80).as_deref(),
            Some("Feed List: ~/.config/tuxwire/casual.toml")
        );
    }

    /// A long path is cut from the left, keeping the file name, and the
    /// result uses exactly the cells available (or fewer).
    #[test]
    fn feed_list_long_path_truncates_from_left() {
        let path = "~/Documents/feeds/very/deeply/nested/folder/structure/casual.toml";
        let text = fit_feed_list("Feed List:", path, "…", 30).unwrap();
        assert!(text.starts_with("Feed List: …"), "{text}");
        assert!(text.ends_with("/casual.toml"), "{text}");
        assert!(cell_width(&text) <= 30, "{text}");
        // The ASCII ellipsis works the same way.
        let ascii = fit_feed_list("Feed List:", path, "...", 30).unwrap();
        assert!(ascii.starts_with("Feed List: ...") && ascii.ends_with("/casual.toml"));
        assert!(ascii.is_ascii() && ascii.len() <= 30);
    }

    /// Wide characters count as two cells each, and the cut never splits
    /// one: a 2-cell character that would overflow by one cell is dropped.
    #[test]
    fn feed_list_wide_characters() {
        // "ニュース" is 4 characters but 8 cells.
        let path = "~/ニュース/feeds.toml";
        let full = fit_feed_list("Feed List:", path, "…", 80).unwrap();
        assert_eq!(cell_width(&full), 11 + 2 + 8 + 11);

        // 11 (label+space) + 1 (…) + 13 cells left: "ス/feeds.toml" is
        // 2 + 11 = 13, and the next "ー" (2 more) doesn't fit.
        let cut = fit_feed_list("Feed List:", path, "…", 25).unwrap();
        assert_eq!(cut, "Feed List: …ス/feeds.toml");
        // 12 cells left: "ス" (2) no longer fits after "/feeds.toml" (11),
        // so the kept part is exactly "/feeds.toml".
        let tighter = fit_feed_list("Feed List:", path, "…", 24).unwrap();
        assert_eq!(tighter, "Feed List: …/feeds.toml");
        assert!(cell_width(&tighter) <= 24);
    }

    /// With no room for the label plus file name, there's no text at all,
    /// including when the space is zero.
    #[test]
    fn feed_list_no_space_hides_path() {
        let path = "~/.config/tuxwire/sources.toml";
        // "Feed List: …/sources.toml" needs 11 + 1 + 13 = 25 cells.
        assert!(fit_feed_list("Feed List:", path, "…", 25).is_some());
        assert_eq!(fit_feed_list("Feed List:", path, "…", 24), None);
        assert_eq!(fit_feed_list("Feed List:", path, "…", 5), None);
        assert_eq!(fit_feed_list("Feed List:", path, "…", 0), None);
        // A bare file name with no `/` is never shortened.
        assert_eq!(fit_feed_list("Feed List:", "sources.toml", "…", 20), None);
    }

    /// `truncate_path_left` on its own (the picker uses it without a label):
    /// whole path, left-cut keeping `/name`, or `None`.
    #[test]
    fn truncate_path_left_keeps_file_name() {
        let path = "~/.config/tuxwire/casual.toml";
        assert_eq!(truncate_path_left(path, "…", 40).as_deref(), Some(path));
        assert_eq!(
            truncate_path_left(path, "…", 13).as_deref(),
            Some("…/casual.toml")
        );
        assert_eq!(truncate_path_left(path, "…", 12), None);
    }

    /// One feed list means `Scope::All` (identical to before lists
    /// existed); several mean only the active list's feed URLs.
    #[test]
    fn list_scope_single_vs_several_lists() {
        let sources = vec![ConfiguredSource::Rss(fetchers::rss::RssFetcher {
            name: "Arch".to_string(),
            url: "https://archlinux.org/feeds/news/".to_string(),
            topic: "linux".to_string(),
        })];

        let single = FeedLists::single(std::path::PathBuf::from("/x/sources.toml"));
        assert_eq!(list_scope(&single, &sources), Scope::All);

        let several = feedlists::config::parse(
            "[[list]]\nname = \"main\"\npath = \"a.toml\"\n[[list]]\nname = \"casual\"\npath = \"b.toml\"",
            Path::new("/cfg"),
            None,
            Path::new("/cfg/sources.toml"),
        )
        .unwrap();
        assert_eq!(
            list_scope(&several, &sources),
            Scope::Feeds(vec!["https://archlinux.org/feeds/news/".to_string()])
        );
    }

    /// With several lists, a list's configured topics appear in the sidebar
    /// before anything has been fetched, so `r` has a topic to refresh. With
    /// one list, the topics are exactly what's stored, as before.
    /// A feed row still shows its older articles after the feed is renamed in
    /// `sources.toml`, because the row is found by the feed's address.
    #[test]
    fn a_renamed_feed_keeps_its_articles() {
        let storage = Storage::open_in_memory().unwrap();
        // An article fetched while the feed was still called "Old Name".
        let mut article = Article::new(
            "Hello".to_string(),
            "https://e/1".to_string(),
            "Old Name".to_string(),
            "news".to_string(),
            chrono::Utc::now(),
        );
        article.feed_url = Some("https://a/feed".to_string());
        storage.insert_article(&article).unwrap();

        // `sources.toml` now calls the same address "New Name".
        let sources = vec![ConfiguredSource::Rss(fetchers::rss::RssFetcher {
            name: "New Name".to_string(),
            url: "https://a/feed".to_string(),
            topic: "news".to_string(),
        })];
        let addresses = feed_addresses_of(&sources);

        let shown = load_articles_for_selection(
            &storage,
            &addresses,
            &Scope::All,
            Some("news"),
            Some("New Name"),
        )
        .unwrap();
        assert_eq!(shown.len(), 1);

        // A name nobody configured has no address and matches nothing.
        let none = load_articles_for_selection(
            &storage,
            &addresses,
            &Scope::All,
            Some("news"),
            Some("Other"),
        )
        .unwrap();
        assert!(none.is_empty());
    }

    /// A topic that only exists in the config (nothing fetched yet) is still
    /// listed, with one list or several, so its feeds show in the sidebar.
    #[test]
    fn list_topics_include_configured_topics() {
        let storage = Storage::open_in_memory().unwrap();
        let sources = vec![ConfiguredSource::Rss(fetchers::rss::RssFetcher {
            name: "LWN".to_string(),
            url: "https://lwn.net/headlines/rss".to_string(),
            topic: "kernel".to_string(),
        })];

        // Single list: scope is everything.
        assert_eq!(
            list_topics(&storage, &Scope::All, &sources).unwrap(),
            ["kernel"]
        );

        // A feed list's scope gives the same answer.
        let several = feedlists::config::parse(
            "[[list]]\nname = \"main\"\npath = \"a.toml\"\n[[list]]\nname = \"casual\"\npath = \"b.toml\"",
            Path::new("/cfg"),
            None,
            Path::new("/cfg/sources.toml"),
        )
        .unwrap();
        let scope = list_scope(&several, &sources);
        assert_eq!(list_topics(&storage, &scope, &sources).unwrap(), ["kernel"]);
    }

    /// Writes a one-source `sources.toml` into a fresh temporary folder and
    /// returns its path. The source points at `url` and is filed under
    /// "Testing". (A unique folder per call keeps parallel tests apart.)
    fn temp_sources_file(url: &str) -> std::path::PathBuf {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("tuxwire-add-test-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sources.toml");
        std::fs::write(
            &path,
            format!(
                "[[source]]\nname = \"Test Blog\"\ntype = \"rss\"\nurl = \"{url}\"\ntopic = \"Testing\"\n"
            ),
        )
        .unwrap();
        path
    }

    /// Starts a tiny web server on `127.0.0.1` (this computer only) that
    /// answers one request with a feed holding two articles, and returns the
    /// feed's address. If a proxy is set in your environment, run the tests
    /// with `NO_PROXY=127.0.0.1`.
    fn serve_two_article_feed() -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = r#"<?xml version="1.0"?><rss version="2.0"><channel><title>Test Blog</title>
<item><title>First</title><link>https://example.com/1</link></item>
<item><title>Second</title><link>https://example.com/2</link></item></channel></rss>"#;
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0u8; 2048];
                let _ = stream.read(&mut buffer);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}/feed.xml")
    }

    /// Fetching a just-added source stores its articles at once, reports them
    /// as new, and files them under the source's topic. (`multi_thread` because
    /// `fetch_added_source` uses `block_in_place`, which needs a runtime with
    /// more than one worker thread, the same as the real app.)
    #[tokio::test(flavor = "multi_thread")]
    async fn adding_a_source_fetches_its_articles_at_once() {
        let url = serve_two_article_feed();
        let path = temp_sources_file(&url);
        let storage = Storage::open_in_memory().unwrap();

        let report = fetch_added_source(&storage, &path, &url).unwrap();

        assert_eq!(report.attempted, 1);
        assert_eq!(report.failed, 0);
        assert_eq!(report.new_article_ids.len(), 2);
        assert_eq!(
            storage
                .articles_by_topic(&Scope::All, "Testing")
                .unwrap()
                .len(),
            2
        );
    }

    /// A feed that cannot be downloaded is not an error for the caller: it is
    /// counted as a failure in the report (so the footer can say so), and the
    /// source stays saved. An address that is not in the file fetches nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_dead_feed_is_reported_not_raised() {
        // Port 9 on this computer refuses connections: a dead feed with no
        // real network needed.
        let url = "http://127.0.0.1:9/feed.xml";
        let path = temp_sources_file(url);
        let storage = Storage::open_in_memory().unwrap();

        let report = fetch_added_source(&storage, &path, url).unwrap();
        assert_eq!(report.attempted, 1);
        assert_eq!(report.failed, 1);
        assert!(report.new_article_ids.is_empty());

        // An address that is not in the file: nothing is attempted.
        let none = fetch_added_source(&storage, &path, "http://127.0.0.1:9/other").unwrap();
        assert_eq!(none.attempted, 0);
    }

    /// Clearing a feed removes only that feed's marker; clearing a topic row
    /// removes every marker under that topic; other topics are untouched; no
    /// topic means nothing happens.
    #[test]
    fn fresh_markers_clear_by_feed_or_topic() {
        let make = || {
            HashSet::from([
                ("linux".to_string(), "Arch".to_string()),
                ("linux".to_string(), "LWN".to_string()),
                ("sim-racing".to_string(), "Overtake".to_string()),
            ])
        };

        let mut fresh = make();
        clear_fresh_feeds(&mut fresh, Some(&"linux".to_string()), Some("Arch"));
        assert_eq!(fresh.len(), 2);
        assert!(fresh.contains(&("linux".to_string(), "LWN".to_string())));

        let mut fresh = make();
        clear_fresh_feeds(&mut fresh, Some(&"linux".to_string()), None);
        assert_eq!(fresh.len(), 1);
        assert!(fresh.contains(&("sim-racing".to_string(), "Overtake".to_string())));

        let mut fresh = make();
        clear_fresh_feeds(&mut fresh, None, None);
        assert_eq!(fresh.len(), 3);
    }

    /// Articles added by a refresh mark the feed they came from, by topic and
    /// source name, and an empty refresh marks nothing.
    /// `N` fetches every topic's feeds, not just the selected one: two file
    /// feeds in different topics both arrive, and a second run finds nothing new.
    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_all_fetches_every_topic() {
        // A unique folder with two feed files and a sources.toml naming them.
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "tuxwire-refresh-all-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut toml = String::new();
        for (name, topic) in [("One", "alpha"), ("Two", "beta")] {
            let file = dir.join(format!("{name}.xml"));
            std::fs::write(
                &file,
                format!(
                    "<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>{name}</title>\
                     <link>http://x</link><description>d</description>\
                     <item><title>{name} story</title><link>http://x/{name}</link>\
                     <guid>{name}</guid></item></channel></rss>"
                ),
            )
            .unwrap();
            toml.push_str(&format!(
                "[[source]]\nname = \"{name}\"\ntype = \"file\"\nurl = \"{}\"\ntopic = \"{topic}\"\n\n",
                file.display()
            ));
        }
        let list = dir.join("sources.toml");
        std::fs::write(&list, toml).unwrap();

        let storage = Storage::open_in_memory().unwrap();
        let sources = fetchers::configured_sources_at(&list).unwrap();
        let addresses = feed_addresses_of(&sources);

        // Refreshing while looking at topic "alpha" still fetches "beta" too.
        let (shown, report) = refresh_all(
            &storage,
            &addresses,
            &Scope::All,
            &list,
            Some("alpha"),
            None,
        )
        .unwrap();
        assert_eq!(report.attempted, 2);
        assert_eq!(report.new_article_ids.len(), 2);
        // The list on screen is the selected topic's.
        assert_eq!(shown.len(), 1);
        assert_eq!(
            storage
                .articles_by_topic(&Scope::All, "beta")
                .unwrap()
                .len(),
            1
        );

        // Nothing changed in the files, so a second refresh finds nothing new.
        let (_, again) = refresh_all(
            &storage,
            &addresses,
            &Scope::All,
            &list,
            Some("alpha"),
            None,
        )
        .unwrap();
        assert_eq!(again.attempted, 2);
        assert!(again.new_article_ids.is_empty());
    }

    /// The "refresh all" result names how many feeds were refreshed, whether
    /// or not anything was new; failures keep the shared wording.
    #[test]
    fn refresh_all_says_how_many_it_refreshed() {
        let icons = IconSettings::new(IconMode::Ascii, HashMap::new());
        let quiet = FetchReport {
            attempted: 3,
            ..FetchReport::default()
        };
        assert_eq!(
            refresh_all_notice(&icons, &quiet).as_deref(),
            Some("@ Refreshed 3 feed(s), nothing new")
        );
        let fresh = FetchReport {
            attempted: 3,
            new_article_ids: HashSet::from([1, 2]),
            ..FetchReport::default()
        };
        assert_eq!(
            refresh_all_notice(&icons, &fresh).as_deref(),
            Some("+ Refreshed 3 feed(s) \u{b7} 2 new article(s)")
        );
        let none = FetchReport::default();
        assert_eq!(
            refresh_all_notice(&icons, &none).as_deref(),
            Some("@ No feeds to refresh")
        );
        let failed = FetchReport {
            attempted: 3,
            failed: 1,
            ..FetchReport::default()
        };
        assert_eq!(
            refresh_all_notice(&icons, &failed),
            fetch_notice(&icons, &failed, true)
        );
    }

    #[test]
    fn new_articles_mark_their_feed() {
        let storage = Storage::open_in_memory().unwrap();
        let mut article = Article::new(
            "Hello".to_string(),
            "https://example.com/hello".to_string(),
            "Overtake".to_string(),
            "sim-racing".to_string(),
            chrono::Utc::now(),
        );
        article.feed_url = Some("import:overtake".to_string());
        let id = storage.insert_article(&article).unwrap();

        let mut fresh = HashSet::new();
        note_fresh_feeds(&storage, &HashSet::from([id]), &mut fresh);
        assert_eq!(
            fresh,
            HashSet::from([("sim-racing".to_string(), "Overtake".to_string())])
        );

        let mut none = HashSet::new();
        note_fresh_feeds(&storage, &HashSet::new(), &mut none);
        assert!(none.is_empty());
    }
}
