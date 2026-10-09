//! Icons: every symbol tuxwire draws, defined once, in three styles.
//!
//! UI code asks for an icon by *name* (`Icon::Unread`, `Icon::Refresh`,
//! ...) through an [`IconSettings`], and gets back the text to draw. It
//! never writes a glyph itself, so switching modes changes every icon on
//! screen at once, and adding a new style later is a change to this one
//! file. Modeled on zerohour's `core/src/icons.rs`.
//!
//! The three modes ([`IconMode`]):
//! - **Nerd** (the default): Nerd Font icons, which are Font Awesome and
//!   Devicons glyphs patched into ordinary programming fonts. They look
//!   the best, but only render if the terminal's font is a Nerd Font.
//!   Otherwise they show up as empty boxes or `?`.
//! - **Unicode**: plain Unicode symbols like `●` and `★` that almost every
//!   font has.
//! - **ASCII**: single printable ASCII characters (`o`, `*`, `>`). These
//!   work everywhere, including a bare Linux console.
//!
//! ## Why "Nerd Font *Mono*" glyphs only
//!
//! A terminal is a grid. Every character gets exactly one cell (or two,
//! for wide characters like CJK and most emoji), and ratatui lays out
//! borders and columns by counting those cells. A regular ("proportional
//! icon") Nerd Font draws many icons about 1.5 to 2 cells wide, but the
//! terminal still advances the cursor by only one cell. The icon then
//! spills into the next character, and box borders and list columns stop
//! lining up. The *Mono* variants (for example "JetBrainsMono Nerd Font
//! Mono") shrink every icon to exactly one cell, so everything stays
//! aligned.
//!
//! So every glyph in the table below must:
//! 1. come from a range Nerd Font Mono renders as one cell. All of them
//!    are Private Use Area codepoints (`U+E000`–`U+F8FF`) from Font
//!    Awesome and Devicons, which ratatui (via `unicode-width`) also
//!    counts as one cell, so ratatui and the font agree;
//! 2. have Unicode and ASCII fallbacks that are also exactly one cell
//!    wide, so an icon column is the same width in every mode. The tests
//!    at the bottom of this file check both.
//!
//! A terminal app can't ship a font. The user picks one in their
//! terminal's settings, which is why the first-run check (see
//! `ui::Mode::IconCheck`) asks whether the test glyph actually shows up
//! and falls back to Unicode if it doesn't.
//!
//! This module (and `config.rs` next to it) deliberately imports nothing
//! from `ratatui`, `crossterm`, or `ui` outside of tests, so the icon table
//! can move into a future Android app unchanged.

// Asks clippy to warn about any item in this module (or its children) that
// is missing a `///` doc comment, including private ones, because the
// project's documentation standard requires a doc comment on every item.
#![warn(clippy::missing_docs_in_private_items)]

pub mod config;

use std::collections::HashMap;

/// Which set of glyphs to draw. Stored in `icons.toml` as `"nerd"`,
/// `"unicode"`, or `"ascii"`.
///
/// `Clone, Copy` because this is a small tag with no data inside it, so
/// copying it is as cheap as passing a reference. `PartialEq, Eq` let code
/// compare modes with `==`, and `Debug` lets tests print one in a failure
/// message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IconMode {
    /// Nerd Font glyphs. `#[default]` makes this what
    /// `IconMode::default()` returns, the mode used before the user
    /// has chosen anything.
    #[default]
    Nerd,
    /// Plain Unicode symbols that almost every font has.
    Unicode,
    /// Single printable ASCII characters; works on any terminal.
    Ascii,
}

impl IconMode {
    /// Every mode, so tests can loop over all of them without listing them
    /// by hand. `#[cfg(test)]` leaves it out of the real binary, where
    /// nothing needs it (the `i` key uses [`IconMode::next`] instead).
    #[cfg(test)]
    pub const ALL: [IconMode; 3] = [IconMode::Nerd, IconMode::Unicode, IconMode::Ascii];

