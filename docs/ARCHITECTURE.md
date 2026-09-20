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

The screen is split into two vertical regions: a fixed-height **banner
bar** across the full terminal width at the top, and everything else
below it. Same pattern as Claude Code's own terminal header — a reserved
top row that never scrolls or changes, with the working area rendered
underneath.

- **Banner bar (top, full width, fixed height):** the `tuxwire` figlet
  wordmark, rendered persistently — full-width means the original figlet
  ASCII banner (37 chars, `standard` font) fits comfortably with room to
  spare, no need for a cramped/narrow substitute. This region never
  changes regardless of topic, view, or selection state.
- **Left pane (topic sidebar), below the banner bar:** three stacked
  sections, always visible regardless of topic or view selected:
  1. **Topics** — an expandable tree, not a flat list:
     - Each topic shows an indicator (`▸` collapsed / `▾` expanded).
     - `Right` / `l` expands the selected topic, revealing its individual
       sources indented underneath (source names come from the already
       -loaded `sources.toml` config, grouped by topic — no new query
       needed, since one-topic-per-source means the grouping is already
       known).
     - `Left` / `h` collapses it back down.
     - `Up`/`Down` flow naturally through topic rows and, when a topic is
       expanded, its nested source rows.
     - Selecting a **topic row** shows all articles across every source in
       that topic — unchanged existing behavior.
     - Selecting a **source row** (only reachable when its parent topic is
       expanded) filters the article list to just that one source. This
       is an additional filter layered on the existing topic filter, not
       a new query path — a source's topic is already fixed, so
       "articles from source X" is just "articles in X's topic, further
       filtered by source name."
  2. **Keys** — a static reference list of the active v1 keybinds (`s`
     save, `x` close, `n` note, `S` saved view, `r` refresh, `a` add
     source). Exists specifically because the person using this has no
     prior Rust/TUI-app experience and shouldn't need to memorize the
     keybind table before the app is usable — the reference lives right
     on screen instead.
  3. **Colors** — a small colored square next to each state label
     (unread/read/saved/skipped), color values pulled live from the
     loaded `theme.toml` rather than hardcoded, so the legend always
     matches whatever's actually rendering in the article list.
  The Keys and Colors sections are purely static — no interactivity, no
  state — and render identically in both the Topic view and the Saved
  view.
- **Right pane, below the banner bar:** article list for the selected
  topic (or saved articles, in Saved view), sorted by recency
- **Footer:** remaining hints not already covered by the sidebar's Keys
  section, plus last-refresh timestamp
- **Saved view:** a dedicated view filtering to `saved = true` across
  all topics, independent of the topic sidebar (toggled with `S`)

#### Keybinds (v1)

| Key       | Action                          |
|-----------|----------------------------------|
| `j` / `k` | navigate list                   |
| `Right` / `l` | expand selected topic (show its sources) |
| `Left` / `h`  | collapse selected topic          |
| `Enter`   | open article in `w3m` (falls back to `$BROWSER`/`xdg-open` if `w3m` isn't installed) |
| `x`       | skip article (recolors, no keyword-learning behind it — see below) |
| `s`       | save article (auto-marks as read) |
| `n`       | edit note on a saved article     |
| `r`       | refresh sources for the selected topic (`View::Topic` only — no-op in Saved/History; see FreshRSS Sync for the `FreshRSS/*` case) |
| `S`       | view saved articles               |
| `y`       | view history (every article ever fetched) |
| `/`       | search titles/notes across the current view |
| `a`       | add a new source                  |
| `E`       | export every saved article into one combined Markdown file (Saved view only; regenerates the file fresh each press) |
| `q`       | quit                             |

---

## Article States & Behavior

| State     | Meaning                                  | Notes                              |
|-----------|-------------------------------------------|-------------------------------------|
| unread    | default state, not yet interacted with    |                                      |
| read      | opened, or saved (saving implies read)     |                                      |
| skipped   | explicitly dismissed                       | purely visual/state — see below     |
| saved     | permanent until manually unsaved           | supports an optional free-text note |

**Saving auto-marks as read** — these are not tracked as independent
booleans in practice; pressing `s` sets both `saved = true, read = true`
in one action.

---

## Skip-Weighting ("Learning") — Deferred to v2

`x` marks an article skipped: it's recolored (mauve, per the theme) and
that's it. This is deliberately simple and considered **complete as-is**
for v1 — it does exactly what's needed (get an uninteresting article out
of visual focus) without any further behavior behind it.

An earlier version of this doc described a keyword-weighting system where
skipping an article would log its keywords and future similar articles
would be auto-deprioritized. That's explicitly **cut from v1 scope**:
- The `skip_weights` table and `Storage::increment_skip_weight` /
  `Storage::skip_weight` methods already exist from the initial schema, but
  are intentionally left unwired — not a bug, not unfinished, just unused
  for now.
