//! Feed lists: several named `sources.toml`-style files, one active at a
//! time (`docs/ARCHITECTURE.md` § Configuration → `lists.toml`).
//!
//! A feed list is just a name plus the path of a file in the usual
//! `[[source]]` format. The lists are defined in `~/.config/tuxwire/lists.toml`
//! (parsed in [`config`]). Without that file, or with only one list in it,
//! there's a single list and tuxwire behaves exactly as it did before
//! lists existed.
//!
//! All lists share one database. An article belongs to a list when its
//! `feed_url` is one of that list's sources (migration 005), and read,
//! saved, and note state are per article, so they're shared across lists.
//!
//! This module decides *which* list is active:
//! - [`activate`] at startup: the remembered list if it loads, otherwise
//!   the next one that does, with a message saying what happened.
//! - [`try_switch`] when the picker asks for another list: switch if the
//!   target loads, otherwise stay put with a message.
//!
//! Both take the "load this list's file" step as a closure, so the logic
//! can be tested without files or the network. Nothing here imports
//! `ratatui`, `crossterm`, or `ui`, so it can move into a future Android
//! app unchanged.

// Has clippy flag any undocumented item here, the same as in `core/src/icons/`.
#![warn(clippy::missing_docs_in_private_items)]

pub mod config;

use std::path::{Path, PathBuf};

/// The name of the one implicit list when there's no `lists.toml`: the
/// usual `~/.config/tuxwire/sources.toml`.
pub const DEFAULT_LIST_NAME: &str = "main";

/// One feed list: what it's called and which file holds its sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedList {
    /// Shown in the picker, and saved as `active = "..."` in `lists.toml`.
    /// Unique among the lists, ignoring case (`config` checks).
    pub name: String,
    /// The list's sources file, already resolved: `~` expanded, and a
    /// relative path made relative to the `lists.toml` directory.
    pub path: PathBuf,
}

/// Every configured feed list, in `lists.toml` order, plus which one the
/// user last picked. Built by [`config::load`] (or [`FeedLists::single`]).
#[derive(Debug, Clone)]
pub struct FeedLists {
    /// The lists, in config order. Never empty: with nothing configured
    /// there is still the implicit [`DEFAULT_LIST_NAME`] list.
    lists: Vec<FeedList>,
    /// `lists.toml`'s `active = "..."`, the list to try first at startup.
    /// It may name a list that no longer exists; [`activate`] reports that.
    preferred: Option<String>,
}

impl FeedLists {
    /// The single implicit list used when there's no `lists.toml`:
    /// [`DEFAULT_LIST_NAME`] pointing at `sources_path`.
    pub fn single(sources_path: PathBuf) -> FeedLists {
        FeedLists {
            lists: vec![FeedList {
                name: DEFAULT_LIST_NAME.to_string(),
                path: sources_path,
            }],
            preferred: None,
        }
    }

    /// Builds the set from parsed config. Private to this module tree
    /// (`pub(crate)` would be wider than needed), so the "never empty" rule
    /// is enforced by its only caller, [`config::parse`].
    fn new(lists: Vec<FeedList>, preferred: Option<String>) -> FeedLists {
        FeedLists { lists, preferred }
    }

    /// Every list, in config order. A slice (`&[FeedList]`) borrows the
    /// lists without copying them.
    pub fn lists(&self) -> &[FeedList] {
        &self.lists
    }

    /// `true` when there's only one list. The picker never opens then, and
    /// everything behaves exactly as before lists existed.
    pub fn is_single(&self) -> bool {
        self.lists.len() == 1
    }

    /// The name remembered as the active list (`active = "..."` in
    /// `lists.toml`), or `None` if nothing is remembered. It may name a list
    /// that no longer exists, so check it with [`FeedLists::index_of`].
    pub fn preferred(&self) -> Option<&str> {
        // `as_deref` turns `&Option<String>` into `Option<&str>`, lending
        // the text out without copying it.
        self.preferred.as_deref()
    }

