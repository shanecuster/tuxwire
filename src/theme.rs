//! Themes: the built-in palettes, and loading/saving `theme.toml`
//! (`docs/ARCHITECTURE.md` § Configuration).
//!
//! Per ARCHITECTURE.md: "All colors are config-driven — never hardcoded in
//! the UI code — so any shade can be tweaked without touching Rust." UI
//! code only ever reads the named slots on a [`Theme`] (`theme.background`,
//! `theme.accent_unread`, ...). Which actual colors fill those slots is
//! decided here, in two layers:
//!
//! 1. **A built-in theme** from [`BUILTIN_THEMES`] (Everforest by default),
//!    chosen with the `t` theme picker and saved as `base = "..."` in
//!    `theme.toml`.
//! 2. **The user's own overrides**, the optional `[theme]` table in
//!    `theme.toml`. Any slot set there replaces the built-in's color for
//!    that slot, whichever built-in is selected. A pre-picker `theme.toml`
//!    (one full `[theme]` table, no `base`) therefore still loads and looks
//!    exactly as it did before, since every slot is overridden.
//!
//! **Adding a theme = adding ONE entry to [`BUILTIN_THEMES`].** The picker,
//! the name lookup, and the contrast tests all read that list.
//!
//! Modeled on zerohour's `core/src/theme.rs`, but keeping tuxwire's own
//! slot names, since the UI already uses them everywhere.

// Has clippy flag any undocumented item here, the same as in `src/icons/`.
#![warn(clippy::missing_docs_in_private_items)]

use anyhow::{Context, anyhow, bail};
use ratatui::style::Color;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The colors the UI layer actually uses, one per role described in
/// ARCHITECTURE.md's `theme.toml` section (backgrounds, borders, text by
/// read-state, and accents by article state).
///
/// Every field is a ready-to-paint `ratatui::style::Color`, never a hex
/// string. Hex parsing happens once, at load time (see `parse_slot`), so
/// no draw call can fail on a bad color.
///
/// `Clone, Copy` because the whole struct is 11 small `Color` values. The
/// theme picker copies a built-in's colors and then applies overrides on
/// top, which is simplest when `Theme` is copied by value. `PartialEq` lets
/// tests compare whole themes, and `Debug` lets them print one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// The whole app's background.
    pub background: Color,
    /// Box borders around every pane and the sidebar sections.
    pub panel_border: Color,
    /// Normal text: pane contents, popup text, sidebar labels.
    pub text_primary: Color,
    /// De-emphasized text: meta lines, hints, the footer, empty states.
    pub text_muted: Color,
    /// Even fainter text, intended for skipped articles. Parsed and
    /// contrast-tested, but nothing draws with it yet.
    #[allow(dead_code)] // theme.toml's skipped-text shade; parsed, not painted yet
    pub text_dim: Color,
    /// Unread (and "new") article titles; the banner.
    pub accent_unread: Color,
    /// Read article titles.
    pub accent_read: Color,
    /// Skipped article titles.
    pub accent_skipped: Color,
    /// Saved article titles, note previews, non-blocking warnings.
    pub accent_saved: Color,
    /// Errors (the add-source popup's failure lines).
    pub accent_breaking: Color,
    /// The selected row's highlight background, key labels, focused fields,
    /// and popup borders.
    pub accent_selected: Color,
}

/// One entry in the built-in theme table: a display name plus a full set
/// of colors.
pub struct BuiltinTheme {
    /// Shown in the picker and saved as `base = "..."` in `theme.toml`.
    /// Matched case-insensitively when loading.
    pub name: &'static str,
    /// The theme's color for every slot.
    pub colors: Theme,
}

