//! Loading and validating `sources.toml` (`docs/ARCHITECTURE.md` §
//! Configuration → `sources.toml`).
//!
//! This module is deliberately the *only* place that knows `sources.toml`
//! exists as a file on disk with a particular TOML shape. Everything else
//! (`fetchers::configured_sources`, `main.rs`, the TUI's `r` refresh) only
//! ever sees the validated `SourceConfig` values this produces -- the same
//! "normalize/validate at the boundary" idea `theme.rs` uses for
//! `theme.toml` and `models.rs` describes for `Article`.

use anyhow::{Context, anyhow};
use serde::Deserialize;
use std::path::PathBuf;

/// Which concrete fetcher a `[[source]]` entry's `type = "..."` names.
///
/// Per ARCHITECTURE.md's own `sources.toml` example, `"rss"` and
/// `"reddit"` are both valid values a user can type today -- but only RSS
/// actually fetches anything yet (see `fetchers::reddit`). Modeling this as
/// an `enum` rather than leaving `source_type` a bare `String` means an
/// entry like `type = "atom-ish-typo"` is caught right here, at config
/// *parse* time, with serde's own "unknown variant" message naming the bad
/// value -- instead of surviving as a `String` that every later piece of
/// code would have to re-validate (or silently mishandle) on its own.
///
/// `#[serde(rename_all = "lowercase")]` is what makes `Rss`/`Reddit` (the
/// idiomatic Rust `PascalCase` for enum variants) match the lowercase
/// `"rss"`/`"reddit"` strings actually written in the TOML file, without
/// needing a `#[serde(rename = "...")]` on each variant individually.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceType {
    Rss,
    Reddit,
}

/// One fully-validated `[[source]]` entry: every field guaranteed present
/// and non-empty by the time anything outside this module sees it.
///
/// This is a *different* type from `RawSource` below for the same reason
/// `theme.rs`'s `Theme` is a different type from its `RawTheme` -- `serde`
/// deserializes into a shape that can represent "this field was missing"
/// (via `Option`), and `validate` below is what turns that into either a
/// clear error or a type where every field is simply, unconditionally
/// there, so no caller downstream ever has to think about a source with a
/// missing `topic` again.
#[derive(Debug, Clone)]
pub struct SourceConfig {
    pub name: String,
    pub source_type: SourceType,
    pub url: String,
    pub topic: String,
}

/// The raw shape `sources.toml` deserializes into: one `Vec<RawSource>`
/// per repeated `[[source]]` block -- TOML's syntax for "an array of
/// tables," which serde/the `toml` crate maps onto a `Vec` field named
/// after the singular table name (`source`, not `sources`) automatically.
#[derive(Deserialize)]
struct RawSourcesFile {
    /// `#[serde(default)]` means a `sources.toml` with no `[[source]]`
    /// blocks at all deserializes to an empty `Vec` instead of an error --
    /// an empty sources file is unusual but not malformed the way a
    /// missing `topic` on an entry that *does* exist is.
    #[serde(default)]
    source: Vec<RawSource>,
}

/// One `[[source]]` block, before validation -- every field is `Option`al
/// here specifically so a *missing* field is something this module can
/// detect and report clearly (see `RawSource::validate` below), rather
/// than something serde itself rejects with a generic "missing field"
/// error that doesn't say which of possibly many `[[source]]` blocks (or
/// why) is at fault.
#[derive(Deserialize)]
struct RawSource {
    name: Option<String>,
    /// TOML's `type = "rss"` is renamed to `source_type` on this struct
    /// because `type` is a reserved keyword in Rust -- it can't be used as
    /// a field name as-is. `#[serde(rename = "type")]` is what tells serde
    /// "read the `type` key from the file, but call it `source_type` in
    /// Rust."
    #[serde(rename = "type")]
    source_type: Option<SourceType>,
    url: Option<String>,
    topic: Option<String>,
}

impl RawSource {
    /// Turns one raw, possibly-incomplete `[[source]]` block into a real
    /// `SourceConfig`, or a clear `Err` naming exactly which source (by
    /// its 1-based position in the file, and its name if it has one) and
    /// which field is missing.
    ///
    /// `index` is the block's position within `sources.toml`'s
    /// `[[source]]` array (0-based, as `Vec`/`enumerate` naturally give
    /// it) -- used only to build a human-readable "source #N" label when
    /// `name` itself might be the very field that's missing.
    fn validate(self, index: usize) -> anyhow::Result<SourceConfig> {
        // Built before `self.name` is consumed by the `ok_or_else` below
        // -- once that runs, `self.name` is moved out of `self` and can no
        // longer be borrowed here.
        let label = match &self.name {
            Some(name) => format!("source #{} (\"{name}\")", index + 1),
            None => format!("source #{}", index + 1),
        };

        let name = self
            .name
            .ok_or_else(|| anyhow!("{label} in sources.toml is missing a required 'name' field"))?;
        let source_type = self.source_type.ok_or_else(|| {
            anyhow!("{label} in sources.toml is missing a required 'type' field (expected \"rss\" or \"reddit\")")
        })?;
        let url = self
            .url
            .ok_or_else(|| anyhow!("{label} in sources.toml is missing a required 'url' field"))?;

        // `topic` gets the most detailed message of the four: it's the
        // one field ARCHITECTURE.md calls out by name as a locked,
        // deliberate design decision ("every source is assigned exactly
        // one topic -- not a list"), so a source missing it is more likely
        // a genuine oversight worth explaining, not just a typo'd key.
        let topic = self.topic.ok_or_else(|| {
            anyhow!(
                "{label} in sources.toml is missing a required 'topic' field -- every \
                 source needs exactly one topic (see docs/ARCHITECTURE.md § sources.toml). \
                 Add e.g. topic = \"linux\" to this [[source]] block."
            )
        })?;

        Ok(SourceConfig { name, source_type, url, topic })
    }
}

