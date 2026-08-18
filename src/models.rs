//! Shared data types used across the whole app.
//!
//! `Article` is the one type every layer agrees on: fetchers (RSS, Reddit,
//! HN, ...) all produce `Article`s regardless of where the data came from,
//! storage persists `Article`s, and the TUI displays `Article`s. This is
//! the "normalize at the boundary" pattern — the messy, format-specific
//! parsing happens once, inside each fetcher, and everything downstream of
//! that only ever has to understand one shape.

use chrono::{DateTime, Utc};
// `serde::Serialize` / `serde::Deserialize` are *traits* (see the note
// below on what a trait is) that describe "this type knows how to turn
// itself into some external format" and "...knows how to be built back up
// from one." We bring both into scope so we can `#[derive]` them on
// `Article`.
use serde::{Deserialize, Serialize};

/// A single news article/post, already normalized into a common shape
/// regardless of which fetcher produced it (RSS, Reddit JSON, HN/Algolia,
/// ...). This mirrors the `Data Model` section of `docs/ARCHITECTURE.md`
/// exactly — if you change one, change the other.
///
/// ## A note on `derive`
///
/// `#[derive(...)]` is a *macro* that runs at compile time and writes
/// extra code for this struct automatically, so we don't have to write it
/// by hand:
///
/// - `Debug` — lets us print an `Article` with `{:?}` for debugging
///   (println!("{:?}", article)). Without it, `Article` simply can't be
///   formatted that way; the compiler would refuse to compile a `{:?}`
///   print of it.
/// - `Clone` — lets us explicitly duplicate an `Article` by calling
///   `.clone()`. This matters because of Rust's *ownership* model: unlike
///   Python/JS, assigning or passing a value normally *moves* it (the
///   original variable becomes unusable) rather than copying a reference
///   to shared, mutable memory. `Clone` opts a type into "make me a real,
///   independent copy on request" — Rust never clones implicitly/silently.
/// - `Serialize` / `Deserialize` (from serde) — generates the code to
///   convert an `Article` to/from formats like JSON, so we can hand one to
///   `serde_json` without writing a manual conversion function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Article {
    /// SQLite row ID. `i64` because that's the integer type SQLite's
    /// `INTEGER PRIMARY KEY` maps onto. Articles that only exist
    /// in-memory (freshly fetched, not yet inserted into the database)
    /// don't have a real ID yet — see the note on `id` below.
    pub id: i64,

    /// The article's headline, as published.
    ///
    /// This is an *owned* `String` rather than a borrowed `&str`. The
    /// difference matters a lot in Rust and comes up constantly:
    /// - `String` owns its text data on the heap; it can live as long as
    ///   it needs to and be passed around freely.
    /// - `&str` is a *borrowed reference* to text owned by something
    ///   else; it's only valid as long as whatever it's borrowed from is
    ///   still alive (the compiler enforces this — a `&str` outliving its
    ///   source data is a compile error, not a runtime crash).
    ///
    /// An `Article` needs to outlive the parsed feed response it was
    /// extracted from (it gets stored, displayed later, etc.), so it must
    /// *own* its strings rather than borrow them.
    pub title: String,

    /// The article's own URL (where `Enter` in the TUI will open it).
    pub url: String,

    /// Human-readable source name, e.g. `"Phoronix"` — matches the
    /// `name` field of the `[[source]]` entry in `sources.toml` that
    /// produced this article.
    pub source: String,

    /// Which topic this article is filed under, for the sidebar in the
    /// TUI. Sources are assigned a topic when added (see
    /// `ARCHITECTURE.md`'s "Adding Sources" section) — a fetcher doesn't
    /// decide this itself.
    pub topic: String,

    /// When the article was published, according to the source feed.
    /// `DateTime<Utc>` (from the `chrono` crate) stores an instant in
    /// time normalized to UTC, sidestepping timezone bugs — feeds publish
    /// timestamps in all sorts of local offsets, and we want one
    /// consistent value to sort/compare against.
    pub timestamp: DateTime<Utc>,

    /// Has this article been opened (or saved — saving implies read; see
    /// `ARCHITECTURE.md`'s "Article States & Behavior").
    pub read: bool,

    /// Was this article explicitly dismissed with `x`? Feeds the
    /// skip-weighting system in `scoring.rs`.
    pub skipped: bool,

    /// Has the user saved this article for later? Saved articles persist
    /// indefinitely and support a note (see `note` below).
    pub saved: bool,

    /// An optional free-text note attached when saving. `Option<String>`
    /// is Rust's way of expressing "this might not exist" *in the type
    /// itself*, rather than using a sentinel like `null` or an empty
    /// string that every caller has to remember to check for. There are
    /// exactly two variants: `Some(String)` (a note is present) and
    /// `None` (no note). The compiler forces every piece of code that
    /// reads `note` to explicitly handle *both* cases (typically via `if
    /// let Some(text) = &note` or a `match`) — it's simply not possible to
    /// accidentally use a "null" note as if it were text, the way you
    /// could with `null` in many other languages.
    pub note: Option<String>,
}

impl Article {
    /// Builds a freshly-fetched `Article` that doesn't exist in the
    /// database yet.
    ///
    /// A fetcher only knows about the four things a feed entry actually
    /// tells it — title, url, timestamp, and (via which fetcher/source
    /// config called it) the source name and topic. Everything else
    /// (`read`, `skipped`, `saved`, `note`) has an obvious default for a
    /// brand new article, and `id` doesn't exist yet because SQLite
    /// assigns it on insert (that's what `INTEGER PRIMARY KEY` does — the
    /// database, not our code, is the source of truth for IDs). We use
    /// `0` as a placeholder; storage code overwrites it with the real
    /// row ID after inserting.
    ///
    /// This constructor pattern — `Type::new(...)` as a plain associated
    /// function, not tied to any trait — is the idiomatic Rust equivalent
    /// of a constructor in other languages. Rust structs have no
    /// built-in concept of a constructor; `new` is just a convention.
    pub fn new(title: String, url: String, source: String, topic: String, timestamp: DateTime<Utc>) -> Self {
        // `Self` here refers to `Article` — inside an `impl Article`
        // block, `Self` is shorthand for the type being implemented, so
        // renaming `Article` later wouldn't require updating this line.
        Self {
            id: 0,
            title,
            url,
            source,
            topic,
            timestamp,
            // Struct field shorthand above (`title,` instead of `title:
            // title,`) works because the parameter names match the field
            // names exactly — Rust fills in the rest for you.
            read: false,
            skipped: false,
            saved: false,
            note: None,
        }
    }
}