    /// The position of the list called `name`, ignoring case, or `None`.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.lists
            .iter()
            .position(|list| list.name.eq_ignore_ascii_case(name.trim()))
    }
}

/// What [`activate`] chose at startup.
///
/// Generic over `T`, whatever the caller's loader returns for a list (in
/// the app, that list's configured sources), so tests can use simple
/// stand-ins.
#[derive(Debug)]
pub struct Activation<T> {
    /// Which list is active: an index into [`FeedLists::lists`].
    pub index: usize,
    /// What the loader returned for that list.
    pub loaded: T,
    /// A one-line footer message when the remembered list couldn't be
    /// used, e.g. `Feed list "casual" couldn't be loaded (...); using
    /// "main"`. `None` when the preferred list loaded fine.
    pub notice: Option<String>,
}

/// Picks the active list at startup, falling back when a list can't be
/// loaded.
///
/// Tries, in order: the remembered list (`active` in `lists.toml`) if it
/// names a real list, then every other list in config order. The first one
/// whose `load` succeeds wins. `home` is only used to show paths with `~`
/// in messages.
///
/// Returns `Err(message)` only when *no* list loads. The caller decides
/// what that means: with a single list it's today's clean startup error,
/// and with several the TUI opens empty and shows the message.
///
/// `mut load: impl FnMut(...)` means "any closure that can be called
/// repeatedly and may change its own captured state". `FnMut` rather than
/// `Fn`, so a test can count calls with a captured counter.
pub fn activate<T>(
    lists: &FeedLists,
    home: Option<&Path>,
    mut load: impl FnMut(&FeedList) -> anyhow::Result<T>,
) -> Result<Activation<T>, String> {
    // Step 1: the order to try. The remembered list first (if it exists),
    // then the rest in config order. `filter` drops the remembered one
    // from the rest so it isn't tried twice.
    let preferred = lists
        .preferred
        .as_deref()
        .and_then(|name| lists.index_of(name));
    let mut order: Vec<usize> = preferred.into_iter().collect();
    order.extend((0..lists.lists.len()).filter(|index| Some(*index) != preferred));

    // A remembered name that matches no list is worth mentioning, since
    // the user may have renamed a list in `lists.toml` and expect it to
    // stick.
    let mut problems: Vec<String> = Vec::new();
    if let Some(name) = lists.preferred.as_deref()
        && preferred.is_none()
    {
        problems.push(format!(
            "lists.toml: active = {name:?} is not a defined list"
        ));
    }

    // Step 2: try each in turn. A failure becomes a short description and
    // the loop moves on, so one broken file never blocks the others.
    for index in order {
        let list = &lists.lists[index];
        match load(list) {
            Ok(loaded) => {
                // Only mention a fallback when something actually went
                // wrong. A clean first try is silent.
                let notice = (!problems.is_empty())
                    .then(|| format!("{}; using {:?}", problems.join("; "), list.name));
                return Ok(Activation {
                    index,
                    loaded,
                    notice,
                });
            }
            Err(err) => problems.push(unavailable(list, home, &err)),
        }
    }

    // Step 3: nothing loaded at all.
    Err(problems.join("; "))
}

/// What happened when the picker asked to switch lists ([`try_switch`]).
#[derive(Debug)]
pub enum Switch<T> {
    /// The target loaded and is now active.
    Switched {
        /// The new active list's index.
        index: usize,
        /// What the loader returned for it.
        loaded: T,
    },
    /// The target is already the active list, so nothing was loaded.
    Unchanged,
    /// The target couldn't be loaded. The current list stays active, and
    /// `message` says why, for the footer.
    Failed {
        /// One line: which list, its file, the reason, and that the current
        /// list is still active.
        message: String,
    },
}

