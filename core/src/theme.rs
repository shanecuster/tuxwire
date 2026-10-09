//! The built-in colour themes, as plain RGB values.
//!
//! This is the shareable half of tuxwire's theming. It knows the eight
//! built-in palettes (Everforest, Dracula, Nord, ...), their names, and how
//! to turn a `"#rrggbb"` string into a colour -- and nothing about *how a
//! colour is painted*. The terminal app turns a [`Palette`] into ratatui
//! colours; the Android app turns the very same palette into Compose
//! colours. That is why a colour here is a bare [`Rgb`] (three bytes) and
//! not a `ratatui::style::Color`.
//!
//! Per ARCHITECTURE.md: "All colors are config-driven -- never hardcoded in
//! the UI code." UI code only ever reads the named slots on a palette
//! (`background`, `accent_unread`, ...). Which actual colours fill those
//! slots is decided here (the built-in table) and, in the terminal app, by
//! the user's `theme.toml` overrides on top of it.
//!
//! **Adding a theme = adding ONE entry to [`BUILTIN_THEMES`].** The picker,
//! the name lookup, and the contrast tests all read that list.
//!
//! Modeled on zerohour's `core/src/theme.rs`, but keeping tuxwire's own
//! slot names, since the UI already uses them everywhere.

// Has clippy flag any undocumented item here, the same as in `icons/`.
#![warn(clippy::missing_docs_in_private_items)]

use anyhow::{Context, bail};

/// A colour as three bytes: red, green and blue, each 0-255.
///
/// This is deliberately the simplest possible colour type. Every front end
/// converts it to whatever its own toolkit wants: ratatui's `Color::Rgb`,
/// Compose's `Color(red, green, blue)`, and so on. `Copy` means it is
/// duplicated by value instead of moved (it is just three bytes), and
/// `PartialEq` lets tests compare two colours with `==`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel, 0-255.
    pub r: u8,
    /// Green channel, 0-255.
    pub g: u8,
    /// Blue channel, 0-255.
    pub b: u8,
}

impl Rgb {
    /// Parses a `"#rrggbb"` string (the `"#"` is optional) into an [`Rgb`].
    ///
    /// `u8::from_str_radix(s, 16)` parses a string as a base-16 (hexadecimal)
    /// number into a `u8` -- "radix" is the general term for a number base,
    /// and `16` is what makes this hex rather than the usual base-10
    /// `str::parse` would assume.
    pub fn from_hex_str(hex: &str) -> anyhow::Result<Rgb> {
        let hex = hex.strip_prefix('#').unwrap_or(hex);

        // The `is_ascii` check matters for the byte slicing below: `&hex[0..2]`
        // slices by *byte* position, and slicing through the middle of a
        // multi-byte character (say, `"€€"`, which is also 6 bytes) panics.
        // Requiring ASCII first means every byte is a whole character.
        if hex.len() != 6 || !hex.is_ascii() {
            bail!("expected a 6-digit hex color like \"#8aadf4\", got {hex:?}");
        }

        // Two hex digits per channel.
        let r = u8::from_str_radix(&hex[0..2], 16).context("invalid red channel")?;
        let g = u8::from_str_radix(&hex[2..4], 16).context("invalid green channel")?;
        let b = u8::from_str_radix(&hex[4..6], 16).context("invalid blue channel")?;

        Ok(Rgb { r, g, b })
    }
}