/// The bundled fallback `sources.toml`, embedded into the compiled binary
/// via `include_str!` -- the same trick `theme.rs` uses for
/// `theme.toml.example`. Unlike the theme fallback (which only ever lives
/// in memory), `load_sources` below *writes* this out to
/// `~/.config/tuxwire/sources.toml` the first time it's needed, so a
/// first run leaves behind a real, editable file with a couple of working
/// sources already in it instead of an empty topic sidebar and nothing to
/// edit.
const DEFAULT_SOURCES_TOML: &str = include_str!("../../sources.toml.example");

/// Loads, parses, and validates `sources.toml`.
///
/// If `~/.config/tuxwire/sources.toml` doesn't exist yet, the bundled
/// default (see `DEFAULT_SOURCES_TOML` above) is written to that path
/// *and* used for this call -- so the very first run both shows real
/// articles immediately and leaves a real file behind to edit afterwards.
/// Any other I/O error (permissions, a directory where a file was
/// expected, ...) is propagated with `?` rather than silently falling
/// back, matching `Theme::load`'s reasoning: only "the file simply isn't
/// there yet" is expected, everything else is a real problem the user
/// needs to see.
///
/// A `sources.toml` that *does* exist but is malformed (bad TOML syntax,
/// an unknown `type`, a source missing its `topic`) is always an error --
/// see `RawSource::validate` above for why silently defaulting a missing
/// `topic` would undermine the "one topic per source" guarantee the whole
/// rest of the app relies on.
pub fn load_sources() -> anyhow::Result<Vec<SourceConfig>> {
    let path = sources_path()?;

    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create config directory {}", parent.display()))?;
            }
            std::fs::write(&path, DEFAULT_SOURCES_TOML)
                .with_context(|| format!("failed to write default sources file to {}", path.display()))?;
            DEFAULT_SOURCES_TOML.to_string()
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read sources file {}", path.display()));
        }
    };

    let raw: RawSourcesFile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse sources file {}", path.display()))?;

    raw.source.into_iter().enumerate().map(|(index, raw_source)| raw_source.validate(index)).collect()
}

/// The full path to the user's sources config: `~/.config/tuxwire/sources.toml`,
/// per ARCHITECTURE.md's "Config lives separately at `~/.config/tuxwire/`".
///
/// Mirrors `theme::theme_path`/`storage::data_dir` exactly -- same
/// XDG-fallback pattern (`$XDG_CONFIG_HOME`, falling back to
/// `$HOME/.config`), just one more filename joined onto the same
/// `tuxwire` config directory those two already agree on.
fn sources_path() -> anyhow::Result<PathBuf> {
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config_home).join("tuxwire").join("sources.toml"));
    }

    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate the sources config")?;

    Ok(PathBuf::from(home).join(".config/tuxwire/sources.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A well-formed file with both a working (`rss`) and a
    /// not-yet-implemented (`reddit`) source parses cleanly -- config
    /// *loading* never rejects a `reddit` entry; only trying to `.fetch()`
    /// one does (see `fetchers::reddit`).
    #[test]
    fn parses_valid_sources() {
        let toml = r#"
            [[source]]
            name = "Phoronix"
            type = "rss"
            url = "https://www.phoronix.com/rss.php"
            topic = "kernel"

            [[source]]
            name = "r/linux"
            type = "reddit"
            url = "linux"
            topic = "distros"
        "#;

        let raw: RawSourcesFile = toml::from_str(toml).unwrap();
        let sources: Vec<SourceConfig> =
            raw.source.into_iter().enumerate().map(|(i, s)| s.validate(i)).collect::<anyhow::Result<_>>().unwrap();

        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].source_type, SourceType::Rss);
        assert_eq!(sources[1].source_type, SourceType::Reddit);
    }

    /// A source missing `topic` fails with a message naming both the
    /// source and the missing field -- this is the exact scenario
    /// ARCHITECTURE.md's "one topic per source" rule depends on catching,
    /// rather than silently defaulting.
    #[test]
    fn missing_topic_fails_with_clear_message() {
        let toml = r#"
            [[source]]
            name = "Phoronix"
            type = "rss"
            url = "https://www.phoronix.com/rss.php"
        "#;

        let raw: RawSourcesFile = toml::from_str(toml).unwrap();
        let err = raw.source.into_iter().next().unwrap().validate(0).unwrap_err();

        let message = err.to_string();
        assert!(message.contains("Phoronix"), "error should name the source: {message}");
        assert!(message.contains("topic"), "error should name the missing field: {message}");
    }

    /// An unknown `type` value is rejected at parse time (serde's own
    /// "unknown variant" error), not silently accepted or defaulted to
    /// `rss`.
    #[test]
    fn unknown_source_type_fails() {
        let toml = r#"
            [[source]]
            name = "Mystery Source"
            type = "carrier-pigeon"
            url = "https://example.com/feed"
            topic = "linux"
        "#;

        let result: Result<RawSourcesFile, _> = toml::from_str(toml);
        assert!(result.is_err(), "an unrecognized source type must fail to parse");
    }
}
