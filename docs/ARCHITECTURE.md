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

Fetches run concurrently, so one slow/dead source never blocks a refresh of
everything else. `fetch_all` (core) downloads up to six feeds at a time
(`MAX_CONCURRENT_FETCHES`) using `futures-util`'s `buffered`, and still returns
one outcome per source **in the order the sources were given**, so callers store
the results one after another and the database is never written concurrently.
Every request has a 30-second limit (`FETCH_TIMEOUT` in `fetchers/rss.rs`): a
server that goes silent fails that one feed with a timeout instead of stalling
the refresh. Every request also sends `User-Agent: tuxwire/<version>` (some sites
answer a request with none by sending an empty page), and a non-2xx status or an
empty body is reported as such ("answered HTTP 403", "sent an empty reply")
rather than as an XML parse error.

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

**Later additions** (each its own migration, additive only):
- Migration 003 adds `articles.saved_at` and `articles.noted_at`.
- Migration 005 adds `articles.feed_url` (which feed an article came from).
- Migration 007 adds `articles.summary` (see "Article summaries" below).
- Migration 006 adds two tables the **Android app** uses and the terminal app
  ignores (the terminal keeps its feeds in TOML files):

```
feeds
  id            INTEGER PRIMARY KEY
  name          TEXT NOT NULL
  url           TEXT NOT NULL UNIQUE      -- matches articles.feed_url
  topic         TEXT NOT NULL
  source_type   TEXT NOT NULL DEFAULT 'rss'

settings
  key           TEXT PRIMARY KEY          -- e.g. 'theme'
  value         TEXT NOT NULL
```

Removing a feed (`Storage::remove_feed`) deletes only its `feeds` row.
Articles already fetched from it stay, in keeping with "saved articles and
notes are permanent". Version 4 stays unused (see 005).

**Article summaries.** Migration 007 adds a nullable `articles.summary`: the
feed entry's short description (RSS `<description>` / Atom `<summary>`), or the
start of its full content as a fallback, as **plain text**. `core/src/text.rs`
removes the HTML (hand-written, no dependency: drops tags, scripts, styles and
comments, turns block tags into line breaks, decodes entities, tidies
whitespace) and then `limit_to_sentences` cuts it at the end of a sentence
within 2000 characters (word boundary plus `...` only if one sentence alone is
too long). Text a feed declares as `text/plain` is only tidied. Re-fetching
fills in a missing summary (`COALESCE`, like `feed_url`) and never touches
read/saved/note state. `Storage::article_by_id` fetches one article. The
terminal app stores the summary but does not show it yet.

**OPML (feed-list import/export).** `core/src/opml.rs` has two pure functions
with no file or path handling: `parse_opml(text)` and `write_opml(&[SourceConfig])`.
Hand-written, no dependency (`quick-xml` is only a transitive dependency of
`feed-rs`). Folders become topics (innermost folder wins); a feed outside any
folder is filed under **"Uncategorized"**. Reddit sources are exported as
`https://www.reddit.com/r/NAME/.rss` and come back as plain RSS feeds. The
reader treats the file as untrusted: at most 2 MiB, 5000 feeds and 32 levels of
folders; only `http`/`https` addresses (anything else is counted as ignored);
custom entities are never expanded; repeats within the file are kept once and
counted. `sources.toml` stays the terminal app's stored format: it is
hand-edited, keeps comments, supports Reddit sources and `lists.toml`, none of
which OPML can express. OPML is only an import/export bridge. The Android app
has an import/export screen; the terminal app has two commands (decided
2026-10-11): `tuxwire import-opml FILE [--list NAME]` and
`tuxwire export-opml FILE [--list NAME]`, in `tui/src/opml_cli.rs`. They run
instead of the full-screen app, never touch the database and never download
anything (the next start fetches the new feeds). Without `--list` they use the
list remembered in `lists.toml`, else the first; a wrong `--list` name is an
error listing the real names, never a silent fallback. Import appends only
feeds whose address is not in the list yet (compared ignoring capitals, and a
Reddit source counts as its `.rss` address), in one write that keeps the
comments in `sources.toml` (`append_sources_at`, which writes a `.tmp` file and
renames it so a failure cannot leave a half-written config). A file that is not
OPML changes nothing. Export refuses to overwrite an existing file; `-` prints
to the screen. The apply step (which file to edit, comment-keeping) is
terminal-only because the two apps store feeds differently (`sources.toml`
against the database); only parsing and writing are shared.

