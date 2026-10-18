//! The terminal commands `tuxwire import-opml` and `tuxwire export-opml`.
//!
//! OPML is the file format feed readers use to move a list of feeds between
//! them. These two commands run *instead of* the full-screen app: they do
//! their one job, print a short result, and exit. Nothing is downloaded and
//! the database is never touched; only the chosen feed list's sources file
//! (`sources.toml` or one of the files named in `lists.toml`) is read or
//! changed.
//!
//! The reading and writing of OPML text itself lives in the shared
//! `tuxwire-core` crate (`parse_opml` and `write_opml`), so this terminal
//! app and the Android app understand files the same way. This module is the
//! terminal-only part: reading the command line, finding the right file on
//! disk, and appending feeds to `sources.toml` without disturbing the
//! comments in it.
//!
//! ```text
//! tuxwire import-opml FILE [--list NAME]
//! tuxwire export-opml FILE [--list NAME]     (FILE "-" prints to the screen)
//! ```

#![warn(clippy::missing_docs_in_private_items)]

use crate::feedlists::{FeedList, FeedLists};
use crate::fetchers::config::{append_sources_at, load_sources_at};
use anyhow::{Context, anyhow, bail};
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use tuxwire_core::fetchers::SourceConfig;
use tuxwire_core::opml::{MAX_OPML_BYTES, feed_address, parse_opml, write_opml};

/// The help text, shown for `--help` and after a mistake on the command line.
pub const USAGE: &str = "\
Usage:
  tuxwire                              open the reader
  tuxwire import-opml FILE [--list NAME]   add the feeds in an OPML file
  tuxwire export-opml FILE [--list NAME]   save a feed list as an OPML file
  tuxwire sync [--test]                optional: sync saved articles, notes and
                                       (if enabled) your feed list with your own
                                       CouchDB (see sync.toml.example)
  tuxwire sync --reset                 forget what sync already sent and received
                                       (asks first); run once after switching to
                                       a different server or database
  tuxwire --help                       show this help

FILE is the OPML file. For export, FILE \"-\" prints to the screen instead, and
an existing file is never overwritten.
--list NAME picks one of the lists in lists.toml. Without it, the list you
used last is chosen.";

/// A command given on the command line, already checked for mistakes.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// `import-opml`: add the feeds in `file` to a feed list.
    Import {
        /// The OPML file to read.
        file: PathBuf,
        /// The `--list` name, if one was given.
        list: Option<String>,
    },
    /// `export-opml`: write a feed list to `file`.
    Export {
        /// Where to write ("-" means the screen).
        file: PathBuf,
        /// The `--list` name, if one was given.
        list: Option<String>,
    },
    /// `--help`: show [`USAGE`].
    Help,
}

/// Reads the words typed after `tuxwire` (the program name already removed).
///
/// Returns:
/// - `Ok(None)` when there are no words at all: the caller opens the reader.
/// - `Ok(Some(command))` for a command it understands.
/// - `Err(message)` for anything else, with a message saying what was wrong.
///
/// A mistake is an error rather than being ignored, so a typo such as
/// `tuxwire import-opm` does not quietly open the full-screen app.
pub fn parse_args(args: &[String]) -> Result<Option<Command>, String> {
    // Step 1: nothing typed means "start the reader".
    let Some(first) = args.first() else {
        return Ok(None);
    };

    // Step 2: the first word names the command.
    match first.as_str() {
        "-h" | "--help" | "help" => Ok(Some(Command::Help)),
        "import-opml" | "export-opml" => {
            // Step 3: collect the file and the optional `--list NAME` from the
            // rest of the words. `file` and `list` start empty and are filled
            // in as the loop below meets them.
            let mut file: Option<PathBuf> = None;
            let mut list: Option<String> = None;
            // `iter()` walks the words one by one. It is a variable (and
            // `mut`) so the `--list` branch can pull the *next* word out of
            // it with `next()`.
            let mut rest = args[1..].iter();
            while let Some(word) = rest.next() {
                if word == "--list" {
                    // `--list NAME`: the name is the following word.
                    let name = rest
                        .next()
                        .ok_or_else(|| "--list needs a list name after it".to_string())?;
                    list = Some(name.clone());
                } else if let Some(name) = word.strip_prefix("--list=") {
                    // `--list=NAME` is the same thing written as one word.
                    list = Some(name.to_string());
                } else if word.starts_with("--") {
                    return Err(format!("unknown option {word}"));
                } else if file.is_some() {
                    return Err(format!(
                        "only one file can be given, but there is also {word}"
                    ));
                } else {
                    file = Some(PathBuf::from(word));
                }
            }

            // Step 4: the file is required. Wrap up as the right command.
            let file = file.ok_or_else(|| format!("{first} needs a FILE"))?;
            if first == "import-opml" {
                Ok(Some(Command::Import { file, list }))
            } else {
                Ok(Some(Command::Export { file, list }))
            }
        }
        other => Err(format!("unknown command {other}")),
    }
}

