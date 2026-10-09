//! Themes in the terminal app: ratatui colours, and loading/saving
//! `theme.toml` (`docs/ARCHITECTURE.md` § Configuration).
//!
//! The built-in palettes themselves (names, RGB values, the picker order)
//! live in the shared `tuxwire-core` crate so the Android app uses exactly the
//! same ones; they are re-exported below. What stays here is terminal-only:
//!
//! 1. [`Theme`], the same slots as a core [`Palette`] but as ready-to-paint
//!    `ratatui::style::Color` values, which the UI code reads
//!    (`theme.background`, `theme.accent_unread`, ...).
//! 2. [`ThemeSettings`], the user's choice read from `~/.config/tuxwire/
//!    theme.toml`: which built-in to start from, plus optional `[theme]`
//!    overrides. Any slot set there replaces the built-in's colour for that
//!    slot, whichever built-in is selected. A pre-picker `theme.toml` (one
//!    full `[theme]` table, no `base`) therefore still loads and looks
//!    exactly as it did before, since every slot is overridden.

// Has clippy flag any undocumented item here, the same as in `src/icons/`.
#![warn(clippy::missing_docs_in_private_items)]

use anyhow::{Context, anyhow};
use ratatui::style::Color;
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub use tuxwire_core::theme::{BUILTIN_THEMES, Palette, Rgb};
pub use tuxwire_core::theme::{builtin_index, default_index, picker_order};

/// The colors the UI layer actually uses, one per role described in
/// ARCHITECTURE.md's `theme.toml` section (backgrounds, borders, text by
/// read-state, and accents by article state).
///
/// Every field is a ready-to-paint `ratatui::style::Color`, never a hex
/// string. Hex parsing happens once, at load time (see `parse_slot`), so
/// no draw call can fail on a bad color.
///
/// This is the terminal-side twin of the core crate's [`Palette`]: the same
/// eleven slots, but as ratatui `Color`s instead of plain RGB bytes. Convert
/// with `Theme::from(palette)`.
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

/// Converts a plain core [`Rgb`] into ratatui's 24-bit `Color::Rgb`.
/// ratatui's `Color` enum also has named variants for the 16 basic terminal
/// colours (`Color::Red`, ...), but theme palettes need exact values, which
/// is what the `Rgb` variant is for.
fn to_color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

/// `impl From<A> for B` teaches Rust how to turn an `A` into a `B`; after
/// that, `Theme::from(palette)` or `palette.into()` both work. It is the
/// idiomatic way to say "this conversion always succeeds".
impl From<Palette> for Theme {
    /// Converts every slot of a core palette to a ratatui colour.
    fn from(p: Palette) -> Theme {
        Theme {
            background: to_color(p.background),
            panel_border: to_color(p.panel_border),
            text_primary: to_color(p.text_primary),
            text_muted: to_color(p.text_muted),
            text_dim: to_color(p.text_dim),
            accent_unread: to_color(p.accent_unread),
            accent_read: to_color(p.accent_read),
            accent_skipped: to_color(p.accent_skipped),
            accent_saved: to_color(p.accent_saved),
            accent_breaking: to_color(p.accent_breaking),
            accent_selected: to_color(p.accent_selected),
        }
    }
}

/// The user's theme choice, loaded from `theme.toml`: which built-in to
/// start from, plus any per-slot overrides. [`ThemeSettings::resolve`]
/// combines them into the [`Theme`] the UI draws with.
///
/// The UI keeps one of these for the whole session. The picker changes
/// `base` and calls `resolve` again to preview, and the overrides come
/// along automatically.
///
/// `Default` is implemented by hand below rather than derived, because a
/// derived one would set `base` to `0` (the first table entry) instead of
/// the named [`DEFAULT_THEME`].
#[derive(Debug, Clone)]
pub struct ThemeSettings {
    /// Index into [`BUILTIN_THEMES`] of the selected built-in. `pub` so the
    /// picker can move it.
    pub base: usize,
    /// The user's `[theme]` overrides. Private, and only filled by
    /// [`ThemeSettings::parse`], so it can't be set to anything unparsed.
    overrides: ThemeOverrides,
}

