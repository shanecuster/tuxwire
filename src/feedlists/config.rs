//! Loading and saving `~/.config/tuxwire/lists.toml`, which defines the
//! feed lists (see `lists.toml.example` for the file format):
//!
//! ```toml
//! active = "main"                        # written by the `f` picker
//!
//! [[list]]
//! name = "main"
//! path = "~/.config/tuxwire/sources.toml"
//!
//! [[list]]
//! name = "casual"
//! path = "casual.toml"                   # relative to this file's folder
//! ```
//!
//! No `lists.toml`, or one with no `[[list]]` entries, means the single
//! implicit list ([`FeedLists::single`]), which is exactly the old
//! behavior. A file that exists but is malformed (bad TOML, an unknown key,
//! a duplicate or empty name, an empty path) is an error, the same policy
//! as `theme.toml` and `icons.toml`. Lists are added by editing this file;
//! there's deliberately no in-app path prompt.

use super::{FeedList, FeedLists};
use anyhow::{Context, anyhow, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The shape `lists.toml` deserializes into. `deny_unknown_fields` turns
/// a typo like `activ = "casual"` into an error naming the bad key.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLists {
    /// The list to start with, written by the picker.
    active: Option<String>,
    /// The `[[list]]` entries. `#[serde(default)]` makes "no lists" an
    /// empty `Vec` instead of a parse error.
    #[serde(default)]
    list: Vec<RawList>,
}

/// One `[[list]]` entry, before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawList {
    /// The list's name, shown in the picker.
    name: String,
    /// Where its sources file is: absolute, `~/...`, or relative to the
    /// folder `lists.toml` is in.
    path: String,
}

/// Loads `lists.toml` from the tuxwire config folder. `default_sources` is
/// the usual `sources.toml` path, used for the implicit single list when
/// there's no `lists.toml`.
///
/// Only "the file doesn't exist" falls back to the single list. Any other
/// read error, and any parse or validation error, is returned, so a
/// mistake in `lists.toml` is shown rather than silently ignored.
pub fn load(default_sources: &Path) -> anyhow::Result<FeedLists> {
    let path = lists_path()?;
    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FeedLists::single(default_sources.to_path_buf()));
        }
        Err(err) => {
            return Err(err).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    // Relative list paths are relative to the folder `lists.toml` is in.
    // `parent()` is `None` only for a root path, which `lists_path` never
    // returns, so the fallback to `"."` never really applies.
    let config_dir = path.parent().unwrap_or(Path::new("."));
    parse(
        &contents,
        config_dir,
        crate::paths::home_dir().as_deref(),
        default_sources,
    )
    .with_context(|| format!("failed to load {}", path.display()))
}

/// Parses and validates `lists.toml` text. Separate from [`load`] so tests
/// can pass strings and a made-up home and config folder.
///
/// Checks, each with an error naming the problem: every name is non-empty
/// and unique ignoring case, and every path is non-empty and resolvable
/// (see `resolve_path`). An `active` naming no list is *not* an error
/// here; [`super::activate`] reports it and moves on, so a renamed list
/// can't stop the app from starting.
pub fn parse(
    contents: &str,
    config_dir: &Path,
    home: Option<&Path>,
    default_sources: &Path,
) -> anyhow::Result<FeedLists> {
    let raw: RawLists = toml::from_str(contents).context("invalid lists.toml")?;

    // No `[[list]]` entries at all: the single implicit list, but still
    // honoring nothing else (an `active` alone means nothing).
    if raw.list.is_empty() {
        return Ok(FeedLists::single(default_sources.to_path_buf()));
    }

    let mut lists: Vec<FeedList> = Vec::with_capacity(raw.list.len());
    // `enumerate` numbers the entries so errors can say "list #2".
    for (number, entry) in raw.list.into_iter().enumerate() {
        let number = number + 1;
        let name = entry.name.trim().to_string();
        if name.is_empty() {
            bail!("[[list]] #{number} has an empty name");
        }
        // `any` stops at the first earlier list with the same name.
        if lists
            .iter()
            .any(|list| list.name.eq_ignore_ascii_case(&name))
        {
            bail!("[[list]] #{number}: the name {name:?} is used more than once");
        }
        let path = resolve_path(&entry.path, config_dir, home)
            .with_context(|| format!("[[list]] {name:?}"))?;
        lists.push(FeedList { name, path });
    }

    Ok(FeedLists::new(lists, raw.active))
}

