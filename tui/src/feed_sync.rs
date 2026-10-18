//! The terminal half of the optional feed-list sync (`docs/ARCHITECTURE.md`
//! § Optional feed-list sync).
//!
//! The shared `tuxwire-core` crate does the syncing itself. It does not know
//! where this app keeps its feeds (a TOML file, `sources.toml`), so this
//! module is the glue:
//!
//! - it turns the chosen feed list into the plain list core wants
//!   ([`syncable_feeds`]);
//! - it adds the feeds other devices added to that file
//!   ([`add_pending_feeds`]);
//! - it deals with feeds other devices *removed*, but only after the person
//!   said yes ([`resolve_removals`]), and even then it never deletes: the
//!   feed's `[[source]]` block is turned into comments ([`comment_out_source`])
//!   so it can be brought back by hand.
//!
//! Nothing here runs unless `feeds = true` and `enabled = true` are both in
//! `sync.toml`. The file is only ever replaced safely (written next to the
//! original, then renamed), and every change is checked to still be valid TOML
//! before it is written.

#![warn(clippy::missing_docs_in_private_items)]

use crate::feedlists::{FeedList, FeedLists};
use crate::fetchers::config::{append_sources_at, load_sources_at};
use anyhow::{Context, anyhow};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use tuxwire_core::fetchers::{SourceConfig, SourceType};
use tuxwire_core::storage::{FeedEntry, Storage};
use tuxwire_core::sync::SyncConfig;
use tuxwire_core::sync::feeds::{feed_key, sync_feeds};

/// The line written above a feed that sync commented out, so the person
/// knows who did it and how to undo it.
const REMOVED_MARKER: &str = "# Taken out by `tuxwire sync`: this feed was removed on another device and you said yes. Delete these lines, or remove the leading \"# \", to get it back.";

/// What one feed-list sync did, for the message the person sees.
#[derive(Debug, Default)]
pub struct FeedSyncOutcome {
    /// Core's own one-line report (changes sent, anything skipped).
    pub report: String,
    /// Feeds other devices added that are now in the list file. They show up
    /// the next time the reader starts.
    pub added: usize,
    /// Feeds another device removed that are still in this list. Nothing has
    /// been done about them: the person must be asked.
    pub removals: Vec<FeedEntry>,
}

/// Picks the feed list to mirror: the one `feed_list` names in `sync.toml`
/// (capitals do not matter), or the first list when none is named.
///
/// A name that matches nothing is an error that shows the real names, never a
/// quiet fallback: syncing the wrong list would add other devices' feeds to
/// the wrong place. The returned `&FeedList` points into `lists` (a borrow),
/// which is why it cannot outlive it.
pub fn choose_list<'a>(lists: &'a FeedLists, wanted: Option<&str>) -> anyhow::Result<&'a FeedList> {
    // Step 1: a named list must exist.
    if let Some(name) = wanted {
        return match lists.index_of(name) {
            Some(index) => Ok(&lists.lists()[index]),
            None => {
                // Collect the real names so the message can list them.
                let names: Vec<&str> = lists.lists().iter().map(|l| l.name.as_str()).collect();
                Err(anyhow!(
                    "sync.toml says feed_list = {name:?}, but there is no such list; \
                     the lists are: {}",
                    names.join(", ")
                ))
            }
        };
    }
    // Step 2: nothing named means the first list.
    lists
        .lists()
        .first()
        .ok_or_else(|| anyhow!("no feed lists are configured"))
}

/// Turns the list's sources into the plain entries core syncs. Only RSS
/// sources go: Reddit sources are not fetched by core yet, so they stay on
/// this device.
pub fn syncable_feeds(sources: &[SourceConfig]) -> Vec<FeedEntry> {
    // Walk the sources, keep the RSS ones, and copy the three text fields into
    // an owned `FeedEntry` (`clone` because core's type owns its strings).
    sources
        .iter()
        .filter(|source| source.source_type == SourceType::Rss)
        .map(|source| FeedEntry {
            name: source.name.clone(),
            url: source.url.clone(),
            topic: source.topic.clone(),
        })
        .collect()
}

