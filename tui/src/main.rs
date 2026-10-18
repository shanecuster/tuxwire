//! tuxwire — a terminal-based Linux news aggregator.
//!
//! See `docs/ARCHITECTURE.md` for the full design. `main` picks the active
//! feed list (`lists.toml`, see `feedlists`; normally just `sources.toml`),
//! loads its sources (see `fetchers::configured_sources_at`), fetches every
//! one of them, inserts whatever comes back into the real on-disk
//! database (creating and migrating it if necessary), and then hands off
//! to the TUI (`ui::run`). The skip-weighting layer described in the
//! architecture doc isn't wired up yet -- `scoring.rs` exists as a
//! documented placeholder so the project layout matches the plan from the
//! start.
//!
//! ## The module tree, and why these lines have to be here
//!
//! Unlike some languages where any file in the project is automatically
//! part of the program, Rust requires every module to be explicitly
//! declared somewhere, starting from the crate root (`main.rs` for a
//! binary). A `mod foo;` line does two things: it tells the compiler
//! "look for `foo.rs` or `foo/mod.rs` and include it," and it creates the
//! `foo::` path other code uses to reach items inside it. Order doesn't
//! matter here — these declarations just make each corresponding
//! file part of the compiled program.
mod export;
mod feed_sync;
mod feedlists;
mod fetchers;
mod icons;
mod opml_cli;
mod paths;
mod sync_cli;
mod sync_config;
mod theme;
mod ui;

use theme::ThemeSettings;
use tuxwire_core::fetch_report::FetchReport;

