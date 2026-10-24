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
use std::path::{Path, PathBuf};

// `SourceType` and `SourceConfig` now live in the shared `tuxwire-core` crate,
// because the Android app needs to describe sources too (it just reads them
// from a database instead of this TOML file). They are re-exported here with
// `pub use` so the rest of the terminal app can keep saying
// `fetchers::config::SourceType` exactly as before. This file is left with
// the part that really is terminal-specific: finding, reading, validating and
// editing `sources.toml` on disk.
pub use tuxwire_core::fetchers::{SourceConfig, SourceType};

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
            anyhow!("{label} in sources.toml is missing a required 'type' field (expected \"rss\", \"reddit\" or \"file\")")
        })?;
        let url = self
            .url
            .ok_or_else(|| anyhow!("{label} in sources.toml is missing a required 'url' field"))?;

        // A `type = "file"` source's `url` is a path on this computer, not a
        // web address. Turn a leading `~/` into the home folder and refuse
        // anything that is not a full path, so every later step (fetching,
        // remembering which feed an article came from) sees one fixed,
        // unambiguous path. The file does NOT have to exist yet: a missing
        // file is reported when that one source is fetched, so the rest of the
        // app still starts.
        //
        // `std::env::var("HOME").ok()` is the home folder if it is set, else
        // `None` (`.ok()` turns "variable missing" into `None`).
        let url = if source_type == SourceType::File {
            let home = std::env::var("HOME").ok().map(PathBuf::from);
            resolve_file_path(&url, home.as_deref())
                .map_err(|reason| anyhow!("{label} in sources.toml: {reason}"))?
        } else {
            url
        };

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

        Ok(SourceConfig {
            name,
            source_type,
            url,
            topic,
        })
    }
}

/// Turns the `url` of a `type = "file"` source into the full path that will be
/// read, or says why it cannot be used.
///
/// - `~/something` becomes `<home>/something` (a shell habit that Rust does not
///   do for you). `home` is passed in, rather than read here, so the tests can
///   give any home folder without touching the real environment.
/// - A `file://` prefix, which some people paste from a browser's address bar,
///   is dropped.
/// - The result must be a full path (starting at the root of the file system).
///   A relative path such as `index.rss` would mean a different file depending
///   on which folder tuxwire was started from, so it is refused.
///
/// `raw: &str` and `home: Option<&Path>` are borrowed; the answer is a new
/// `String` because it may differ from what was typed. The error is a plain
/// `String` reason that the caller puts after the source's label.
pub fn resolve_file_path(raw: &str, home: Option<&Path>) -> Result<String, String> {
    // Step 1: tidy what was typed and drop a pasted `file://` prefix.
    let trimmed = raw.trim();
    let without_scheme = trimmed.strip_prefix("file://").unwrap_or(trimmed);

    // Step 2: expand a leading `~/` (or a lone `~`).
    let expanded: PathBuf = if let Some(rest) = without_scheme.strip_prefix("~/") {
        let home = home.ok_or("can't expand '~' in the file path: HOME is not set")?;
        home.join(rest)
    } else if without_scheme == "~" {
        home.ok_or("can't expand '~' in the file path: HOME is not set")?
            .to_path_buf()
    } else {
        PathBuf::from(without_scheme)
    };

    // Step 3: it must be a full path.
    if !expanded.is_absolute() {
        return Err(format!(
            "the file path \"{raw}\" is not a full path -- start it with / or ~/ \
             (for example ~/Downloads/index.rss)"
        ));
    }

    // Step 4: hand back text. A path that is not valid text (rare on Linux)
    // cannot be stored in a config file, so it is refused.
    expanded
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| "the file path is not valid text".to_string())
}