**Fetching is two steps.** `fetchers::fetch_one` / `fetch_all` only talk to
the network and return `FetchOutcome`s (an article list or an error each);
`FetchReport::record_outcome` stores them and updates the tally. They are
separate so a program that shares its `Storage` behind a lock (the Android
app) can release it during the slow network step. The startup fetch, the `r`
refresh and the Android refresh all use these two steps.

#### Migrations

Every schema change ships as a numbered SQL file in `core/migrations/`
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
  section (or a one-shot notice), plus the active feed list's file,
  right-aligned: "Feed List: <path>" (the file icon in Nerd mode), home as
  `~`, `text_muted`. Hints/notices keep priority; the path gets the cells
  left over after them plus a 2-cell gap, is cut from the left by display
  cells keeping `/<filename>`, and is hidden when even the label plus file
  name won't fit, or below 60 columns (`NARROW_FOOTER_WIDTH`)
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
| `u`       | un-save the selected article, after a yes/no question (an article with a note loses the note too -- see "Un-save and the note lock") |
| `n`       | write or edit a note (a real note also saves the article; emptying the box asks before deleting an existing note) |
| `r`       | refresh all sources               |
| `S`       | view saved articles               |
| `y`       | view history (every article ever fetched) |
| `/`       | search titles/notes across the current view |
| `a`       | add a new source                  |
| `E`       | export every saved article into one combined Markdown file (Saved view only -- pressing it elsewhere shows a hint; regenerates the file fresh each press, and asks first if the file already exists) |
| `q`       | quit                             |

---

## Article States & Behavior

| State     | Meaning                                  | Notes                              |
|-----------|-------------------------------------------|-------------------------------------|
| unread    | default state, not yet interacted with    |                                      |
| read      | opened, or saved (saving implies read)     |                                      |
| skipped   | explicitly dismissed                       | purely visual/state — see below     |
| saved     | stays until the person un-saves it (`u`, after a yes/no question) | supports an optional free-text note |

**Saving auto-marks as read** — these are not tracked as independent
booleans in practice; pressing `s` sets both `saved = true, read = true`
in one action. Pressing `s` on an article that is already saved changes
nothing, including its save date.

### Un-save and the note lock (decided 2026-10-11, Shane)

- **Un-saving exists.** `u` (a key of its own, so `s` and `u` can never be
  confused) asks "Un-save?" naming the article; only a plain `y` does it.
  `Storage::unsave_article` clears `saved`, `saved_at`, `note` and
  `noted_at`; the article stays in the database and stays read.
- **A note and a save are locked together.** A note only exists on a saved
  article. Storing a real note (`Storage::update_note`) saves the article
  too, and un-saving deletes the note. So if the article has a note, the
  question says so ("un-saving also DELETES the note"); answering no leaves
  both in place and the footer explains that a note keeps the article in
  Saved. Clearing only the note is a separate action and leaves the article
  saved.
- **Nothing is deleted or replaced without a yes/no question** (a plain `y`
  confirms; any other key cancels): un-saving, deleting a note (emptying the
  note box on an article that has one), and `E` when the export file already
  exists. Android asks for the same things in the same words. Deliberately
  *not* confirmed for now: `x` (skip, which only recolors) and `Esc` in the
  note box (discards typing that was never saved); revisit if they cause
  trouble.
- No schema change and no migration: the existing columns are reused.

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
shade can be tweaked without touching Rust.

**Built-in themes + overrides (built).** `core/src/theme.rs` holds
`BUILTIN_THEMES` (plain RGB values, shared with the Android app), one table
entry per theme; `tui/src/theme.rs` turns them into ratatui colors and reads
`theme.toml`. The themes: Everforest (default), then
Dracula, Gruvbox, Catppuccin Mocha, Nord, Solarized Dark, Tokyo Night, and
Catppuccin Macchiato. Each fills the same 11 slots below; no new slots were
needed. `theme.toml` gains a root `base = "<name>"` (written by the `t`
picker via `toml_edit`), and every `[theme]` key became optional: a set key
overrides that slot on top of whichever base is selected. A full old-style
file therefore still resolves to exactly its own colors. An unknown `base`
is a load error listing the valid names.

Adding a theme = one new `BuiltinTheme` entry. Tests then require, for every
theme, WCAG contrast against `background` of ≥ 4.5 for `text_primary` and
≥ 3.0 for `text_muted` and every accent, plus ΔE (CIE76) ≥ 20 between each
pair of unread/read/skipped/saved/selected. Nord's `accent_read` uses
nord14, not nord7, because nord7 vs nord8 (`accent_selected`) is ΔE ≈ 10.

