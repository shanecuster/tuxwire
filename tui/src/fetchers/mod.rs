//! The terminal app's view of the `fetchers` module.
//!
//! The real work -- the `Fetcher` trait, the RSS fetcher, `ConfiguredSource`
//! -- lives in the shared `tuxwire-core` crate. This file re-exports all of
//! it (`pub use ...::*` means "make every public item of that module
//! available here too") so the rest of the terminal app keeps writing
//! `fetchers::ConfiguredSource` and `fetchers::rss::fetch_feed` unchanged,
//! and then adds the one thing that is specific to a terminal: reading the
//! sources from a TOML file on disk (`config`).

pub use tuxwire_core::fetchers::*;

pub mod config;

/// Loads and validates the sources file at `path` (via
/// `config::load_sources_at`) and resolves every entry into a real
/// `ConfiguredSource` ready to `.fetch()`. `path` is the active feed list's
/// file: normally `sources.toml`, or any list from `lists.toml`. This is
/// the single place the app's initial fetch (`main.rs`), the TUI's `r`
/// refresh, and feed-list switching read sources from.
///
/// Only the default `~/.config/tuxwire/sources.toml` is created from the
/// bundled sample when it's missing, exactly as before lists existed. Any
/// other missing list file is an error the caller reports (see
/// `config::load_sources_at`). If `sources_path()` itself can't be worked
/// out (no `$HOME`), nothing is treated as the default, so nothing is
/// created.
///
/// Returns `anyhow::Result` because loading a config file genuinely can
/// fail: the file might not parse as TOML, or a source in it might be
/// missing its required `topic`. Every caller already propagates this with
/// `?`, same as any other fallible step.
pub fn configured_sources_at(path: &std::path::Path) -> anyhow::Result<Vec<ConfiguredSource>> {
    // `is_ok_and` is `true` only when `sources_path()` worked *and* the
    // closure says it's the same path.
    let is_default = config::sources_path().is_ok_and(|default| default == path);

    // Read and validate the TOML file into plain `SourceConfig` values...
    let sources = config::load_sources_at(path, is_default)?;

    // ...then let the shared core turn those into ready-to-run fetchers.
    Ok(configured_sources(sources))
}