- A plain skip-and-recolor loop has proven sufficient through active use;
  keyword-scoring adds real complexity (extraction, stopword handling,
  scoring-and-resorting on every fetch) for a behavior that hasn't turned
  out to be missed.
- If this is revisited for v2, the existing schema and methods are already
  there to build on — nothing needs to be re-designed, just wired up.

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

**Current source list:** 9to5Linux, Phoronix, It's FOSS, LinuxGamingNews,
Linuxiac, LWN.net, OMG! Ubuntu (candidates to add: Arch Linux news, a
dedicated CVE/security feed, KrebsOnSecurity).

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
│   ├── freshrss/        # PLANNED (feature/freshrss): optional FreshRSS sync, behind the `freshrss` Cargo feature
│   ├── scoring.rs        # reserved for v2 skip-weighting (unused in v1)
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
├── freshrss.toml.example   # PLANNED (feature/freshrss)
└── Cargo.toml
```

---

## Documentation Standards

**This is a hard requirement, not a nice-to-have.** The person building this
has no prior Rust experience and is learning the language *through* this
codebase — by reading it, both on GitHub and locally — and wants **every
section of code documented with what it does.** This is stricter than
"non-trivial blocks only": something can be simple to write and still be
new to a beginner, and it's the beginner's experience this standard is
written for, not the author's.

- **Every module, struct, enum, field, function, and method** gets a `///`
  (or `//!` for a module) doc comment saying what it is for and what it
  returns or changes — not just the ones whose purpose feels non-obvious to
  someone who already knows Rust.
- **Inside function bodies, every logical section gets a `//` comment**
  explaining what that section does, in plain language, step by step — a
  short comment above each group of lines, not just one comment at the top
  of the function. If a line uses a Rust concept that's new to a beginner,
  say what it does right there, not just in the function's own doc comment.
- **Also explain *why*,** not just *what* — especially anything involving
  ownership and borrowing (`&str` vs `String`, `.clone()`, `&mut`),
  lifetimes, traits, `Result`/`?` versus `unwrap`, `Option`, `match`,
  iterator chains (what they expand to conceptually), and async/`await`,
  since those are the concepts genuinely new to a newcomer. A one-line
  "gets the article title" comment on an obvious getter is not the goal;
  explaining *why* a `&str` vs `String` was chosen, or why a `Result` is
  being propagated with `?` instead of unwrapped, is.
- Write for someone who has never seen Rust. Don't assume any concept is
  obvious. A terse idiom gets a comment saying what it expands to
  conceptually, not just a note that it's terse.
- Prefer comments that teach the underlying Rust concept over comments that
  just restate the code in English.
- **Comments must be accurate.** If code changes, update its comments in
  the same commit — a misleading comment is worse than none.
- Before writing, reread one or two of the best-commented existing files
  (for example under `src/storage/`) and match their tone and depth, rather
  than reinventing the level of detail from scratch each time.
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
- At the end of each build phase (see, e.g., the FreshRSS Sync section's
  "Build phases"), show one or two functions that best demonstrate the
  comment style, so the person can confirm it actually teaches.

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

- Triggered by the `E` keybind from the Saved view — no-op from
  `View::Topic`/`View::History`, since there's no guarantee the selected
  row there is even a saved article, and there's nothing selection-
  dependent about the export itself anyway (see below).
- **One combined file, not one file per article.** `E` writes *every*
  saved article into a single `saved-articles.md`, each article's block
  separated from the next by a Markdown thematic break (`---` on its own
  line). Simpler than a one-file-per-article design (no per-title
  filename to sanitize or disambiguate, no directory listing to manage)
  and a better fit for the actual use case — skimming or grepping the
  whole saved collection at once, or feeding it into the blog pipeline as
  one document, rather than opening files one at a time.
- **Regenerates the file from scratch every press, never appends.**
  `E` always re-derives the whole file from the current saved set
  (`Storage::saved_articles`), so it can never drift out of sync with an
  article being un-saved, re-noted, or removed since the last export —
  there's no "what's new since last time" delta to track, just "write
  what's saved right now."
- **Must include a link back to the original article** — the note is
  context, not a replacement for the source. Uses the real `saved_at`
  (and `noted_at`, if the note was added/edited after saving) timestamps
  — not the article's publish date:

```markdown
## Btrfs send/receive got noticeably faster in 6.18
Source: [r/linux](https://reddit.com/r/linux/...) — saved 2026-08-12, noted 2026-08-14

> your note goes here, verbatim from the `note` column

---

## Another saved article
Source: [Phoronix](https://phoronix.com/...) — saved 2026-08-10
```

  (A saved article with no note simply has no `>` line at all — nothing
  to quote.) Every subsequent article's block is separated from the one
  before it by a `---` on its own line, so the combined file still reads
  as a sequence of distinct entries rather than one run-on document.

