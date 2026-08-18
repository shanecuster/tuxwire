//! tuxwire — a terminal-based Linux news aggregator.
//!
//! See `docs/ARCHITECTURE.md` for the full design. This file is currently
//! just enough to prove the first milestone works end to end: fetch one
//! real RSS feed and print what came back. The TUI, storage, and
//! skip-weighting layers described in the architecture doc aren't wired
//! up yet — their modules exist as documented placeholders (see
//! `storage/mod.rs`, `scoring.rs`, `theme.rs`, `ui/mod.rs`) so the project
//! layout matches the plan from the start.
//!
//! ## The module tree, and why these lines have to be here
//!
//! Unlike some languages where any file in the project is automatically
//! part of the program, Rust requires every module to be explicitly
//! declared somewhere, starting from the crate root (`main.rs` for a
//! binary). A `mod foo;` line does two things: it tells the compiler
//! "look for `foo.rs` or `foo/mod.rs` and include it," and it creates the
//! `foo::` path other code uses to reach items inside it. Order doesn't
//! matter here — these five declarations just make each corresponding
//! file part of the compiled program.
mod fetchers;
mod models;
mod scoring;
mod storage;
mod theme;
mod ui;

// Bringing `Fetcher` into scope is what makes `.fetch()` callable below.
// This is a Rust-specific quirk worth calling out: trait *methods* are
// only usable on a value if the trait itself is `use`d somewhere in the
// current file, even though the concrete type (`RssFetcher`) is already
// imported. It's a deliberate design choice — it means a method call
// can never silently come from some trait you didn't know was in play;
// every trait providing methods you use has to be named explicitly.
use fetchers::Fetcher;
use fetchers::rss::RssFetcher;

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
    // For this milestone, the source is hardcoded rather than loaded from
    // `sources.toml` (config loading isn't built yet). This mirrors
    // exactly one `[[source]]` entry from the example in
    // ARCHITECTURE.md's Configuration section.
    let phoronix = RssFetcher {
        name: String::from("Phoronix"),
        url: String::from("https://www.phoronix.com/rss.php"),
        topic: String::from("linux-news"),
    };

    println!("Fetching {}...", phoronix.name);

    // `.fetch().await?` — call the async method, wait for it to finish,
    // and propagate any error up out of `main` (which is what triggers
    // the "print the error and exit non-zero" behavior mentioned above).
    let articles = phoronix.fetch().await?;

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

    Ok(())
}