/// The colours a front end needs, one per role described in
/// ARCHITECTURE.md's `theme.toml` section (backgrounds, borders, text by
/// read-state, and accents by article state).
///
/// Every field is a ready-to-use [`Rgb`], never a hex string. Hex parsing
/// happens once, at load time, so no draw call can fail on a bad colour.
///
/// `Clone, Copy` because the whole struct is 11 small colours. The theme
/// picker copies a built-in's colours and then applies overrides on top,
/// which is simplest when a palette is copied by value. `PartialEq` lets
/// tests compare whole palettes, and `Debug` lets them print one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The whole app's background.
    pub background: Rgb,
    /// Box borders around every pane and the sidebar sections.
    pub panel_border: Rgb,
    /// Normal text: pane contents, popup text, sidebar labels.
    pub text_primary: Rgb,
    /// De-emphasized text: meta lines, hints, the footer, empty states.
    pub text_muted: Rgb,
    /// Even fainter text, intended for skipped articles. Contrast-tested,
    /// but nothing draws with it yet.
    pub text_dim: Rgb,
    /// Unread (and "new") article titles; the banner.
    pub accent_unread: Rgb,
    /// Read article titles.
    pub accent_read: Rgb,
    /// Skipped article titles.
    pub accent_skipped: Rgb,
    /// Saved article titles, note previews, non-blocking warnings.
    pub accent_saved: Rgb,
    /// Errors (the add-source popup's failure lines).
    pub accent_breaking: Rgb,
    /// The selected row's highlight background, key labels, focused fields,
    /// and popup borders.
    pub accent_selected: Rgb,
}

/// One entry in the built-in theme table: a display name plus a full set
/// of colors.
pub struct BuiltinTheme {
    /// Shown in the picker and saved as `base = "..."` in `theme.toml`.
    /// Matched case-insensitively when loading.
    pub name: &'static str,
    /// The theme's colour for every slot.
    pub colors: Palette,
}

/// Builds an [`Rgb`] from a hex number like `0x2d353b` (the same digits as
/// CSS `#2d353b`), so palettes can be copied straight from each theme's
/// official docs.
///
/// `const fn` means the compiler can run it at compile time, which is what
/// lets [`BUILTIN_THEMES`] be a `const` table. `>>` shifts the bits right
/// and `& 0xff` keeps the lowest 8, pulling out the red, green, and blue
/// bytes in turn. `as u8` then narrows each to a byte, which can't lose
/// anything after the mask.
const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: ((hex >> 16) & 0xff) as u8,
        g: ((hex >> 8) & 0xff) as u8,
        b: (hex & 0xff) as u8,
    }
}

/// The default theme, **by name**: what a `theme.toml` with no `base`
/// line gets, and what `ThemeSettings::default()` starts with (see
/// [`default_index`]).
///
/// Named on purpose rather than "whatever is first in [`BUILTIN_THEMES`]".
/// The picker lists themes alphabetically ([`picker_order`]), and nothing
/// about the table's order or the picker's order may change which theme
/// is the default. The bundled `theme.toml.example` says
/// `base = "Everforest"` too; a test keeps the two in agreement.
pub const DEFAULT_THEME: &str = "Everforest";