- **Output location: config-driven, no per-export prompt.** Consistent
  with the rest of the app (`sources.toml`, `theme.toml` — nothing else
  interactively prompts for a path), export writes to a default directory
  set in a new `export.toml`:

```toml
[export]
path = "~/tuxwire-notes/"
```

  `~/tuxwire-notes/` (i.e. inside the person's own home directory, not
  `/home/` directly — that top-level path needs root to write to on most
  systems) is the shipped default. rclone isn't set up yet, so this isn't
  pointed at a synced folder for now — easy to repoint at one later by
  editing this single line once rclone is in place. Fully user-editable
  by design: anyone who wants a different location just edits this file,
  same as customizing sources or theme. No interactive file-save dialog
  — not a natural TUI pattern and unnecessary for something that doesn't
  change often. Documented in the README's Configuration section so it's
  discoverable without reading the source.
- **Filename is fixed, not derived from any article title** — always
  `saved-articles.md` under that directory. Combining every saved article
  into one file removes the need to sanitize a title for filesystem
  safety or disambiguate two articles that would otherwise collide on the
  same name — problems that only existed in the one-file-per-article
  design this replaced.
- A saved-without-a-note article omits the `>` blockquote entirely rather
  than emitting an empty one — nothing to quote.

### Notes Retrieval & Search

Notes are currently write-and-forget — saved, but nothing resurfaces them
or makes them easy to find later beyond scrolling the Saved view. Two
small, low-complexity additions close that gap without a new subsystem:

**Schema additions:**
- `saved_at`, set the moment `s` is pressed. The existing `timestamp`
  column is the article's *publish* date, not when the person saved it —
  those are genuinely different facts, and export/retrieval both want
  the latter.