    /// The text this mode is saved as in `icons.toml`. Returns
    /// `&'static str`, a string literal baked into the binary, so no new
    /// `String` is allocated each time.
    pub fn as_str(self) -> &'static str {
        match self {
            IconMode::Nerd => "nerd",
            IconMode::Unicode => "unicode",
            IconMode::Ascii => "ascii",
        }
    }

    /// Reads a mode back from its `icons.toml` text. Returns `None` for
    /// anything it doesn't recognize, so the caller decides what an unknown
    /// value means (`config::parse` turns it into an error naming the
    /// bad value).
    pub fn parse(s: &str) -> Option<IconMode> {
        match s {
            "nerd" => Some(IconMode::Nerd),
            "unicode" => Some(IconMode::Unicode),
            "ascii" => Some(IconMode::Ascii),
            _ => None,
        }
    }

    /// The next mode in the `i` key's cycle: Nerd → Unicode → ASCII → Nerd.
    pub fn next(self) -> IconMode {
        match self {
            IconMode::Nerd => IconMode::Unicode,
            IconMode::Unicode => IconMode::Ascii,
            IconMode::Ascii => IconMode::Nerd,
        }
    }
}

/// The name of every icon tuxwire draws. The glyphs themselves live in
/// one place, [`Icon::glyphs`].
///
/// The variants fall into three groups: article states, status messages
/// (refresh/error/offline/new), and feed categories (`Linux` through
/// `Category`). Category variants are what a sidebar topic gets, either
/// from the built-in name list ([`Icon::for_category`]) or from the user's
/// `[categories]` table in `icons.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    /// An article that hasn't been opened yet.
    Unread,
    /// An article that has been opened (or saved, since saving marks it read).
    Read,
    /// A saved ("starred") article. The `s` key.
    Starred,
    /// An article marked skipped with `x`.
    Skipped,
    /// An unread article that first arrived during this session (the
    /// startup fetch or an `r` refresh). Also used in the "N new" message.
    New,
    /// The `r` refresh result message.
    Refresh,
    /// Error popups and "some sources failed" messages.
    Error,
    /// Shown when every source failed because the network is unreachable.
    Offline,
    /// The sidebar marker on a topic whose sources are showing (`→`/`l`).
    Expanded,
    /// The sidebar marker on a topic whose sources are hidden (`←`/`h`).
    /// Also the pointer next to the highlighted row in the theme picker.
    Collapsed,
    /// The theme picker's title (`t`).
    Theme,
    /// A solid block, drawn in a theme's own colors as a preview swatch in
    /// the theme picker.
    Swatch,
    /// The feed list file shown in the footer ("Feed List: <path>"). Only
    /// its Nerd glyph is used there: in Unicode and ASCII modes the footer
    /// spells out "Feed List:" instead (see [`IconSettings::feed_list_label`]).
    FeedFile,
    /// Category: Linux. Also the first-run test glyph (Tux the penguin).
    Linux,
    /// Category: the Rust language.
    Rust,
    /// Category: security / infosec / privacy.
    Security,
    /// Category: general and world news.
    News,
    /// Category: sports.
    Sports,
    /// Category: technology and hardware.
    Tech,
    /// Category: video games.
    Gaming,
    /// Category: blogs.
    Blog,
    /// Category: science.
    Science,
    /// Category: programming and software development.
    Code,
    /// Category: music.
    Music,
    /// Category: generic feeds / RSS.
    Feed,
    /// The fallback category icon, a folder, for any topic with no
    /// built-in or user-configured icon.
    Category,
}

impl Icon {
    /// Every icon, in declaration order. Used by [`Icon::from_name`] to look
    /// up a name from `icons.toml`, and by the tests to check every row of
    /// the table. The `[Icon; 26]` length is checked by the compiler, so
    /// adding a variant without adding it here fails to build.
    pub const ALL: [Icon; 26] = [
        Icon::Unread,
        Icon::Read,
        Icon::Starred,
        Icon::Skipped,
        Icon::New,
        Icon::Refresh,
        Icon::Error,
        Icon::Offline,
        Icon::Expanded,
        Icon::Collapsed,
        Icon::Theme,
        Icon::Swatch,
        Icon::FeedFile,
        Icon::Linux,
        Icon::Rust,
        Icon::Security,
        Icon::News,
        Icon::Sports,
        Icon::Tech,
        Icon::Gaming,
        Icon::Blog,
        Icon::Science,
        Icon::Code,
        Icon::Music,
        Icon::Feed,
        Icon::Category,
    ];

