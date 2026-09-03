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

use crate::models::Article;
use anyhow::Context;
use std::path::PathBuf;

/// The resolved export destination -- a real, existing directory on disk,
/// `~` already expanded to `$HOME`. Kept as its own type (rather than
/// callers just passing a bare `PathBuf` around) so `ExportConfig::load`
/// is the *only* place that has to know about `export.toml`'s shape, its
/// default, or the `~` expansion -- everything downstream just gets a
/// directory it can write into.
pub struct ExportConfig {
    pub path: PathBuf,
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
                    std::fs::create_dir_all(parent)
                        .with_context(|| format!("failed to create config directory {}", parent.display()))?;
                }
                std::fs::write(&path, DEFAULT_EXPORT_TOML)
                    .with_context(|| format!("failed to write default export file to {}", path.display()))?;
                DEFAULT_EXPORT_TOML.to_string()
            }
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read export file {}", path.display()));
            }
        };

        let raw: RawExportFile = toml::from_str(&contents)
            .with_context(|| format!("failed to parse export file {}", path.display()))?;

        let export_dir = expand_tilde(&raw.export.path)?;
        std::fs::create_dir_all(&export_dir)
            .with_context(|| format!("failed to create export directory {}", export_dir.display()))?;

        Ok(ExportConfig { path: export_dir })
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
        let home = std::env::var("HOME")
            .context("HOME environment variable is not set -- can't expand '~' in export.toml's path")?;
        return Ok(PathBuf::from(home).join(rest));
    }

    if raw == "~" {
        let home = std::env::var("HOME")
            .context("HOME environment variable is not set -- can't expand '~' in export.toml's path")?;
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
        return Ok(PathBuf::from(xdg_config_home).join("tuxwire").join("export.toml"));
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

/// Formats one saved article as the Markdown block `docs/ARCHITECTURE.md`
/// specifies:
/// ```markdown
/// ## <title>
/// Source: [<source name>](<url>) — saved <saved_at date>, noted <noted_at date>
///
/// > <note>
/// ```
/// `saved_at` falls back to the article's publish `timestamp` for a saved
/// article that predates migration 003 (tuxwire didn't track `saved_at`
/// before then) -- the exact same fallback `ui/mod.rs`'s `saved_meta_line`
/// already uses for the Saved view's own on-screen date, so the exported
/// file and the screen it was exported from never disagree about which
/// date "saved" means. `noted_at` is only appended when it's a genuinely
/// different day than `saved_at` -- a note added the same day it was saved
/// (the common case) would otherwise show a redundant "noted 2026-08-12"
/// right next to "saved 2026-08-12."
///
/// The note itself is only included when there is one -- an empty `>`
/// blockquote for a saved-without-a-note article would just be visual
/// noise in the exported file. A multi-line note gets `> ` prefixed onto
/// *every* line (`str::lines`), not just the first, since Markdown's
/// blockquote syntax only continues a quote across a line break if each
/// line carries its own `>` marker.
pub fn format_article_markdown(article: &Article) -> String {
    let saved_day = article
        .saved_at
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| article.timestamp.format("%Y-%m-%d").to_string());

    let noted_day = article.noted_at.map(|d| d.format("%Y-%m-%d").to_string());

    let mut header = format!("Source: [{}]({}) — saved {saved_day}", article.source, article.url);
    if let Some(noted_day) = noted_day.filter(|day| *day != saved_day) {
        header.push_str(&format!(", noted {noted_day}"));
    }

    let mut out = format!("## {}\n{header}\n", article.title);

    if let Some(note) = article.note.as_deref().filter(|note| !note.trim().is_empty()) {
        let quoted: String = note.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n");
        out.push('\n');
        out.push_str(&quoted);
        out.push('\n');
    }

    out
}