/// Runs one feed-list sync for the list file at `path`.
///
/// 1. Reads the list (a missing file counts as empty, which core refuses to
///    treat as "everything was removed").
/// 2. Calls core's `sync_feeds` with the list.
/// 3. Adds the feeds other devices added ([`add_pending_feeds`]).
/// 4. Returns the removals still waiting for the person's answer; nothing is
///    removed here.
pub async fn run_feed_sync(
    storage: &Mutex<Storage>,
    config: &SyncConfig,
    path: &Path,
) -> anyhow::Result<FeedSyncOutcome> {
    // Step 1: read the current list. `exists()` first so a brand-new list
    // file is "empty", while a file that is there but broken is a real error.
    let sources = if path.exists() {
        load_sources_at(path, false)?
    } else {
        Vec::new()
    };

    // Step 2: let core exchange the feed list with the server.
    let report = sync_feeds(storage, config, syncable_feeds(&sources)).await?;

    // Step 3 and 4: apply additions, then look up what is waiting to be asked.
    // `lock()` takes the database for these quick local steps; the guard is
    // dropped at the end of the block, before the function returns.
    let (added, removals) = {
        let guard = storage
            .lock()
            .map_err(|_| anyhow!("the database lock was poisoned"))?;
        let added = add_pending_feeds(&guard, path, &sources)?;
        let removals = guard.pending_feed_removals()?;
        (added, removals)
    };

    Ok(FeedSyncOutcome {
        report: report.summary(),
        added,
        removals,
    })
}

/// Adds the feeds other devices added to the list file at `path`, then tells
/// core they are here. Returns how many were written.
///
/// `current` is the list as read before the sync; a pending feed whose address
/// is already in it (ignoring capitals) is not added twice, but is still marked
/// as present so it stops being offered. The new feeds go in with one safe
/// write that keeps the comments in the file.
pub fn add_pending_feeds(
    storage: &Storage,
    path: &Path,
    current: &[SourceConfig],
) -> anyhow::Result<usize> {
    // Step 1: what other devices added that this device does not have yet.
    let pending = storage.pending_feed_additions()?;
    if pending.is_empty() {
        return Ok(0);
    }

    // Step 2: split into "really new" and "already in the file". A `HashSet`
    // answers "have I seen this address?" quickly; `insert` returns `false`
    // when it was already there, which also drops repeats within `pending`.
    let mut known: HashSet<String> = current.iter().map(|s| feed_key(&s.url)).collect();
    let mut fresh: Vec<SourceConfig> = Vec::new();
    for entry in &pending {
        if known.insert(feed_key(&entry.url)) {
            fresh.push(SourceConfig {
                name: entry.name.clone(),
                source_type: SourceType::Rss,
                url: entry.url.clone(),
                topic: entry.topic.clone(),
            });
        }
    }

    // Step 3: write first, tell core second. If the write fails (`?`) core
    // still thinks the feeds are waiting, and the next sync tries again.
    if !fresh.is_empty() {
        append_sources_at(path, &fresh)?;
    }
    for entry in &pending {
        storage.feed_added_locally(&entry.url)?;
    }
    Ok(fresh.len())
}

/// Carries out the person's answers about feeds another device removed.
///
/// `decide` is asked once per feed and returns `true` for "yes, remove it
/// here". A yes comments the feed out of the list file and tells core; a no
/// tells core to stop asking until the other device changes it again. Returns
/// `(removed, kept)`.
///
/// `decide` is a closure (a small function passed in as a value), so the real
/// command can ask on the keyboard while the tests simply answer "yes".
pub fn resolve_removals(
    storage: &Storage,
    path: &Path,
    removals: &[FeedEntry],
    mut decide: impl FnMut(&FeedEntry) -> bool,
) -> anyhow::Result<(usize, usize)> {
    let mut removed = 0;
    let mut kept = 0;
    for entry in removals {
        if decide(entry) {
            // Yes: change the file first, so a failure leaves the feed both in
            // the file and still waiting to be asked about.
            comment_out_source(path, &entry.url)?;
            storage.feed_removed_locally(&entry.url)?;
            removed += 1;
        } else {
            // No: remember the answer; nothing is sent to the server.
            storage.dismiss_feed_removal(&entry.url)?;
            kept += 1;
        }
    }
    Ok((removed, kept))
}