    /// THE icon table: `(Nerd Font glyph, Unicode fallback, ASCII fallback)`
    /// for every icon.
    ///
    /// It's a `match` rather than an array of rows so the compiler refuses
    /// to build if a variant is missing: a `match` on an enum must cover
    /// every variant, so you can't add an `Icon` and forget its glyphs.
    ///
    /// The Nerd glyphs are written as `\u{f111}`-style escapes because most
    /// editors can't display Private Use Area characters. The comment on
    /// each row gives the glyph's Nerd Font name, which you can search for
    /// at <https://www.nerdfonts.com/cheat-sheet>. Every glyph here is one
    /// terminal cell wide in all three modes; see this module's doc comment
    /// for why that matters.
    fn glyphs(self) -> (&'static str, &'static str, &'static str) {
        match self {
            // Article states.
            Icon::Unread => ("\u{f111}", "●", "o"), // nf-fa-circle
            Icon::Read => ("\u{f10c}", "○", "."),   // nf-fa-circle_o
            Icon::Starred => ("\u{f005}", "★", "*"), // nf-fa-star
            Icon::Skipped => ("\u{f05e}", "⊘", "x"), // nf-fa-ban
            Icon::New => ("\u{f0a3}", "✦", "+"),    // nf-fa-certificate

            // Status messages.
            Icon::Refresh => ("\u{f021}", "↻", "@"), // nf-fa-refresh
            Icon::Error => ("\u{f071}", "✗", "!"),   // nf-fa-exclamation_triangle
            Icon::Offline => ("\u{f127}", "◌", "~"), // nf-fa-chain_broken

            // Sidebar tree markers.
            Icon::Expanded => ("\u{f0d7}", "▾", "v"), // nf-fa-caret_down
            Icon::Collapsed => ("\u{f0da}", "▸", ">"), // nf-fa-caret_right

            // Theme picker.
            Icon::Theme => ("\u{f1fc}", "◐", "&"), // nf-fa-paint_brush
            Icon::Swatch => ("\u{f0c8}", "█", "#"), // nf-fa-square
            Icon::FeedFile => ("\u{f0f6}", "▭", "f"), // nf-fa-file_text_o

            // Feed categories.
            Icon::Linux => ("\u{f17c}", "λ", "L"), // nf-fa-linux
            Icon::Rust => ("\u{e7a8}", "⚙", "R"),  // nf-dev-rust
            Icon::Security => ("\u{f023}", "◈", "#"), // nf-fa-lock
            Icon::News => ("\u{f1ea}", "▤", "N"),  // nf-fa-newspaper_o
            Icon::Sports => ("\u{f1e3}", "◉", "%"), // nf-fa-futbol_o
            Icon::Tech => ("\u{f108}", "▣", "T"),  // nf-fa-desktop
            Icon::Gaming => ("\u{f11b}", "►", "G"), // nf-fa-gamepad
            Icon::Blog => ("\u{f040}", "✎", "B"),  // nf-fa-pencil
            Icon::Science => ("\u{f0c3}", "∴", "S"), // nf-fa-flask
            Icon::Code => ("\u{f121}", "»", "<"),  // nf-fa-code
            Icon::Music => ("\u{f001}", "♪", "m"), // nf-fa-music
            Icon::Feed => ("\u{f09e}", "≋", "F"),  // nf-fa-rss
            Icon::Category => ("\u{f07b}", "▪", "-"), // nf-fa-folder
        }
    }

