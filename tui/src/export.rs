//! Markdown export for saved articles (`docs/ARCHITECTURE.md` § Markdown
//! Export).
//!
//! Loads `~/.config/tuxwire/export.toml` (writing out a bundled default the
//! first time it's needed, same pattern `fetchers::config::load_sources` and
//! `theme::Theme::load` already use) and writes every saved article into one
//! combined Markdown file under whatever directory that config points at --
//! `~/tuxwire-notes/` out of the box. `ui/mod.rs`'s `E` keybind (Saved view
//! only) is the one caller of `export_saved_articles`; this module knows
//! nothing about the TUI itself.

use anyhow::Context;
use std::path::PathBuf;
use tuxwire_core::export::{ExportOptions, render_saved_markdown};
use tuxwire_core::models::Article;

/// The resolved export destination -- a real, existing directory on disk,
/// `~` already expanded to `$HOME`. Kept as its own type (rather than
/// callers just passing a bare `PathBuf` around) so `ExportConfig::load`
/// is the *only* place that has to know about `export.toml`'s shape, its
/// default, or the `~` expansion -- everything downstream just gets a
/// directory it can write into.
pub struct ExportConfig {
    /// The directory the export file is written into.
    pub path: PathBuf,
    /// Whether saved articles that have no note are listed too (`true`, the
    /// default) or only the articles that have a note (`false`). Read from
    /// `include_unnoted` in `export.toml`.
    pub include_unnoted: bool,
}

/// The shape `export.toml` deserializes into -- one table, one key,
/// matching `docs/ARCHITECTURE.md`'s
/// ```toml
/// [export]
/// path = "~/tuxwire-notes/"
/// ```
/// exactly. `path` is a plain `String` here (not yet a `PathBuf`) because
/// it can contain a leading `~`, which isn't something a `PathBuf` or the
/// filesystem itself understands -- that expansion happens by hand in
/// `expand_tilde`, once, at load time, the same reason `theme.rs` converts
/// hex strings to `Color` at load time rather than leaving every call site
/// to do it.
#[derive(serde::Deserialize)]
struct RawExportFile {
    export: RawExport,
}

#[derive(serde::Deserialize)]
struct RawExport {
    path: String,
    /// Optional, so an `export.toml` written before this setting existed
    /// still loads. `serde(default = "...")` calls the named function to get
    /// the value when the key is missing.
    #[serde(default = "include_unnoted_by_default")]
    include_unnoted: bool,
}

/// The value used when `export.toml` does not say: export everything saved.
fn include_unnoted_by_default() -> bool {
    true
}

/// The bundled fallback `export.toml`, embedded into the compiled binary
/// via `include_str!` -- the same trick `theme.rs`/`fetchers::config.rs`
/// use for their own `.toml.example` files. `ExportConfig::load` writes
/// this out to `~/.config/tuxwire/export.toml` the first time an export is
/// requested and finds no config there yet, so the very first `E` press
/// both works immediately (writing into the shipped default,
/// `~/tuxwire-notes/`) and leaves a real, editable file behind.
const DEFAULT_EXPORT_TOML: &str = include_str!("../export.toml.example");

impl ExportConfig {
    /// Loads `~/.config/tuxwire/export.toml`, writing out the bundled
    /// default first if it doesn't exist yet (see `DEFAULT_EXPORT_TOML`
    /// above), then expands `~` in the configured path and creates that
    /// directory if it isn't there already -- per ARCHITECTURE.md's
    /// "Create the export directory if it doesn't exist," so
    /// `export_saved_articles` never has to think about a missing
    /// directory itself, only about writing into one that's guaranteed to
    /// exist by the time it runs.
    ///
    /// Any I/O error besides "the config file simply isn't there yet" (bad
    /// permissions, a malformed `export.toml`, a `path` that can't be
    /// created as a directory -- e.g. a plain file already sitting at that
    /// path) is propagated with `?` rather than silently falling back,
    /// matching `Theme::load`/`load_sources`'s own reasoning: a real
    /// mistake in the user's config should surface as an error, not be
    /// swallowed by the same fallback that covers "no config yet."
    pub fn load() -> anyhow::Result<ExportConfig> {
        let path = export_config_path()?;

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("failed to create config directory {}", parent.display())
                    })?;
                }
                std::fs::write(&path, DEFAULT_EXPORT_TOML).with_context(|| {
                    format!("failed to write default export file to {}", path.display())
                })?;
                DEFAULT_EXPORT_TOML.to_string()
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to read export file {}", path.display()));
            }
        };

        let raw: RawExportFile = toml::from_str(&contents)
            .with_context(|| format!("failed to parse export file {}", path.display()))?;

        let export_dir = expand_tilde(&raw.export.path)?;
        std::fs::create_dir_all(&export_dir).with_context(|| {
            format!("failed to create export directory {}", export_dir.display())
        })?;

        Ok(ExportConfig {
            path: export_dir,
            include_unnoted: raw.export.include_unnoted,
        })
    }
}