/// True when text typed into the "add a source" box is a place on this
/// computer rather than a web address: it starts with `/`, `~/`, `./`, `../`
/// or `file://`. A web address starts with `http`, so the two never collide.
/// Only the start is looked at; whether the file exists is checked later.
pub fn looks_like_file_path(text: &str) -> bool {
    let text = text.trim();
    // `any` is true if at least one prefix in the list matches the text.
    ["/", "~/", "./", "../", "file://"]
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

/// The bundled fallback `sources.toml`, embedded into the compiled binary
/// via `include_str!` -- the same trick `theme.rs` uses for
/// `theme.toml.example`. Unlike the theme fallback (which only ever lives
/// in memory), `load_sources_at` below *writes* this out to
/// `~/.config/tuxwire/sources.toml` the first time it's needed, so a
/// first run leaves behind a real, editable file with a couple of working
/// sources already in it instead of an empty topic sidebar and nothing to
/// edit.
const DEFAULT_SOURCES_TOML: &str = include_str!("../../sources.toml.example");

/// Loads, parses, and validates a sources file at `path`: the usual
/// `sources.toml`, or any feed list's file (see `lists.toml`).
///
/// If the file doesn't exist and `create_if_missing` is `true`, the bundled
/// default (see `DEFAULT_SOURCES_TOML` above) is written to that path *and*
/// used for this call -- so the very first run both shows real articles
/// immediately and leaves a real file behind to edit afterwards. That's
/// only done for the default `~/.config/tuxwire/sources.toml` (see
/// `fetchers::configured_sources_at`): a missing *other* list file is an
/// error, so a typo in `lists.toml` is reported rather than quietly
/// creating a new file full of sample feeds.
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
pub fn load_sources_at(path: &Path, create_if_missing: bool) -> anyhow::Result<Vec<SourceConfig>> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        // The `if create_if_missing` guard means a missing non-default
        // list file falls through to the general error arm below.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && create_if_missing => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create config directory {}", parent.display())
                })?;
            }
            std::fs::write(path, DEFAULT_SOURCES_TOML).with_context(|| {
                format!("failed to write default sources file to {}", path.display())
            })?;
            DEFAULT_SOURCES_TOML.to_string()
        }
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read sources file {}", path.display()));
        }
    };

    let raw: RawSourcesFile = toml::from_str(&contents)
        .with_context(|| format!("failed to parse sources file {}", path.display()))?;

    raw.source
        .into_iter()
        .enumerate()
        .map(|(index, raw_source)| raw_source.validate(index))
        .collect()
}

/// The full path to the user's sources config: `~/.config/tuxwire/sources.toml`,
/// per ARCHITECTURE.md's "Config lives separately at `~/.config/tuxwire/`".
///
/// Mirrors `theme::theme_path`/`paths::data_dir` exactly -- same
/// XDG-fallback pattern (`$XDG_CONFIG_HOME`, falling back to
/// `$HOME/.config`), just one more filename joined onto the same
/// `tuxwire` config directory those two already agree on.
///
/// `pub` so the TUI footer can show which feed list file is in use.
pub fn sources_path() -> anyhow::Result<PathBuf> {
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config_home)
            .join("tuxwire")
            .join("sources.toml"));
    }

    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate the sources config")?;

    Ok(PathBuf::from(home).join(".config/tuxwire/sources.toml"))
}

/// Appends one new `[[source]]` block to the sources file at `path` (the
/// active feed list's file, normally `sources.toml`) -- the write side of
/// the in-app "add a source" flow (`ui`'s `a` keybind, see
/// `docs/ARCHITECTURE.md`'s Adding Sources section). Called only
/// *after* the URL the user typed has already been proven valid by
/// successfully fetching and parsing it (see `fetchers::rss::fetch_feed`),
/// so this function itself does no further validation beyond "these
/// fields aren't empty" -- that's the caller's job.
///
/// Uses `toml_edit` (see the comment on it in `Cargo.toml`) rather than
/// deserializing into `RawSourcesFile`, editing the `Vec`, and
/// re-serializing with plain `toml` -- that round-trip would silently
/// drop every comment already in the file (like the topic-grouping notes
/// a user might leave for themselves), which `toml_edit`'s document model
/// preserves untouched apart from the one block being added.
///
/// There's no separate "reload the in-memory source list" step needed
/// after this returns: `fetchers::configured_sources` always re-reads
/// `sources.toml` from disk on every call (it's never cached), so the
/// very next refresh -- in the TUI, `ui`'s `r` keybind -- already sees
/// whatever this function just wrote.
///
/// Taking `path` explicitly (rather than always using `sources_path()`)
/// is what lets `a` add to whichever feed list is active, and lets the
/// tests below point it at a temporary file instead of the real config.
pub fn add_source_at(
    path: &std::path::Path,
    name: &str,
    source_type: SourceType,
    url: &str,
    topic: &str,
) -> anyhow::Result<()> {
    // One source is just a batch of one: build the `SourceConfig` the batch
    // function wants and hand it over, so there is a single code path that
    // edits the file. (`to_string` turns each `&str` into an owned `String`,
    // because `SourceConfig` owns its text rather than borrowing it.)
    append_sources_at(
        path,
        &[SourceConfig {
            name: name.to_string(),
            source_type,
            url: url.to_string(),
            topic: topic.to_string(),
        }],
    )
}