/// The `E` keybind's one job: write every saved article into a single
/// `saved-articles.md` under `config.path`, each article's own block (see
/// `format_article_markdown`) separated from the next by a Markdown
/// thematic break (`---` on its own line), and hand back the path written
/// so `ui/mod.rs` can show it back to the user as confirmation.
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
pub fn export_saved_articles(config: &ExportConfig, articles: &[Article]) -> anyhow::Result<PathBuf> {
    let combined = articles.iter().map(format_article_markdown).collect::<Vec<_>>().join("\n---\n");

    let path = config.path.join(EXPORT_FILENAME);
    std::fs::write(&path, combined).with_context(|| format!("failed to write {}", path.display()))?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    /// A minimal saved article, note included -- enough for
    /// `format_article_markdown` to have every field it looks at.
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

    /// The exact shape ARCHITECTURE.md specifies: a `##` title line, the
    /// source/link/date line, and the note as a blockquote -- with
    /// `noted_at` shown because it's a different day than `saved_at`.
    #[test]
    fn formats_the_documented_markdown_shape() {
        let markdown = format_article_markdown(&saved_article());

        assert_eq!(
            markdown,
            "## Btrfs send/receive got noticeably faster in 6.18\n\
             Source: [r/linux](https://reddit.com/r/linux/comments/example) — saved 2026-08-12, noted 2026-08-14\n\
             \n\
             > worth trying on the homelab NAS\n"
        );
    }

    /// `noted_at` on the *same* day as `saved_at` is the common case (a
    /// note written at save time) and would be redundant to print twice --
    /// only a genuinely different day should show.
    #[test]
    fn omits_noted_date_when_it_matches_saved_date() {
        let mut article = saved_article();
        article.noted_at = article.saved_at;

        let markdown = format_article_markdown(&article);

        assert!(markdown.contains("saved 2026-08-12"));
        assert!(!markdown.contains("noted"));
    }

    /// A saved article with no note at all shouldn't render an empty `>`
    /// blockquote -- there's nothing to quote.
    #[test]
    fn omits_the_blockquote_when_there_is_no_note() {
        let mut article = saved_article();
        article.note = None;
        article.noted_at = None;

        let markdown = format_article_markdown(&article);

        assert!(!markdown.contains('>'));
    }

    /// A saved article from before migration 003 has no `saved_at` at all
    /// -- falls back to the publish `timestamp`, same as `saved_meta_line`
    /// in `ui/mod.rs`.
    #[test]
    fn falls_back_to_publish_timestamp_when_saved_at_is_missing() {
        let mut article = saved_article();
        article.saved_at = None;

        let markdown = format_article_markdown(&article);

        assert!(markdown.contains("saved 2026-08-10"));
    }

    /// A multi-line note needs `> ` on every line for Markdown to render it
    /// as one continuous blockquote rather than a blockquote followed by
    /// plain paragraphs.
    #[test]
    fn quotes_every_line_of_a_multiline_note() {
        let mut article = saved_article();
        article.note = Some("first line\nsecond line".to_string());

        let markdown = format_article_markdown(&article);

        assert!(markdown.contains("> first line\n> second line"));
    }

    /// Every saved article lands in the *one* combined file, each block
    /// separated from the next by a `---` thematic break on its own line
    /// -- not concatenated directly against each other, which would run
    /// one article's text straight into the next `##` heading.
    #[test]
    fn export_saved_articles_joins_every_article_with_a_separator() {
        let dir = tempdir();
        let config = ExportConfig { path: dir.clone() };

        let mut first = saved_article();
        first.title = "First article".to_string();
        let mut second = saved_article();
        second.title = "Second article".to_string();

        let expected = format!("{}\n---\n{}", format_article_markdown(&first), format_article_markdown(&second));

        let path = export_saved_articles(&config, &[first, second]).unwrap();
        assert_eq!(path, dir.join(EXPORT_FILENAME));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), expected);

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
        let config = ExportConfig { path: dir.clone() };

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
        let config = ExportConfig { path: dir.clone() };

        export_saved_articles(&config, &[saved_article()]).unwrap();
        let path = export_saved_articles(&config, &[]).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A fresh temp directory under the OS temp dir, unique per call (PID +
    /// a `static` counter) so tests running in parallel never collide on
    /// the same path -- `cfg(test)`-only, no bearing on the real binary.
    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);

        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tuxwire-export-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