/// Builds a `Color` from a hex number like `0x2d353b` (the same digits as
/// CSS `#2d353b`), so palettes can be copied straight from each theme's
/// official docs.
///
/// `const fn` means the compiler can run it at compile time, which is what
/// lets [`BUILTIN_THEMES`] be a `const` table. `>>` shifts the bits right
/// and `& 0xff` keeps the lowest 8, pulling out the red, green, and blue
/// bytes in turn. `as u8` then narrows each to a byte, which can't lose
/// anything after the mask.
const fn rgb(hex: u32) -> Color {
    Color::Rgb(
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

/// The default theme's name. The code itself uses "index 0" as the
/// default (when `theme.toml` has no `base`, or doesn't exist); this
/// constant lets the tests check that index 0 really is Everforest.
/// `#[cfg(test)]` leaves it out of the real binary, where nothing reads it.
#[cfg(test)]
pub const DEFAULT_THEME: &str = "Everforest";

/// Every built-in theme, in the order the picker lists them. The first
/// entry is the default.
///
/// **To add a theme, copy one block, rename it, and change the colors.**
/// Nothing else needs touching. The tests in this file then check that the
/// new theme's text, muted text, and accents contrast clearly with its
/// background, and that its four article-state colors (plus `selected`)
/// are easy to tell apart.
///
/// Slot conventions, so every theme reads the same way: unread = orange,
/// read = teal/green, skipped = purple, saved = yellow, breaking = red,
/// selected = blue (or the theme's signature accent).
pub const BUILTIN_THEMES: &[BuiltinTheme] = &[
    // https://github.com/sainnhe/everforest (dark, medium contrast)
    BuiltinTheme {
        name: "Everforest",
        colors: Theme {
            background: rgb(0x2d353b),
            panel_border: rgb(0x475258),
            text_primary: rgb(0xd3c6aa),
            text_muted: rgb(0x859289),
            text_dim: rgb(0x56635f),
            accent_unread: rgb(0xe69875),
            accent_read: rgb(0x83c092),
            accent_skipped: rgb(0xd699b6),
            accent_saved: rgb(0xdbbc7f),
            accent_breaking: rgb(0xe67e80),
            accent_selected: rgb(0x7fbbb3),
        },
    },
    // https://draculatheme.com/contribute
    BuiltinTheme {
        name: "Dracula",
        colors: Theme {
            background: rgb(0x282a36),
            panel_border: rgb(0x44475a),
            text_primary: rgb(0xf8f8f2),
            text_muted: rgb(0x6272a4),
            text_dim: rgb(0x44475a),
            accent_unread: rgb(0xffb86c),
            accent_read: rgb(0x8be9fd),
            accent_skipped: rgb(0xbd93f9),
            accent_saved: rgb(0xf1fa8c),
            accent_breaking: rgb(0xff5555),
            accent_selected: rgb(0xff79c6),
        },
    },
    // https://github.com/morhetz/gruvbox (dark, medium)
    BuiltinTheme {
        name: "Gruvbox",
        colors: Theme {
            background: rgb(0x282828),
            panel_border: rgb(0x504945),
            text_primary: rgb(0xebdbb2),
            text_muted: rgb(0x928374),
            text_dim: rgb(0x665c54),
            accent_unread: rgb(0xfe8019),
            accent_read: rgb(0x8ec07c),
            accent_skipped: rgb(0xd3869b),
            accent_saved: rgb(0xfabd2f),
            accent_breaking: rgb(0xfb4934),
            accent_selected: rgb(0x83a598),
        },
    },
    // https://catppuccin.com/palette (Mocha flavor)
    BuiltinTheme {
        name: "Catppuccin Mocha",
        colors: Theme {
            background: rgb(0x1e1e2e),
            panel_border: rgb(0x313244),
            text_primary: rgb(0xcdd6f4),
            text_muted: rgb(0x6c7086),
            text_dim: rgb(0x45475a),
            accent_unread: rgb(0xfab387),
            accent_read: rgb(0x94e2d5),
            accent_skipped: rgb(0xcba6f7),
            accent_saved: rgb(0xf9e2af),
            accent_breaking: rgb(0xf38ba8),
            accent_selected: rgb(0x89b4fa),
        },
    },
    // https://www.nordtheme.com/docs/colors-and-palettes
    //
    // `accent_read` is nord14 (green), not nord7 (`#8fbcbb`): nord7 is
    // almost the same color as nord8, used for `accent_selected`, so read
    // titles and key labels blurred together. This is the same kind of
    // Nord near-match zerohour hit; `state_colors_are_distinct_in_every_theme`
    // below catches it.
    BuiltinTheme {
        name: "Nord",
        colors: Theme {
            background: rgb(0x2e3440),
            panel_border: rgb(0x434c5e),
            text_primary: rgb(0xd8dee9),
            text_muted: rgb(0x7b88a1),
            text_dim: rgb(0x4c566a),
            accent_unread: rgb(0xd08770),
            accent_read: rgb(0xa3be8c),
            accent_skipped: rgb(0xb48ead),
            accent_saved: rgb(0xebcb8b),
            accent_breaking: rgb(0xbf616a),
            accent_selected: rgb(0x88c0d0),
        },
    },
    // https://ethanschoonover.com/solarized/ (dark)
    BuiltinTheme {
        name: "Solarized Dark",
        colors: Theme {
            background: rgb(0x002b36),
            panel_border: rgb(0x586e75),
            text_primary: rgb(0x93a1a1),
            text_muted: rgb(0x657b83),
            text_dim: rgb(0x586e75),
            accent_unread: rgb(0xcb4b16),
            accent_read: rgb(0x2aa198),
            accent_skipped: rgb(0x6c71c4),
            accent_saved: rgb(0xb58900),
            accent_breaking: rgb(0xdc322f),
            accent_selected: rgb(0x268bd2),
        },
    },
    // https://github.com/folke/tokyonight.nvim (night)
    BuiltinTheme {
        name: "Tokyo Night",
        colors: Theme {
            background: rgb(0x1a1b26),
            panel_border: rgb(0x3b4261),
            text_primary: rgb(0xc0caf5),
            text_muted: rgb(0x737aa2),
            text_dim: rgb(0x565f89),
            accent_unread: rgb(0xff9e64),
            accent_read: rgb(0x73daca),
            accent_skipped: rgb(0xbb9af7),
            accent_saved: rgb(0xe0af68),
            accent_breaking: rgb(0xf7768e),
            accent_selected: rgb(0x7aa2f7),
        },
    },
    // https://catppuccin.com/palette (Macchiato flavor). tuxwire's original
    // and only palette before built-in themes existed, with exactly the
    // colors the old `theme.toml.example` shipped.
    BuiltinTheme {
        name: "Catppuccin Macchiato",
        colors: Theme {
            background: rgb(0x24273a),
            panel_border: rgb(0x363a4f),
            text_primary: rgb(0xcad3f5),
            text_muted: rgb(0x6e738d),
            text_dim: rgb(0x494d64),
            accent_unread: rgb(0xf5a97f),
            accent_read: rgb(0x8bd5ca),
            accent_skipped: rgb(0xc6a0f6),
            accent_saved: rgb(0xeed49f),
            accent_breaking: rgb(0xed8796),
            accent_selected: rgb(0x8aadf4),
        },
    },
];

/// The position of the built-in theme called `name` in [`BUILTIN_THEMES`],
/// ignoring upper/lower case, or `None` if there's no such theme.
///
/// `.iter().position(...)` walks the table and returns the index of the
/// first entry the closure says `true` for. `eq_ignore_ascii_case` compares
/// two strings while treating `A` and `a` as equal.
pub fn builtin_index(name: &str) -> Option<usize> {
    BUILTIN_THEMES
        .iter()
        .position(|theme| theme.name.eq_ignore_ascii_case(name.trim()))
}

/// The user's theme choice, loaded from `theme.toml`: which built-in to
/// start from, plus any per-slot overrides. [`ThemeSettings::resolve`]
/// combines them into the [`Theme`] the UI draws with.
///
/// The UI keeps one of these for the whole session. The picker changes
/// `base` and calls `resolve` again to preview, and the overrides come
/// along automatically.
#[derive(Debug, Clone, Default)]
pub struct ThemeSettings {
    /// Index into [`BUILTIN_THEMES`] of the selected built-in. `pub` so the
    /// picker can move it. `Default` gives `0`, the default theme.
    pub base: usize,
    /// The user's `[theme]` overrides. Private, and only filled by
    /// [`ThemeSettings::parse`], so it can't be set to anything unparsed.
    overrides: ThemeOverrides,
}

/// The optional `[theme]` slots from `theme.toml`, already converted to
/// colors. `None` means "not set, use the built-in's color".
///
/// `Default` makes every field `None`, i.e. "no overrides".
#[derive(Debug, Clone, Default)]
struct ThemeOverrides {
    /// Override for [`Theme::background`].
    background: Option<Color>,
    /// Override for [`Theme::panel_border`].
    panel_border: Option<Color>,
    /// Override for [`Theme::text_primary`].
    text_primary: Option<Color>,
    /// Override for [`Theme::text_muted`].
    text_muted: Option<Color>,
    /// Override for [`Theme::text_dim`].
    text_dim: Option<Color>,
    /// Override for [`Theme::accent_unread`].
    accent_unread: Option<Color>,
    /// Override for [`Theme::accent_read`].
    accent_read: Option<Color>,
    /// Override for [`Theme::accent_skipped`].
    accent_skipped: Option<Color>,
    /// Override for [`Theme::accent_saved`].
    accent_saved: Option<Color>,
    /// Override for [`Theme::accent_breaking`].
    accent_breaking: Option<Color>,
    /// Override for [`Theme::accent_selected`].
    accent_selected: Option<Color>,
}

/// The shape `theme.toml` deserializes into. Everything is optional, so
/// the old format (only a full `[theme]` table) and the new format (`base`
/// plus any number of `[theme]` overrides) both parse.
///
/// Unknown keys are ignored (no `deny_unknown_fields`) to match how
/// `theme.toml` always behaved, so an existing file can't start failing to
/// load after an upgrade.
#[derive(Deserialize)]
struct RawTheme {
    /// The built-in theme's name, e.g. `"Dracula"`. Written by the picker.
    base: Option<String>,
    /// The `[theme]` table. `#[serde(default)]` turns a missing table into
    /// an all-`None` one instead of a parse error.
    #[serde(default)]
    theme: RawThemeColors,
}

/// The `[theme]` table as raw hex strings, one `Option` per slot. Field
/// names match the `theme.toml` keys exactly.
#[derive(Deserialize, Default)]
struct RawThemeColors {
    /// `background = "#rrggbb"`.
    background: Option<String>,
    /// `panel_border = "#rrggbb"`.
    panel_border: Option<String>,
    /// `text_primary = "#rrggbb"`.
    text_primary: Option<String>,
    /// `text_muted = "#rrggbb"`.
    text_muted: Option<String>,
    /// `text_dim = "#rrggbb"`.
    text_dim: Option<String>,
    /// `accent_unread = "#rrggbb"`.
    accent_unread: Option<String>,
    /// `accent_read = "#rrggbb"`.
    accent_read: Option<String>,
    /// `accent_skipped = "#rrggbb"`.
    accent_skipped: Option<String>,
    /// `accent_saved = "#rrggbb"`.
    accent_saved: Option<String>,
    /// `accent_breaking = "#rrggbb"`.
    accent_breaking: Option<String>,
    /// `accent_selected = "#rrggbb"`.
    accent_selected: Option<String>,
}

/// The default `theme.toml`, compiled into the binary with `include_str!`
/// (the same trick `storage/migrations.rs` uses for the `.sql` files). It
/// selects Everforest with no overrides, which is what tuxwire uses before
/// `~/.config/tuxwire/theme.toml` exists. It's also the starting text when
/// the picker first saves a choice, so the new file arrives with its
/// explanatory comments.
const DEFAULT_THEME_TOML: &str = include_str!("../theme.toml.example");

impl ThemeSettings {
    /// Loads `~/.config/tuxwire/theme.toml`, or the bundled default if that
    /// file doesn't exist yet.
    ///
    /// Any *other* I/O error (permissions, a directory where a file was
    /// expected, ...) is still propagated with `?` rather than silently
    /// falling back — only "the file simply isn't there" is treated as
    /// expected. A malformed `theme.toml` that *does* exist (bad TOML, an
    /// unknown `base`, a hex string that doesn't parse) is also always an
    /// error: a user who wrote a mistake into their config should see that
    /// mistake, not have it silently swallowed by the same fallback that
    /// covers "no config yet."
    pub fn load() -> anyhow::Result<ThemeSettings> {
        let path = theme_path()?;

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                DEFAULT_THEME_TOML.to_string()
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to read theme file {}", path.display()));
            }
        };

        ThemeSettings::parse(&contents)
            .with_context(|| format!("failed to load theme file {}", path.display()))
    }

    /// Parses `theme.toml` text. Separate from [`ThemeSettings::load`] so
    /// tests can feed it strings directly, with no files involved.
    pub fn parse(contents: &str) -> anyhow::Result<ThemeSettings> {
        // Step 1: TOML text → `RawTheme` (all strings, all optional).
        let raw: RawTheme = toml::from_str(contents).context("invalid theme.toml")?;

        // Step 2: `base` → an index into `BUILTIN_THEMES`. Missing means the
        // default (index 0). An unknown name is an error that lists the
        // valid names, rather than a silent fallback the user wouldn't
        // notice. `.map(|t| t.name)` pulls each built-in's name out, and
        // `.collect::<Vec<_>>().join(", ")` glues them into one string.
        let base = match raw.base.as_deref() {
            None => 0,
            Some(name) => builtin_index(name).ok_or_else(|| {
                let names: Vec<&str> = BUILTIN_THEMES.iter().map(|t| t.name).collect();
                anyhow!("base = {name:?}: expected one of {}", names.join(", "))
            })?,
        };

        // Step 3: each optional hex string → an optional `Color`. Done slot
        // by slot (rather than in a loop) so each error names exactly the
        // `theme.toml` key the typo is in.
        let t = raw.theme;
        let overrides = ThemeOverrides {
            background: parse_slot(t.background, "theme.background")?,
            panel_border: parse_slot(t.panel_border, "theme.panel_border")?,
            text_primary: parse_slot(t.text_primary, "theme.text_primary")?,
            text_muted: parse_slot(t.text_muted, "theme.text_muted")?,
            text_dim: parse_slot(t.text_dim, "theme.text_dim")?,
            accent_unread: parse_slot(t.accent_unread, "theme.accent_unread")?,
            accent_read: parse_slot(t.accent_read, "theme.accent_read")?,
            accent_skipped: parse_slot(t.accent_skipped, "theme.accent_skipped")?,
            accent_saved: parse_slot(t.accent_saved, "theme.accent_saved")?,
            accent_breaking: parse_slot(t.accent_breaking, "theme.accent_breaking")?,
            accent_selected: parse_slot(t.accent_selected, "theme.accent_selected")?,
        };

        Ok(ThemeSettings { base, overrides })
    }

    /// The colors to draw with: the selected built-in, with every
    /// overridden slot replaced by the user's color.
    ///
    /// `let mut theme = ...colors;` copies the built-in's colors (`Theme` is
    /// `Copy`), so changing slots here never touches the `const` table.
    /// `Option::unwrap_or(fallback)` gives the override if there is one,
    /// otherwise the built-in's color that was already there.
    pub fn resolve(&self) -> Theme {
        let o = &self.overrides;
        let mut theme = BUILTIN_THEMES[self.base].colors;
        theme.background = o.background.unwrap_or(theme.background);
        theme.panel_border = o.panel_border.unwrap_or(theme.panel_border);
        theme.text_primary = o.text_primary.unwrap_or(theme.text_primary);
        theme.text_muted = o.text_muted.unwrap_or(theme.text_muted);
        theme.text_dim = o.text_dim.unwrap_or(theme.text_dim);
        theme.accent_unread = o.accent_unread.unwrap_or(theme.accent_unread);
        theme.accent_read = o.accent_read.unwrap_or(theme.accent_read);
        theme.accent_skipped = o.accent_skipped.unwrap_or(theme.accent_skipped);
        theme.accent_saved = o.accent_saved.unwrap_or(theme.accent_saved);
        theme.accent_breaking = o.accent_breaking.unwrap_or(theme.accent_breaking);
        theme.accent_selected = o.accent_selected.unwrap_or(theme.accent_selected);
        theme
    }

    /// The selected built-in's name, e.g. `"Everforest"`.
    pub fn base_name(&self) -> &'static str {
        BUILTIN_THEMES[self.base].name
    }

    /// How many slots the user's `[theme]` table overrides (0–11). The
    /// picker shows this, because overrides apply on top of *every*
    /// built-in, and a full old-style `[theme]` table makes the picker
    /// look like it does nothing.
    ///
    /// Builds an array of "is this slot set?" booleans and counts the
    /// `true`s. `Option::is_some` is `true` for `Some(_)`.
    pub fn override_count(&self) -> usize {
        let o = &self.overrides;
        [
            o.background.is_some(),
            o.panel_border.is_some(),
            o.text_primary.is_some(),
            o.text_muted.is_some(),
            o.text_dim.is_some(),
            o.accent_unread.is_some(),
            o.accent_read.is_some(),
            o.accent_skipped.is_some(),
            o.accent_saved.is_some(),
            o.accent_breaking.is_some(),
            o.accent_selected.is_some(),
        ]
        .iter()
        .filter(|set| **set)
        .count()
    }
}