**Order and default.** The table's order doesn't matter. The `t` picker
lists themes alphabetically by name, ignoring case
(`theme::picker_order()`), and stores "which theme" as a `BUILTIN_THEMES`
index, so sorting only affects display. The default is named, not
positional: `DEFAULT_THEME = "Everforest"`, resolved by `default_index()`
for both `ThemeSettings::default()` and a `theme.toml` without `base`. The
bundled `theme.toml.example` also says `base = "Everforest"` (what a fresh
install loads). Tests pin the default to Everforest along every path,
check that the picker's first row (Catppuccin Macchiato) doesn't leak into
it, and check that the picker order is alphabetical.

The `t` picker previews live: moving the selection sets
`ThemeSettings::base` and re-resolves the theme, so the whole screen
repaints in it. `Enter` saves `base`, and `Esc` restores the original.

The original palette, now the `Catppuccin Macchiato` built-in (an
old-style `theme.toml` with exactly these lines resolves to it):

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

**Palette history:** Catppuccin Macchiato was the original (and only)
palette, chosen for being colorful without being chaotic. It is now one of
the eight built-ins, and Everforest is the default. All built-ins are dark.

### `icons.toml` (built)

Same idea as colors, applied to symbols: every icon is config-driven and
never hard-coded in UI code. `core/src/icons/mod.rs` holds the one icon table,
`Icon` name → (Nerd Font glyph, Unicode fallback, ASCII fallback). The UI
asks an `IconSettings` for an icon by name. `tui/src/icons/config.rs` reads and
writes `~/.config/tuxwire/icons.toml`. Modeled on zerohour's
`core/src/icons.rs`.

- **Modes:** `nerd` (default), `unicode`, `ascii`. `i` cycles them live and
  saves via `toml_edit`, which rewrites only the `mode` line.
- **Nerd Font Mono only.** Every glyph in the table, in all three modes, is
  exactly one terminal cell wide (a unit test checks this with ratatui's
  own width measurement). Regular Nerd Fonts render many icons wider than
  the one cell the terminal advances, which breaks ratatui's border and
  column alignment; the Mono variants don't.
- **Category icons:** built-in defaults for common topic names (with
  aliases, e.g. `infosec` → security), a user `[categories]` table whose
  values are an icon name or a single literal character, and a folder icon
  for anything unknown. A literal Nerd glyph is skipped in Unicode/ASCII
  mode, so it never shows as a box.
- **First run:** no `icons.toml` means the check hasn't been answered. The
  TUI opens with a popup showing a Nerd glyph and asking "Do you see an
  icon here?" `y` keeps Nerd, `n` switches to Unicode, and either answer
  writes the file. `Esc` defers the question to the next launch.
- **"New" items:** an article inserted during this session (startup fetch
  or `r`) that is still unread. Detected in `core/src/fetch_report.rs` by the
  `articles` row count growing across `insert_article`, so `storage` is
  unchanged.
- **Status notices:** `r` and startup report new/failed/offline in a
  one-shot footer message that clears on the next key. "Offline" means
  every attempted source failed with a `reqwest` connect/timeout error.

### `lists.toml` — multiple feed lists (built)

Optional `~/.config/tuxwire/lists.toml` (`tui/src/feedlists/`, ratatui-free)
defines named feed lists, each a file in the `sources.toml` format, with
`active = "<name>"` remembering the last one picked (`f`), saved via
`toml_edit`. Paths: absolute, `~/...`, or relative to the `lists.toml`
folder. Names are unique ignoring case. A malformed `lists.toml` is a
startup error, like the other config files.

- **One list (no file, no `[[list]]`, or one entry):** identical to before.
  Topic queries use `Scope::All`, `f` isn't in Keys (pressing it only shows
  a hint), and a broken `sources.toml` is still a clean startup error.
- **Shared storage, shared state.** One database. Migration **005** adds
  a nullable `articles.feed_url` (the `[[source]]` url) plus an index. It's
  numbered 5 because the archived FreshRSS build used 4, so a database
  that ran it is already at `user_version = 4`. A list's topic views are
  `feed_url IN (<its sources' urls>)` (`storage::Scope::Feeds`).
  Read/skipped/saved/note are per article, so they're shared across lists.
  Saved, History, search, and export stay global.
