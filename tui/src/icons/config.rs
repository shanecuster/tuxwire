//! Loading and saving `~/.config/tuxwire/icons.toml`: the icon mode and
//! the user's custom category icons (see `icons.toml.example` for the
//! file format).
//!
//! Reading follows `theme.rs`: a missing file falls back to the bundled
//! default, and a file that exists but is malformed is an error the user
//! sees. Writing follows `fetchers::config::add_source`: it goes through
//! `toml_edit`, so the user's comments and `[categories]` entries are kept
//! when tuxwire changes `mode`.
//!
//! The file's *existence* is also the first-run signal. No `icons.toml`
//! means the "Do you see an icon here?" check hasn't been answered yet
//! ([`LoadedIcons::first_run`]), and answering it writes the file.

use super::{CategoryIcon, IconMode, IconSettings};
use anyhow::{Context, anyhow};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The default `icons.toml`, compiled into the binary with `include_str!`.
/// That macro reads the file at *compile* time and embeds its text as a
/// `&'static str`, the same trick `theme.rs` uses for `theme.toml.example`.
/// It's used as the settings before `icons.toml` exists, and as the
/// starting text the first time tuxwire writes one, so the user gets the
/// fully commented file rather than a bare `mode = "..."` line.
const DEFAULT_ICONS_TOML: &str = include_str!("../../icons.toml.example");

/// What [`load`] returns: the settings to use, plus whether this is the
/// first run (no `icons.toml` yet), which tells the UI to show the
/// "Do you see an icon here?" check.
pub struct LoadedIcons {
    /// The mode and category overrides to start the session with.
    pub settings: IconSettings,
    /// `true` when `icons.toml` doesn't exist yet.
    pub first_run: bool,
}

/// The shape `icons.toml` deserializes into. Every field is optional
/// (`Option` or `#[serde(default)]`), so a file that only sets one thing
/// is valid. `deny_unknown_fields` turns a typo like `mdoe = "ascii"` into
/// a load error that names the bad key, instead of a setting that is
/// silently ignored.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawIcons {
    /// `"nerd"`, `"unicode"`, or `"ascii"`. `None` (key absent) means the
    /// default, Nerd.
    mode: Option<String>,
    /// `[categories]`: topic name → icon name or single-character glyph.
    /// `#[serde(default)]` makes a missing table an empty map instead of an
    /// error.
    #[serde(default)]
    categories: HashMap<String, String>,
}

/// Loads `~/.config/tuxwire/icons.toml`, or the bundled default (with
/// `first_run: true`) if the file doesn't exist.
///
/// Same error policy as `Theme::load`: only "file not found" falls back.
/// Any other read error, and any parse error in a file that *does* exist,
/// is returned with `?` so the user sees it rather than having their
/// settings silently ignored.
pub fn load() -> anyhow::Result<LoadedIcons> {
    let path = icons_path()?;

    // `match` on the read result, so the one expected failure (no file
    // yet) can be handled differently from every other failure.
    let (contents, first_run) = match std::fs::read_to_string(&path) {
        Ok(contents) => (contents, false),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            (DEFAULT_ICONS_TOML.to_string(), true)
        }
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read icons file {}", path.display()));
        }
    };

    let settings = parse(&contents)
        .with_context(|| format!("failed to load icons file {}", path.display()))?;
    Ok(LoadedIcons {
        settings,
        first_run,
    })
}

/// Parses `icons.toml` text into [`IconSettings`]. Separate from [`load`]
/// so tests can feed it a string directly, with no files involved.
///
/// Errors, each naming the offending value: malformed TOML or an unknown
/// key, an unrecognized `mode`, or a `[categories]` value that is neither
/// a single character nor an icon name.
pub fn parse(contents: &str) -> anyhow::Result<IconSettings> {
    // Step 1: TOML text → `RawIcons`. The `?` returns early with the
    // parse error (TOML syntax, wrong type, unknown key) if it fails.
    let raw: RawIcons = toml::from_str(contents).context("invalid icons.toml")?;

    // Step 2: the mode string → `IconMode`. `match` on the `Option` so a
    // missing key means the default, while a present but unknown value is
    // an error. `ok_or_else` turns `parse`'s `None` into an `Err` carrying
    // our message.
    let mode = match raw.mode.as_deref() {
        None => IconMode::default(),
        Some(text) => IconMode::parse(text).ok_or_else(|| {
            anyhow!("mode = {text:?}: expected \"nerd\", \"unicode\", or \"ascii\"")
        })?,
    };

    // Step 3: each `[categories]` value → `CategoryIcon`. `into_iter()`
    // consumes the map so the key `String`s move into the new map without
    // copying. The loop returns early on the first bad entry, so the error
    // can name it.
    let mut overrides = HashMap::new();
    for (category, value) in raw.categories {
        let icon = CategoryIcon::parse(&value).ok_or_else(|| {
            anyhow!(
                "[categories] {category:?} = {value:?}: expected an icon name (like \"security\") \
                 or a single character"
            )
        })?;
        overrides.insert(category, icon);
    }

    Ok(IconSettings::new(mode, overrides))
}

/// Saves `mode` as the `mode = "..."` line in `~/.config/tuxwire/icons.toml`,
/// creating the file (from the bundled, commented default) if it doesn't
/// exist yet. Used by the first-run check and the `i` key.
pub fn save_mode(mode: IconMode) -> anyhow::Result<()> {
    save_mode_at(&icons_path()?, mode)
}

