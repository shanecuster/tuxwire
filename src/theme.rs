//! Loading `theme.toml` (`docs/ARCHITECTURE.md` § Configuration).
//!
//! Per ARCHITECTURE.md: "All colors are config-driven — never hardcoded in
//! the UI code — so any shade can be tweaked without touching Rust." This
//! module is the *only* place that parses a hex string like `"#8aadf4"`
//! into something ratatui can actually paint with; `ui/mod.rs` only ever
//! reaches for the fields on a already-built `Theme`.

use anyhow::{Context, bail};
use ratatui::style::Color;
use serde::Deserialize;
use std::path::PathBuf;

/// The colors the UI layer actually uses, one per role described in
/// ARCHITECTURE.md's `theme.toml` section (backgrounds, borders, text by
/// read-state, and accents by article state).
///
/// This is deliberately a *different* type from the one `toml` deserializes
/// into (`RawTheme`, below) rather than deriving `Deserialize` directly on
/// this struct. `ratatui::style::Color` has no `Deserialize` impl of its
/// own — it doesn't know what a hex string is — so something has to sit
/// between "text read from a file" and "a `Color` ratatui can render."
/// Doing that conversion once, at load time, means every other function in
/// this codebase gets a `Theme` whose fields are already valid, renderable
/// colors, instead of hex strings every call site would have to parse (and
/// possibly fail to parse) itself.
pub struct Theme {
    pub background: Color,
    pub panel_border: Color,
    pub text_primary: Color,
    pub text_muted: Color,
    pub text_dim: Color,
    pub accent_unread: Color,
    pub accent_read: Color,
    pub accent_skipped: Color,
    pub accent_saved: Color,
    pub accent_breaking: Color,
    pub accent_selected: Color,
}

/// The shape `theme.toml` deserializes into, matching its `[theme]` table
/// field-for-field. Every field is a plain `String` (the hex code exactly
/// as written in the file) — `serde` can derive `Deserialize` for that
/// automatically, since strings are one of the formats it understands
/// natively, unlike `ratatui::style::Color` above.
#[derive(Deserialize)]
struct RawTheme {
    theme: RawThemeColors,
}

#[derive(Deserialize)]
struct RawThemeColors {
    background: String,
    panel_border: String,
    text_primary: String,
    text_muted: String,
    text_dim: String,
    accent_unread: String,
    accent_read: String,
    accent_skipped: String,
    accent_saved: String,
    accent_breaking: String,
    accent_selected: String,
}

/// The default theme, embedded directly into the compiled binary via
/// `include_str!` — the same trick `storage/migrations.rs` uses to bundle
/// the `migrations/*.sql` files. This is the Catppuccin Macchiato palette
/// documented in `docs/ARCHITECTURE.md`, and is what `Theme::load` falls
/// back to when the user hasn't created `~/.config/tuxwire/theme.toml` for
/// themselves yet — a first run of tuxwire should never fail (or draw an
/// unstyled UI) just because no config file exists.
const DEFAULT_THEME_TOML: &str = include_str!("../theme.toml.example");

impl Theme {
    /// Loads the theme from `~/.config/tuxwire/theme.toml`, or falls back
    /// to the bundled Catppuccin Macchiato default (see
    /// `DEFAULT_THEME_TOML` above) if that file doesn't exist yet.
    ///
    /// Any *other* I/O error (permissions, a directory where a file was
    /// expected, ...) is still propagated with `?` rather than silently
    /// falling back — only "the file simply isn't there" is treated as
    /// expected. A malformed `theme.toml` that *does* exist (bad TOML, a
    /// misspelled key, a hex string that doesn't parse) is also always an
    /// error: a user who wrote a syntax mistake into their config should
    /// see that mistake, not have it silently swallowed by the same
    /// fallback that covers "no config yet."
    pub fn load() -> anyhow::Result<Theme> {
        let path = theme_path()?;

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => DEFAULT_THEME_TOML.to_string(),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read theme file {}", path.display()));
            }
        };

        let raw: RawTheme = toml::from_str(&contents)
            .with_context(|| format!("failed to parse theme file {}", path.display()))?;

        raw.theme.into_theme()
    }
}

impl RawThemeColors {
    /// Converts every hex-string field into a `Color`, bailing out with a
    /// message naming the specific field if any one of them isn't valid
    /// `#rrggbb`. Doing this field-by-field (rather than, say, a loop over
    /// a list of pairs) keeps each error message pointing at exactly the
    /// `theme.toml` key a typo was made in.
    fn into_theme(self) -> anyhow::Result<Theme> {
        Ok(Theme {
            background: hex_to_color(&self.background).context("theme.background")?,
            panel_border: hex_to_color(&self.panel_border).context("theme.panel_border")?,
            text_primary: hex_to_color(&self.text_primary).context("theme.text_primary")?,
            text_muted: hex_to_color(&self.text_muted).context("theme.text_muted")?,
            text_dim: hex_to_color(&self.text_dim).context("theme.text_dim")?,
            accent_unread: hex_to_color(&self.accent_unread).context("theme.accent_unread")?,
            accent_read: hex_to_color(&self.accent_read).context("theme.accent_read")?,
            accent_skipped: hex_to_color(&self.accent_skipped).context("theme.accent_skipped")?,
            accent_saved: hex_to_color(&self.accent_saved).context("theme.accent_saved")?,
            accent_breaking: hex_to_color(&self.accent_breaking).context("theme.accent_breaking")?,
            accent_selected: hex_to_color(&self.accent_selected).context("theme.accent_selected")?,
        })
    }
}

/// Parses a `"#rrggbb"` string (the `"#"` is optional) into
/// `Color::Rgb(r, g, b)`. ratatui's `Color` enum has named variants for
/// basic terminal colors (`Color::Red`, etc.), but Catppuccin's palette
/// needs exact 24-bit values, which is what the `Rgb` variant is for.
///
/// `u8::from_str_radix(s, 16)` parses a string as a base-16 (hexadecimal)
/// number into a `u8` — "radix" is the general term for a number base, and
/// `16` is what makes this hex rather than the usual base-10 `str::parse`
/// would assume.
fn hex_to_color(hex: &str) -> anyhow::Result<Color> {
    let hex = hex.strip_prefix('#').unwrap_or(hex);

    if hex.len() != 6 {
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
        return Ok(PathBuf::from(xdg_config_home).join("tuxwire").join("theme.toml"));
    }

    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate the theme config")?;

    Ok(PathBuf::from(home).join(".config/tuxwire/theme.toml"))
}