    /// The glyph to draw for this icon in `mode`.
    pub fn get(self, mode: IconMode) -> &'static str {
        // Destructures the 3-tuple `glyphs` returns into three named
        // variables, then picks the one `mode` asks for.
        let (nerd, unicode, ascii) = self.glyphs();
        match mode {
            IconMode::Nerd => nerd,
            IconMode::Unicode => unicode,
            IconMode::Ascii => ascii,
        }
    }

    /// The name used for this icon in `icons.toml`'s `[categories]` table
    /// (for example `linux = "security"`). Lowercase and stable: renaming
    /// one would break users' existing config files.
    pub fn name(self) -> &'static str {
        match self {
            Icon::Unread => "unread",
            Icon::Read => "read",
            Icon::Starred => "starred",
            Icon::Skipped => "skipped",
            Icon::New => "new",
            Icon::Refresh => "refresh",
            Icon::Error => "error",
            Icon::Offline => "offline",
            Icon::Expanded => "expanded",
            Icon::Collapsed => "collapsed",
            Icon::Theme => "theme",
            Icon::Swatch => "swatch",
            Icon::FeedFile => "file",
            Icon::Linux => "linux",
            Icon::Rust => "rust",
            Icon::Security => "security",
            Icon::News => "news",
            Icon::Sports => "sports",
            Icon::Tech => "tech",
            Icon::Gaming => "gaming",
            Icon::Blog => "blog",
            Icon::Science => "science",
            Icon::Code => "code",
            Icon::Music => "music",
            Icon::Feed => "feed",
            Icon::Category => "category",
        }
    }

    /// The icon whose [`Icon::name`] is `name`, or `None` if no icon has
    /// that name. Any icon may be used for a category, not just the
    /// category ones: `"breaking" = "error"` is allowed.
    ///
    /// `.iter().copied().find(...)` walks `ALL` one icon at a time, copies
    /// each one out of the array (`Icon` is `Copy`, so that's free), and
    /// stops at the first one whose name matches. It's a linear search, but
    /// over 26 items and only while loading config, so a `HashMap` would be
    /// overkill.
    pub fn from_name(name: &str) -> Option<Icon> {
        Icon::ALL.iter().copied().find(|icon| icon.name() == name)
    }

    /// The built-in default icon for a category name, or `None` if the name
    /// isn't one tuxwire recognizes. `name` must already be lowercase
    /// (see [`IconSettings::category_glyph`], which lowercases before
    /// calling this).
    ///
    /// Several spellings map to each icon (`"infosec"` and `"security"`,
    /// `"games"` and `"gaming"`), because topics are free text in
    /// `sources.toml` and people name them differently.
    pub fn for_category(name: &str) -> Option<Icon> {
        // `a | b | c` in a `match` arm means "any of these patterns".
        let icon = match name {
            "linux" | "arch" | "archlinux" | "kernel" | "distro" | "distros" => Icon::Linux,
            "rust" | "rustlang" => Icon::Rust,
            "security" | "infosec" | "cybersecurity" | "privacy" => Icon::Security,
            "news" | "world" | "worldnews" | "headlines" => Icon::News,
            "sports" | "sport" | "football" | "soccer" | "nfl" | "nba" => Icon::Sports,
            "tech" | "technology" | "hardware" | "gadgets" => Icon::Tech,
            "gaming" | "games" | "game" => Icon::Gaming,
            "blog" | "blogs" => Icon::Blog,
            "science" => Icon::Science,
            "programming" | "dev" | "development" | "code" | "coding" => Icon::Code,
            "music" => Icon::Music,
            "rss" | "feeds" => Icon::Feed,
            // `_` matches anything not listed above. Returning `None` here
            // (rather than `Icon::Category`) lets the caller tell apart
            // "a known category" from "fell back to the default".
            _ => return None,
        };
        Some(icon)
    }
}

/// What a user-configured category maps to in `icons.toml`'s
/// `[categories]` table. Built by [`CategoryIcon::parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CategoryIcon {
    /// A name from the icon table (`homelab = "security"`). Gets that
    /// icon's proper Nerd/Unicode/ASCII glyph for whatever mode is active.
    Named(Icon),
    /// A literal single character the user pasted in (`homelab = "⌂"`, or
    /// a Nerd Font glyph from the cheat sheet). There's only one version of
    /// it, so [`IconSettings::category_glyph`] skips it in a mode where it
    /// can't render and uses the built-in default instead.
    Literal(String),
}