/// Turns every `[[source]]` block whose `url` is `url` (ignoring capitals)
/// into `# ` comments, with a note above it, instead of deleting it.
///
/// Returns `true` if a block was changed and `false` if no block had that
/// address. Everything else in the file (other feeds, the person's own
/// comments, blank lines, even Windows line endings) is left byte for byte as
/// it was.
///
/// A block runs from its `[[source]]` line to the line before the next
/// `[`-header, minus any blank or comment lines at its end: those belong to
/// whatever comes next, and are not ours to change. The edited text is parsed
/// again before it is written; if it is somehow not valid TOML, nothing is
/// written and an error comes back.
pub fn comment_out_source(path: &Path, url: &str) -> anyhow::Result<bool> {
    // Step 1: read the file. `split_inclusive('\n')` cuts it into lines but
    // keeps each line's own ending, so a rewrite changes nothing else.
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read sources file {}", path.display()))?;
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let wanted = feed_key(url);

    // Step 2: find where each `[[source]]` block starts.
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with("[[source]]"))
        .map(|(index, _)| index)
        .collect();

    // Step 3: for each block work out its last line and whether it is ours.
    // `ranges` collects the (first, last-plus-one) line numbers to comment out.
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for &start in &starts {
        // The block ends where the next header (a line starting with `[`)
        // begins, or at the end of the file.
        let mut end = lines.len();
        for (offset, line) in lines[start + 1..].iter().enumerate() {
            if line.trim_start().starts_with('[') {
                end = start + 1 + offset;
                break;
            }
        }
        // Give back the blank and comment lines at the end of the block.
        while end > start + 1 {
            let last = lines[end - 1].trim();
            if last.is_empty() || last.starts_with('#') {
                end -= 1;
            } else {
                break;
            }
        }
        // Read just this block as TOML and compare its address. A block that
        // does not parse is not ours to touch.
        let block: String = lines[start..end].concat();
        let Ok(table) = toml::from_str::<toml::Table>(&block) else {
            continue;
        };
        let address = table
            .get("source")
            .and_then(|value| value.as_array())
            .and_then(|array| array.first())
            .and_then(|first| first.get("url"))
            .and_then(|value| value.as_str());
        if address.is_some_and(|address| feed_key(address) == wanted) {
            ranges.push((start, end));
        }
    }
    if ranges.is_empty() {
        return Ok(false);
    }

    // Step 4: build the new text, line by line. Inside a chosen range every
    // non-blank line gets `# ` in front; the note goes above the range.
    let mut out = String::with_capacity(text.len() + 256);
    for (index, line) in lines.iter().enumerate() {
        if ranges.iter().any(|&(start, _)| start == index) {
            out.push_str(REMOVED_MARKER);
            out.push('\n');
        }
        let inside = ranges.iter().any(|&(s, e)| index >= s && index < e);
        if inside && !line.trim().is_empty() {
            out.push_str("# ");
        }
        out.push_str(line);
    }
    // A last line with no ending still needs one before anything is appended
    // later; harmless if it already has one.
    if !out.ends_with('\n') {
        out.push('\n');
    }

    // Step 5: refuse to write anything that is not valid TOML.
    toml::from_str::<toml::Table>(&out).map_err(|err| {
        anyhow!("commenting the feed out would break the file ({err}); nothing was changed")
    })?;

    // Step 6: replace the file safely (write beside it, then rename).
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = std::path::PathBuf::from(temporary);
    std::fs::write(&temporary, out)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .with_context(|| format!("failed to replace sources file {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuxwire_core::sync::feeds::FeedDoc;

    /// A fresh empty folder, unique per call so parallel tests never share
    /// files. Nothing here touches the real config or database.
    fn temp_dir() -> std::path::PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // One counter for every test in the program: each call takes the next
        // number.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let number = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("tuxwire-feed-sync-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A list file with comments, two feeds, and a note between them.
    const LIST: &str = "# my feeds\n\n[[source]]\nname = \"Alpha\"\ntype = \"rss\"\nurl = \"https://alpha.example/feed\"\ntopic = \"News\"\n\n# about beta\n[[source]]\nname = \"Beta\"\ntype = \"rss\"\nurl = \"https://BETA.example/feed\"\ntopic = \"News\"\n";

    /// Pretends the server delivered one feed document, so the database has a
    /// pending addition (`present`) or removal (`!present`) without a network.
    fn deliver(storage: &Storage, url: &str, present: bool, rev: &str) {
        let doc = FeedDoc {
            id: tuxwire_core::sync::feeds::feed_doc_id(url),
            rev: Some(rev.to_string()),
            v: 1,
            url: url.to_string(),
            name: "From phone".to_string(),
            topic: "Phone".to_string(),
            source_type: "rss".to_string(),
            present,
            updated_at: 9_000_000_000_000,
        };
        storage.apply_remote_feed_batch(vec![doc], "1").unwrap();
    }

    /// Only RSS sources are offered to sync.
    #[test]
    fn only_rss_sources_are_synced() {
        let sources = vec![
            SourceConfig {
                name: "A".into(),
                source_type: SourceType::Rss,
                url: "https://a.example/f".into(),
                topic: "t".into(),
            },
            SourceConfig {
                name: "r/linux".into(),
                source_type: SourceType::Reddit,
                url: "linux".into(),
                topic: "t".into(),
            },
        ];
        let entries = syncable_feeds(&sources);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://a.example/f");
    }

    /// Commenting out hits the right block (address compared ignoring
    /// capitals), leaves everything else alone, and the file still parses.
    #[test]
    fn commenting_out_keeps_the_rest_and_stays_valid() {
        let path = temp_dir().join("sources.toml");
        std::fs::write(&path, LIST).unwrap();

        assert!(comment_out_source(&path, "https://beta.example/FEED").unwrap());

        let after = std::fs::read_to_string(&path).unwrap();
        // Beta is commented out, with the note above it...
        assert!(after.contains("# [[source]]\n# name = \"Beta\""), "{after}");
        assert!(after.contains("Taken out by `tuxwire sync`"));
        // ...the person's own comments and Alpha are untouched...
        assert!(after.starts_with("# my feeds\n\n[[source]]\nname = \"Alpha\""));
        assert!(after.contains("\n# about beta\n"));
        // ...and only Alpha is still a feed.
        let left = load_sources_at(&path, false).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "Alpha");
        // The temporary file was renamed away.
        assert!(!path.with_extension("toml.tmp").exists());
    }

    /// An address that is not in the file changes nothing at all.
    #[test]
    fn commenting_out_an_unknown_address_changes_nothing() {
        let path = temp_dir().join("sources.toml");
        std::fs::write(&path, LIST).unwrap();
        assert!(!comment_out_source(&path, "https://nope.example/").unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), LIST);
    }

    /// Windows line endings survive, and a block at the very end of a file
    /// with no final newline works.
    #[test]
    fn line_endings_survive_and_the_last_block_works() {
        let path = temp_dir().join("sources.toml");
        let text = "[[source]]\r\nname = \"A\"\r\ntype = \"rss\"\r\nurl = \"https://a.example/f\"\r\ntopic = \"t\"";
        std::fs::write(&path, text).unwrap();
        assert!(comment_out_source(&path, "https://a.example/f").unwrap());
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("# name = \"A\"\r\n"), "{after:?}");
        assert!(load_sources_at(&path, false).unwrap().is_empty());
    }

    /// A feed another device added is written to the file, not duplicated if
    /// it is already there, and no longer offered afterwards.
    #[test]
    fn pending_additions_are_written_once() {
        let dir = temp_dir();
        let path = dir.join("sources.toml");
        std::fs::write(&path, LIST).unwrap();
        let storage = Storage::open_at(&dir.join("db.sqlite")).unwrap();
        deliver(&storage, "https://phone.example/feed", true, "1-a");
        // Also delivered, but already in the file in other capitals.
        deliver(&storage, "https://alpha.example/FEED", true, "1-b");

        let current = load_sources_at(&path, false).unwrap();
        let added = add_pending_feeds(&storage, &path, &current).unwrap();
        assert_eq!(added, 1);

        let names: Vec<String> = load_sources_at(&path, false)
            .unwrap()
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert_eq!(names, ["Alpha", "Beta", "From phone"]);
        // Nothing is pending any more, and a second call adds nothing.
        assert!(storage.pending_feed_additions().unwrap().is_empty());
        assert_eq!(add_pending_feeds(&storage, &path, &current).unwrap(), 0);
        // The comment at the top of the file is still there.
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# my feeds")
        );
    }

    /// A yes comments the feed out; a no keeps it and stops the question.
    #[test]
    fn removals_follow_the_answers() {
        let dir = temp_dir();
        let path = dir.join("sources.toml");
        std::fs::write(&path, LIST).unwrap();
        let storage = Storage::open_at(&dir.join("db.sqlite")).unwrap();

        // This device has both feeds; the server says both were removed.
        let current: Vec<FeedEntry> = syncable_feeds(&load_sources_at(&path, false).unwrap());
        storage.record_local_feeds(&current).unwrap();
        deliver(&storage, "https://alpha.example/feed", false, "1-a");
        deliver(&storage, "https://beta.example/feed", false, "1-b");
        let waiting = storage.pending_feed_removals().unwrap();
        assert_eq!(waiting.len(), 2);

        // Say yes to Alpha only.
        let (removed, kept) =
            resolve_removals(&storage, &path, &waiting, |e| e.url.contains("alpha")).unwrap();
        assert_eq!((removed, kept), (1, 1));

        // Alpha is commented out, Beta is still a feed.
        let left = load_sources_at(&path, false).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "Beta");
        // Nothing is waiting to be asked about any more.
        assert!(storage.pending_feed_removals().unwrap().is_empty());
    }

    /// A named list must exist; no name means the first list.
    #[test]
    fn the_named_list_is_used_and_the_first_is_the_default() {
        let text = "[[list]]\nname = \"main\"\npath = \"main.toml\"\n[[list]]\nname = \"casual\"\npath = \"casual.toml\"\n";
        let lists = crate::feedlists::config::parse(
            text,
            Path::new("/cfg"),
            None,
            Path::new("/cfg/sources.toml"),
        )
        .unwrap();
        assert_eq!(choose_list(&lists, None).unwrap().name, "main");
        assert_eq!(choose_list(&lists, Some("CASUAL")).unwrap().name, "casual");
        let message = choose_list(&lists, Some("nope")).unwrap_err().to_string();
        assert!(message.contains("main") && message.contains("casual"));
    }
}