/// Chooses which feed list a command works on.
///
/// With `--list NAME` it is that list (capitals do not matter). Without it,
/// it is the list remembered in `lists.toml` from last time, or the first list
/// if nothing is remembered or the remembered name no longer exists. Unlike
/// the reader, the commands do not quietly fall back after a typo in
/// `--list`: they stop and show the real names, so a feed is never added to
/// the wrong list.
///
/// The returned `&FeedList` *borrows* from `lists` (it points into it rather
/// than copying it), which is why both share the lifetime `'a`.
fn pick_list<'a>(lists: &'a FeedLists, requested: Option<&str>) -> anyhow::Result<&'a FeedList> {
    // Step 1: an explicit name must match a list exactly (ignoring case).
    if let Some(name) = requested {
        return match lists.index_of(name) {
            Some(index) => Ok(&lists.lists()[index]),
            None => {
                // `map` turns each list into its name, `collect` gathers the
                // names into one `Vec`, and `join` glues them with commas.
                let names: Vec<&str> = lists.lists().iter().map(|l| l.name.as_str()).collect();
                Err(anyhow!(
                    "there is no list called {name:?}; the lists are: {}",
                    names.join(", ")
                ))
            }
        };
    }

    // Step 2: otherwise the remembered list, else the first one.
    // `and_then` runs `index_of` only if a name is remembered, and gives
    // `None` if that name no longer matches anything.
    let remembered = lists
        .preferred()
        .and_then(|name| lists.index_of(name))
        .map(|index| &lists.lists()[index]);
    remembered
        .or_else(|| lists.lists().first())
        .ok_or_else(|| anyhow!("no feed lists are configured"))
}

/// What an import did, for the one-line message at the end.
#[derive(Debug, PartialEq, Eq)]
pub struct ImportSummary {
    /// Feeds that were new and have been added to the sources file.
    pub added: usize,
    /// Feeds skipped because the list already has them (or the file listed
    /// them more than once).
    pub already_there: usize,
    /// Entries skipped because they could not be used, for example an
    /// address that is not `http` or `https`.
    pub ignored: usize,
}

/// Adds the feeds from the OPML `text` to the sources file at `path`.
///
/// The text is parsed *first*, before the sources file is looked at, so a file
/// that is not OPML changes nothing. A feed whose address is already in the
/// list (compared ignoring capitals, and on the same terms the export uses, so
/// a Reddit source counts as its `.rss` address) is skipped. All the new feeds
/// are then added in one write that keeps the file's comments.
///
/// A sources file that does not exist yet is treated as an empty list and
/// created, which is how a brand-new list gets its first feeds.
fn import_into(path: &Path, text: &str) -> anyhow::Result<ImportSummary> {
    // Step 1: read the OPML. `?` returns early with the error if the text is
    // not usable OPML, before anything has been changed.
    let parsed = parse_opml(text)?;

    // Step 2: the addresses the list already has, lower-cased, in a
    // `HashSet` (a collection that answers "is this in here?" very fast).
    // A missing file simply means the list is empty.
    let existing = if path.exists() {
        load_sources_at(path, false)?
    } else {
        Vec::new()
    };
    let mut known: HashSet<String> = existing
        .iter()
        .map(|source| feed_address(source).to_lowercase())
        .collect();

    // Step 3: keep the feeds that are new. `insert` returns `true` if the
    // address was not in the set yet (and adds it), so the same address
    // appearing twice in a row cannot slip through twice.
    let mut fresh: Vec<SourceConfig> = Vec::new();
    let mut already_there = parsed.repeated;
    for feed in parsed.feeds {
        if known.insert(feed.url.to_lowercase()) {
            fresh.push(feed);
        } else {
            already_there += 1;
        }
    }

    // Step 4: write all the new feeds at once (and only if there are any, so
    // an import that adds nothing does not even touch the file).
    if !fresh.is_empty() {
        append_sources_at(path, &fresh)?;
    }

    Ok(ImportSummary {
        added: fresh.len(),
        already_there,
        ignored: parsed.ignored,
    })
}