/// Expands a leading `~` (or `~/...`) to `$HOME`, the one piece of shell
/// behavior a config file doesn't get for free -- `PathBuf::from("~/x")`
/// treats `~` as a perfectly ordinary directory named `~`, since path
/// expansion is a shell feature, not something the filesystem or Rust's
/// standard library does on your behalf. Anything not starting with `~` is
/// passed through unchanged (an absolute path, or a relative one resolved
/// against tuxwire's current working directory).
fn expand_tilde(raw: &str) -> anyhow::Result<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var("HOME").context(
            "HOME environment variable is not set -- can't expand '~' in export.toml's path",
        )?;
        return Ok(PathBuf::from(home).join(rest));
    }

    if raw == "~" {
        let home = std::env::var("HOME").context(
            "HOME environment variable is not set -- can't expand '~' in export.toml's path",
        )?;
        return Ok(PathBuf::from(home));
    }

    Ok(PathBuf::from(raw))
}

/// The full path to the user's export config: `~/.config/tuxwire/export.toml`,
/// per ARCHITECTURE.md's "Config lives separately at `~/.config/tuxwire/`."
/// Mirrors `theme::theme_path`/`fetchers::config::sources_path` exactly --
/// same XDG-fallback pattern, one more filename joined onto the same
/// `tuxwire` config directory those two already agree on.
fn export_config_path() -> anyhow::Result<PathBuf> {
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config_home)
            .join("tuxwire")
            .join("export.toml"));
    }

    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate the export config")?;

    Ok(PathBuf::from(home).join(".config/tuxwire/export.toml"))
}

/// The combined export's fixed filename -- unlike an earlier one-file-per-
/// article design, there's no title to derive a name from anymore (or
/// need to sanitize one for filesystem safety), since every saved article
/// lands in this same file under `ExportConfig::path`.
const EXPORT_FILENAME: &str = "saved-articles.md";

/// The file `E` writes: the fixed filename inside the configured folder.
/// Public so the UI can check whether it already exists and ask before
/// replacing it.
pub fn export_file_path(config: &ExportConfig) -> PathBuf {
    config.path.join(EXPORT_FILENAME)
}

