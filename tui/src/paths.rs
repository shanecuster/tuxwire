//! Where the terminal app keeps things on a Linux desktop, and small helpers
//! for showing file paths to the user.
//!
//! Two jobs live here:
//!
//! - the `~` abbreviation for the home directory, used by the footer's
//!   "Feed List:" path (and by the feed-list picker);
//! - working out where the database file lives (`~/.local/share/tuxwire/`),
//!   then opening it through the shared core crate.
//!
//! The second job used to sit inside the storage code itself. It moved here
//! because "look at `$HOME`/`$XDG_DATA_HOME`" is a Linux-desktop habit: the
//! shared core crate is also used by an Android app, which has no `$HOME`
//! and is told its data directory by the operating system. Each program now
//! decides where its own data lives and passes the path to the core.

// Has clippy flag any undocumented item here, the same as in `src/icons/`.
#![warn(clippy::missing_docs_in_private_items)]

use anyhow::Context;
use std::path::{Path, PathBuf};
use tuxwire_core::storage::Storage;

/// The directory tuxwire's data (right now, just the database file) lives
/// in: `~/.local/share/tuxwire`, per ARCHITECTURE.md's Storage section.
///
/// ## Why this doesn't reach for the `dirs` crate
///
/// Plenty of Rust CLI apps pull in the `dirs` crate to resolve
/// platform-appropriate data/config directories portably, since Windows,
/// macOS, and Linux all disagree on where these belong. The terminal app
/// only ever targets Linux -- ARCHITECTURE.md's whole reason for existing
/// describes it living in a Hyprland scratchpad -- so it's simpler, and one
/// fewer dependency, to implement just the single path this program
/// actually needs: the XDG Base Directory spec's `$XDG_DATA_HOME`, falling
/// back to `$HOME/.local/share` when that variable isn't set, which is what
/// the spec itself documents as the default.
fn data_dir() -> anyhow::Result<PathBuf> {
    // If the user has set `$XDG_DATA_HOME`, that wins.
    if let Ok(xdg_data_home) = std::env::var("XDG_DATA_HOME") {
        return Ok(PathBuf::from(xdg_data_home).join("tuxwire"));
    }

    // Otherwise fall back to the spec's default under `$HOME`. `.context(..)`
    // attaches a human-readable explanation to the error if `$HOME` is unset.
    let home = std::env::var("HOME")
        .context("HOME environment variable is not set -- can't locate the data directory")?;

    Ok(PathBuf::from(home).join(".local/share/tuxwire"))
}

/// The full path to the database file itself: `data_dir()/tuxwire.db`.
fn db_path() -> anyhow::Result<PathBuf> {
    Ok(data_dir()?.join("tuxwire.db"))
}

/// Opens (creating if necessary) the terminal app's real database in its
/// usual Linux location, migrating its schema up to date.
///
/// This is the one-line replacement for the old `Storage::open()`: work out
/// the path here (the terminal app's job), then hand it to the shared core
/// (`Storage::open_at`), which does the actual opening.
pub fn open_storage() -> anyhow::Result<Storage> {
    // Where the file goes...
    let path = db_path()?;

    // ...and let the core open it.
    Storage::open_at(&path)
}

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