/// Saves `name` as `base = "..."` in `~/.config/tuxwire/theme.toml`,
/// creating the file (from the bundled, commented default) if it doesn't
/// exist. Used when the picker's `Enter` confirms a theme.
pub fn save_base(name: &str) -> anyhow::Result<()> {
    save_base_at(&theme_path()?, name)
}

/// The body of [`save_base`], taking the path as a parameter so tests can
/// point it at a temporary file. Same split as `icons::config::save_mode`.
///
/// Goes through `toml_edit::DocumentMut` (see the `toml_edit` comment in
/// `Cargo.toml`), so only the `base` line changes. The user's comments and
/// `[theme]` overrides stay exactly as written.
fn save_base_at(path: &Path, name: &str) -> anyhow::Result<()> {
    // Start from the existing file, or the bundled default the first time.
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => DEFAULT_THEME_TOML.to_string(),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read theme file {}", path.display()));
        }
    };

    let mut doc: toml_edit::DocumentMut = contents
        .parse()
        .with_context(|| format!("failed to parse theme file {} as TOML", path.display()))?;

    // `doc["base"] = ...` replaces the value in place, or adds the key if
    // it's missing (an old-style file). Root-level keys are always written
    // before the first `[table]`, so `base` lands above `[theme]` where
    // TOML requires it.
    doc["base"] = toml_edit::value(name);

    // `~/.config/tuxwire/` might not exist yet on a true first run.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    std::fs::write(path, doc.to_string())
        .with_context(|| format!("failed to write theme file {}", path.display()))
}