- `noted_at`, set whenever `update_note` stores a non-empty note (i.e.
  every time `n` is confirmed with `Enter` and the note isn't empty) —
  distinct from `saved_at`, since a note can be written or edited well
  after the article was originally saved. If the note is cleared back to
  empty, `noted_at` clears too (`None`), keeping "no note" and "no note
  date" consistent with each other. Small migration, not a redesign —
  same shape as the `saved_at` addition.

**No new dedicated "notes view" needed.** The existing Saved view (`S`)
already covers this once it's given two small additions:
- Show source name and `saved_at` date under each title in the list
  (currently only shows the truncated note preview); if `noted_at` differs
  meaningfully from `saved_at` (note added/edited after the initial save),
  show that too rather than only the save date
- The existing `n` popup already functions as "read the full note" —
  since it opens pre-filled with the complete text and `Esc` leaves it
  untouched, no separate read-only view is needed
- Opening the article (`Enter`) already provides the link-back

**History view (`y` keybind):** a dedicated view of every article ever
fetched, regardless of read/saved/skipped state — nothing in tuxwire
deletes articles today, so this data already exists, this just exposes
it. Solves "I read something a few days ago, didn't save it, and want it
back" without requiring the person to have remembered to save it in the
moment. Same list rendering as Topic/Saved views, reusing existing
navigation (`j`/`k`, `Enter`, `n`, `s`, `x` all work the same way here).

**Search (`/` keybind, plain substring match, not full-text search):**
"Keyword search" and "full-text search" aren't the same thing — full-text
search (SQLite's FTS5 extension) adds ranking, stemming, and fuzzy
matching, but requires a separate virtual table kept in sync with
`articles` via triggers — a real extra subsystem. At the scale of one
person's article collection (realistically hundreds, not thousands), a
plain `LIKE '%term%'` substring match across `title` + `note` is
proportionate and sufficient. Searches across **all** articles, not just
saved ones — this is what actually answers "find that thing I read a
few days ago," not a separate log/tracking table. Live-filters whichever
view is currently open (Topic, Saved, or History) as the person types;
`Esc` clears the search and restores the full list. Revisit FTS5 only if
the collection ever grows large enough that relevance ranking genuinely
starts to matter — unlikely for a personal tool at this scale.

**Considered and deliberately not built: a separate reaction/interaction
log** (a table recording every read/skip/save event over time, auto-
pruned after 30 days). Unnecessary complexity for what's actually needed
— nothing currently deletes articles, so the History view + widened
search already solves the real use case (finding a past article) without
a second data model to maintain. A **pruning policy** is still worth
having eventually, just framed differently: unsaved articles could be
cleaned up after some age to keep the database from growing forever,
while saved articles are never pruned (same permanence rule already
established for saved state) — a small housekeeping feature, separate
from retrieval, and not urgent.

**Deferred, not v1.1:** resurfacing old saved articles back to the person
periodically (a lightweight "things you meant to try" nudge, distinct from
search since it's proactive rather than something the person has to
remember to go look for). Genuinely novel — most readers, terminal or
GUI, treat "saved" as a one-way archive. Worth building once retrieval
and search themselves are solid and have seen real use.

### Stats View — priority near-term feature

A lightweight self-hosted-analytics-style view, in the same spirit as the
GoatCounter dashboards and the homelab dashboard already tracking the
blogs and desktop stats. Explicitly the next build priority among the
remaining near-term features.

- Articles read per day/week, broken down by topic and by source
- Top sources by volume, most-skipped articles/topics (simple counts —
  not dependent on the deferred skip-weighting system)
- Could be a dedicated ratatui view with simple bar/sparkline widgets, or
  even just a formatted text summary to start — doesn't need to be
  visually elaborate to be useful

### Offline Article Caching — cut, not building this

Considered for solving spotty cell service at work, but decided against —
same category of cut as ntfy phone-capture and in-app highlighting: real
complexity (a new `content` column, an HTML-to-text extraction step,
pruning policy for cached-but-unsaved content) for a problem that turned
out not to be worth solving this way. Not pursuing.

---

## FreshRSS Sync (optional) — in progress on `feature/freshrss`

**Status: planned, being built on the `feature/freshrss` branch. Not merged
into `master` until it's been tested and is genuinely liked** — same process
as the `w3m` integration. Until that merge, nothing in this section
describes code that exists on `master`.

### Goals and non-goals

**Goals:** let someone who runs a FreshRSS server pull their subscriptions
into tuxwire, keep read state in sync with the server, and optionally add a
new feed to FreshRSS from inside tuxwire.

**Non-goals:**
- FreshRSS is **never** the main or only source. Local `sources.toml`
  sources and the existing `a` flow stay the default, unchanged.
- No change of any kind to existing behavior when FreshRSS is off.
- No saved/starred sync, no notes sync, no skip sync in the first version
  (see "Deferred" below). Skipped, saved, and notes are local-only.
- No managing the FreshRSS server itself (no deleting feeds, renaming
  categories, OPML import/export).

### The prime rule: off means identical to `master`

Two independent layers gate the feature:

1. **Compile time:** a Cargo feature named `freshrss` (not in `default`
   while on the branch). Without it, none of the FreshRSS code is compiled,
   so `cargo build` produces the same program as `master`.
2. **Run time:** a `freshrss.toml` config file. If it's missing, or has
   `enabled = false`, no FreshRSS code path runs and no network request is
   made.

Whether `freshrss` joins the `default` features is decided at merge time,
not before.

### Protocol: the Google Reader-compatible API

FreshRSS exposes a Google Reader-style HTTP API at
`<base-url>/api/greader.php`. It's plain HTTP + form posts + JSON, all
already covered by `reqwest` and `serde_json` — **no new dependencies
should be needed.**

On the server, the person has to (a) allow API access in FreshRSS's
authentication settings and (b) set an **API password** under their profile.
The API password is separate from the account login password.

Summary of the calls needed (paths relative to `/api/greader.php`).
**Verify each against FreshRSS's own API documentation before coding —
treat this table as a starting map, not a spec:**

| Purpose | Call |
|---|---|
| Log in | `POST /accounts/ClientLogin` with `Email` + `Passwd`; the plain-text response contains an `Auth=` token, sent on later requests as `Authorization: GoogleLogin auth=<token>` |
| List subscriptions (feeds + their categories) | `GET /reader/api/0/subscription/list?output=json` |
| List unread item IDs | `GET /reader/api/0/stream/items/ids?output=json&s=user/-/state/com.google/reading-list&xt=user/-/state/com.google/read` |
| Fetch item contents by ID | `POST /reader/api/0/stream/items/contents` with the item IDs |
| Get a write token | `GET /reader/api/0/token` |
| Mark items read | `POST /reader/api/0/edit-tag` with `a=user/-/state/com.google/read`, the item IDs, and the write token |
| Subscribe to a new feed | `POST /reader/api/0/subscription/quickadd` with the feed URL and the write token |

Item ID formats can differ between endpoints (short numeric vs. long-form).
Normalize them in exactly one place in the client so the rest of the code
only ever sees one form.

**Verified against a live test server** (2026-09-20) — the two corrections
below supersede the table above wherever they overlap; everything else in
the table held up as described.

**`stream/items/ids` pagination.** Confirmed request/response shape:

```
GET stream/items/ids?output=json&s=user/-/state/com.google/reading-list
    &xt=user/-/state/com.google/read&n=<page size>[&c=<continuation>]

{"itemRefs": [{"id": "1789934360946639"}, ...], "continuation": "1789934360946635"}
```

- `id` values here are plain decimal numeric strings — the *short* form,
  not hex and not the `tag:google.com,2005:reader/item/...` long form.
- `continuation` is present whenever there might be more data, **including
  on the last page that still has items on it** — observed a page of 5
  items (the actual final 5, out of 20 total) come back with a
  `continuation` value that, when used for one more request, produced
  `{"itemRefs": []}` with no `continuation` field at all. A client
  **cannot** treat "this page came back full" or "this page had a
  continuation" as a stop signal — the only reliable end-of-data signal is
  an empty `itemRefs` array. Concretely: keep requesting with `c=` set to
  the previous response's `continuation` until a response comes back with
  an empty `itemRefs`, and separately cap the number of pages fetched as a
  hard safety net regardless (a bug, or a server that never terminates the
  stream, must not be able to loop forever) — see Phase 3 below.
- Treat `continuation` as an opaque token to round-trip, not something to
  parse or compute — this FreshRSS build happens to set it to the last
  item's own `id`, but that's an implementation detail, not a contract.

**`stream/items/contents` shape.** `POST` with one repeated `i=<id>` form
field per item — the plain decimal `id` from `stream/items/ids` is accepted
directly, no conversion needed on the request side. Tested up to 20 ids in
a single POST with no issue, confirming this is the right place to batch
requests rather than fetching one item per request.

Response:

```json
{
  "id": "user/-/state/com.google/reading-list",
  "updated": 1789934414,
  "items": [
    {
      "id": "tag:google.com,2005:reader/item/00065bef91c167cf",
      "crawlTimeMsec": "1789934360946",
      "timestampUsec": "1789934360946639",
      "published": 1789933681,
      "title": "Show HN: A browser MMO designed to be played by scripts, not people",
      "canonical": [{"href": "https://space-core.at/"}],
      "alternate": [{"href": "https://space-core.at/"}],
      "categories": [
        "user/-/state/com.google/reading-list",
        "user/-/label/Uncategorized",
        "user/-/state/org.freshrss/main"
      ],
      "origin": {"streamId": "feed/2", "htmlUrl": "https://news.ycombinator.com/newest", "title": "Hacker News: Newest"},
      "summary": {"content": "<p>Article URL: ...</p>"},
      "author": "SpaceCoreDev"
    }
  ]
}
```

**Correction to "normalize them in exactly one place":** the id round-trip
is more asymmetric than the original table implied. `stream/items/ids`
returns the short decimal form; `stream/items/contents`' own `items[].id`
comes back in the **long** `tag:google.com,2005:reader/item/<16-hex-chars>`
form instead — confirmed the hex suffix is the same number, just base 16
and zero-padded to 16 digits (`int("00065bef91c167cf", 16) ==
1789934360946639`). Normalization has to convert this long form back down
to plain decimal, since that's the form `stream/items/ids` produces and
the form the read-state diff (see "Pull read state") compares against.
`items[].timestampUsec` conveniently already carries the same value as a
plain decimal string — usable as a normalization shortcut that skips the
hex-parsing step, but `id` is the field documented to be present on every
item, so normalization shouldn't rely on `timestampUsec` being there.

Fields relevant to the eventual `Article` mapping (Phase 4, not Phase 3):
`title`; `canonical[0].href` (falling back to `alternate[0].href`) for
`Article.url`; `published` (Unix seconds) for `Article.timestamp`;
`origin.streamId` for `freshrss_items.feed_id`; `origin.title` for the
source name.

**Login failure shape**, for error handling: a bad password gets back
`HTTP 401`, a `Google-Bad-Token: true` header, and a plain-text body
(`Unauthorized!`) — no JSON. A successful login's body is plain text too,
three `Key=Value` lines (`SID=`, `LSID=`, `Auth=`) in no guaranteed order;
parse by scanning for the line starting `Auth=`, not by line position.

### How it fits the existing architecture

```
  freshrss/ module ──▶ normalizes into the same Article struct ──▶ Storage ──▶ TUI
```

The three-layer design doesn't change. FreshRSS is one more thing that
feeds the fetch layer, and everything downstream (storage, TUI) keeps
working on `Article` values without caring where they came from.

- **New module `src/freshrss/`**, compiled only with the `freshrss`
  feature, split into: `config` (parse `freshrss.toml`), `client` (HTTP and
  protocol only — knows nothing about `Storage` or the UI), `mapping`
  (turns the client's raw subscription/item shapes into `Article` values
  and normalized IDs), and `sync` (reconciles FreshRSS state with
  `Storage`).
  - **Deliberately not a `Fetcher` impl.** The existing `Fetcher` trait
    (`src/fetchers/mod.rs`) is shaped around "one source, one independent
    fetch" — `main`'s fetch loop and `refresh_topic` call `.fetch()` once
    per `ConfiguredSource`, catching each one's failure separately. The
    Google Reader API is the opposite shape: one login + one write token
    covers *every* subscribed feed, and unread items are fetched as a
    single batch across the whole reading list, not per-feed. Forcing that
    into one `Fetcher` per FreshRSS feed would mean re-logging-in and
    re-fetching the full unread-ID list once per feed for no benefit.
    Instead, `freshrss::sync` owns the whole batch (log in once, fetch
    once, fan out to every feed's items) and produces `Article` values
    itself via `freshrss::mapping` — it's driven directly (see "Sync
    behavior" below and "Refresh triggers"), not through the `Fetcher`
    trait.
- **Hard boundary: nothing under `src/freshrss/` may import `ratatui`,
  `crossterm`, or anything from `src/ui/`**, and it must not hardcode
  filesystem paths (paths and config are passed in by the caller). This
  keeps the module extractable: the Tier 3 native Android app plans to
  move fetchers/storage into a shared Rust core (see Distribution Roadmap),
  and this module should then be a move, not a rewrite.
- **Mapping FreshRSS onto the one-topic-per-source rule:** a FreshRSS feed
  becomes a source (name = the feed's title), and its FreshRSS category
  becomes its topic, namespaced as `FreshRSS/<category>` (uncategorized
  feeds go under `FreshRSS/Uncategorized`). The namespace means FreshRSS
  topics can never collide with or merge into a local topic of the same
  name, and they group together in the sidebar.
- **Sidebar integration (resolved in Phase 0):** the top-level topic list
  needs no change — `Storage::topics()` already does a plain `SELECT
  DISTINCT topic FROM articles`, so a `FreshRSS/<category>` topic shows up
  automatically the moment a FreshRSS article is inserted. The nested
  *source* rows under an expanded topic are the part that doesn't fall out
  for free: `ui::run` builds an immutable `HashMap<String, Vec<String>>`
  (topic → source names) once at startup, from `sources.toml`-configured
  sources only (`group_sources_by_topic`), and `sidebar_rows` reads that
  map to decide what to draw. The fix is additive, not a rework: after
  `group_sources_by_topic` runs, merge in FreshRSS source names from a
  small new `Storage` query — distinct `(topic, source)` pairs where
  `topic LIKE 'FreshRSS/%'` — into the same map, gated behind the
  `freshrss` feature so the merge step doesn't exist at all when the
  feature is off. This merge is re-run after every sync completes (not
  just once at startup), so a feed added to FreshRSS mid-session appears
  in the sidebar without restarting tuxwire, matching how the `a` flow
  already updates `topics` live for local sources.
  - **Zero-article-feed limitation:** because the sidebar merge queries
    distinct `(topic, source)` pairs out of `articles` itself (not the
    subscription list), a FreshRSS feed with **zero articles synced so
    far** has no row to be distinct over yet, so it won't appear in the
    sidebar at all — unlike a local `sources.toml` source, which shows up
    from config alone regardless of whether it's produced any articles.
    In practice this only matters for the brief window between
    subscribing to a feed (via `a`'s FreshRSS destination, or directly on
    the server) and the first sync that pulls at least one item from it;
    a feed that's simply empty or fully read on the server has the same
    "not visible until it has content" gap as any topic with no articles
    at all. Not fixed in the first version — see "Deferred" below.
- **Duplicates across origins:** if the same feed exists both locally and
  in FreshRSS, its articles will appear twice (once per origin). That's
  accepted for the first version and folds into the existing open question
  about duplicate-article detection.

### Storage additions (additive only)

One new numbered migration in `migrations/` (use the next number after the
highest existing file — check the directory, don't assume). **New tables
only; no `ALTER` on `articles` or any other existing table**, keeping the
no-destructive-schema-changes rule and letting an old database open cleanly
with or without the feature.

```
freshrss_items
  article_id         INTEGER PRIMARY KEY REFERENCES articles(id)
  remote_id          TEXT UNIQUE NOT NULL   -- FreshRSS item ID; dedupe + read sync
  feed_id            TEXT NOT NULL          -- FreshRSS feed identifier
  pending_read_push  BOOLEAN DEFAULT 0      -- local read change not yet sent

freshrss_meta
  key                TEXT PRIMARY KEY       -- e.g. last successful sync time
  value              TEXT
```

### Refresh triggers

Unlike local sources, FreshRSS sync is never scoped to "the currently
selected topic" — one login covers every subscribed feed at once (see
"Deliberately not a `Fetcher` impl" above), so it doesn't fit `r`'s
existing per-topic shape. Concretely, `r` today (`ui::draw_until_quit`)
only refreshes sources filed under whichever topic is selected in
`View::Topic`, and does nothing at all in `View::Saved`/`View::History` —
FreshRSS sync piggybacks on that same key rather than replacing it:

1. **At startup**, when the feature is on, `freshrss.toml` has
   `enabled = true`, and `sync_on_start` is `true` (see Configuration
   below) — runs once in the background before/alongside the TUI opening,
   the same "must never block or fail a local refresh" guarantee as
   below.
2. **On `r`**, but only while a `FreshRSS/*` topic is selected in
   `View::Topic`. `r` on any local topic behaves exactly as it does on
   `master` today (refreshes that topic's local sources only, via
   `refresh_topic`) — FreshRSS sync never runs as a side effect of
   refreshing an unrelated topic. `r` remains a no-op in `View::Saved` and
   `View::History` for both local and FreshRSS sources, unchanged from
   today.

Both triggers run the same sync steps, described next, **never blocking
local sources' own fetch/refresh** (same principle as the existing
concurrent fetches: one slow or dead source never blocks the rest).

### Sync behavior

1. Log in and get a token.
2. Fetch the subscription list (feeds → categories).
3. Fetch unread item IDs; fetch contents only for IDs not already in
   `freshrss_items`; insert `articles` rows plus their `freshrss_items` link
   rows.
4. **Pull read state:** any locally-unread FreshRSS-origin article whose ID
   is no longer in the server's unread set was read elsewhere → mark it
   read locally.
5. **Push read state:** for rows with `pending_read_push = 1`, send
   `edit-tag` read, and clear the flag only after the server confirms.

When the person marks a FreshRSS-origin article read locally (`Enter` or
`s`), set `pending_read_push = 1`. **Conflict rule:** a pending local change
wins until it has been pushed; otherwise the server's state wins.

**Failure handling:** if FreshRSS is unreachable, times out, or rejects the
login, show a short message in the footer and carry on — local sources
refresh normally, pending flags persist, and the next trigger (the next
startup, or the next `r` on a `FreshRSS/*` topic) retries. A failed startup
sync must never delay or block the TUI opening. All requests have explicit
timeouts. A FreshRSS problem must never fail or delay a local refresh.

### Adding feeds

The default `a` flow is **unchanged**: paste a URL, validate with `feed-rs`,
write to `sources.toml`. When FreshRSS is enabled, the confirm screen gains
one optional choice of **destination: Local (default) or FreshRSS**.
Choosing FreshRSS sends the feed URL via `quickadd` instead of writing to
`sources.toml`; it appears in the sidebar after the next sync. If FreshRSS
can't be reached, show the error and let the person retry or switch to
Local — never lose the typed URL.

### Configuration: `freshrss.toml`

Follows the existing pattern (`sources.toml`, `theme.toml`, `export.toml`):
config-driven, no interactive prompts, lives in `~/.config/tuxwire/`.

```toml
[freshrss]
enabled       = false
url           = "https://freshrss.example.com"   # base URL only, no /api/greader.php
username      = "your-username"
# Whether to run a sync in the background at startup (see "Refresh
# triggers" above), in addition to `r` on a FreshRSS/* topic. Defaults to
# true -- most people who enable FreshRSS at all want it current as soon
# as tuxwire opens, without needing to remember to press `r` first.
sync_on_start = true

# The API password never goes in this file. Provide it one of two ways:
#   1. password_file: path to a file containing only the API password
#      (chmod 600), or
#   2. the TUXWIRE_FRESHRSS_PASSWORD environment variable, which takes
#      precedence over password_file when both are set.
password_file = "~/.config/tuxwire/freshrss.pass"
```

Ship `freshrss.toml.example` in the repo. Add `freshrss.toml` and `*.pass`
to `.gitignore` as a safety net.

### Security rules

- Never disable TLS verification. Warn if `url` is `http://` and the host is
  not localhost.
- The password and auth/write tokens are never logged, never included in
  error messages, and never printed by `Debug` (wrap them in a small secret
  type or implement `Debug` by hand to redact them).
- On Unix, warn if `password_file` is readable by group or others.
- Tokens live in memory only; log in again per refresh session.

### Testing plan

1. **Regression, feature off.** Build without `--features freshrss` and
   confirm the program behaves exactly like `master`: adding sources, the
   sidebar, `w3m`, and an existing database loading unchanged. Use a *copy*
   of a real database.
2. **Feature on, against a throwaway server.** Run FreshRSS in Docker
   (`freshrss/freshrss`), enable API access, set an API password, add a few
   test feeds, and exercise login, import, read sync both directions, and
   adding a feed. Never point development at the real FreshRSS instance or
   the real tuxwire database.
3. **Unit tests** for parsing subscription/item JSON from saved fixture
   files, ID normalization, and the reconcile logic — none of which need a
   network.
4. **Termux check.** `git fetch` and check out the branch in the existing
   Termux clone; build and run there.

### Build phases (stop and report after each)

- **Phase 0 — read only.** Read this document and the code; report how
  `Fetcher`, `Storage::topics()`, the sidebar, and the `a` flow actually
  work, whether the `Fetcher` trait fits FreshRSS as-is, and what the next
  migration number is. No code changes.
- **Phase 1 — skeleton.** `freshrss` Cargo feature, empty `src/freshrss/`
  module, `freshrss.toml` parsing, `freshrss.toml.example`. Off by default,
  no behavior change.
- **Phase 2 — storage.** The additive migration and the `Storage` methods
  the sync needs, with tests.
- **Phase 3 — client.** Login, subscription list, unread IDs, item
  contents, against fixtures. No storage or UI yet.
- **Phase 4 — pull.** Normalize items to `Article`, insert, dedupe by
  `remote_id`, surface FreshRSS sources/topics in the sidebar, hook into
  `r` with graceful failure.
- **Phase 5 — read sync.** Pull reconcile + `pending_read_push` + push.
- **Phase 6 — add feed.** The destination choice in the `a` flow.
- **Phase 7 — polish.** README configuration section, `CHANGELOG.md` entry,
  update this document from "planned" to describing what was actually
  built, and full doc-comment pass per the Documentation Standards.

### Deferred, not in the first version

- Syncing **saved** as FreshRSS **starred** (and back). Saved articles are
  permanent and carry notes, so the mapping deserves its own design.
- Category/folder management, OPML.
- Making the `freshrss` Cargo feature part of `default` — a merge-time
  decision.
- Android: when the shared core is extracted, revisit the TLS backend
  (`rustls` is generally easier to cross-compile for Android than
  OpenSSL). Do **not** change `reqwest`'s features on this branch.

---

## Distribution Roadmap — the next chapter, not the current one

**Explicitly not started.** We're still in the build phase — the app
should be in a clean, polished, feature-complete state we're happy with
before any packaging work begins, so there's room to tweak and change
things without also having to update a published package every time.
This section exists so the plan doesn't rely on memory once that time
comes, not as a current task.

**Tier 1 — Linux package managers (approachable, well-trodden path):**
- **AUR** — the natural first target, given the desktop is CachyOS/Arch-
  based already. A `PKGBUILD` describing fetch/build/install for a Rust
  binary is a short, well-documented pattern.
- **COPR** (Fedora) — similar effort tier, RPM `.spec` file instead of a
  `PKGBUILD`. Less familiar syntax, same shape of task.
- Further out, similarly scoped: a Nix flake, a Homebrew tap.

**Tier 2 — Termux packages:** getting `tuxwire` into `termux-packages`
(a build-recipe PR) so `pkg install tuxwire` works directly, rather than
requiring a manual clone-and-build on Android.

**Tier 3 — native Android app (F-Droid):** a genuinely separate project,
not a packaging step — extracting fetchers/storage/scoring into a shared
Rust core (`uniffi-rs`) and building a real Kotlin/Jetpack Compose UI
around it, since F-Droid distributes actual Android app packages (APK,
manifest, Activity), not CLI binaries running inside Termux. Realistically
weeks-to-months of casual evening work, several genuinely new skill areas
(Kotlin, Compose, Android's build toolchain, cross-compiling via
`uniffi-rs`). A long-term "someday" chapter, not a near-term target.
The optional `src/freshrss/` module is deliberately written with no UI or
terminal dependencies so it can move into that shared core as-is. The Android
app will live in its own separate repository so `master` here stays untouched
while that work happens.

**Packaging niceties already decided, ready for whenever Tier 1 starts:**
`w3m` should be listed as an `optdepends` (AUR) / `Recommends` (COPR) —
optional, not required, surfaced to the person at install time by the
package manager itself rather than an in-app first-run popup (deliberately
not building the latter — anyone capable of building this from source can
read one line in the README about it, same reasoning as the cuts above).

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

**Currently in exploration on the `terminal-browser-integration` branch —
not merged into master until it's genuinely liked:**
- **In-terminal article reading.** Two very different approaches under
  consideration, worth stating separately since they're different sized
  problems, not two flavors of the same one:
  - **Option A (starting point):** `Enter` suspends tuxwire's screen, spawns
    a terminal browser (`w3m` first choice — good rendering, common,
    scriptable) full-screen pointed at the article URL, and resumes
    tuxwire exactly where it left off on quit. Same suspend/resume pattern
    already used for shelling out to `$EDITOR` on notes — genuinely
    adapting existing plumbing to a new target program, not new
    architecture. Not true side-by-side; it's a full takeover, same as
    the current `$BROWSER` behavior just rendered in-terminal instead of
    a GUI browser.
  - **Option B (stretch goal, only if A feels unsatisfying):** a true
    split pane — sidebar/article list still visible while the browser
    renders in an adjacent region. This needs an embedded pty inside a
    ratatui widget (the `tui-term` crate, built on `vt100` to interpret
    the spawned program's escape codes) — a real, working approach but
    genuinely new complexity: a new dependency, coordinating resize
    events between two "terminals," and routing keystrokes to the right
    pane without tuxwire's own nav keys colliding with the browser's.
    Treat as its own dedicated effort, not a quick add.
  - No offline-caching/content-extraction work needed for either option —
    unlike the earlier in-app-reading idea, a terminal browser handles its
    own fetching and rendering, so this sidesteps that whole subsystem.

**Other open questions:**
- Multi-line vs. single-line notes UI — inline popup (`tui-textarea`) vs.
  shelling out to `$EDITOR` for real Vim; likely support both, config-toggled
- Whether the "Saved" view is a pseudo-topic in the sidebar or a separate pane
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