- **Backfill.** `insert_article` sets `feed_url` with
  `ON CONFLICT(url) DO UPDATE SET feed_url = COALESCE(feed_url, excluded.feed_url)`,
  which fills it only when missing and touches nothing else. On startup,
  `backfill_feed_urls` fills remaining NULLs by source *name* across
  every list, skipping names that map to more than one feed URL. Unfilled
  rows stay visible in the global views.
- **Startup.** `feedlists::activate` tries the remembered list, then the
  others in config order. Failures become one footer notice (list, `~` path,
  root-cause first line). If nothing loads, the TUI opens empty with the
  message. Only the active list is fetched.
- **Switching (`f`).** The picker shows name, `~` path, and unread count
  (`Storage::unread_count` per list; other lists' files are read, never
  fetched). `feedlists::try_switch` loads only the target and stays on
  the current list on failure. Success reloads topics and articles from the
  database (instant), updates the footer path, and saves `active`. The
  sidebar also includes the active list's configured topics before they
  have articles, so a never-fetched list can be refreshed with `r`.
- **`r` / `a`** act on the active list's file. Only the default
  `sources.toml` is auto-created when missing; any other missing list file
  is reported.
- **Adding lists:** by editing `lists.toml` only (config-driven, no
  interactive path prompts).

---

## Adding Sources

For v1, a single entry point: the in-app `a` keybind.

**Decision (2026-10-10, Shane): feed discovery is now part of adding a
source.** This reverses the original "no HTML parsing, no autodiscovery"
rule. Paste a feed address *or* a web page; `core/src/fetchers/discover.rs`
(`discover_feed`) works out the feed, and the terminal app and the Android app
share it:

1. **The address is a feed.** It downloads and parses as RSS/Atom (`feed-rs`,
   the same crate used for every regular fetch). Used as it is, one request.
2. **The page advertises a feed.** The `<link rel="alternate"
   type="application/rss+xml|atom+xml|feed+json" href="...">` lines in the
   page's `<head>` are read by hand (no HTML-parser dependency; the tag
   helpers are shared with the OPML reader). The first one that downloads as
   a real feed wins; up to 5 are tried.
3. **Common addresses.** If nothing is advertised, `/feed /rss /rss.xml
   /atom.xml /feed.xml /index.xml` are tried under the page's own path and
   then the top of the site: at most 12 guesses, 10 seconds each, never
   spawning work in the background. A guess only counts if it parses as a
   feed, so a web page that happens to exist at `/feed` is rejected.

**Comment feeds are never picked up by discovery** (steps 2-3): a link is
skipped when its `title` or its path/query has the *word* "comments"
("Comments Feed", `/comments/feed`, `?feed=comments-rss2`), and a downloaded
feed is skipped when its own title *starts with* "comments" or ends with
"comments feed". It is a rule of thumb: a blog called "Commentary" or
"Comment is free" is not rejected. A feed address typed directly (step 1) is
never second-guessed. If a page offers only a comments feed, the error says so.

The rest of the flow is unchanged:

1. Prompt for a feed or page address (`https://` is added when no scheme is
   typed; only `http`/`https` are accepted).
2. Discovery as above. Everything it downloads is checked the same way as a
   refresh: HTTP status, size limit (2 MiB read from a page, 20 MiB per feed),
   empty reply, and a `tuxwire/<version>` User-Agent.