/// Appends several `[[source]]` blocks to the sources file at `path` in one
/// go: the file is read and parsed once, every block is added, and the file
/// is written once.
///
/// This is what `import-opml` uses. Calling [`add_source_at`] once per feed
/// would re-read and re-write the whole file for every feed, which is slow
/// for an import of hundreds of feeds. Like `add_source_at` it keeps every
/// comment and blank line already in the file (see the comment there for why
/// `toml_edit` is used), and does no validation: the caller has already
/// checked the addresses.
///
/// The file is replaced safely: the new text is first written to a sibling
/// file ending in `.tmp`, then renamed over the real one. A rename is a single
/// step, so a crash or a full disk part-way through leaves the original file
/// untouched instead of half-written.
pub fn append_sources_at(path: &std::path::Path, new: &[SourceConfig]) -> anyhow::Result<()> {
    // Step 1: read the file, or start from nothing if it does not exist yet
    // (a brand-new list). Any other read error (permissions, a folder where
    // a file should be) is a real problem and is passed on with `?`.
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to read sources file {}", path.display()));
        }
    };

    // `DocumentMut` is `toml_edit`'s mutable, formatting-preserving parse
    // of the whole file -- as opposed to `toml::from_str::<RawSourcesFile>`
    // above, which only keeps the *values*, not how they were written.
    let mut doc: toml_edit::DocumentMut = contents
        .parse()
        .with_context(|| format!("failed to parse sources file {} as TOML", path.display()))?;

    // Step 2: make sure there is a `source` list to append to. A missing file
    // or a hand-edited file with the key removed gets an empty one created
    // ("auto-vivified") instead of an error.
    if doc
        .get("source")
        .and_then(|item| item.as_array_of_tables())
        .is_none()
    {
        doc["source"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }

    let sources = doc["source"].as_array_of_tables_mut().ok_or_else(|| {
        anyhow!("sources file's 'source' key exists but isn't an array of [[source]] tables")
    })?;

    // Step 3: add one block per new source. Field order (name, type, url,
    // topic) matches every existing `[[source]]` block in
    // `sources.toml.example` and the real config -- purely cosmetic (TOML
    // doesn't care), but keeps a newly-appended block looking like the rest.
    for source in new {
        let mut table = toml_edit::Table::new();
        table["name"] = toml_edit::value(source.name.as_str());
        table["type"] = toml_edit::value(match source.source_type {
            SourceType::Rss => "rss",
            SourceType::Reddit => "reddit",
            SourceType::File => "file",
        });
        table["url"] = toml_edit::value(source.url.as_str());
        table["topic"] = toml_edit::value(source.topic.as_str());
        sources.push(table);
    }

    // Step 4: make sure the folder exists (a new list may live in a folder
    // that has not been created yet), then replace the file safely.
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create folder {}", parent.display()))?;
    }
    // `doc.to_string()` renders the whole document back out, including
    // every byte of formatting/comments that was there before.
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = std::path::PathBuf::from(temporary);
    std::fs::write(&temporary, doc.to_string())
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("failed to replace sources file {}", path.display()))?;

    Ok(())
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
        let sources: Vec<SourceConfig> = raw
            .source
            .into_iter()
            .enumerate()
            .map(|(i, s)| s.validate(i))
            .collect::<anyhow::Result<_>>()
            .unwrap();

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
        let err = raw
            .source
            .into_iter()
            .next()
            .unwrap()
            .validate(0)
            .unwrap_err();

        let message = err.to_string();
        assert!(
            message.contains("Phoronix"),
            "error should name the source: {message}"
        );
        assert!(
            message.contains("topic"),
            "error should name the missing field: {message}"
        );
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
        assert!(
            result.is_err(),
            "an unrecognized source type must fail to parse"
        );
    }

    /// A temporary `sources.toml`-shaped file, seeded with `contents` --
    /// backs the `add_source_at` tests below. `tempfile::NamedTempFile`
    /// isn't a dependency of this project, so this hand-rolls the same
    /// idea with `std::env::temp_dir()` plus a `#[test]`-unique filename
    /// (via the current thread's ID, since `cargo test` runs tests on
    /// separate threads in parallel) -- good enough for a test that writes
    /// one file and never runs concurrently with itself.
    fn temp_sources_file(contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "tuxwire-test-sources-{:?}.toml",
            std::thread::current().id()
        ));
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// Appending a source preserves every comment and blank line already
    /// in the file, and the new `[[source]]` block round-trips with the
    /// exact fields passed in -- the whole point of using `toml_edit`
    /// over plain `toml` here (see the doc comment on `add_source`).
    #[test]
    fn add_source_preserves_comments_and_appends_block() {
        let original = r#"# a comment a real user left for themselves
# about how they've grouped their topics

[[source]]
name = "Phoronix"
type = "rss"
url = "https://www.phoronix.com/rss.php"
topic = "kernel"
"#;
        let path = temp_sources_file(original);

        add_source_at(
            &path,
            "Hacker News",
            SourceType::Rss,
            "https://hnrss.org/frontpage",
            "tech",
        )
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(
            written.contains("# a comment a real user left for themselves"),
            "existing comments must survive a write: {written}"
        );
        assert!(
            written.contains(r#"name = "Phoronix""#),
            "the original source must still be there: {written}"
        );

        let raw: RawSourcesFile = toml::from_str(&written).unwrap();
        assert_eq!(
            raw.source.len(),
            2,
            "the new source must be appended, not replace the file: {written}"
        );

        let added = &raw.source[1];
        assert_eq!(added.name.as_deref(), Some("Hacker News"));
        assert_eq!(added.source_type, Some(SourceType::Rss));
        assert_eq!(added.url.as_deref(), Some("https://hnrss.org/frontpage"));
        assert_eq!(added.topic.as_deref(), Some("tech"));
    }

    /// Appending to a file with no `[[source]]` blocks at all (an edge
    /// case `load_sources_at`'s own `#[serde(default)]` already treats as
    /// valid, just empty) still produces a well-formed array-of-tables
    /// rather than erroring -- see the `doc.get("source")...is_none()`
    /// guard in `add_source_at`.
    #[test]
    fn add_source_to_file_with_no_existing_sources() {
        let path = temp_sources_file("# empty sources file, nothing configured yet\n");

        add_source_at(
            &path,
            "Phoronix",
            SourceType::Rss,
            "https://www.phoronix.com/rss.php",
            "kernel",
        )
        .unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();

        let raw: RawSourcesFile = toml::from_str(&written).unwrap();
        assert_eq!(raw.source.len(), 1);
        assert_eq!(raw.source[0].name.as_deref(), Some("Phoronix"));
    }

    /// `~/` becomes the home folder, a pasted `file://` prefix is dropped, and
    /// a full path is kept as it is. The home folder is passed in, so this
    /// touches nothing real.
    #[test]
    fn file_paths_are_resolved() {
        let home = Path::new("/home/someone");
        assert_eq!(
            resolve_file_path("~/Downloads/index.rss", Some(home)).unwrap(),
            "/home/someone/Downloads/index.rss"
        );
        assert_eq!(
            resolve_file_path("file:///tmp/index.rss", Some(home)).unwrap(),
            "/tmp/index.rss"
        );
        assert_eq!(
            resolve_file_path("  /srv/feeds/a.xml ", None).unwrap(),
            "/srv/feeds/a.xml"
        );
    }

    /// A relative path, or `~` with no home folder to expand it, is refused
    /// with a reason that says what to do.
    #[test]
    fn bad_file_paths_are_refused() {
        let relative = resolve_file_path("index.rss", Some(Path::new("/home/someone")));
        assert!(relative.unwrap_err().contains("not a full path"));

        let no_home = resolve_file_path("~/index.rss", None);
        assert!(no_home.unwrap_err().contains("HOME is not set"));
    }

    /// A `type = "file"` block loads as a file source, its path unchanged when
    /// it is already a full path; a relative path fails with the source named.
    #[test]
    fn file_sources_load_and_relative_paths_fail() {
        let good = r#"
            [[source]]
            name = "Saved Site"
            type = "file"
            url = "/home/someone/Downloads/index.rss"
            topic = "sports"
        "#;
        let raw: RawSourcesFile = toml::from_str(good).unwrap();
        let source = raw.source.into_iter().next().unwrap().validate(0).unwrap();
        assert_eq!(source.source_type, SourceType::File);
        assert_eq!(source.url, "/home/someone/Downloads/index.rss");

        let bad = r#"
            [[source]]
            name = "Saved Site"
            type = "file"
            url = "index.rss"
            topic = "sports"
        "#;
        let raw: RawSourcesFile = toml::from_str(bad).unwrap();
        let message = raw
            .source
            .into_iter()
            .next()
            .unwrap()
            .validate(0)
            .unwrap_err()
            .to_string();
        assert!(message.contains("Saved Site"), "{message}");
        assert!(message.contains("not a full path"), "{message}");
    }

    /// Adding a file source writes `type = "file"` and the path, and leaves the
    /// comments in the file alone, like any other source.
    #[test]
    fn add_source_writes_type_file() {
        let path = temp_sources_file("# keep me\n");
        add_source_at(
            &path,
            "Saved Site",
            SourceType::File,
            "/home/someone/Downloads/index.rss",
            "sports",
        )
        .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(written.contains("# keep me"), "{written}");
        assert!(written.contains(r#"type = "file""#), "{written}");
        assert!(
            written.contains(r#"url = "/home/someone/Downloads/index.rss""#),
            "{written}"
        );
    }

    /// Local paths are told apart from web addresses by how they start.
    #[test]
    fn paths_are_told_apart_from_web_addresses() {
        for path in [
            "/a/b.rss",
            "~/b.rss",
            "./b.rss",
            "../b.rss",
            "file:///b.rss",
            "  ~/b.rss",
        ] {
            assert!(looks_like_file_path(path), "{path}");
        }
        for web in [
            "https://example.com/feed",
            "example.com",
            "http://x.org/a.rss",
        ] {
            assert!(!looks_like_file_path(web), "{web}");
        }
    }
}
