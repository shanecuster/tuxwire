//! Loading `theme.toml` (`docs/ARCHITECTURE.md` § Configuration).
//!
//! Not implemented yet. When built, this module will define a `Theme`
//! struct (`#[derive(Deserialize)]`, mirroring the `Article` pattern in
//! `models.rs`) matching the fields in `theme.toml.example`, load it with
//! the `toml` crate, and hand `ratatui::style::Color` values out to the UI
//! layer. Per ARCHITECTURE.md, colors are never to be hardcoded directly
//! in UI code — everything routes through here so the whole palette can
//! be swapped by editing the config file.