impl CategoryIcon {
    /// Reads one `[categories]` value: exactly one character means a
    /// literal glyph, and anything longer must be an icon name from the
    /// table. Returns `None` for a longer value that isn't a known icon name,
    /// so `config::parse` can report the bad entry.
    ///
    /// Icon names are all several letters long, so "one character" and
    /// "a name" never overlap.
    pub fn parse(value: &str) -> Option<CategoryIcon> {
        // `.chars().count()` counts Unicode characters, not bytes. A Nerd
        // Font glyph is 3 bytes in UTF-8 but one character, which is what
        // matters here.
        if value.chars().count() == 1 {
            Some(CategoryIcon::Literal(value.to_string()))
        } else {
            Icon::from_name(value).map(CategoryIcon::Named)
        }
    }
}

/// The user's icon settings, from `icons.toml`: which [`IconMode`] is
/// active, and any custom per-category icons. The UI holds one of these
/// and asks it for every glyph it draws.
///
/// `Default` gives Nerd mode with no custom categories, the same as an
/// `icons.toml` that sets nothing.
#[derive(Debug, Clone, Default)]
pub struct IconSettings {
    /// The active glyph set. `pub` so the `i` key can change it in place,
    /// which is how a mode change takes effect on the very next frame.
    pub mode: IconMode,
    /// User overrides from `[categories]`, keyed by *lowercase* category
    /// name. Private, and only filled through [`IconSettings::new`], so the
    /// "keys are lowercase" rule can't be broken from outside this module.
    category_overrides: HashMap<String, CategoryIcon>,
}

impl IconSettings {
    /// Builds settings from a mode and the user's category overrides.
    /// Lowercases every key so `Linux = "rust"` and `linux = "rust"` mean
    /// the same thing, matching how [`IconSettings::category_glyph`]
    /// lowercases the topic it looks up.
    ///
    /// Takes `overrides` by value (ownership moves in) because the settings
    /// keep the map. `.into_iter()` then consumes it, handing out each
    /// `(key, value)` pair by value so the values can move into the new map
    /// without being cloned.
    pub fn new(mode: IconMode, overrides: HashMap<String, CategoryIcon>) -> IconSettings {
        let category_overrides = overrides
            .into_iter()
            .map(|(key, value)| (key.trim().to_lowercase(), value))
            .collect();
        IconSettings {
            mode,
            category_overrides,
        }
    }

    /// The glyph for `icon` in the active mode. A shortcut for
    /// `icon.get(settings.mode)`, so UI code only has to pass the settings
    /// around, not the settings *and* the mode.
    pub fn glyph(&self, icon: Icon) -> &'static str {
        icon.get(self.mode)
    }

    /// The label in front of the footer's feed list path: the
    /// [`Icon::FeedFile`] glyph in Nerd mode, and the words "Feed List:" in
    /// Unicode and ASCII modes, where a one-character fallback symbol
    /// wouldn't say what the path is.
    ///
    /// This lives outside the [`Icon::glyphs`] table on purpose. That table
    /// promises every entry is exactly one terminal cell wide (a test
    /// checks it), and "Feed List:" is ten.
    pub fn feed_list_label(&self) -> &'static str {
        match self.mode {
            IconMode::Nerd => Icon::FeedFile.get(IconMode::Nerd),
            IconMode::Unicode | IconMode::Ascii => "Feed List:",
        }
    }

    /// The "something was cut off here" marker for truncated text: `…` (one
    /// cell) in Nerd and Unicode modes, and `...` in ASCII mode, which
    /// promises plain ASCII output.
    pub fn ellipsis(&self) -> &'static str {
        match self.mode {
            IconMode::Nerd | IconMode::Unicode => "…",
            IconMode::Ascii => "...",
        }
    }

    /// The glyph for a sidebar topic (a feed category) in the active mode.
    ///
    /// Lookup order, first match wins:
    /// 1. the user's `[categories]` entry for the topic name;
    /// 2. the built-in icon for that name ([`Icon::for_category`]);
    /// 3. [`Icon::Category`], the generic folder.
    ///
    /// A user [`CategoryIcon::Literal`] that can't render in the active
    /// mode (a Nerd glyph in Unicode mode, or anything non-ASCII in ASCII
    /// mode) is skipped, and lookup continues with step 2. Unicode mode
    /// therefore never draws a Nerd glyph that would show up as a box.
    ///
    /// The return type `&str` (not `&'static str`) is tied to `&self`'s
    /// lifetime, because a literal glyph is a `String` stored inside these
    /// settings. The caller can use it for as long as it holds the
    /// settings borrow, which is all one frame of drawing needs.
    pub fn category_glyph(&self, category: &str) -> &str {
        // Normalize once: case-insensitive, ignoring stray spaces.
        let name = category.trim().to_lowercase();

        // Step 1: the user's own override, if there is one this mode can
        // draw.
        match self.category_overrides.get(&name) {
            Some(CategoryIcon::Named(icon)) => return icon.get(self.mode),
            Some(CategoryIcon::Literal(glyph)) if literal_renders_in(glyph, self.mode) => {
                return glyph;
            }
            // No override, or a literal this mode can't show: fall through
            // to the built-ins.
            _ => {}
        }

        // Steps 2 and 3: the built-in name list, then the generic folder.
        // `.unwrap_or(...)` supplies the folder when `for_category` returns
        // `None`.
        Icon::for_category(&name)
            .unwrap_or(Icon::Category)
            .get(self.mode)
    }
}