/// The `E` keybind's one job: write every saved article into a single
/// `saved-articles.md` under `config.path`, and hand back the path written so
/// `ui/mod.rs` can show it back to the user as confirmation.
///
/// The text itself comes from `tuxwire_core::export::render_saved_markdown`,
/// the same function the Android app uses, so both apps write the same file:
/// saved articles without a note first, then the notes. This function only
/// adds the terminal-specific part, which is where the file goes (from
/// `export.toml`) and whether articles without a note are included.
///
/// **Regenerates the file from scratch every time** rather than appending
/// -- `std::fs::write` truncates and replaces whatever was there, same as
/// every other write in this module. This is deliberate, not an
/// oversight: `articles` (via `Storage::saved_articles`) is already the
/// complete, current list of every saved article, so re-deriving the
/// whole file from that each run is simpler than tracking "what's new
/// since the last export" and can never drift out of sync with an
/// article being un-saved, re-noted, or removed in the meantime the way
/// an append-only log could.
pub fn export_saved_articles(
    config: &ExportConfig,
    articles: &[Article],
) -> anyhow::Result<PathBuf> {
    // Step 1: build the text with the shared formatter, passing along the
    // person's choice about articles that have no note.
    let options = ExportOptions {
        include_unnoted: config.include_unnoted,
    };
    let text = render_saved_markdown(articles, &options);

    // Step 2: write it (replacing any earlier export) and report where.
    let path = export_file_path(config);
    std::fs::write(&path, text).with_context(|| format!("failed to write {}", path.display()))?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    /// A saved article with a note. (How one article is formatted is tested
    /// in `tuxwire_core::export`; the tests here are about the file the
    /// terminal app writes.)
    fn saved_article() -> Article {
        let mut article = Article::new(
            "Btrfs send/receive got noticeably faster in 6.18".to_string(),
            "https://reddit.com/r/linux/comments/example".to_string(),
            "r/linux".to_string(),
            "kernel".to_string(),
            Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap(),
        );
        article.saved = true;
        article.saved_at = Some(Utc.with_ymd_and_hms(2026, 8, 12, 9, 0, 0).unwrap());
        article.note = Some("worth trying on the homelab NAS".to_string());
        article.noted_at = Some(Utc.with_ymd_and_hms(2026, 8, 14, 20, 0, 0).unwrap());
        article
    }

    /// A saved article with no note.
    fn plain_article() -> Article {
        let mut article = saved_article();
        article.title = "Just saved, no note".to_string();
        article.note = None;
        article.noted_at = None;
        article
    }

    /// The file holds the shared two-section text: saved articles without a
    /// note first, then the notes, and it lands in `saved-articles.md`.
    #[test]
    fn export_writes_both_sections_to_the_one_file() {
        let dir = tempdir();
        let config = ExportConfig {
            path: dir.clone(),
            include_unnoted: true,
        };

        let path = export_saved_articles(&config, &[saved_article(), plain_article()]).unwrap();
        assert_eq!(path, dir.join(EXPORT_FILENAME));

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.find("## Saved articles (1)").unwrap() < text.find("## Notes (1)").unwrap());
        assert!(text.contains("Just saved, no note"));
        assert!(text.contains("> worth trying on the homelab NAS"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `export_file_path` names exactly the file `export_saved_articles`
    /// writes, so the "file already exists, replace it?" check in the UI
    /// looks at the right place: it is missing before the first export and
    /// present afterwards.
    #[test]
    fn export_file_path_is_the_file_that_gets_written() {
        let dir = tempdir();
        let config = ExportConfig {
            path: dir.clone(),
            include_unnoted: true,
        };

        // Before any export there is nothing to replace.
        assert!(!export_file_path(&config).exists());

        let written = export_saved_articles(&config, &[saved_article()]).unwrap();
        assert_eq!(written, export_file_path(&config));
        assert!(export_file_path(&config).exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    /// With `include_unnoted` off, articles without a note are left out.
    #[test]
    fn export_can_leave_out_articles_without_notes() {
        let dir = tempdir();
        let config = ExportConfig {
            path: dir.clone(),
            include_unnoted: false,
        };

        let path = export_saved_articles(&config, &[saved_article(), plain_article()]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("## Notes (1)"));
        assert!(!text.contains("Just saved, no note"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Running the export again with a *different* set of saved articles
    /// must fully replace the previous file's contents, not append to it
    /// -- "regenerating the file fresh each time it's run" is the whole
    /// point of writing from `Storage::saved_articles`'s current snapshot
    /// rather than tracking a delta.
    #[test]
    fn export_saved_articles_regenerates_rather_than_appending() {
        let dir = tempdir();
        let config = ExportConfig {
            path: dir.clone(),
            include_unnoted: true,
        };

        let mut first = saved_article();
        first.title = "Old article, later un-saved".to_string();
        export_saved_articles(&config, &[first]).unwrap();

        let mut second = saved_article();
        second.title = "Only article still saved".to_string();
        let path = export_saved_articles(&config, &[second]).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("Only article still saved"));
        assert!(!contents.contains("Old article, later un-saved"));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An empty saved list still regenerates the file (to an empty
    /// string) rather than erroring or leaving a stale file with articles
    /// that are no longer saved sitting in it.
    #[test]
    fn export_saved_articles_with_none_saved_writes_an_empty_file() {
        let dir = tempdir();
        let config = ExportConfig {
            path: dir.clone(),
            include_unnoted: true,
        };

        export_saved_articles(&config, &[saved_article()]).unwrap();
        let path = export_saved_articles(&config, &[]).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An `export.toml` from before the setting existed has no
    /// `include_unnoted` key, and must still load, exporting everything.
    /// The key, when present, is respected.
    #[test]
    fn include_unnoted_defaults_to_on_and_can_be_turned_off() {
        let old: RawExportFile = toml::from_str("[export]\npath = \"~/notes/\"\n").unwrap();
        assert!(old.export.include_unnoted);

        let off: RawExportFile =
            toml::from_str("[export]\npath = \"~/notes/\"\ninclude_unnoted = false\n").unwrap();
        assert!(!off.export.include_unnoted);
    }

    /// A fresh temp directory under the OS temp dir, unique per call (PID +
    /// a `static` counter) so tests running in parallel never collide on
    /// the same path -- `cfg(test)`-only, no bearing on the real binary.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);

        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("tuxwire-export-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