/// The body of [`save_mode`], taking the path as a parameter so tests can
/// point it at a temporary file instead of the real config. Same split as
/// `fetchers::config::add_source` / `add_source_at`.
///
/// Uses `toml_edit::DocumentMut`, a parse that remembers the file's exact
/// formatting. Setting one key and writing it back changes only that line,
/// and every comment and `[categories]` entry stays as the user wrote it.
fn save_mode_at(path: &Path, mode: IconMode) -> anyhow::Result<()> {
    // Start from the existing file, or from the bundled default the first
    // time, so a brand new `icons.toml` comes with all its explanatory
    // comments.
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => DEFAULT_ICONS_TOML.to_string(),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read icons file {}", path.display()));
        }
    };

    let mut doc: toml_edit::DocumentMut = contents
        .parse()
        .with_context(|| format!("failed to parse icons file {} as TOML", path.display()))?;

    // `doc["mode"] = ...` replaces the existing `mode` value in place
    // (keeping its position and trailing comment), or adds the key if the
    // user deleted it. `toml_edit::value` wraps a plain `&str` in the
    // document's own `Item` type.
    doc["mode"] = toml_edit::value(mode.as_str());

    // `~/.config/tuxwire/` might not exist yet on a true first run, before
    // `sources.toml` has been written. `create_dir_all` is a no-op if it
    // already exists.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    std::fs::write(path, doc.to_string())
        .with_context(|| format!("failed to write icons file {}", path.display()))
}

/// The full path to `icons.toml`: `$XDG_CONFIG_HOME/tuxwire/icons.toml`,
/// or `~/.config/tuxwire/icons.toml` when `XDG_CONFIG_HOME` isn't set.
/// Same XDG fallback as `theme.rs`'s `theme_path` (see that function, and
/// `storage::data_dir`, for why there's no `dirs` crate).
fn icons_path() -> anyhow::Result<PathBuf> {
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config_home)
            .join("tuxwire")
            .join("icons.toml"));
    }

    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate the icons config")?;

    Ok(PathBuf::from(home).join(".config/tuxwire/icons.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icons::Icon;

    /// The bundled default parses cleanly and means "Nerd mode, built-in
    /// category icons only". If someone breaks `icons.toml.example`, every
    /// first run would fail to start, so this guards it.
    #[test]
    fn bundled_default_parses() {
        let settings = parse(DEFAULT_ICONS_TOML).unwrap();
        assert_eq!(settings.mode, IconMode::Nerd);
        assert_eq!(
            settings.category_glyph("linux"),
            Icon::Linux.get(IconMode::Nerd)
        );
        assert_eq!(
            settings.category_glyph("homelab"),
            Icon::Category.get(IconMode::Nerd)
        );
    }

    /// An empty file is valid and means all defaults.
    #[test]
    fn empty_file_means_defaults() {
        let settings = parse("").unwrap();
        assert_eq!(settings.mode, IconMode::Nerd);
    }

    /// `mode` and `[categories]` are read, including both kinds of
    /// category value and a TOML `\uXXXX` escape for a Nerd glyph.
    #[test]
    fn parses_mode_and_categories() {
        let settings = parse(
            r#"
            mode = "unicode"
            [categories]
            Homelab = "tech"
            kernel = "\uF17C"
            "#,
        )
        .unwrap();
        assert_eq!(settings.mode, IconMode::Unicode);
        assert_eq!(settings.category_glyph("homelab"), "▣");
        // The Nerd literal can't show in Unicode mode, so `kernel` falls
        // back to its built-in (Linux) icon.
        assert_eq!(settings.category_glyph("kernel"), "λ");
    }

    /// Each kind of bad input is rejected, with a message naming the
    /// problem.
    #[test]
    fn rejects_bad_mode_bad_category_and_unknown_keys() {
        let err = parse(r#"mode = "emoji""#).unwrap_err();
        assert!(format!("{err:#}").contains("emoji"), "{err:#}");

        let err = parse("[categories]\nlinux = \"penguin\"").unwrap_err();
        assert!(format!("{err:#}").contains("penguin"), "{err:#}");

        assert!(parse(r#"mdoe = "ascii""#).is_err());
    }

    /// A per-test temporary path. Same hand-rolled approach as
    /// `fetchers::config`'s `temp_sources_file` (there's no `tempfile`
    /// dependency): the thread ID keeps parallel tests from colliding, and
    /// `tag` keeps different tests on the same thread apart.
    fn temp_icons_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tuxwire-test-icons-{tag}-{:?}",
            std::thread::current().id()
        ));
        // Clear leftovers from an earlier run. Ignoring the result is fine:
        // "nothing to remove" is the usual case.
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("icons.toml")
    }

    /// Saving when no file exists creates it (and its directory) from the
    /// commented default, with the new mode, and it loads back as that mode.
    #[test]
    fn save_mode_creates_file_from_default() {
        let path = temp_icons_path("create");
        save_mode_at(&path, IconMode::Unicode).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("# tuxwire icons"),
            "default comments missing"
        );
        assert_eq!(parse(&written).unwrap().mode, IconMode::Unicode);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Saving into an existing file changes only `mode`, keeping the user's
    /// comments and `[categories]` entries exactly as written.
    #[test]
    fn save_mode_preserves_comments_and_categories() {
        let path = temp_icons_path("preserve");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original =
            "# my own note\nmode = \"nerd\"\n\n[categories]\nhomelab = \"tech\" # server stuff\n";
        std::fs::write(&path, original).unwrap();

        save_mode_at(&path, IconMode::Ascii).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("# my own note"));
        assert!(written.contains("homelab = \"tech\" # server stuff"));
        assert!(written.contains("mode = \"ascii\""));
        let settings = parse(&written).unwrap();
        assert_eq!(settings.mode, IconMode::Ascii);
        assert_eq!(settings.category_glyph("homelab"), "T");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