/// Turns a `path = "..."` value into a full path:
/// - `~` or `~/rest` becomes the home directory (plus `rest`);
/// - an absolute path (`/...`) is used as is;
/// - anything else is relative to `config_dir` (the `lists.toml` folder),
///   so `casual.toml` sits next to `sources.toml`.
///
/// `~otheruser/...` isn't supported (it would need the system user
/// database) and is an error rather than a surprising relative path.
fn resolve_path(raw: &str, config_dir: &Path, home: Option<&Path>) -> anyhow::Result<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("path is empty");
    }

    // `strip_prefix('~')` returns the rest of the string if it started
    // with `~`, and `None` otherwise.
    if let Some(rest) = raw.strip_prefix('~') {
        let home = home.ok_or_else(|| anyhow!("path {raw:?} uses ~ but $HOME is not set"))?;
        if rest.is_empty() {
            return Ok(home.to_path_buf());
        }
        // `~/x` gives a `rest` of `/x`. Dropping that `/` keeps `join` from
        // treating it as an absolute path, which would discard `home`.
        return match rest.strip_prefix('/') {
            Some(rest) => Ok(home.join(rest)),
            None => bail!("path {raw:?}: only ~ and ~/... are supported, not ~user"),
        };
    }

    let path = Path::new(raw);
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(config_dir.join(path))
    }
}

/// Saves `name` as `active = "..."` in `lists.toml`, so the picker's choice
/// is used on the next startup. Only called when `lists.toml` defines
/// several lists, so the file always exists by then.
pub fn save_active(name: &str) -> anyhow::Result<()> {
    save_active_at(&lists_path()?, name)
}

/// The body of [`save_active`], taking the path as a parameter so tests can
/// use a temporary file. Goes through `toml_edit`, so only the `active`
/// line changes and every comment and `[[list]]` stays as written.
fn save_active_at(path: &Path, name: &str) -> anyhow::Result<()> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = contents
        .parse()
        .with_context(|| format!("failed to parse {} as TOML", path.display()))?;

    // Replaces the value in place, or adds the key if missing. Root keys
    // are written before the first `[[list]]`, where TOML requires them.
    doc["active"] = toml_edit::value(name);

    std::fs::write(path, doc.to_string())
        .with_context(|| format!("failed to write {}", path.display()))
}