/// Every built-in theme. The order here doesn't matter: the picker sorts
/// them by name ([`picker_order`]), and the default is chosen by name
/// ([`DEFAULT_THEME`]), never by position.
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
        colors: Palette {
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
        colors: Palette {
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
        colors: Palette {
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
        colors: Palette {
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
        colors: Palette {
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
        colors: Palette {
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
        colors: Palette {
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
        colors: Palette {
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

/// The index in [`BUILTIN_THEMES`] of the default theme, found by its name
/// ([`DEFAULT_THEME`]) rather than assumed to be `0`.
///
/// `.expect(...)` unwraps the `Option`, and would panic with that message
/// if `DEFAULT_THEME` named a theme that isn't in the table. That can only
/// happen through a code edit, never through user config, and the test
/// `default_is_everforest_by_name_not_by_position` fails first if it does.
pub fn default_index() -> usize {
    builtin_index(DEFAULT_THEME).expect("DEFAULT_THEME must name an entry in BUILTIN_THEMES")
}

/// The order the `t` picker lists the built-in themes in: indices into
/// [`BUILTIN_THEMES`], sorted alphabetically by name, ignoring case.
///
/// Returning indices (not the themes themselves) lets the picker keep
/// storing "which theme" as a `BUILTIN_THEMES` index, the same thing
/// `ThemeSettings::base` holds, so the sort only affects display.
///
/// `(0..len).collect()` builds the list `[0, 1, 2, ...]`. Then
/// `sort_by_key` reorders it by a key computed per element, here each
/// theme's name in lowercase, so `"catppuccin"` and `"Catppuccin"` sort
/// alike. Allocating a fresh 8-item `Vec` on every call is negligible.
pub fn picker_order() -> Vec<usize> {
    let mut order: Vec<usize> = (0..BUILTIN_THEMES.len()).collect();
    order.sort_by_key(|&index| BUILTIN_THEMES[index].name.to_lowercase());
    order
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The red, green, and blue bytes of a theme colour, as a tuple.
    fn channels(color: Rgb) -> (u8, u8, u8) {
        (color.r, color.g, color.b)
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
    fn luminance(color: Rgb) -> f64 {
        let (r, g, b) = channels(color);
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    /// WCAG contrast ratio between two colors: 1.0 (identical) to 21.0
    /// (black on white). Text needs 4.5 to pass WCAG AA, and large text or
    /// UI elements need 3.0.
    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        // `f64::max`/`min` pick the brighter and darker of the two, so the
        // order the colors are passed in doesn't matter.
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    /// A color in CIE L*a*b*, a color space where straight-line distance
    /// roughly matches how different two colors *look*. Raw RGB distance
    /// doesn't: two blues can be far apart in RGB yet look nearly identical.
    fn lab(color: Rgb) -> (f64, f64, f64) {
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
    fn delta_e(a: Rgb, b: Rgb) -> f64 {
        let (l1, a1, b1) = lab(a);
        let (l2, a2, b2) = lab(b);
        ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
    }

    /// The picker lists every built-in exactly once, in case-insensitive
    /// alphabetical order by name.
    #[test]
    fn picker_order_is_alphabetical() {
        let order = picker_order();

        // Every theme appears exactly once: same length as the table, and
        // no index repeated (a `HashSet` drops duplicates).
        assert_eq!(order.len(), BUILTIN_THEMES.len());
        let unique: std::collections::HashSet<usize> = order.iter().copied().collect();
        assert_eq!(unique.len(), order.len());

        // Each name is <= the next one, compared in lowercase.
        // `.windows(2)` walks overlapping pairs: (0,1), (1,2), ...
        let names: Vec<String> = order
            .iter()
            .map(|&index| BUILTIN_THEMES[index].name.to_lowercase())
            .collect();
        for pair in names.windows(2) {
            assert!(pair[0] <= pair[1], "{} sorts after {}", pair[0], pair[1]);
        }

        // Spelled out, so a reader sees the actual picker order.
        let display: Vec<&str> = order
            .iter()
            .map(|&index| BUILTIN_THEMES[index].name)
            .collect();
        assert_eq!(
            display,
            [
                "Catppuccin Macchiato",
                "Catppuccin Mocha",
                "Dracula",
                "Everforest",
                "Gruvbox",
                "Nord",
                "Solarized Dark",
                "Tokyo Night",
            ]
        );
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

    /// `Rgb::from_hex_str` accepts with or without `#`, and rejects non-ASCII
    /// input instead of panicking on a mid-character slice.
    #[test]
    fn hex_parsing() {
        let expected = Rgb {
            r: 0x2d,
            g: 0x35,
            b: 0x3b,
        };
        assert_eq!(Rgb::from_hex_str("#2d353b").unwrap(), expected);
        assert_eq!(Rgb::from_hex_str("2d353b").unwrap(), expected);
        assert!(Rgb::from_hex_str("€€").is_err());
        assert!(Rgb::from_hex_str("#zzzzzz").is_err());
        assert_eq!(rgb(0x2d353b), expected);
    }
}
