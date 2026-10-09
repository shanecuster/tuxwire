//! The terminal app's view of the `icons` module.
//!
//! The icon table itself (`Icon`, `IconMode`, `IconSettings`) lives in the
//! shared `tuxwire-core` crate, because the Android app uses the same table.
//! It is re-exported here so the rest of the terminal app keeps writing
//! `icons::Icon` unchanged. What stays in this crate is `config`, which reads
//! and writes `~/.config/tuxwire/icons.toml` -- a desktop-only concern.

pub use tuxwire_core::icons::*;

pub mod config;

// `#[cfg(test)]`: compiled only for `cargo test`.
#[cfg(test)]
mod tests {
    use super::*;
    // Used only here, to measure glyphs the same way ratatui does when it
    // lays out a line. This is why this test lives in the terminal crate
    // rather than next to the icon table: the shared core crate must stay
    // free of ratatui, even in its tests.
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
}