/// Converts one optional `[theme]` value into an optional `Color`. `None`
/// (the key wasn't in the file) stays `None`, and a present value must be
/// valid hex. The error is labeled with `key`, so the message says which
/// line to fix.
///
/// `value.map(...)` runs the parse only when there's a value, giving
/// `Option<Result<Color>>`. `.transpose()` flips that into
/// `Result<Option<Color>>`, which is what `?` needs: an `Err` stops
/// loading, and otherwise you get the `Option` back.
fn parse_slot(value: Option<String>, key: &str) -> anyhow::Result<Option<Color>> {
    value
        .map(|hex| hex_to_color(&hex).with_context(|| key.to_string()))
        .transpose()
}

/// Parses a `"#rrggbb"` string (the `"#"` is optional) into
/// `Color::Rgb(r, g, b)`. ratatui's `Color` enum has named variants for
/// basic terminal colors (`Color::Red`, etc.), but theme palettes need
/// exact 24-bit values, which is what the `Rgb` variant is for.
///
/// `u8::from_str_radix(s, 16)` parses a string as a base-16 (hexadecimal)
/// number into a `u8` — "radix" is the general term for a number base, and
/// `16` is what makes this hex rather than the usual base-10 `str::parse`
/// would assume.
fn hex_to_color(hex: &str) -> anyhow::Result<Color> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);

    // The `is_ascii` check matters for the byte slicing below: `&hex[0..2]`
    // slices by *byte* position, and slicing through the middle of a
    // multi-byte character (say, `"€€"`, which is also 6 bytes) panics.
    // Requiring ASCII first means every byte is a whole character.
    if hex.len() != 6 || !hex.is_ascii() {
        bail!("expected a 6-digit hex color like \"#8aadf4\", got {hex:?}");
    }

    let r = u8::from_str_radix(&hex[0..2], 16).context("invalid red channel")?;
    let g = u8::from_str_radix(&hex[2..4], 16).context("invalid green channel")?;
    let b = u8::from_str_radix(&hex[4..6], 16).context("invalid blue channel")?;

    Ok(Color::Rgb(r, g, b))
}