/// Whether a user's literal glyph can be shown in `mode`:
/// - Nerd: always, since the user chose it for their own font.
/// - Unicode: only if it isn't a Private Use Area character. Nerd Font
///   glyphs all live there, and a plain font draws them as boxes.
/// - ASCII: only if it's plain ASCII.
fn literal_renders_in(glyph: &str, mode: IconMode) -> bool {
    match mode {
        IconMode::Nerd => true,
        // `!glyph.chars().any(is_private_use)` means "no character in the
        // string is a Private Use Area one". `any` stops at the first hit.
        IconMode::Unicode => !glyph.chars().any(is_private_use),
        IconMode::Ascii => glyph.is_ascii(),
    }
}

/// Whether `c` is in one of Unicode's three Private Use Areas, the ranges
/// set aside for font-specific symbols like Nerd Font icons. `c as u32`
/// turns the `char` into its numeric codepoint, and `matches!` checks it
/// against the three inclusive ranges (`..=`).
fn is_private_use(c: char) -> bool {
    matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

// `#[cfg(test)]`: compiled only for `cargo test`, so none of this ends up
// in the real binary.
#[cfg(test)]
mod tests {
    use super::*;
    // Used only here, to measure glyphs the same way ratatui does when it
    // lays out a line. The non-test code above stays ratatui-free.
    use ratatui::text::Span;

    /// Every glyph, in every mode, is exactly one character and one
    /// terminal cell wide. This is the "single-cell" guarantee from the
    /// module doc comment that keeps borders and columns aligned. If
    /// someone adds a wide emoji or a multi-character ASCII fallback, this
    /// fails and names the icon.
    #[test]
    fn every_glyph_is_one_char_and_one_cell_in_every_mode() {
        for icon in Icon::ALL {
            for mode in IconMode::ALL {
                let glyph = icon.get(mode);
                assert_eq!(glyph.chars().count(), 1, "{icon:?} in {mode:?}: {glyph:?}");
                // `Span::width` is ratatui's own cell-width measurement.
                assert_eq!(
                    Span::raw(glyph).width(),
                    1,
                    "{icon:?} in {mode:?}: {glyph:?}"
                );
            }
        }
    }

    /// Every Nerd glyph is a Private Use Area character (where Nerd Font
    /// Mono puts its single-cell icons), and no fallback is. If a fallback
    /// were PUA, it would be a box on the very terminals the fallback
    /// exists for.
    #[test]
    fn nerd_glyphs_are_private_use_and_fallbacks_are_not() {
        for icon in Icon::ALL {
            assert!(
                icon.get(IconMode::Nerd).chars().all(is_private_use),
                "{icon:?}"
            );
            assert!(
                !icon.get(IconMode::Unicode).chars().any(is_private_use),
                "{icon:?}"
            );
            assert!(icon.get(IconMode::Ascii).is_ascii(), "{icon:?}");
        }
    }

    /// The article-state icons must look different from each other in
    /// every mode, or the list can't tell unread from read at a glance.
    #[test]
    fn article_state_icons_are_distinct_in_every_mode() {
        let states = [
            Icon::Unread,
            Icon::Read,
            Icon::Starred,
            Icon::Skipped,
            Icon::New,
        ];
        for mode in IconMode::ALL {
            // Collecting into a `HashSet` drops duplicates, so if the set
            // is smaller than the list, two states shared a glyph.
            let glyphs: std::collections::HashSet<&str> =
                states.iter().map(|icon| icon.get(mode)).collect();
            assert_eq!(
                glyphs.len(),
                states.len(),
                "duplicate state glyph in {mode:?}"
            );
        }
    }

    /// `from_name` finds every icon by its own `name`, and names are unique
    /// (otherwise `from_name` would silently return the first of two icons
    /// sharing a name).
    #[test]
    fn icon_names_round_trip_and_are_unique() {
        for icon in Icon::ALL {
            assert_eq!(Icon::from_name(icon.name()), Some(icon));
        }
        let names: std::collections::HashSet<&str> =
            Icon::ALL.iter().map(|icon| icon.name()).collect();
        assert_eq!(names.len(), Icon::ALL.len());
        assert_eq!(Icon::from_name("no-such-icon"), None);
    }

    /// Modes round-trip through their `icons.toml` text, `parse` rejects
    /// junk, the `i` cycle visits every mode and wraps back to the start,
    /// and the default is Nerd.
    #[test]
    fn mode_round_trip_cycle_and_default() {
        for mode in IconMode::ALL {
            assert_eq!(IconMode::parse(mode.as_str()), Some(mode));
        }
        assert_eq!(IconMode::parse("emoji"), None);
        assert_eq!(IconMode::Nerd.next(), IconMode::Unicode);
        assert_eq!(IconMode::Unicode.next(), IconMode::Ascii);
        assert_eq!(IconMode::Ascii.next(), IconMode::Nerd);
        assert_eq!(IconMode::default(), IconMode::Nerd);
    }

    /// The footer label is the file glyph in Nerd mode and the words
    /// "Feed List:" otherwise, and the ellipsis is plain ASCII in ASCII mode.
    #[test]
    fn feed_list_label_and_ellipsis_per_mode() {
        let mut settings = IconSettings::default();
        assert_eq!(settings.feed_list_label(), "\u{f0f6}");
        assert_eq!(settings.ellipsis(), "…");
        settings.mode = IconMode::Unicode;
        assert_eq!(settings.feed_list_label(), "Feed List:");
        settings.mode = IconMode::Ascii;
        assert_eq!(settings.feed_list_label(), "Feed List:");
        assert_eq!(settings.ellipsis(), "...");
    }

    /// `IconSettings::glyph` follows the settings' current mode, including
    /// after the mode is changed in place (what the `i` key does).
    #[test]
    fn settings_glyph_follows_mode_changes() {
        let mut settings = IconSettings::default();
        assert_eq!(settings.glyph(Icon::Starred), "\u{f005}");
        settings.mode = IconMode::Unicode;
        assert_eq!(settings.glyph(Icon::Starred), "★");
        settings.mode = IconMode::Ascii;
        assert_eq!(settings.glyph(Icon::Starred), "*");
    }

    /// Built-in categories match case-insensitively and through aliases.
    #[test]
    fn builtin_categories_and_aliases() {
        let settings = IconSettings::default();
        assert_eq!(
            settings.category_glyph("linux"),
            Icon::Linux.get(IconMode::Nerd)
        );
        assert_eq!(
            settings.category_glyph("Linux"),
            Icon::Linux.get(IconMode::Nerd)
        );
        assert_eq!(
            settings.category_glyph("infosec"),
            Icon::Security.get(IconMode::Nerd)
        );
        assert_eq!(
            settings.category_glyph("Sports"),
            Icon::Sports.get(IconMode::Nerd)
        );
        assert_eq!(
            settings.category_glyph("rust"),
            Icon::Rust.get(IconMode::Nerd)
        );
    }

    /// An unknown category gets the generic folder icon in every mode.
    #[test]
    fn unknown_category_falls_back_to_default_icon() {
        for mode in IconMode::ALL {
            let settings = IconSettings::new(mode, HashMap::new());
            assert_eq!(
                settings.category_glyph("knitting"),
                Icon::Category.get(mode)
            );
            assert_eq!(settings.category_glyph(""), Icon::Category.get(mode));
        }
    }

    /// A user override works for a topic with no built-in icon, matched
    /// case-insensitively and ignoring stray spaces.
    #[test]
    fn user_override_for_custom_topic() {
        let overrides = HashMap::from([("homelab".to_string(), CategoryIcon::Named(Icon::Tech))]);
        let settings = IconSettings::new(IconMode::Unicode, overrides);
        assert_eq!(settings.category_glyph(" Homelab "), "▣");
    }

    /// A user override beats the built-in mapping, and override keys are
    /// case-insensitive.
    #[test]
    fn user_override_beats_builtin() {
        let overrides = HashMap::from([("Linux".to_string(), CategoryIcon::Named(Icon::Rust))]);
        let settings = IconSettings::new(IconMode::Ascii, overrides);
        assert_eq!(settings.category_glyph("linux"), "R");
    }

    /// A literal Nerd glyph override shows in Nerd mode, but in Unicode and
    /// ASCII modes it falls back to the category's built-in (or default)
    /// icon instead of drawing a box.
    #[test]
    fn literal_nerd_override_falls_back_outside_nerd_mode() {
        let tux = "\u{f17c}".to_string();
        let overrides = HashMap::from([
            ("blog".to_string(), CategoryIcon::Literal(tux.clone())),
            ("homelab".to_string(), CategoryIcon::Literal(tux.clone())),
        ]);

        let nerd = IconSettings::new(IconMode::Nerd, overrides.clone());
        assert_eq!(nerd.category_glyph("blog"), tux);

        let unicode = IconSettings::new(IconMode::Unicode, overrides.clone());
        assert_eq!(
            unicode.category_glyph("blog"),
            Icon::Blog.get(IconMode::Unicode)
        );
        assert_eq!(
            unicode.category_glyph("homelab"),
            Icon::Category.get(IconMode::Unicode)
        );

        let ascii = IconSettings::new(IconMode::Ascii, overrides);
        assert_eq!(ascii.category_glyph("blog"), "B");
    }

    /// A literal Unicode override shows in Nerd and Unicode modes but not
    /// ASCII, and a literal ASCII override shows everywhere.
    #[test]
    fn literal_unicode_and_ascii_overrides() {
        let overrides = HashMap::from([
            (
                "homelab".to_string(),
                CategoryIcon::Literal("⌂".to_string()),
            ),
            ("misc".to_string(), CategoryIcon::Literal("?".to_string())),
        ]);
        for mode in IconMode::ALL {
            let settings = IconSettings::new(mode, overrides.clone());
            let expected_homelab = if mode == IconMode::Ascii { "-" } else { "⌂" };
            assert_eq!(
                settings.category_glyph("homelab"),
                expected_homelab,
                "{mode:?}"
            );
            assert_eq!(settings.category_glyph("misc"), "?", "{mode:?}");
        }
    }

    /// `CategoryIcon::parse`: one character is a literal, a known name is
    /// `Named`, and anything else is rejected.
    #[test]
    fn category_icon_parse() {
        assert_eq!(
            CategoryIcon::parse("security"),
            Some(CategoryIcon::Named(Icon::Security))
        );
        assert_eq!(
            CategoryIcon::parse("⌂"),
            Some(CategoryIcon::Literal("⌂".to_string()))
        );
        assert_eq!(
            CategoryIcon::parse("\u{f17c}"),
            Some(CategoryIcon::Literal("\u{f17c}".to_string()))
        );
        assert_eq!(CategoryIcon::parse("not-an-icon"), None);
        assert_eq!(CategoryIcon::parse(""), None);
    }
}
