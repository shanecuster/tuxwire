//! Small helpers for showing file paths to the user, kept free of
//! `ratatui`, `crossterm`, and `ui` so they can move into a future Android
//! app unchanged.
//!
//! Right now this is just the `~` abbreviation for the home directory,
//! used by the footer's "Feed List:" path (and by the feed-list picker).

// Has clippy flag any undocumented item here, the same as in `src/icons/`.
#![warn(clippy::missing_docs_in_private_items)]

use std::path::{Path, PathBuf};

/// The user's home directory, from `$HOME`, or `None` if it isn't set.
///
/// `std::env::var` returns a `Result` (the variable might be missing or not
/// valid Unicode). `.ok()` turns that into an `Option`, discarding the
/// error, because "no home directory" just means "don't abbreviate", which
/// isn't worth reporting. `.map(PathBuf::from)` converts the `String` into
/// a path.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// `path` as text, with the home directory replaced by `~`:
/// `/home/shane/.config/tuxwire/sources.toml` becomes
/// `~/.config/tuxwire/sources.toml`. A path outside `home`, or a missing
/// `home`, comes back unchanged.
///
/// Uses `Path::strip_prefix`, which compares whole path *components*
/// rather than characters. That way `/home/shane2/x` is not mistaken for
/// something under `/home/shane`, which a plain string `starts_with` check
/// would get wrong.
///
/// `to_string_lossy()` converts the path to text, replacing any bytes that
/// aren't valid UTF-8 (possible in Linux file names) with `�` instead of
/// failing. That's fine for display.
pub fn abbreviate_home(path: &Path, home: Option<&Path>) -> String {
    // `if let ... && let ...` (a let-chain): both the home directory must
    // exist *and* `path` must sit underneath it.
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
    {
        // `rest` is the part after the home directory. An empty `rest`
        // means `path` *is* the home directory, which is just `~`.
        if rest.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", rest.to_string_lossy());
    }
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path under home gets `~/` in place of the home directory.
    #[test]
    fn abbreviates_path_under_home() {
        let home = Path::new("/home/shane");
        let path = Path::new("/home/shane/.config/tuxwire/sources.toml");
        assert_eq!(
            abbreviate_home(path, Some(home)),
            "~/.config/tuxwire/sources.toml"
        );
    }

    /// The home directory itself is just `~`.
    #[test]
    fn home_itself_is_tilde() {
        let home = Path::new("/home/shane");
        assert_eq!(abbreviate_home(home, Some(home)), "~");
    }

    /// A sibling directory whose name merely *starts* with the home
    /// directory's name is not under home, and is left alone.
    #[test]
    fn does_not_abbreviate_lookalike_prefix() {
        let home = Path::new("/home/shane");
        let path = Path::new("/home/shane2/feeds.toml");
        assert_eq!(abbreviate_home(path, Some(home)), "/home/shane2/feeds.toml");
    }

    /// Paths outside home, and a missing home, come back unchanged.
    #[test]
    fn leaves_other_paths_alone() {
        let path = Path::new("/etc/tuxwire/sources.toml");
        assert_eq!(
            abbreviate_home(path, Some(Path::new("/home/shane"))),
            "/etc/tuxwire/sources.toml"
        );
        assert_eq!(abbreviate_home(path, None), "/etc/tuxwire/sources.toml");
    }
}