impl Default for ThemeSettings {
    /// The default theme ([`default_index`]) with no overrides, the same
    /// as an empty `theme.toml`.
    fn default() -> ThemeSettings {
        ThemeSettings {
            base: default_index(),
            overrides: ThemeOverrides::default(),
        }
    }
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
        // default theme, looked up by name (`default_index`), not whatever
        // happens to be first in the table. An unknown name is an error that lists the
        // valid names, rather than a silent fallback the user wouldn't
        // notice. `.map(|t| t.name)` pulls each built-in's name out, and
        // `.collect::<Vec<_>>().join(", ")` glues them into one string.
        let base = match raw.base.as_deref() {
            None => default_index(),
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
        let mut theme = Theme::from(BUILTIN_THEMES[self.base].colors);
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
        .map(|hex| {
            Rgb::from_hex_str(&hex)
                .map(to_color)
                .with_context(|| key.to_string())
        })
        .transpose()
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
    // Only the tests below need the default theme's name.
    use tuxwire_core::theme::DEFAULT_THEME;

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

    /// The default is Everforest, found by name, and the alphabetical
    /// picker order has nothing to do with it. Every path that yields "the
    /// default" is checked against the literal `"Everforest"` (not the
    /// `DEFAULT_THEME` constant, so an accidental edit to the constant fails
    /// too): `default_index`, `ThemeSettings::default()`, and a
    /// `theme.toml` with no `base` line.
    ///
    /// The last two asserts are the "confused" case: the picker's first row
    /// is Catppuccin Macchiato (alphabetically first), so if anything ever
    /// treated "first in the picker" (or "first after sorting") as the
    /// default, the default would come out as Macchiato and this test fails.
    #[test]
    fn default_is_everforest_by_name_not_by_position() {
        assert_eq!(DEFAULT_THEME, "Everforest");
        assert_eq!(BUILTIN_THEMES[default_index()].name, "Everforest");
        assert_eq!(ThemeSettings::default().base_name(), "Everforest");
        assert_eq!(ThemeSettings::parse("").unwrap().base_name(), "Everforest");
        assert_eq!(
            ThemeSettings::parse("[theme]\naccent_read = \"#123456\"")
                .unwrap()
                .base_name(),
            "Everforest"
        );

        let first_in_picker = BUILTIN_THEMES[picker_order()[0]].name;
        assert_eq!(first_in_picker, "Catppuccin Macchiato");
        assert_ne!(
            BUILTIN_THEMES[picker_order()[0]].name,
            ThemeSettings::default().base_name(),
            "the picker's first row must not decide the default"
        );
    }

    /// The bundled example selects the default theme with no overrides.
    #[test]
    fn bundled_default_is_everforest_without_overrides() {
        let settings = ThemeSettings::parse(DEFAULT_THEME_TOML).unwrap();
        assert_eq!(settings.base_name(), DEFAULT_THEME);
        assert_eq!(settings.override_count(), 0);
        assert_eq!(
            settings.resolve(),
            Theme::from(BUILTIN_THEMES[default_index()].colors)
        );
    }

    /// `base` picks the built-in, case-insensitively.
    #[test]
    fn base_selects_builtin() {
        let settings = ThemeSettings::parse(r#"base = "dracula""#).unwrap();
        assert_eq!(settings.base_name(), "Dracula");
        assert_eq!(settings.resolve(), Theme::from(BUILTIN_THEMES[1].colors));
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
        assert_eq!(theme, Theme::from(BUILTIN_THEMES[macchiato].colors));
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
        assert_eq!(
            gruvbox.background,
            to_color(BUILTIN_THEMES[2].colors.background)
        );

        settings.base = builtin_index("Nord").unwrap();
        let nord = settings.resolve();
        assert_eq!(nord.accent_unread, Color::Rgb(0xff, 0, 0));
        assert_eq!(
            nord.background,
            to_color(BUILTIN_THEMES[4].colors.background)
        );
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