3. On success, confirm screen: the address *of the feed* (which may differ
   from what was typed, with a line saying how it was found), a guessed name
   (from the feed's own `<title>` metadata, editable), and a **single
   required topic** — pick from existing topics (queried live via
   `Storage::topics()`) or type a new one. No source can be left without a
   topic.
4. On failure, show discovery's own message ("answered HTTP 404", "no feed
   found at ... (tried /feed /rss ...)", "only offers a comments feed") and
   let the user retry or cancel.
5. On confirm, write a new `[[source]]` block into `sources.toml` using the
   `toml_edit` crate rather than plain `toml` — `toml_edit` preserves
   existing formatting/comments on write instead of rewriting the whole
   file.
6. **Decision (2026-10-11, Shane): the new source is fetched right away.**
   Straight after the block is written, just that one source is fetched and
   stored (`fetch_added_source` in `tui/src/ui/mod.rs`; Android calls
   `refresh_feed` after `add_feed`), so the person sees its articles at once
   instead of an empty feed that looks broken. The footer reports the result
   like `r` does ("2 new article(s)"). A feed that fails to download stays
   saved, with the reason shown; its articles arrive on the next refresh.

Validation is still the existing parser: a feed that parses *is* a valid
feed. Discovery only decides *which address* to hand it.

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
    saved_at: Option<DateTime<Utc>>,
    noted_at: Option<DateTime<Utc>>,
    feed_url: Option<String>, // the feed (source url) it came from; migration 005
    summary: Option<String>,  // short plain-text summary; migration 007
}
```

---

## Project Layout

The repository is a Cargo **workspace** of two crates. `core/` is the shared
engine with no terminal code in it; `tui/` is the terminal program. A native
Android app (a separate, private repository, `tuxwire-android`) uses `core/`
too, through a thin UniFFI wrapper, so fetching, parsing, storage, themes and
icons are written once.

```
tuxwire/
├── Cargo.toml             # workspace root: members + shared dependency versions
├── core/                  # crate `tuxwire-core` -- NO ratatui/crossterm, NO $HOME
│   ├── Cargo.toml
│   ├── migrations/        # numbered SQL files (next: 008)
│   └── src/
│       ├── lib.rs
│       ├── models.rs      # Article
│       ├── text.rs        # HTML -> plain text, sentence-aware length cap
│       ├── opml.rs        # OPML feed-list reader and writer (no file access)
│       ├── export.rs      # Markdown text of the saved-articles/notes export (no paths)
│       ├── storage/       # SQLite access + migrations (Storage::open_at(path))
│       ├── fetchers/      # Fetcher trait, RSS, feed discovery, Reddit stub, SourceConfig
│       ├── fetch_report.rs# per-fetch new/failed/offline tally
│       ├── icons/         # the icon table (Icon, IconMode, IconSettings)
│       ├── theme.rs       # Rgb, Palette, the eight built-in palettes
│       └── scoring.rs     # reserved for v2 skip-weighting (unused in v1)
├── tui/                   # crate `tuxwire` -- the terminal program (binary)
│   ├── Cargo.toml
│   ├── *.toml.example     # bundled default configs
│   └── src/
│       ├── main.rs
│       ├── ui/            # ratatui views/widgets
│       ├── theme.rs       # Theme (ratatui Colors) + theme.toml loading/saving
│       ├── fetchers/      # sources.toml loading/validation/editing
│       ├── icons/         # icons.toml loading/saving
│       ├── feedlists/     # lists.toml + active-list choice/switching
│       ├── export.rs      # where the export file goes (export.toml); calls core's export
│       ├── opml_cli.rs    # import-opml / export-opml commands
│       └── paths.rs       # where the database lives (XDG) + ~ abbreviation
├── docs/
│   └── ARCHITECTURE.md    # this file
├── CHANGELOG.md           # git-commit / changelog style, matches blog aesthetic
└── NOTES.md
```

**The rule between the two crates:** `core/` never decides where a file lives
and never touches a terminal. The program using it passes paths in
(`Storage::open_at`), and turns core's plain data (`Rgb`, `SourceConfig`)
into whatever its own toolkit needs (ratatui `Color`s here, Compose colors on
Android). The terminal's file-based config (`sources.toml`, `lists.toml`,
`theme.toml`, `icons.toml`, `export.toml`) stays in `tui/`; the Android app
keeps its feeds and settings in the database instead.

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

- Triggered by the `E` keybind from the Saved view (if the export file
  already exists, `E` asks "Replace export?" first, since the file is
  rewritten from scratch) — no-op from
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
- **Two sections in the one file (decided 2026-10-11).** Saved articles
  *without* a note come first, as a short list; the articles *with* notes
  follow under "Notes", each with its note in full. Within each section the
  newest comes first (by when the note was written, else when it was saved),
  and each heading shows a count. The formatting lives in the shared core
  crate (`core/src/export.rs`: `render_saved_markdown` and
  `format_article_markdown`) so the terminal app and the Android app write
  exactly the same file; each app only decides where the file goes. Exporting
  never changes or deletes anything. `include_unnoted` (in `export.toml`; a
  switch on Android) chooses whether the first section is written; it
  defaults to on, so everything saved is exported.
- **Must include a link back to the original article** — the note is
  context, not a replacement for the source. Uses the real `saved_at`
  (and `noted_at`, if the note was added/edited after saving) timestamps
  — not the article's publish date:

```markdown
# Saved articles and notes

## Saved articles (1)