/// Switches from list `current` to list `target`, if `target` loads.
///
/// On failure the caller simply keeps `current`: nothing changes until a
/// load succeeds, so a broken file can never leave the app with no list.
/// Only `target` is loaded; no other list is touched.
pub fn try_switch<T>(
    lists: &FeedLists,
    current: usize,
    target: usize,
    home: Option<&Path>,
    mut load: impl FnMut(&FeedList) -> anyhow::Result<T>,
) -> Switch<T> {
    if target == current {
        return Switch::Unchanged;
    }
    let list = &lists.lists[target];
    match load(list) {
        Ok(loaded) => Switch::Switched {
            index: target,
            loaded,
        },
        Err(err) => Switch::Failed {
            message: format!(
                "{}; still on {:?}",
                unavailable(list, home, &err),
                lists.lists[current].name
            ),
        },
    }
}

/// `Feed list "casual" couldn't be loaded (~/.config/tuxwire/casual.toml:
/// No such file or directory)`: the shared wording for a list that failed
/// to load, kept to one line for the footer.
///
/// Uses the error's *root cause* (`err.root_cause()`), the innermost error
/// in the `anyhow` context chain, because the outer layers repeat the path
/// that's already in the message. Only its first line is kept: TOML parse
/// errors span several lines with a code snippet, and the footer is one row.
fn unavailable(list: &FeedList, home: Option<&Path>, err: &anyhow::Error) -> String {
    let reason = err.root_cause().to_string();
    let reason = reason.lines().next().unwrap_or("").trim();
    format!(
        "Feed list {:?} couldn't be loaded ({}: {reason})",
        list.name,
        crate::paths::abbreviate_home(&list.path, home)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    /// Three lists, "main", "casual", and "work", remembering `preferred`.
    fn three_lists(preferred: Option<&str>) -> FeedLists {
        FeedLists::new(
            ["main", "casual", "work"]
                .iter()
                .map(|name| FeedList {
                    name: name.to_string(),
                    path: PathBuf::from(format!("/home/shane/.config/tuxwire/{name}.toml")),
                })
                .collect(),
            preferred.map(str::to_string),
        )
    }

    /// A loader that succeeds for every list except those named in
    /// `broken`, returning the list's name so tests can see which loaded.
    fn loader(broken: &[&str]) -> impl FnMut(&FeedList) -> anyhow::Result<String> {
        // `move` copies the (borrowed) names into the closure. The `Vec`
        // owns its strings, so the closure doesn't borrow from the caller.
        let broken: Vec<String> = broken.iter().map(|s| s.to_string()).collect();
        move |list| {
            if broken.contains(&list.name) {
                Err(anyhow!("No such file or directory (os error 2)"))
            } else {
                Ok(list.name.clone())
            }
        }
    }

    /// The home directory used in tests, so paths show as `~`.
    fn home() -> Option<&'static Path> {
        Some(Path::new("/home/shane"))
    }

    /// With nothing remembered, the first list is active and there's no
    /// message.
    #[test]
    fn activate_defaults_to_first_list() {
        let chosen = activate(&three_lists(None), home(), loader(&[])).unwrap();
        assert_eq!(chosen.index, 0);
        assert_eq!(chosen.loaded, "main");
        assert_eq!(chosen.notice, None);
    }

    /// The remembered list is used when it loads, case-insensitively.
    #[test]
    fn activate_uses_remembered_list() {
        let chosen = activate(&three_lists(Some("Casual")), home(), loader(&[])).unwrap();
        assert_eq!(chosen.loaded, "casual");
        assert_eq!(chosen.notice, None);
    }

    /// A remembered list that fails falls back to the next loadable list in
    /// config order, starting from the first, with a footer message naming
    /// the list, its `~` path, the reason, and the fallback.
    #[test]
    fn activate_falls_back_when_remembered_list_fails() {
        let chosen = activate(&three_lists(Some("casual")), home(), loader(&["casual"])).unwrap();
        assert_eq!(chosen.loaded, "main");
        assert_eq!(
            chosen.notice.as_deref(),
            Some(
                "Feed list \"casual\" couldn't be loaded \
                 (~/.config/tuxwire/casual.toml: No such file or directory (os error 2)); \
                 using \"main\""
            )
        );

        // If "main" is broken too, the next in order ("work") is used.
        let chosen = activate(
            &three_lists(Some("casual")),
            home(),
            loader(&["casual", "main"]),
        )
        .unwrap();
        assert_eq!(chosen.loaded, "work");
        let notice = chosen.notice.unwrap();
        assert!(
            notice.contains("\"casual\"") && notice.contains("\"main\""),
            "{notice}"
        );
        assert!(notice.ends_with("using \"work\""), "{notice}");
    }

    /// A remembered name that isn't a list is reported, and the first list
    /// is used.
    #[test]
    fn activate_reports_unknown_remembered_name() {
        let chosen = activate(&three_lists(Some("gone")), home(), loader(&[])).unwrap();
        assert_eq!(chosen.loaded, "main");
        assert_eq!(
            chosen.notice.as_deref(),
            Some("lists.toml: active = \"gone\" is not a defined list; using \"main\"")
        );
    }

    /// When no list loads, the result is an error message covering each one.
    #[test]
    fn activate_fails_when_nothing_loads() {
        let message = activate(
            &three_lists(None),
            home(),
            loader(&["main", "casual", "work"]),
        )
        .unwrap_err();
        assert_eq!(
            message.matches("couldn't be loaded").count(),
            3,
            "{message}"
        );
    }

    /// Each list is loaded at most once, and loading stops at the first
    /// success: lists that aren't needed are never touched.
    #[test]
    fn activate_loads_only_what_it_needs() {
        let mut calls = Vec::new();
        let chosen = activate(&three_lists(Some("work")), home(), |list| {
            calls.push(list.name.clone());
            Ok(())
        })
        .unwrap();
        assert_eq!(chosen.index, 2);
        assert_eq!(calls, ["work"]);
    }

    /// Switching to a loadable list switches. Switching to the current list
    /// is a no-op that loads nothing.
    #[test]
    fn switch_to_another_list() {
        let lists = three_lists(None);
        match try_switch(&lists, 0, 1, home(), loader(&[])) {
            Switch::Switched { index, loaded } => {
                assert_eq!(index, 1);
                assert_eq!(loaded, "casual");
            }
            other => panic!("expected Switched, got {other:?}"),
        }
        let mut loaded_anything = false;
        let outcome = try_switch(&lists, 1, 1, home(), |_| {
            loaded_anything = true;
            Ok(())
        });
        assert!(matches!(outcome, Switch::Unchanged));
        assert!(!loaded_anything);
    }

    /// A target that fails to load leaves the current list active, with a
    /// message saying so.
    #[test]
    fn switch_to_broken_list_stays_put() {
        let lists = three_lists(None);
        match try_switch(&lists, 0, 2, home(), loader(&["work"])) {
            Switch::Failed { message } => {
                assert!(
                    message.starts_with("Feed list \"work\" couldn't be loaded"),
                    "{message}"
                );
                assert!(message.contains("~/.config/tuxwire/work.toml"), "{message}");
                assert!(message.ends_with("still on \"main\""), "{message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// A multi-line error (as TOML parse errors are) is cut to its first
    /// line, so the footer message stays one line.
    #[test]
    fn failure_reason_is_one_line() {
        let lists = three_lists(None);
        let outcome = try_switch(&lists, 0, 1, home(), |_| -> anyhow::Result<()> {
            Err(anyhow!(
                "TOML parse error at line 3\n  |\n3 | name =\n  |       ^"
            ))
        });
        match outcome {
            Switch::Failed { message } => {
                assert!(!message.contains('\n'), "{message}");
                assert!(message.contains("TOML parse error at line 3"), "{message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    /// `single` is one list named "main", and `index_of` ignores case.
    #[test]
    fn single_and_index_of() {
        let single = FeedLists::single(PathBuf::from("/x/sources.toml"));
        assert!(single.is_single());
        assert_eq!(single.lists()[0].name, DEFAULT_LIST_NAME);
        let three = three_lists(None);
        assert!(!three.is_single());
        assert_eq!(three.index_of(" WORK "), Some(2));
        assert_eq!(three.index_of("nope"), None);
    }
}