/// Reads the whole text of the OPML file at `path`, refusing anything bigger
/// than [`MAX_OPML_BYTES`] without loading all of it first.
fn read_opml_file(path: &Path) -> anyhow::Result<String> {
    let file =
        std::fs::File::open(path).with_context(|| format!("could not open {}", path.display()))?;

    // `take(n)` makes the reader stop after `n` bytes, so a huge file is
    // never read into memory in full. One byte more than the limit is read so
    // that "exactly at the limit" and "over it" can be told apart. (`as u64`
    // converts the number type, because `take` wants a `u64`.)
    let mut text = String::new();
    file.take(MAX_OPML_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .with_context(|| format!("{} is not a text file", path.display()))?;
    if text.len() > MAX_OPML_BYTES {
        bail!(
            "{} is too big to be a feed list (the limit is {} MB)",
            path.display(),
            MAX_OPML_BYTES / (1024 * 1024)
        );
    }
    Ok(text)
}

/// Builds the OPML text for the feed list whose sources file is at `path`.
///
/// Unlike the reader, this does not create a missing file: exporting a list
/// that does not exist is a mistake worth telling the person about.
fn export_from(path: &Path) -> anyhow::Result<String> {
    // Say plainly what is wrong when the list has no file yet, instead of
    // showing the operating system's bare "No such file or directory".
    if !path.exists() {
        bail!(
            "there is nothing to export: {} does not exist yet (open tuxwire once, or import a file, to create it)",
            path.display()
        );
    }
    let sources = load_sources_at(path, false)?;
    Ok(write_opml(&sources))
}

/// Prints `text` to the screen without panicking if the reader closes early.
///
/// `print!` panics when the output goes to a pipe that has been closed, for
/// example `tuxwire export-opml - | head -3` (`head` quits after three lines).
/// Here that case (`BrokenPipe`) is treated as a normal end instead, because
/// the person simply stopped reading. Any other write problem is a real error.
fn print_text(text: &str) -> anyhow::Result<()> {
    use std::io::Write;

    // `lock` holds the screen output for the whole write; `write_all` sends
    // every byte, and `flush` pushes anything still waiting in the buffer.
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.context("could not write to the screen"),
    }
}

/// Saves `text` to `destination`, or prints it when the destination is `-`.
///
/// An existing file is never overwritten: a mistyped name could otherwise
/// destroy something that matters. `create_new(true)` asks the operating
/// system to create the file only if it does not exist, and to fail if it
/// does, which is safer than checking first and writing second (the file
/// could appear in between).
fn save_export(destination: &Path, text: &str) -> anyhow::Result<()> {
    use std::io::Write;

    // "-" is the usual command-line way of saying "the screen".
    if destination == Path::new("-") {
        return print_text(text);
    }

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                anyhow!(
                    "{} already exists; choose another name, or remove it first",
                    destination.display()
                )
            } else {
                anyhow!("could not create {}: {err}", destination.display())
            }
        })?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("could not write {}", destination.display()))?;
    Ok(())
}

