# tuxwire — Architecture

A terminal-based Linux news aggregator. Pulls articles from multiple sources,
lets you filter by topic, learns what you don't care about over time, and
lets you save articles with personal notes for later reference.

This document is the source of truth for how the project is structured and
why. Update it whenever a real architectural decision is made — this file
should always reflect the *current* state of the project, not just the v1 plan.

---

## Tech Stack

- **Language:** Rust
- **TUI framework:** [ratatui](https://ratatui.rs)
- **Async runtime:** tokio
- **HTTP client:** reqwest
- **Serialization:** serde (+ serde_json for APIs, toml for config)
- **Database:** SQLite via rusqlite (embedded, no server)
- **Editor:** Zed (rust-analyzer built in, no config needed)

---

## Why Rust + ratatui

- Single static binary, fast startup — this app lives in a Hyprland
  scratchpad and gets toggled open/closed constantly. Startup latency matters.
- No garbage collector, predictable resource use for something running
  in the background all day.
- ratatui's widget set (List, Tabs, Paragraph, Block) maps directly onto
  the layout this app needs.

---

## High-Level Architecture

Three layers, kept deliberately separate so each can be worked on
independently:

```
┌─────────────┐     ┌──────────────┐     ┌─────────────┐
│  Fetchers    │ --> │   Storage    │ --> │     TUI      │
│ (RSS, Reddit,│     │  (SQLite)    │     │  (ratatui)   │
│  HN, custom)  │     │              │     │              │
└─────────────┘     └──────────────┘     └─────────────┘
```

### 1. Fetchers

Each source type (RSS/Atom, Reddit JSON, HN/Algolia, future custom sources)
implements a common `Fetcher` trait. Every fetcher normalizes whatever it
pulls down into the same `Article` struct — the rest of the app never needs
to know or care where an article came from.

Fetches run concurrently via tokio, so one slow/dead source never blocks a
refresh of everything else.

**Adding a new source should never require touching fetcher logic** unless
the source has no RSS feed. See `docs/ADDING_A_SOURCE.md`.

### 2. Storage (SQLite)

Embedded via rusqlite — no server, no daemon, just a `.db` file on disk.

- **Location:** `~/.local/share/tuxwire/tuxwire.db`
- **Config lives separately** at `~/.config/tuxwire/` (`sources.toml`,
  `theme.toml`) — data and config are never mixed.

#### Schema (v1)

```
articles
  id            INTEGER PRIMARY KEY
  title         TEXT
  url           TEXT
  source        TEXT
  topic         TEXT
  timestamp     TEXT
  read          BOOLEAN DEFAULT 0
  skipped       BOOLEAN DEFAULT 0
  saved         BOOLEAN DEFAULT 0
  note          TEXT NULL

skip_weights
  keyword       TEXT PRIMARY KEY
  weight        INTEGER DEFAULT 0
```

#### Migrations

Every schema change ships as a numbered SQL file in `migrations/`
(e.g. `002_add_priority.sql`). On startup, tuxwire checks how many
migrations have already run (`PRAGMA user_version` or a small
migrations-log table) and applies any new ones, in order, automatically.

**Rule: no destructive schema changes.** Existing saved articles and notes
must survive every update. This matters more here than in most projects —
saved articles are meant to persist indefinitely until manually removed.

### 3. TUI (ratatui)

- **Left pane:** topic list, toggle/select topics
- **Right pane:** article list for the selected topic, sorted by recency
- **Footer:** keybind hints + last-refresh timestamp
- **Saved view:** a dedicated view/pane filtering to `saved = true` across
  all topics, independent of the topic sidebar

#### Keybinds (v1)

| Key       | Action                          |
|-----------|----------------------------------|
| `j` / `k` | navigate list                   |
| `Enter`   | open article in `$BROWSER`      |
| `x`       | skip article (feeds skip-weighting) |
| `s`       | save article (auto-marks as read) |
| `n`       | edit note on a saved article     |
| `r`       | refresh all sources               |
| `S`       | view saved articles               |
| `a`       | add a new source                  |
| `q`       | quit                             |

---

## Article States & Behavior

| State     | Meaning                                  | Notes                              |
|-----------|-------------------------------------------|-------------------------------------|
| unread    | default state, not yet interacted with    |                                      |
| read      | opened, or saved (saving implies read)     |                                      |
| skipped   | explicitly dismissed                       | feeds the skip-weighting system     |
| saved     | permanent until manually unsaved           | supports an optional free-text note |

**Saving auto-marks as read** — these are not tracked as independent
booleans in practice; pressing `s` sets both `saved = true, read = true`
in one action.

---

## The "Learning" System (Skip-Weighting)

Not machine learning — a simple, transparent keyword/topic weighting system:

1. Pressing `x` on an article logs its keywords/tags into `skip_weights`,
   incrementing their weight.
2. On each refresh, incoming articles are scored against `skip_weights`.
3. High-weight (frequently-skipped) keywords cause matching articles to be
   filtered or sorted to the bottom.

This is intentionally simple and inspectable — you should always be able to
look at `skip_weights` and understand exactly why an article was
deprioritized.

---

## Configuration

### `sources.toml`

Adding a new source is a config edit, not a code change (for anything with
an RSS feed). Every source is assigned exactly **one** topic — not a list.
This is a deliberate choice: a source's articles only ever live under one
sidebar category, which keeps topic filtering, the skip-weighting system,
and duplicate-article handling all working against a single unambiguous
grouping instead of articles potentially appearing in more than one place:

```toml
[[source]]
name = "Phoronix"
type = "rss"
url = "https://www.phoronix.com/rss.php"
topic = "kernel"

[[source]]
name = "KrebsOnSecurity"
type = "rss"
url = "https://krebsonsecurity.com/feed/"
topic = "security"

[[source]]
name = "r/linux"
type = "reddit"
url = "linux"
topic = "distros"
```

Topics themselves are **not** a fixed list anywhere in code — `Storage::topics()`
returns whatever distinct topic values exist across configured sources, so
the sidebar is fully driven by what the user has actually assigned. Adding
a brand-new category is just typing a new `topic = "..."` value on a source;
no code change required.

**Current source list:** 9to5Linux, Phoronix, It's FOSS, GamingOnLinux
(the site behind the "LinuxGamingNews" pick), Linuxiac, LWN.net, OMG!
Ubuntu, KrebsOnSecurity — configured in `~/.config/tuxwire/sources.toml`,
grouped as `kernel` (Phoronix, LWN.net), `distros` (9to5Linux, Linuxiac,
OMG! Ubuntu), `linux` (It's FOSS), `gaming` (GamingOnLinux), and
`security` (KrebsOnSecurity). See that file's own comments for the
per-source reasoning — it's a first pass, freely adjustable by just
editing `topic = "..."`.

### `theme.toml`

All colors are config-driven — never hardcoded in the UI code — so any
shade can be tweaked without touching Rust:

```toml
[theme]
background   = "#24273a"  # Catppuccin Macchiato — base
panel_border = "#363a4f"  # surface0
text_primary = "#cad3f5"  # text (unread)
text_muted   = "#6e738d"  # overlay0 (read)
text_dim     = "#494d64"  # surface1 (skipped)

accent_unread   = "#f5a97f"  # peach
accent_read     = "#8bd5ca"  # teal
accent_skipped  = "#c6a0f6"  # mauve
accent_saved    = "#eed49f"  # yellow
accent_breaking = "#ed8796"  # red
accent_selected = "#8aadf4"  # blue
```

**Palette: Catppuccin Macchiato** (dark only — no light theme). Chosen for
being colorful without being chaotic; every hue in the palette is
desaturated to sit together cleanly. Mocha and Frappé remain available as
easy drop-in swaps (same accent hues, different base/text shades) since the
theme loader reads from this file rather than anything compiled in.

---

## Adding Sources

For v1, a single entry point: the in-app `a` keybind.

Deliberately simple — no HTML parsing, no feed autodiscovery. Paste the
actual feed URL, tuxwire validates it by trying to parse it, that's the
whole mechanism:

1. Prompt for a feed URL directly (not the site's homepage) — most sites'
   feed URLs are easy to find or guess (`/feed`, `/rss.xml`, `/feed.xml`).
2. **Validation via the existing parser, nothing new:** tuxwire attempts to
   fetch and parse the URL with `feed-rs`, the same crate already used for
   every regular fetch. If it parses successfully, that *is* the proof it's
   a valid feed — no separate discovery logic needed.
3. On success, confirm screen: guessed name (from the feed's own `<title>`
   metadata, editable), and a **single required topic** — pick from
   existing topics (queried live via `Storage::topics()`) or type a new
   one. No source can be left without a topic.
4. On failure, show a clear error ("couldn't parse this as a feed — check
   the URL") and let the user retry or cancel.
5. On confirm, write a new `[[source]]` block into `sources.toml` using the
   `toml_edit` crate rather than plain `toml` — `toml_edit` preserves
   existing formatting/comments on write instead of rewriting the whole
   file.

This intentionally reuses the app's existing fetch/parse path as validation
rather than adding HTML scraping or a fallback-path-guessing system — less
code, fewer moving parts, and one less thing that can break independently
of the rest of the app.

A phone-capture inbox (sharing a URL from Android straight into a pending
list) was considered and deliberately deferred — see Roadmap.

---

## Data Model

```rust
struct Article {
    id: i64,
    title: String,
    url: String,
    source: String,
    topic: String,
    timestamp: DateTime<Utc>,
    read: bool,
    skipped: bool,
    saved: bool,
    note: Option<String>,
}
```

---

## Project Layout (proposed)

```
tuxwire/
├── src/
│   ├── main.rs
│   ├── fetchers/        # one module per source type
│   ├── storage/         # SQLite access + migrations
│   ├── scoring.rs        # skip-weighting logic
│   ├── theme.rs           # theme.toml loading
│   └── ui/                # ratatui views/widgets
├── migrations/
│   └── 001_init.sql
├── docs/
│   ├── ARCHITECTURE.md    # this file
│   └── ADDING_A_SOURCE.md
├── CHANGELOG.md            # git-commit / changelog style, matches blog aesthetic
├── sources.toml.example
├── theme.toml.example
└── Cargo.toml
```

---

## Documentation Standards

**This is a hard requirement, not a nice-to-have.** The person building this
has no prior Rust experience and is learning the language *through* this
codebase — by reading it, both on GitHub and locally. Code that isn't
heavily commented isn't just unfriendly here, it defeats the actual purpose
of the project.

- **Every function, struct, and non-trivial block gets a doc comment
  (`///`)** explaining not just *what* it does but *why* it's written this
  way — especially anything involving ownership, borrowing, lifetimes,
  traits, or async, since those are the concepts genuinely new to a
  newcomer. A one-line "gets the article title" comment on an obvious
  getter is not the goal; explaining *why* a `&str` vs `String` was chosen,
  or why a `Result` is being propagated with `?` instead of unwrapped, is.
- Prefer comments that teach the underlying Rust concept over comments that
  just restate the code in English.
- Doc comments should be written so `cargo doc --open` produces something
  genuinely useful to read — treat this as building a personal Rust
  reference alongside the app itself, not just satisfying a linter.
- When a non-obvious design decision is made (why this crate over an
  alternative, why this pattern over a simpler one), it belongs either in
  a comment at the point of use, or as a note in `ARCHITECTURE.md` if it's
  project-wide — not left unexplained.
- Favor clarity over cleverness. Idiomatic Rust is the goal, but a very
  terse idiom that's opaque to a newcomer is worth a short comment
  explaining what it expands to conceptually.

---

## Planned Features (Near-Term)

These aren't v1, but are far enough along in design that they belong here
rather than staying as loose chat ideas. Build order: v1 core first
(fetchers → storage → TUI → notes), then these three, roughly in the order
listed.

### Markdown Export (saved articles + notes)

Exports saved articles as Markdown files, fitting directly into the
existing Hugo/rclone blog pipeline — "things I want to try" can become a
ready-made post skeleton.

- Triggered by a keybind from the Saved view (e.g. `E`)
- One article → one `.md` file, or a "export all saved" batch mode
- **Must include a link back to the original article** — the note is
  context, not a replacement for the source. Suggested format:

```markdown
## Btrfs send/receive got noticeably faster in 6.18
Source: [r/linux](https://reddit.com/r/linux/...) — saved 2026-08-12

> your note goes here, verbatim from the `note` column
```

- Output location configurable in `theme.toml` or a new `export.toml` —
  likely defaulting to a folder that rclone already watches, so saved
  articles can flow into the blog pipeline with no extra manual step

### Offline Article Caching

Solves spotty cell service at work — read saved (or even just unread)
articles without a live connection.

- Requires a new `content` column on `articles` (full article body/text,
  not just title+link) — populated at fetch time, or on-demand when an
  article is saved
- Likely needs an HTML-to-text/markdown extraction step (a crate like
  `readability` or similar) rather than storing raw HTML, since raw HTML
  won't render well in a ratatui `Paragraph`
- Cached content should probably be pruned for unsaved articles after
  some age, but **never pruned for saved articles** — same
  permanence rule as the saved state itself
- Worth deciding later: cache everything on fetch (heavier on storage,
  always available) vs. cache only on save/open (lighter, but requires
  connectivity at least once)

### Stats View

A lightweight self-hosted-analytics-style view, in the same spirit as the
GoatCounter dashboards already tracking the blogs.

- Articles read per day/week, broken down by topic and by source
- Top sources by volume, top skipped keywords (surfaces what the
  skip-weighting system has actually learned — useful for sanity-checking
  it isn't over-filtering)
- Could be a dedicated ratatui view with simple bar/sparkline widgets, or
  even just a formatted text summary to start — doesn't need to be
  visually elaborate to be useful

---

## Roadmap / Not Yet Decided

**Deferred until the rest of v1 + near-term features are solid:**
- **Phone capture → ntfy inbox.** Would reuse the existing ntfy
  phone→desktop pipeline (same pattern as the notification scratchpad) so
  sharing a URL from Android drops it into a `pending_sources` table for
  later review. Deliberately cut from v1 scope: it's a full extra
  subsystem (polling, auth/tunnel-down handling, a second review UI) for
  a convenience that manual capture-and-paste already covers. Notes and
  the stats view are a better use of time right now. Revisit once the
  core app (including notes + stats) is genuinely daily-usable.

**Other open questions:**
- Multi-line vs. single-line notes UI — inline popup (`tui-textarea`) vs.
  shelling out to `$EDITOR` for real Vim; likely support both, config-toggled
- Custom fetchers for any source without RSS
- Possible future: syncing the SQLite db via rclone across machines
- Offline caching strategy: cache-on-fetch vs. cache-on-save/open
- Export location/config for Markdown export — likely an `export.toml`
- Duplicate article detection across sources (same story, multiple sites —
  note: with the one-topic-per-source rule now settled, this is scoped to
  duplicates *within* a topic, not cross-topic, which simplifies it)
- A `:` command mode as an alternative to single-key binds for less-common
  actions

This is a living document — treat every future feature decision as an edit
here first, code second.