/// `#[tokio::main]` is a *macro* that rewrites this function at compile
/// time. Rust's real `fn main()` can't be `async` — something has to own
/// starting up the async runtime before any `.await` can run — so this
/// attribute generates an ordinary, synchronous `fn main()` that spins up
/// a `tokio` runtime and immediately runs the `async` body you wrote as a
/// task on it. It's a convenience: without it, we'd have to write that
/// runtime setup by hand every time. Everything below `.await`s freely
/// because we're now running inside tokio's runtime.
///
/// The return type `anyhow::Result<()>` (shorthand for `Result<(), anyhow::Error>`)
/// means `main` itself can fail and propagate an error with `?`, same as
/// any other function — if it returns `Err`, the runtime prints the error
/// and exits with a non-zero status instead of panicking.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Which feed lists exist: `~/.config/tuxwire/lists.toml`, or, without
    // it, the single implicit list on `~/.config/tuxwire/sources.toml`.
    // `?` because a malformed `lists.toml` is a real mistake to show, the
    // same policy as every other config file.
    let default_sources = fetchers::config::sources_path()?;
    let feed_lists = feedlists::config::load(&default_sources)?;

    // Command-line words after `tuxwire` (`import-opml`, `export-opml`,
    // `--help`) run one small job and exit *instead of* opening the reader.
    // `skip(1)` drops the program's own name, which is always the first
    // item. With no words, `parse_args` says `None` and the reader starts as
    // usual. A mistake is turned into an error here (`anyhow::bail!` returns
    // early with a message) so a typo never silently opens the full-screen app.
    let words: Vec<String> = std::env::args().skip(1).collect();

    // `tuxwire sync` (and `sync --test`) is the optional CouchDB sync. It
    // is tried first so its words never reach the OPML parser below, and it
    // returns before any feed is downloaded. A mistake after `sync` is
    // reported the same way as an OPML one.
    match sync_cli::parse_args(&words) {
        Ok(Some(command)) => return sync_cli::run(command).await,
        Ok(None) => {}
        Err(message) => anyhow::bail!("{message}\n\n{}", opml_cli::USAGE),
    }
    match opml_cli::parse_args(&words) {
        Ok(Some(command)) => return opml_cli::run(command, &feed_lists),
        Ok(None) => {}
        Err(message) => anyhow::bail!("{message}\n\n{}", opml_cli::USAGE),
    }
    let home = paths::home_dir();

    // Pick the active list and load its sources (just the file; nothing is
    // fetched yet). Two cases:
    //
    // - One list: exactly as before lists existed. `sources.toml` is
    //   created from the bundled sample if missing, and a malformed file
    //   is a clean startup error (`?`), since there's nothing to fall back
    //   to.
    // - Several lists: `feedlists::activate` tries the remembered list,
    //   then the others in order. A broken file becomes a footer message
    //   (`list_notice`) and another list is used. If none loads, the TUI
    //   still opens, empty, with the message saying why.
    //
    // Only the active list's sources are fetched below; the other lists
    // aren't refreshed until they're switched to and `r` is pressed.
    let (active_list, sources, list_notice) = if feed_lists.is_single() {
        let sources = fetchers::configured_sources_at(&feed_lists.lists()[0].path)?;
        (0, sources, None)
    } else {
        match feedlists::activate(&feed_lists, home.as_deref(), |list| {
            fetchers::configured_sources_at(&list.path)
        }) {
            Ok(chosen) => (chosen.index, chosen.loaded, chosen.notice),
            Err(message) => (0, Vec::new(), Some(message)),
        }
    };
    let mut articles = Vec::new();

    // Tallies this startup fetch (sources tried/failed/unreachable, and
    // later which articles were new) for the TUI's first footer message
    // and its "new" article markers. See `fetch_report.rs`. `mut` because
    // the loops below update it.
    let mut report = FetchReport::default();

    // The fetch results, kept whole so they can be stored (and counted) later
    // by `FetchReport::record_outcome`, once the database is open. Each is a
    // `FetchOutcome`: the source's name plus either its articles or the error.
    let mut outcomes = Vec::new();

    for source in &sources {
        println!("Fetching {}...", source.name());

        // `fetch_one` (shared with the refresh keybind and the Android app)
        // returns the failure *inside* the outcome rather than as an `Err` to
        // propagate with `?`. Per ARCHITECTURE.md, "one slow/dead source never
        // blocks a refresh of everything else" -- a dead network, a 404, or
        // (for now) any `type = "reddit"` source, see `fetchers::reddit`,
        // prints a message here and the loop moves on to the next source.
        let outcome = fetchers::fetch_one(source).await;

        // `&outcome.result` borrows the result so the outcome stays whole for
        // storing afterwards. `fetched.iter().cloned()` copies the articles
        // into `articles`, which is only used for the printout below.
        match &outcome.result {
            Ok(fetched) => articles.extend(fetched.iter().cloned()),
            Err(err) => eprintln!("  skipped {}: {err:#}", outcome.name),
        }
        outcomes.push(outcome);
    }

    println!("Got {} articles:\n", articles.len());

    // Print each article's title and URL. `&articles` (a *reference* to
    // the `Vec`, via `for article in &articles`) is used instead of
    // `articles` so this loop *borrows* each `Article` instead of
    // *consuming* the `Vec` — we're only reading here, not moving
    // ownership away, and borrowing is the right default whenever a value
    // still needs to exist afterwards (not required in this specific
    // program since nothing happens after the loop, but it's the habit
    // worth having: prefer borrowing over taking ownership unless you
    // actually need to consume the value).
    for article in &articles {
        println!("- {}", article.title);
        println!("  {}", article.url);
    }

    // Third milestone: persist what was fetched above. `Storage::open()`
    // resolves `~/.local/share/tuxwire` (creating it if this is the first
    // run), opens (or creates) `tuxwire.db` inside it, and runs every
    // migration in `storage/migrations.rs` against it -- all of which
    // either succeeds silently or returns an `Err` that the `?` below
    // propagates out of `main`, same as the fetch above.
    println!("\nOpening storage...");
    let storage = paths::open_storage()?;
    println!("Storage ready at ~/.local/share/tuxwire/tuxwire.db (migrations up to date).");

    // Best-effort backfill of `feed_url` (migration 005) for articles
    // stored before it existed, by matching their `source` name against
    // every list's configured sources, so old articles land in the right
    // list. Other lists' files are only *read* here (TOML parsing, no
    // network), and ones that fail to load are skipped (they were already
    // reported above if they matter). Rows that already have a `feed_url`
    // are untouched, so this is a cheap no-op on every later startup.
    //
    // `flat_map` turns "each list → its sources" into one long list of
    // sources. `.ok()` drops lists whose file can't be loaded, and
    // `.unwrap_or_default()` makes that an empty `Vec`.
    let source_feeds: Vec<(String, String)> = feed_lists
        .lists()
        .iter()
        .enumerate()
        .flat_map(|(index, list)| {
            if index == active_list {
                // Already loaded above; no need to read it again.
                sources
                    .iter()
                    .map(|source| (source.name().to_string(), source.url().to_string()))
                    .collect::<Vec<_>>()
            } else {
                fetchers::configured_sources_at(&list.path)
                    .ok()
                    .unwrap_or_default()
                    .iter()
                    .map(|source| (source.name().to_string(), source.url().to_string()))
                    .collect()
            }
        })
        .collect();
    let filled = storage.backfill_feed_urls(&source_feeds)?;
    if filled > 0 {
        println!("Recorded the feed for {filled} older article(s).");
    }

    // `insert_article` is idempotent on `url` (see its doc comment in
    // `storage/mod.rs`), so running this binary again against the same
    // feed re-inserts nothing -- every article that was already on disk
    // just gets its existing row's ID handed back instead of a new row.
    // `record_insert` does that same insert and also notes which IDs were
    // genuinely new (see its doc comment for how).
    //
    // `record_outcome` also counts each source as attempted, and each failed
    // one as failed, so the TUI can say "offline" or "N sources failed" inside
    // the alternate screen where the `eprintln!` above is hidden.
    for outcome in outcomes {
        report.record_outcome(&storage, outcome)?;
    }

    println!(
        "Storage now holds {} article(s) total.",
        storage.article_count()?
    );

    // Fourth milestone: the TUI shell itself. `ThemeSettings::load` reads
    // `~/.config/tuxwire/theme.toml` (the selected built-in theme plus any
    // color overrides), falling back to the bundled default, Everforest,
    // if that file doesn't exist yet -- see `theme.rs`. `ui::run` takes it
    // from there: raw mode, the
    // alternate screen, and the two-pane layout + footer, all restored
    // back to a normal terminal the moment this returns. Everything
    // printed above (the fetch log, the storage summary) will already have
    // scrolled off by the time this happens, since entering the alternate
    // screen swaps to a blank buffer -- that's expected, not a bug losing
    // that output.
    let themes = ThemeSettings::load()?;

    // Icon settings from `~/.config/tuxwire/icons.toml`, or the bundled
    // defaults (Nerd mode) when that file doesn't exist yet. A missing
    // file also means the first-run "Do you see an icon here?" check
    // hasn't been answered, so `first_run` asks the TUI to open with it.
    // `?` for the same reason as `ThemeSettings::load`: a malformed `icons.toml`
    // should be reported, not silently ignored.
    let icons = icons::config::load()?;
    let session = ui::SessionStart {
        themes,
        icons: icons.settings,
        show_icon_check: icons.first_run,
        report,
        feed_lists,
        active_list,
        list_notice,
    };

    ui::run(&storage, &sources, session)?;

    Ok(())
}