/// Carries out a [`Command`] and prints what happened.
///
/// `lists` is the set of feed lists found in the person's config. Errors come
/// back as `Err` so `main` can show them and exit with a failure status.
pub fn run(command: Command, lists: &FeedLists) -> anyhow::Result<()> {
    match command {
        // The help text goes to the screen and nothing else happens.
        Command::Help => print_text(&format!("{USAGE}\n")),
        Command::Import { file, list } => {
            // Choose the list, read the file, add the feeds, then report.
            let target = pick_list(lists, list.as_deref())?;
            let text = read_opml_file(&file)?;
            let summary = import_into(&target.path, &text)?;
            println!(
                "Imported into list {:?} ({}): {} added, {} already there, {} ignored.",
                target.name,
                target.path.display(),
                summary.added,
                summary.already_there,
                summary.ignored
            );
            if summary.added > 0 {
                println!("Their articles are downloaded the next time you open tuxwire.");
            }
            Ok(())
        }
        Command::Export { file, list } => {
            // Choose the list, build the text, then save or print it.
            let target = pick_list(lists, list.as_deref())?;
            let text = export_from(&target.path)?;
            save_export(&file, &text)?;
            // When printing to the screen the OPML is the output, so the
            // confirmation line is left out to keep that output clean.
            if file != Path::new("-") {
                println!("Saved list {:?} to {}.", target.name, file.display());
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuxwire_core::fetchers::SourceType;

    /// Turns string literals into the `Vec<String>` the parser takes.
    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|word| word.to_string()).collect()
    }

    /// A new empty folder under the system temp folder, unique to this call so
    /// tests running side by side never share files. These tests never touch
    /// the real config or database.
    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        // A counter shared by every test: each call takes the next number.
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let number = NEXT.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("tuxwire-opml-cli-{}-{number}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A small OPML file: two feeds in a folder, one outside any folder, one
    /// with an address that cannot be used, and one repeated address.
    const SAMPLE: &str = r#"<?xml version="1.0"?>
<opml version="2.0"><body>
  <outline text="News">
    <outline type="rss" text="Alpha" xmlUrl="https://alpha.example/feed"/>
    <outline type="rss" text="Beta" xmlUrl="https://beta.example/feed"/>
    <outline type="rss" text="Alpha again" xmlUrl="https://alpha.example/feed"/>
  </outline>
  <outline type="rss" text="Loose" xmlUrl="https://loose.example/feed"/>
  <outline type="rss" text="Bad" xmlUrl="ftp://bad.example/feed"/>
</body></opml>"#;

    /// A sources file with a hand-written comment, to prove comments survive.
    const EXISTING: &str = "# my own note\n\n[[source]]\nname = \"Alpha\"\ntype = \"rss\"\nurl = \"https://ALPHA.example/feed\"\ntopic = \"Mine\"\n";

    /// No words means "open the reader".
    #[test]
    fn no_arguments_opens_the_reader() {
        assert_eq!(parse_args(&[]), Ok(None));
    }

    /// Both commands read a file, and `--list` works in both spellings.
    #[test]
    fn commands_read_a_file_and_an_optional_list() {
        assert_eq!(
            parse_args(&words(&["import-opml", "feeds.opml"])),
            Ok(Some(Command::Import {
                file: PathBuf::from("feeds.opml"),
                list: None
            }))
        );
        assert_eq!(
            parse_args(&words(&["export-opml", "--list", "casual", "out.opml"])),
            Ok(Some(Command::Export {
                file: PathBuf::from("out.opml"),
                list: Some("casual".to_string())
            }))
        );
        assert_eq!(
            parse_args(&words(&["import-opml", "a.opml", "--list=work"])),
            Ok(Some(Command::Import {
                file: PathBuf::from("a.opml"),
                list: Some("work".to_string())
            }))
        );
        assert_eq!(parse_args(&words(&["--help"])), Ok(Some(Command::Help)));
    }

    /// Mistakes are errors that say what was wrong, never a silent fallback.
    #[test]
    fn mistakes_are_reported() {
        for bad in [
            vec!["import-opm", "a.opml"],
            vec!["import-opml"],
            vec!["import-opml", "a.opml", "b.opml"],
            vec!["import-opml", "a.opml", "--list"],
            vec!["import-opml", "a.opml", "--bogus"],
        ] {
            assert!(
                parse_args(&words(&bad)).is_err(),
                "{bad:?} should be refused"
            );
        }
    }

    /// Builds two lists ("main" and "casual") from `lists.toml` text, the way
    /// the real app does, with `casual` remembered as the active one.
    fn two_lists(remembered: &str) -> FeedLists {
        let text = format!(
            "active = \"{remembered}\"\n[[list]]\nname = \"main\"\npath = \"main.toml\"\n[[list]]\nname = \"casual\"\npath = \"casual.toml\"\n"
        );
        crate::feedlists::config::parse(
            &text,
            Path::new("/cfg"),
            None,
            Path::new("/cfg/sources.toml"),
        )
        .unwrap()
    }

    /// `--list` picks by name ignoring case, a wrong name lists the real
    /// ones, and with no name the remembered list wins.
    #[test]
    fn the_right_list_is_chosen() {
        let lists = two_lists("casual");
        assert_eq!(pick_list(&lists, Some("MAIN")).unwrap().name, "main");
        assert_eq!(pick_list(&lists, None).unwrap().name, "casual");
        let message = pick_list(&lists, Some("nope")).unwrap_err().to_string();
        assert!(message.contains("main") && message.contains("casual"));

        // A remembered name that matches nothing falls back to the first.
        let stale = two_lists("gone");
        assert_eq!(pick_list(&stale, None).unwrap().name, "main");
    }

    /// New feeds are added under their topics, repeats and unusable entries
    /// are counted, comments are kept, and a feed already in the list (even
    /// with different capitals) is not added twice.
    #[test]
    fn import_adds_only_new_feeds_and_keeps_comments() {
        let path = temp_dir().join("sources.toml");
        std::fs::write(&path, EXISTING).unwrap();

        let summary = import_into(&path, SAMPLE).unwrap();
        assert_eq!(
            summary,
            ImportSummary {
                added: 2,
                // Alpha is already in the list, and "Alpha again" repeats it.
                already_there: 2,
                ignored: 1,
            }
        );

        // The comment is still there, and the new feeds are in their topics.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# my own note"));
        let sources = load_sources_at(&path, false).unwrap();
        let names: Vec<(&str, &str)> = sources
            .iter()
            .map(|s| (s.name.as_str(), s.topic.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("Alpha", "Mine"),
                ("Beta", "News"),
                ("Loose", "Uncategorized")
            ]
        );

        // Importing the same file again changes nothing at all.
        let before = std::fs::read_to_string(&path).unwrap();
        let again = import_into(&path, SAMPLE).unwrap();
        assert_eq!(again.added, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    /// A file that is not OPML is an error and leaves the sources file
    /// exactly as it was.
    #[test]
    fn a_bad_file_changes_nothing() {
        let path = temp_dir().join("sources.toml");
        std::fs::write(&path, EXISTING).unwrap();
        assert!(import_into(&path, "<html>not a feed list</html>").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXISTING);
    }

    /// A list whose file does not exist yet is created by the first import.
    #[test]
    fn a_missing_sources_file_is_created() {
        let path = temp_dir().join("brand-new").join("later.toml");
        let summary = import_into(&path, SAMPLE).unwrap();
        assert_eq!(summary.added, 3);
        assert_eq!(load_sources_at(&path, false).unwrap().len(), 3);
    }

    /// Exporting and importing again gives the same feeds back, which proves
    /// the two commands fit together.
    #[test]
    fn export_then_import_round_trips() {
        let first = temp_dir().join("one.toml");
        import_into(&first, SAMPLE).unwrap();
        let exported = export_from(&first).unwrap();

        let second = temp_dir().join("two.toml");
        let summary = import_into(&second, &exported).unwrap();
        assert_eq!(summary.added, 3);
        let urls = |path: &Path| -> Vec<String> {
            load_sources_at(path, false)
                .unwrap()
                .iter()
                .map(|s| s.url.clone())
                .collect()
        };
        assert_eq!(urls(&first), urls(&second));
    }

    /// Exporting a list whose sources file does not exist says so plainly.
    #[test]
    fn exporting_a_missing_list_is_explained() {
        let err = export_from(&temp_dir().join("nope.toml")).unwrap_err();
        assert!(err.to_string().contains("nothing to export"));
    }

    /// Exporting never overwrites a file that is already there.
    #[test]
    fn export_refuses_to_overwrite() {
        let file = temp_dir().join("out.opml");
        save_export(&file, "<opml/>").unwrap();
        let err = save_export(&file, "something else").unwrap_err();
        assert!(err.to_string().contains("already exists"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "<opml/>");
    }

    /// A file over the size limit is refused while reading, with a clear
    /// message, and a missing file is a clear error too.
    #[test]
    fn oversized_and_missing_files_are_refused() {
        let dir = temp_dir();
        let big = dir.join("big.opml");
        std::fs::write(&big, vec![b'x'; MAX_OPML_BYTES + 1]).unwrap();
        assert!(
            read_opml_file(&big)
                .unwrap_err()
                .to_string()
                .contains("too big")
        );
        assert!(read_opml_file(&dir.join("missing.opml")).is_err());
        // A file exactly at the limit is accepted.
        let exact = dir.join("exact.opml");
        std::fs::write(&exact, vec![b'x'; MAX_OPML_BYTES]).unwrap();
        assert_eq!(read_opml_file(&exact).unwrap().len(), MAX_OPML_BYTES);
    }

    /// A Reddit source in the list counts as its `.rss` address, so importing
    /// that address does not add the same subreddit a second time.
    #[test]
    fn a_reddit_source_counts_as_its_feed_address() {
        let path = temp_dir().join("sources.toml");
        std::fs::write(
            &path,
            "[[source]]\nname = \"r/linux\"\ntype = \"reddit\"\nurl = \"linux\"\ntopic = \"Distros\"\n",
        )
        .unwrap();
        let opml = r#"<opml version="2.0"><body>
          <outline type="rss" text="Linux" xmlUrl="https://www.reddit.com/r/linux/.rss"/>
        </body></opml>"#;
        let summary = import_into(&path, opml).unwrap();
        assert_eq!((summary.added, summary.already_there), (0, 1));
        // `SourceType` is used so the import is checked to stay out of it.
        assert!(matches!(
            load_sources_at(&path, false).unwrap()[0].source_type,
            SourceType::Reddit
        ));
    }
}