/// The full path to `lists.toml`: `$XDG_CONFIG_HOME/tuxwire/lists.toml`, or
/// `~/.config/tuxwire/lists.toml`. Same XDG fallback as `theme.rs`'s
/// `theme_path`. `pub` so the TUI can say where lists are defined.
pub fn lists_path() -> anyhow::Result<PathBuf> {
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config_home)
            .join("tuxwire")
            .join("lists.toml"));
    }
    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate lists.toml")?;
    Ok(PathBuf::from(home).join(".config/tuxwire/lists.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The made-up config folder, home, and default sources path the tests
    /// parse against.
    fn dirs() -> (&'static Path, Option<&'static Path>, &'static Path) {
        (
            Path::new("/home/shane/.config/tuxwire"),
            Some(Path::new("/home/shane")),
            Path::new("/home/shane/.config/tuxwire/sources.toml"),
        )
    }

    /// Parses `text` with the made-up folders.
    fn parse_str(text: &str) -> anyhow::Result<FeedLists> {
        let (config_dir, home, default_sources) = dirs();
        parse(text, config_dir, home, default_sources)
    }

    /// A full file: names, `~`, relative, and absolute paths, and `active`.
    #[test]
    fn parses_lists_and_resolves_paths() {
        let lists = parse_str(
            r#"
            active = "casual"
            [[list]]
            name = "main"
            path = "~/.config/tuxwire/sources.toml"
            [[list]]
            name = "casual"
            path = "casual.toml"
            [[list]]
            name = "work"
            path = "/srv/feeds/work.toml"
            "#,
        )
        .unwrap();
        let paths: Vec<&Path> = lists.lists().iter().map(|l| l.path.as_path()).collect();
        assert_eq!(
            paths,
            [
                Path::new("/home/shane/.config/tuxwire/sources.toml"),
                Path::new("/home/shane/.config/tuxwire/casual.toml"),
                Path::new("/srv/feeds/work.toml"),
            ]
        );
        assert_eq!(lists.preferred.as_deref(), Some("casual"));
        assert!(!lists.is_single());
    }

    /// An empty file, or one with only `active`, is the single implicit
    /// list, identical to having no `lists.toml`.
    #[test]
    fn no_lists_means_single_default_list() {
        for text in ["", "active = \"casual\""] {
            let lists = parse_str(text).unwrap();
            assert!(lists.is_single());
            assert_eq!(lists.lists()[0].name, "main");
            assert_eq!(lists.lists()[0].path, dirs().2);
        }
    }

    /// One configured list is still "single": the picker never shows.
    #[test]
    fn one_configured_list_is_single() {
        let lists = parse_str("[[list]]\nname = \"daily\"\npath = \"daily.toml\"").unwrap();
        assert!(lists.is_single());
        assert_eq!(lists.lists()[0].name, "daily");
    }

    /// Each kind of mistake is an error that names it.
    #[test]
    fn rejects_bad_lists() {
        let cases = [
            ("[[list]]\nname = \"  \"\npath = \"a.toml\"", "empty name"),
            (
                "[[list]]\nname = \"a\"\npath = \"a.toml\"\n[[list]]\nname = \"A\"\npath = \"b.toml\"",
                "used more than once",
            ),
            ("[[list]]\nname = \"a\"\npath = \"\"", "path is empty"),
            (
                "[[list]]\nname = \"a\"\npath = \"~bob/x.toml\"",
                "not ~user",
            ),
            ("[[list]]\nname = \"a\"", "missing field"),
            ("activ = \"a\"", "unknown field"),
        ];
        for (text, expected) in cases {
            let err = format!("{:#}", parse_str(text).unwrap_err());
            assert!(err.contains(expected), "{text:?}: {err}");
        }
    }

    /// `~` needs a home directory, and bare `~` is the home itself.
    #[test]
    fn tilde_handling() {
        let config_dir = Path::new("/cfg");
        assert!(resolve_path("~/x.toml", config_dir, None).is_err());
        assert_eq!(
            resolve_path("~", config_dir, Some(Path::new("/home/shane"))).unwrap(),
            Path::new("/home/shane")
        );
    }

    /// Saving `active` keeps comments and lists, adds the key if missing,
    /// and reads back.
    #[test]
    fn save_active_preserves_file() {
        let dir = std::env::temp_dir().join(format!(
            "tuxwire-test-lists-{:?}",
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lists.toml");
        std::fs::write(
            &path,
            "# my lists\n[[list]]\nname = \"main\" # daily\npath = \"sources.toml\"\n\
             [[list]]\nname = \"casual\"\npath = \"casual.toml\"\n",
        )
        .unwrap();

        save_active_at(&path, "casual").unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("# my lists") && written.contains("# daily"),
            "{written}"
        );
        let lists = parse(&written, &dir, None, &dir.join("sources.toml")).unwrap();
        assert_eq!(lists.preferred.as_deref(), Some("casual"));
        assert_eq!(lists.lists().len(), 2);

        save_active_at(&path, "main").unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.matches("active").count(), 1, "{written}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