/// The full path to the user's theme config: `~/.config/tuxwire/theme.toml`,
/// per ARCHITECTURE.md's "Config lives separately at `~/.config/tuxwire/`".
///
/// This mirrors `storage::data_dir`'s reasoning almost exactly (see that
/// function's doc comment for the full explanation of why no `dirs` crate
/// is used) — same XDG-fallback pattern, just for `$XDG_CONFIG_HOME`
/// instead of `$XDG_DATA_HOME`, since config and data are two different
/// XDG base directories with two different environment variables.
fn theme_path() -> anyhow::Result<PathBuf> {
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config_home)
            .join("tuxwire")
            .join("theme.toml"));
    }

    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate the theme config")?;

    Ok(PathBuf::from(home).join(".config/tuxwire/theme.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old bundled default (Catppuccin Macchiato, one full `[theme]`
    /// table and no `base`), exactly as `theme.toml.example` shipped before
    /// the picker existed. A user who copied it must see the same colors.
    const OLD_STYLE_THEME_TOML: &str = r##"
[theme]
background   = "#24273a"
panel_border = "#363a4f"
text_primary = "#cad3f5"
text_muted   = "#6e738d"
text_dim     = "#494d64"
accent_unread   = "#f5a97f"
accent_read     = "#8bd5ca"
accent_skipped  = "#c6a0f6"
accent_saved    = "#eed49f"
accent_breaking = "#ed8796"
accent_selected = "#8aadf4"
"##;

    /// The red, green, and blue bytes of a theme color. Every built-in uses
    /// `Color::Rgb`, so anything else is a bug in the table.
    fn channels(color: Color) -> (u8, u8, u8) {
        match color {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("expected an Rgb color, got {other:?}"),
        }
    }

    /// One sRGB channel (0–255) converted to linear light (0.0–1.0). This
    /// is the standard sRGB "gamma" curve, needed because screen values
    /// aren't proportional to actual brightness.
    fn linear(channel: u8) -> f64 {
        let c = channel as f64 / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG relative luminance: how bright a color looks, 0.0 (black) to
    /// 1.0 (white). Green weighs most because eyes are most sensitive to it.
    fn luminance(color: Color) -> f64 {
        let (r, g, b) = channels(color);
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    /// WCAG contrast ratio between two colors: 1.0 (identical) to 21.0
    /// (black on white). Text needs 4.5 to pass WCAG AA, and large text or
    /// UI elements need 3.0.
    fn contrast(a: Color, b: Color) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        // `f64::max`/`min` pick the brighter and darker of the two, so the
        // order the colors are passed in doesn't matter.
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// A color in CIE L*a*b*, a color space where straight-line distance
    /// roughly matches how different two colors *look*. Raw RGB distance
    /// doesn't: two blues can be far apart in RGB yet look nearly identical.
    fn lab(color: Color) -> (f64, f64, f64) {
        let (r, g, b) = channels(color);
        let (r, g, b) = (linear(r), linear(g), linear(b));
        // sRGB → CIE XYZ (D65 white point), each divided by the white
        // point's own X/Y/Z so white comes out as (1, 1, 1).
        let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
        // The Lab curve: a cube root, with a straight-line piece near black.
        let f = |t: f64| {
            if t > 0.008856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let (fx, fy, fz) = (f(x), f(y), f(z));
        (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
    }

    /// CIE76 ΔE: the Lab distance between two colors. Around 2.3 is the
    /// smallest difference most people can see; 20+ is "clearly different
    /// colors at a glance", which is the bar the tests below use.
    fn delta_e(a: Color, b: Color) -> f64 {
        let (l1, a1, b1) = lab(a);
        let (l2, a2, b2) = lab(b);
        ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
    }

    /// The default is the first entry, so `ThemeSettings::default()`
    /// (base `0`) and "no `base` in theme.toml" both mean Everforest.
    #[test]
    fn default_theme_is_first() {
        assert_eq!(BUILTIN_THEMES[0].name, DEFAULT_THEME);
        assert_eq!(ThemeSettings::default().base_name(), DEFAULT_THEME);
    }

    /// All eight built-in themes exist, with no duplicate names.
    #[test]
    fn all_eight_themes_exist_with_unique_names() {
        for expected in [
            "Everforest",
            "Dracula",
            "Gruvbox",
            "Catppuccin Mocha",
            "Nord",
            "Solarized Dark",
            "Tokyo Night",
            "Catppuccin Macchiato",
        ] {
            assert!(
                builtin_index(expected).is_some(),
                "missing theme {expected}"
            );
        }
        let mut names: Vec<String> = BUILTIN_THEMES
            .iter()
            .map(|t| t.name.to_lowercase())
            .collect();
        let total = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate theme names");
    }

    /// Lookup ignores case and surrounding spaces, and unknown names give
    /// `None`.
    #[test]
    fn builtin_lookup_is_case_insensitive() {
        assert_eq!(builtin_index("nord"), builtin_index("Nord"));
        assert_eq!(
            builtin_index(" TOKYO NIGHT ").map(|i| BUILTIN_THEMES[i].name),
            Some("Tokyo Night")
        );
        assert_eq!(builtin_index("Monokai"), None);
    }

    /// Text and every accent stand out from the background in every theme,
    /// using WCAG contrast: primary text ≥ 4.5 (AA for body text); muted
    /// text and accents ≥ 3.0 (AA for large text/UI, and accents are titles
    /// and highlights). `accent_selected` also covers the highlighted row,
    /// which draws `background`-colored text *on* `accent_selected`, the
    /// same pair. Borders and dim text only need to be visible (≥ 1.25 and
    /// ≥ 1.5), since they're meant to recede.
    #[test]
    fn text_and_accents_contrast_with_background_in_every_theme() {
        for builtin in BUILTIN_THEMES {
            let t = &builtin.colors;
            let bg = t.background;
            let checks = [
                ("text_primary", t.text_primary, 4.5),
                ("text_muted", t.text_muted, 3.0),
                ("accent_unread", t.accent_unread, 3.0),
                ("accent_read", t.accent_read, 3.0),
                ("accent_skipped", t.accent_skipped, 3.0),
                ("accent_saved", t.accent_saved, 3.0),
                ("accent_breaking", t.accent_breaking, 3.0),
                ("accent_selected", t.accent_selected, 3.0),
                ("panel_border", t.panel_border, 1.25),
                ("text_dim", t.text_dim, 1.5),
            ];
            for (slot, color, minimum) in checks {
                let ratio = contrast(color, bg);
                assert!(
                    ratio >= minimum,
                    "{}: {slot} vs background contrast {ratio:.2} < {minimum}",
                    builtin.name
                );
            }
        }
    }

    /// The four article-state colors (and `accent_selected`, which colors
    /// key labels right next to them in the sidebar) are clearly different
    /// from each other in every theme. Nord's nord7/nord8 pair (ΔE ≈ 10)
    /// failed this before `accent_read` moved to nord14.
    #[test]
    fn state_colors_are_distinct_in_every_theme() {
        for builtin in BUILTIN_THEMES {
            let t = &builtin.colors;
            let slots = [
                ("unread", t.accent_unread),
                ("read", t.accent_read),
                ("skipped", t.accent_skipped),
                ("saved", t.accent_saved),
                ("selected", t.accent_selected),
            ];
            // Every pair once: `i` walks the list, `j` only the entries
            // after it, so (a, b) is checked but (b, a) isn't repeated.
            for i in 0..slots.len() {
                for j in (i + 1)..slots.len() {
                    let (name_a, a) = slots[i];
                    let (name_b, b) = slots[j];
                    let distance = delta_e(a, b);
                    assert!(
                        distance >= 20.0,
                        "{}: {name_a} and {name_b} are too similar (ΔE {distance:.1})",
                        builtin.name
                    );
                }
            }
        }
    }

    /// The bundled example selects the default theme with no overrides.
    #[test]
    fn bundled_default_is_everforest_without_overrides() {
        let settings = ThemeSettings::parse(DEFAULT_THEME_TOML).unwrap();
        assert_eq!(settings.base_name(), DEFAULT_THEME);
        assert_eq!(settings.override_count(), 0);
        assert_eq!(settings.resolve(), BUILTIN_THEMES[0].colors);
    }

    /// `base` picks the built-in, case-insensitively.
    #[test]
    fn base_selects_builtin() {
        let settings = ThemeSettings::parse(r#"base = "dracula""#).unwrap();
        assert_eq!(settings.base_name(), "Dracula");
        assert_eq!(settings.resolve(), BUILTIN_THEMES[1].colors);
    }

    /// A pre-picker `theme.toml` still loads, and looks exactly as before:
    /// all 11 slots are overrides, so the Macchiato colors win over the
    /// default built-in.
    #[test]
    fn old_style_full_theme_toml_still_works() {
        let settings = ThemeSettings::parse(OLD_STYLE_THEME_TOML).unwrap();
        assert_eq!(settings.override_count(), 11);
        let theme = settings.resolve();
        assert_eq!(theme.background, Color::Rgb(0x24, 0x27, 0x3a));
        assert_eq!(theme.accent_selected, Color::Rgb(0x8a, 0xad, 0xf4));

        // ...and those are exactly the Catppuccin Macchiato built-in, so a
        // user can delete their 11 override lines, pick Macchiato, and see
        // no difference at all.
        let macchiato = builtin_index("Catppuccin Macchiato").unwrap();
        assert_eq!(theme, BUILTIN_THEMES[macchiato].colors);
    }

    /// A partial `[theme]` table overrides only the slots it sets, and those
    /// overrides follow along when the base changes (what the picker's
    /// live preview does).
    #[test]
    fn partial_overrides_apply_on_top_of_any_base() {
        let mut settings = ThemeSettings::parse(
            r##"
            base = "Gruvbox"
            [theme]
            accent_unread = "#ff0000"
            "##,
        )
        .unwrap();
        assert_eq!(settings.override_count(), 1);

        let gruvbox = settings.resolve();
        assert_eq!(gruvbox.accent_unread, Color::Rgb(0xff, 0, 0));
        assert_eq!(gruvbox.background, BUILTIN_THEMES[2].colors.background);

        settings.base = builtin_index("Nord").unwrap();
        let nord = settings.resolve();
        assert_eq!(nord.accent_unread, Color::Rgb(0xff, 0, 0));
        assert_eq!(nord.background, BUILTIN_THEMES[4].colors.background);
    }

    /// An unknown `base` and a bad hex color are errors that name the
    /// problem, not silent fallbacks.
    #[test]
    fn rejects_unknown_base_and_bad_hex() {
        let err = ThemeSettings::parse(r#"base = "Monokai""#).unwrap_err();
        let message = format!("{err:#}");
        assert!(
            message.contains("Monokai") && message.contains("Everforest"),
            "{message}"
        );

        let err = ThemeSettings::parse("[theme]\naccent_read = \"#12345\"").unwrap_err();
        assert!(format!("{err:#}").contains("theme.accent_read"), "{err:#}");
    }

    /// `hex_to_color` accepts with or without `#`, and rejects non-ASCII
    /// input instead of panicking on a mid-character slice.
    #[test]
    fn hex_parsing() {
        assert_eq!(
            hex_to_color("#2d353b").unwrap(),
            Color::Rgb(0x2d, 0x35, 0x3b)
        );
        assert_eq!(
            hex_to_color("2d353b").unwrap(),
            Color::Rgb(0x2d, 0x35, 0x3b)
        );
        assert!(hex_to_color("€€").is_err());
        assert!(hex_to_color("#zzzzzz").is_err());
        assert_eq!(rgb(0x2d353b), Color::Rgb(0x2d, 0x35, 0x3b));
    }

    /// A per-test temporary path, same hand-rolled approach as
    /// `icons::config`'s tests (no `tempfile` dependency).
    fn temp_theme_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tuxwire-test-theme-{tag}-{:?}",
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("theme.toml")
    }

    /// Saving into an old-style file adds `base` above `[theme]` and keeps
    /// every override and comment, and the result loads back.
    #[test]
    fn save_base_preserves_overrides_and_comments() {
        let path = temp_theme_path("preserve");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = format!("# my macchiato\n{OLD_STYLE_THEME_TOML}");
        std::fs::write(&path, &original).unwrap();

        save_base_at(&path, "Nord").unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("# my macchiato"));
        assert!(written.contains("accent_unread   = \"#f5a97f\""));
        let settings = ThemeSettings::parse(&written).unwrap();
        assert_eq!(settings.base_name(), "Nord");
        assert_eq!(settings.override_count(), 11);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Saving when no file exists creates it from the commented default.
    #[test]
    fn save_base_creates_file_from_default() {
        let path = temp_theme_path("create");
        save_base_at(&path, "Tokyo Night").unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        let settings = ThemeSettings::parse(&written).unwrap();
        assert_eq!(settings.base_name(), "Tokyo Night");
        assert_eq!(settings.override_count(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