- [Another saved article](https://phoronix.com/...) — Phoronix, saved 2026-08-10

## Notes (1)

### Btrfs send/receive got noticeably faster in 6.18
Source: [r/linux](https://reddit.com/r/linux/...) — saved 2026-08-12, noted 2026-08-14

> your note goes here, verbatim from the `note` column
```

  Several notes are separated by a `---` on its own line. A title is kept on
  one line and its square brackets are escaped so it cannot break its link.

- **Output location: config-driven, no per-export prompt.** Consistent
  with the rest of the app (`sources.toml`, `theme.toml` — nothing else
  interactively prompts for a path), export writes to a default directory
  set in a new `export.toml`:

```toml
[export]
path = "~/tuxwire-notes/"
include_unnoted = true
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

**Packaging niceties already decided, ready for whenever Tier 1 starts:**
`w3m` should be listed as an `optdepends` (AUR) / `Recommends` (COPR) —
optional, not required, surfaced to the person at install time by the
package manager itself rather than an in-app first-run popup (deliberately
not building the latter — anyone capable of building this from source can
read one line in the README about it, same reasoning as the cuts above).

---

## Optional CouchDB sync (decided 2026-10-12, Shane)

Sync shares **saved articles and their notes**, and the **Read Later list**
(see "Read Later" below), between a person's devices
through a CouchDB server they run themselves. It is **off by default** and
nothing is ever sent unless the person turns it on and asks for a sync.
Read state is *not* synced (it would mean a document for every article ever
read); that may become a separate switch later.

**Privacy and safety rules (hard requirements).**
- Nothing syncs, and no request is made, while sync is off. `sync::sync_now`
  refuses outright; there is no background or startup sync in this first version
  (the person presses "sync now" / runs `tuxwire sync`).
- The server address must be `https://`. Plain `http://` is accepted only for
  this computer (`localhost`, `127.0.0.1`), so a password is never sent
  unprotected to another machine. Credentials may not be written inside the
  address.
- The password is never stored in the SQLite database, never in the repository,
  never in a log or an error message, and `SyncConfig`'s `Debug` output hides it
  and the server address. `core/` never decides where it lives: the app passes
  it in. Android keeps it in private app storage (Keystore-backed); the terminal
  app reads it from the OS keyring or a mode-0600 file, or an environment
  variable. Nothing identifying (hostnames, usernames, passwords, tailnet names)
  belongs in the repo, the docs examples, tests or changelog.
- Errors are short plain sentences ("wrong username or password", "database not
  found", "offline: timed out") and never echo the address or the password.
- Documents are stored unencrypted on the server (like any CouchDB data); the
  server is the person's own and should be reachable only privately (for
  example over Tailscale).

**How it works.**
- **One document per article**, named `article:<the article's address>`. The
  address is the identity because `articles.url` is already unique and, unlike
  local article numbers, is the same on every device. A document carries the
  title, source, topic, published date and summary (so a device that never saw
  the article can show it), plus `saved`, `saved_at`, `note`, `noted_at` and
  `updated_at`. Format version `v` is 1; documents from a newer version are
  skipped and reported, and unreadable ones are skipped and counted.
- **Newer wins.** `articles.sync_updated_at` (migration 008) is a change stamp in
  milliseconds, strictly increasing per article, set by save, note and un-save
  (not by no-ops). When a pulled document's `updated_at` is strictly newer, its
  whole saved-and-note state replaces the local one; otherwise the local state
  stays and is pushed. The state moves as a unit, never half a note.
- **Un-save is a document**, not a deletion: `saved: false` with a newer
  `updated_at`. A CouchDB deletion would leave nothing to compare dates with, so
  the other device could not tell "un-saved later" from "never heard of it" and
  would save it again.
- **The note lock holds across devices.** Every document is normalized: a real
  note means saved; an unsaved article has no note.
- **Bookkeeping.** `sync_docs` remembers each document's CouchDB revision and the
  stamp last pushed or pulled; the pull checkpoint (`settings` key `sync.since`)
  moves in the *same transaction* as the data it covers, so a crash cannot skip
  changes. Changing the server or database should call `reset_sync_state`: the Android
  app does it when the address or database changes on its Sync screen, and the
  terminal app has `tuxwire sync --reset` (asks y/N, needs no `sync.toml`, no
  network and a terminal; it never touches articles, notes or feeds).
- **One cycle:** pull (CouchDB `_changes`, 200 at a time), then push everything
  changed locally (`_bulk_docs`, 200 at a time); a conflict (another device wrote
  in between) triggers another pull-then-push round, up to three. Deleting a
  document by hand on the server just makes it be written again.
- **Migration 008** adds one nullable column and one table, in one transaction,
  and stamps already-saved articles with the time of the migration so the first
  sync sends them. As with every migration, back up the real database before the
  first run of a build that contains it.
- Built on `reqwest` and `serde_json`, which core already had: no new
  dependency. Tests use an in-memory fake CouchDB (`core/src/sync/fake.rs`) that
  checks the login and answers stale revisions with conflicts.

**Known limits.** A conflict replaces the whole saved/note state of one article
(editing the note on one device while another un-saves it keeps only the newer
change). Clocks: "newer" is judged by each device's clock, so devices whose
clocks are far apart can pick the wrong winner (a clock check is a possible
later addition). A pulled article this device does not have shows in Saved under
its topic but belongs to no local feed.

**Order of work.** Core (done) -> Android (done: settings screen, sync now,
password encrypted with the Android Keystore) -> terminal (done, below).

**Terminal.** `tui/src/sync_config.rs` reads `~/.config/tuxwire/sync.toml`
(never created automatically; no file means no sync). A `password` key in it is
an error. The password comes from `TUXWIRE_SYNC_PASSWORD`, else a
`password_file` that must not be readable by group or others (checked on Unix).
Decision (2026-10-12): **no keyring support**, because a keyring crate would be
a new dependency; revisit only if asked. `tui/src/sync_cli.rs` has
`tuxwire sync [--test]` and the reader's `c` key; both open a *second*
connection to the database for core's `sync_now` (which wants a
`&Mutex<Storage>`), so `ui::run` keeps its plain `&Storage`. They never run
together because the reader waits for the sync. `c` blocks the screen while it
runs (up to ~30 s per request) and has no progress line.

### Read Later (decided 2026-10-19, Shane)

A list of links to come back to, **separate from Saved**. Saved is a permanent
archive with notes; an item on Read Later is meant to be read and then discarded
or promoted to a saved article. Any web address can go on it (pasted in), not
only articles from a feed.

- **Table:** migration 010 adds `read_later` (key = the address exactly as
  written, title, `added_at`, `present`, `read`, `updated_at`, `rev`,
  `pushed_updated_at`). The table is both the list and its sync record. A
  removal keeps its row with `present = 0`, like feeds, so other devices can tell
  "removed later" from "never heard of it". Only `http`/`https` addresses are
  accepted (`is_syncable_url`), in core and again on every pulled document.
- **Rules (`core/src/storage/read_later_store.rs`):** adding an address already
  on the list is a no-op; adding one that was removed brings it back, unread.
  Opening marks it read and it stays (shown in the usual read colour) until it
  is discarded or saved. `promote_read_later` and `save_link` share one helper:
  reuse the article for that address, leave an already-saved one (and its note)
  alone, or make a new article (source and topic "Read Later") for an address
  tuxwire has not seen; saving stamps it for the ordinary saved-article sync and
  takes the link off the list, in one transaction.
- **Sync (`core/src/sync/later.rs`):** one `later:<address>` document per link,
  sent as part of `sync_now` on the **same switch as saved articles** (a list
  the person builds on purpose). Its own pull checkpoint (`sync.later.since`), so
  a device updated later still reads earlier documents; the newer `updated_at`
  wins; `reset_sync_state` forgets it too. Builds that do not know `later:` ignore
  it. Sent: address, title, added time, present, read. Nothing else.
- **Pasting a link:** `fetchers::link_title::fetch_page_title` downloads with the
  shared careful download (status, 4 MiB cap, 10 s), searches the first 256 KiB
  for `<title>`, decodes entities and tidies white space. Failure means "no
  title": the address is used. The person is **asked each time** whether the
  link goes to Read Later or to Saved.
- **Terminal (`tui/src/ui/read_later.rs`):** a popup screen like the `f`/`t`
  pickers, not a fourth view, so Topic/Saved/History are untouched. Keys: `L`
  list, `R` add the selected article, `p` paste. In the list: `Enter` open,
  `s` save, `x` discard (asks), `p` paste, `Esc`/`q`/`L` close.
- **Not decided yet:** read marks for ordinary articles. They would be their own
  switch (they show what someone opened), one small `read:<address>` document per
  *opened* article, with an age limit on a first sync; not built.

### Optional feed-list sync (decided 2026-10-17, Shane)

Mirrors a person's **feed list** between devices through the same CouchDB, so a
feed added on the phone appears in the terminal app and the other way round.
It is a **separate switch** from saved-article sync (`SyncConfig::feeds`, set
with `with_feeds`), **off by default**, and `sync::feeds::sync_feeds` refuses
unless both switches are on, because a feed list shows what someone reads.

- **One document per feed**, `feed:<address in lower case>`: `url`, `name`,
  `topic`, `source_type` (only `rss`), `present`, `updated_at`, `v` = 1. The
  lower-case address is the identity, so capitals never make two feeds. Article
  documents and feed documents share the database; each sync ignores the other's
  documents, and builds without feed sync ignore `feed:` documents.
- **Adds are automatic, removals are asked.** A feed another device added is
  offered to the app (`pending_feed_additions`), which adds it to its own list
  and calls `feed_added_locally`. A feed another device *removed* is only listed
  (`pending_feed_removals`); the app asks the person, and only on a yes removes
  it and calls `feed_removed_locally`. A no calls `dismiss_feed_removal`: the
  feed stays and nothing is sent. A newer change from the other device asks
  again. This keeps the rule that nothing is removed without a confirmation.
- **Removal is a document**, `present: false` with a newer `updated_at`, for the
  same reason un-save is: a deletion would leave nothing to compare.
- **Newer wins** (the same `remote_wins` rule as saved articles); a re-added feed
  is a new addition.
- **Core never touches an app's feed list.** The terminal app keeps feeds in
  `sources.toml` (comments, several lists, Reddit) and Android in the `feeds`
  table, so each app passes its whole current list to `sync_feeds`. Core keeps
  only the `feed_sync` table (migration 009): per address, present or removed,
  since when, whether this device has it, the CouchDB revision. Changes are
  noticed at sync time, so their time is the time of the sync that saw them.
- **An empty list is distrusted.** Handing in an empty list while sync believed
  the device had feeds records no removals (the report says so): far more likely
  a read problem than the person deleting everything.
- **Order of a sync:** pull, record the device's list, push; conflicts go another
  round, up to three, like articles. The pull has its **own checkpoint**
  (`sync.feeds.since`), so turning the switch on later misses nothing written
  while it was off.
- **Input is untrusted.** A pulled document is used only if the name matches the
  address, the kind is `rss`, the address is an ordinary `http(s)` address of at
  most 2048 characters, name (200) and topic (100) are non-empty, and nothing
  contains a control character (terminal escape sequences). At most 5000 feed
  documents are read in one sync. Others are skipped and counted.
- **Not synced:** renaming a feed, changing its topic, Reddit sources (core does
  not fetch them yet), and which list of several a feed is in (the terminal app
  syncs one list, named in `sync.toml`).
- **Terminal app (`tui/src/feed_sync.rs`, `sync_cli.rs`):** `sync.toml` gains
  `feeds` (bool, default false) and `feed_list` (a name in `lists.toml`; default
  the first list; an unknown name is an error, never a quiet fallback). Only RSS
  sources go to core. Additions are appended with `append_sources_at` (comments
  kept, safe replace) and then `feed_added_locally`. **A confirmed removal
  comments the `[[source]]` block out** (`# ` in front of each line, a note
  above it saying how to undo it) instead of deleting it, is re-parsed as TOML
  before the file is replaced, and only then calls `feed_removed_locally`; a no
  calls `dismiss_feed_removal`. `tuxwire sync` asks y/N on a terminal; with
  stdin not a terminal it lists the removals and changes nothing (they are asked
  again next time). `c` in the reader never prompts: it reports new feeds and
  waiting removals. A change by hand to `sources.toml` is noticed at the next
  sync, like any other local change.
- **Migration 009** adds the `feed_sync` table only. Back up the real database
  before the first run of a build that contains it.

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
- Optional sync of saved articles and notes to a user's own CouchDB, off by
  default and tucked away in settings (core built; Android and terminal screens
  to come -- see "Optional CouchDB sync" above). Syncing read state is a possible
  later, separate switch, and the Read Later list (built) rides on the sync switch.
- Offline caching strategy: cache-on-fetch vs. cache-on-save/open
- Export location/config for Markdown export — likely an `export.toml`
- Duplicate article detection across sources (same story, multiple sites —
  note: with the one-topic-per-source rule now settled, this is scoped to
  duplicates *within* a topic, not cross-topic, which simplifies it)
- A `:` command mode as an alternative to single-key binds for less-common
  actions

This is a living document — treat every future feature decision as an edit
here first, code second.
