# tuxwire — Notes

A plain-language recap of each piece of work: what exists, how to run it, and what's worth
remembering.

---

## Core split: `core/` + `tui/` workspace (branch `feature/core-split`, 2026-10-09)

Groundwork for the native Android app. The repo is now a Cargo workspace with two crates, so
the Android app (a separate private repo) can reuse tuxwire's logic instead of copying it.
**Nothing changes for you as a user**: same commands, same config files, same database, same UI.

### What moved where

| Crate | Contains |
|---|---|
| `core/` (`tuxwire-core`) | `Article`, SQLite storage + migrations (`Storage::open_at(path)`), the `Fetcher` trait / RSS fetcher, `SourceConfig`, `FetchReport`, the icon table, and the eight built-in theme palettes as plain `Rgb`. **No ratatui, no `$HOME`.** |
| `tui/` (`tuxwire`) | The ratatui UI, key handling, `sources.toml` / `lists.toml` / `theme.toml` / `icons.toml` / `export.toml` loading, w3m/browser launching, and `paths.rs` (where the database lives). |

### Commits, in order
1. Move the crate into `tui/` and make the repo a workspace (a pure file move).
2. Extract `core/` (models, storage, fetchers, fetch report, icons).
3. Move the theme palettes into `core/` as plain RGB (`Rgb`, `Palette`); `tui/` converts to ratatui colors.
4. Docs (this entry, CLAUDE.md, ARCHITECTURE.md, CHANGELOG).

### How to test
```bash
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check   # 101 tests: 62 tui + 39 core
cargo tree -p tuxwire-core | grep -E 'ratatui|crossterm'                       # prints nothing
cargo build && t=$(mktemp -d) && env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME="$t" ./target/debug/tuxwire
```
Then try your usual: `t` themes, `i` icon modes, `f` lists, `r` refresh. Back up your real database
before the first run of the new build against it (nothing migrates, but it is your data).

### Worth knowing
- `Storage::open()` became `Storage::open_at(&Path)`. The XDG lookup moved to `tui/src/paths.rs`.
  `Storage::open_in_memory` and `IconMode::ALL` lost `#[cfg(test)]` so the other crate's tests can use them.
- `#[allow(async_fn_in_trait)]` on `Fetcher`: a lint that appears now that the trait is `pub` in a library.
  Harmless here; the comment in `core/src/fetchers/mod.rs` explains why.
- Dependency versions and features are unchanged; they are now declared once in the root `Cargo.toml`
  (`[workspace.dependencies]`). `serde_json` is still listed but unused, as it was before.
- The fetch-and-insert duplication noted here was removed on `feature/android-feeds`: both loops now use
  `fetchers::fetch_one` + `FetchReport::record_outcome` (see below).
- Next migration number is **008** (004 was the FreshRSS one, never merged; 006 added `feeds` and `settings`, 007 added `summary`).

## Standalone UI: icons + themes on master (branch `feature/standalone-ui`, 2026-10-08)

tuxwire is a standalone RSS reader. The FreshRSS integration is dropped: `feature/freshrss` is
archived as the tag `archive/freshrss` and won't be merged. The icon and theme work, first
built on top of that branch (`feature/icons`, `feature/themes`), was re-applied here onto
`master`, without anything FreshRSS-specific. All of those old branches and the tag are
unchanged.

### Commits, in order

1. `cargo fmt` + clippy fixes, redone against master (the old fmt commit also formatted
   `src/freshrss/`).
2. Fix: a non-ASCII hex color in `theme.toml` (e.g. `"€€"`) panicked at startup.
3. Icons. This includes the escape-sequence docs fix: the examples use the literal TOML escape
   `"\uF015"`, not the decoded glyph. It can't be a separate earlier commit here, because
   the files it fixes are created by this one.
4. Theme picker with seven built-in themes.
5. Catppuccin Macchiato as the eighth built-in.
6. Docs (README, ARCHITECTURE, CHANGELOG, this file).
7. `CLAUDE.md` for the standalone app (FreshRSS rules removed; the branch rule is now
   "feature branches off master; master changes only by merging a branch whose tests pass").
8. Theme picker in alphabetical order, with the default made explicit (see below).

### What was left out (FreshRSS-only)

- The `FreshRSS/<category>` → `<category>` last-segment icon lookup and its test.
- The `footer_status` plumbing (FreshRSS's persistent warnings). Here the one-shot `notice` is
  the footer's only status line.
- `#[cfg(feature = "freshrss")]` signatures, `FreshRssStartup`, the "Syncing FreshRSS…"
  redraws, and every `--features freshrss` command and test count in the docs.

### What exists

| File | What it does |
|---|---|
| `src/icons/mod.rs` | **The one icon table**: `Icon` → (Nerd glyph, Unicode, ASCII), plus `IconMode`, built-in category names (`linux`, `infosec`, `sports`, ...), and `IconSettings`, which the UI asks for every glyph. |
| `src/icons/config.rs` | Reads/writes `~/.config/tuxwire/icons.toml`. A missing file means first run. Saving rewrites only `mode`. |
| `src/fetch_report.rs` | One fetch round's tally: sources tried/failed/unreachable, and which article IDs were new. |
| `src/theme.rs` | **The one theme table**, `BUILTIN_THEMES` (8 entries). `ThemeSettings` = `base` + optional `[theme]` overrides, combined by `resolve()`. Also the contrast tests. |
| `src/ui/mod.rs` | Draws icons everywhere and the footer `notice`. `i` cycles icon mode, `t` opens the theme picker (live preview), and the first run opens the icon check. |
| `src/main.rs` | Loads both settings files, records the startup fetch, and hands everything to `ui::run` as `SessionStart`. |

**Tests:** 67 (master had 29). They cover glyph widths in every mode, icon names, category
lookup and overrides, `icons.toml` parsing and saving, offline detection against a refused
connection on `127.0.0.1:1`, article state icons, footer messages, theme lookup, override
merging, `theme.toml` saving, and per-theme contrast and distinctness for all 8 themes.

### Making Catppuccin Macchiato the default

(Updated after the alphabetical-picker commit: the default is now a named constant, not
"entry 0", so table order no longer matters.)

1. **`src/theme.rs`: set `DEFAULT_THEME = "Catppuccin Macchiato"`.** That's the default for a
   `theme.toml` with no `base` line and for `ThemeSettings::default()` (both go through
   `default_index()`). There's no need to move any table entry.
2. **`theme.toml.example`: change `base = "Everforest"` to `base = "Catppuccin Macchiato"`.**
   That's what a fresh install loads. Its "(default)" comment and its commented-out override
   values (Everforest's, cosmetic only) should follow.
3. **Tests:** `default_is_everforest_by_name_not_by_position` deliberately pins the literal
   `"Everforest"` and checks that the picker's first row (Macchiato) is *not* the default. It
   must be rewritten for Macchiato, and the "first row isn't the default" assert dropped,
   since Macchiato is alphabetically first. Rename
   `bundled_default_is_everforest_without_overrides` too.

Then update the docs that say "Everforest (default)": README, ARCHITECTURE, and the CHANGELOG
"Changed" entry, which goes away (Macchiato was the old default too). Users with an old full
`theme.toml` see no difference either way.

### Contrast rules

- `text_primary` vs `background` ≥ 4.5:1 (WCAG AA); `text_muted` and every accent ≥ 3.0:1.
- `panel_border` ≥ 1.25 and `text_dim` ≥ 1.5 (visible, but meant to recede).
- unread / read / skipped / saved / selected pairwise ΔE ≥ 20 (CIE Lab). Nord's nord7 vs
  nord8 (ΔE ≈ 10) failed this, so Nord's `accent_read` is nord14 green.
- Lowest margins: Solarized Dark text 5.6, Nord `accent_breaking` 3.05, Macchiato muted 3.15.

### Commands

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo test icons      # icon table + config tests
cargo test theme      # theme table, overrides, contrast

# Run against a throwaway config/data dir (never the real one). Build first, then override
# HOME only for the binary. -u drops XDG_* so they can't point back at the real dirs.
cargo build
t=$(mktemp -d); env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME="$t" ./target/debug/tuxwire

# Simulate offline: point every request at a dead local proxy.
env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME="$t" HTTPS_PROXY=http://127.0.0.1:1 \
    HTTP_PROXY=http://127.0.0.1:1 ./target/debug/tuxwire

# Drive the TUI from a script in a *named* tmux session, and kill only that session.
tmux new-session -d -s tuxwire-test -x 140 -y 48 \
    "env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME=$t ./target/debug/tuxwire; sleep 600"
tmux send-keys -t tuxwire-test t k              # open theme picker, preview the previous theme
tmux capture-pane -p -t tuxwire-test            # dump the screen as text
# The highlighted row's background color (changes with the theme preview):
tmux capture-pane -p -e -t tuxwire-test | sed -n 9p | grep -o '48;2;[0-9;]*' | head -1
tmux kill-session -t tuxwire-test               # never `tmux kill-server`
```

Note: `capture-pane` output keeps Nerd glyph bytes, but many viewers can't display Private Use
Area characters, so rows can *look* shorter than they are. Check with `od -c` before
concluding something is misaligned.

### Decisions worth knowing

- **Keys:** `i` (icon mode) and `t` (theme) were both free.
- **Why Nerd Font *Mono*:** regular Nerd Fonts draw many icons wider than the one cell the
  terminal advances, so borders drift. Mono keeps every icon to one cell, and all fallbacks
  are one cell too (a test checks it).
- **Separate `icons.toml`** rather than a section of `theme.toml`, and **first run = no
  `icons.toml`** (delete it to see the check again).
- **`base` is a root key**, because `theme` is already the `[theme]` table's name.
- **Overrides apply to every built-in.** That keeps old files identical, but a full old
  `[theme]` makes the picker look inert, so the popup says "overrides 11 of 11 colors".
- **Unknown `base` is an error**, matching tuxwire's other config handling (zerohour falls
  back instead).
- **"New"** = inserted this session and still unread, detected by the `articles` row count
  growing across each insert, so `storage/` didn't change.

---

## Alphabetical theme picker, explicit default (branch `feature/standalone-ui`, 2026-10-08)

The `t` picker now lists the built-in themes alphabetically by name, ignoring case:
Catppuccin Macchiato, Catppuccin Mocha, Dracula, Everforest, Gruvbox, Nord, Solarized Dark,
Tokyo Night. **The default is still Everforest.**

### Why the default needed fixing first

Before this, "the default" meant `BUILTIN_THEMES[0]`. That was used when `theme.toml` had no
`base` line, and by `ThemeSettings::default()` (a derived `Default`, so `base: 0`). Any change
to the table's order would have silently changed the default. Sorting the picker by moving the
table entries would have made the default Catppuccin Macchiato.

### What changed

| Where | Change |
|---|---|
| `src/theme.rs` | `DEFAULT_THEME = "Everforest"` is now a real constant (it was test-only), and `default_index()` looks it up by name. `ThemeSettings`'s `Default` is written by hand to use it, and `parse` uses it when `base` is missing. New `picker_order()` returns `BUILTIN_THEMES` indices sorted by lowercase name. The table itself is unchanged; its order no longer matters. |
| `src/ui/mod.rs` | `Mode::ThemePicker.selected` is now a *row* (a position in `picker_order()`). Moving maps row → theme with `order[row]`, and opening finds the current theme's row. `themes.base` stays a table index, so saving and `Esc` work as before. |

### Tests (67, up from 66)

- `default_is_everforest_by_name_not_by_position`: the literal `"Everforest"` comes out of
  `default_index()`, `ThemeSettings::default()`, an empty `theme.toml`, and a `theme.toml` with
  only overrides. It also checks that the picker's first row (Macchiato) is not the default.
  I verified it catches the bug: temporarily making `default_index()` return
  `picker_order()[0]` fails it with `"Catppuccin Macchiato"` vs `"Everforest"`.
- `picker_order_is_alphabetical`: each theme appears exactly once, names are non-decreasing
  ignoring case, and the full expected order is spelled out.
- The bundled-example test now compares against `BUILTIN_THEMES[default_index()]`, not `[0]`.

### Commands

```sh
cargo test theme      # includes the two new tests
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check

# Live check (named session; kill only it):
tmux new-session -d -s tuxwire-sort-test -x 140 -y 48 \
    "env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME=$t ./target/debug/tuxwire; sleep 600"
tmux send-keys -t tuxwire-sort-test t
tmux capture-pane -p -t tuxwire-sort-test       # rows alphabetical, ▸ on Everforest
tmux kill-session -t tuxwire-sort-test
```

---

## Multiple feed lists + footer path (branch `feature/feed-lists`, 2026-10-08)

Branched from `master` (which has the icons/themes work merged). Note: the "footer-path"
work that was expected on master didn't exist anywhere, so it's built here as the first
commit.

### Decisions (agreed before building)

- **Shared read state.** One database. Read/saved/note follow the article, whichever list
  you're in. Saved, History, `/` search, and `E` export stay global.
- **Key `f`**, a separate `lists.toml`, and lists added **only by editing `lists.toml`**
  (CLAUDE.md: "config-driven, no interactive path prompts").
- **One list = identical to before**, including a broken `sources.toml` being a clean startup
  error (nothing to fall back to). With several lists: footer message plus fallback.

### Commits, in order

1. **Footer path.** "Feed List: <path>" right-aligned, `~` for home, cut from the left by
   display cells keeping `/<filename>`, hidden when it can't fit or below 60 columns (tuxwire
   had no narrow mode, so `NARROW_FOOTER_WIDTH = 60`, zerohour's threshold). Nerd mode shows
   the file glyph; Unicode/ASCII say "Feed List:". Muted slot, no new theme slots. New
   `src/paths.rs` (ratatui-free `~` abbreviation).
2. **Migration 005, `articles.feed_url`.** Numbered 5, not 4: the archived FreshRSS build used
   4, so a database that ever ran it is at `user_version = 4` and would silently skip a new
   migration 4 (a test covers this). `insert_article` fills a missing `feed_url` on
   re-fetch (`COALESCE`, nothing else touched), and a startup backfill matches old rows by
   source name, skipping ambiguous names.
3. **`src/feedlists/`** (ratatui-free): `lists.toml` parsing/validation, `~`/relative paths,
   `activate` (startup choice with fallback), and `try_switch` (switch, or stay put). Loaders
   are closures, so the logic is tested without files. It carried a temporary
   `#![allow(dead_code)]` until commit 4 so that each commit passes clippy on its own.
4. **Picker and switching.** `storage::Scope` limits topic views and unread counts to the
   active list. `f` picker, instant switch (database only), `r`/`a` on the active list's file.
   - **Bug found in the live test and fixed:** a never-fetched list had no topics, and `r`
     refreshes the selected topic, so it couldn't be fetched at all. With several lists the
     sidebar now also shows the list's *configured* topics.
5. **Docs.**

### How it behaves

| Situation | Result |
|---|---|
| No `lists.toml` | Single list on `sources.toml`; no `f` in Keys; `f` shows a hint about `lists.toml` |
| Several lists, remembered one loads | That list; only it is fetched |
| Remembered list's file missing/broken | Footer: `✗ Feed list "x" couldn't be loaded (~/…: reason); using "main"` |
| `active` names no list | Footer says so; first list used |
| No list loads | TUI opens empty, footer explains |
| Picker on a broken list | Row shows `✗ unavailable`; Enter → message, `still on "<current>"` |
| Switch | Instant (database only); footer path updates; `active` saved; press `r` to fetch |

### Tests (101, up from 67)

Footer: short path, home abbreviation, long path, wide characters, no space, plus
label/ellipsis per icon mode. `paths`: under home, home itself, lookalike prefix, outside home.
Storage: `feed_url` round trip, fill-only-missing (state preserved), backfill
(unique/ambiguous/unconfigured/already set/rerun), migration 005 on a version-4 database, and
`Scope` filtering plus unread counts. feedlists: parsing and paths, single-list cases, every
validation error, `~` handling, `save_active` preserving the file, activation
(default/remembered/fallback/unknown/none/loads-only-what-it-needs), switching
(ok/no-op/failed), one-line reasons. UI: `truncate_path_left`, `list_scope` single vs
several, configured topics in the sidebar.

### Commands

```sh
cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check
cargo test feedlists      # list config + fallback + switching
cargo test feed_list      # footer path fitting
cargo test scope          # per-list database queries

# Throwaway setup with three lists (one deliberately broken):
t=$(mktemp -d); mkdir -p "$t/.config/tuxwire"
cat > "$t/.config/tuxwire/lists.toml" <<'TOML'
[[list]]
name = "main"
path = "sources.toml"
[[list]]
name = "casual"
path = "casual.toml"
[[list]]
name = "broken"
path = "~/nowhere/missing.toml"
TOML
printf '[[source]]\nname = "LWN"\ntype = "rss"\nurl = "https://lwn.net/headlines/rss"\ntopic = "kernel"\n' \
    > "$t/.config/tuxwire/casual.toml"
cargo build
tmux new-session -d -s tuxwire-lists-test -x 140 -y 48 \
    "env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME=$t ./target/debug/tuxwire; sleep 600"
tmux send-keys -t tuxwire-lists-test f          # picker
tmux send-keys -t tuxwire-lists-test j Enter    # switch to casual, then r to fetch it
tmux capture-pane -p -t tuxwire-lists-test | tail -1   # footer
tmux kill-session -t tuxwire-lists-test         # only this session
```

### Not done / worth knowing

- **Moving a feed between lists:** works by editing the files. Its old articles follow it,
  since membership is by `feed_url`.
- **Two lists with the same feed:** both show it, with shared state. Same article URL from two
  *different* feeds: `feed_url` keeps the first feed it was fetched from.
- **Not updated: CLAUDE.md's "Environment notes".** They still list the config files without
  `lists.toml`. Not in this task's doc list.

## Android groundwork in core (branch `feature/android-feeds`, 2026-10-09)

1. **Migration 006: `feeds` + `settings`.** For the Android app; the terminal app ignores both.
   `Storage::{add_feed, remove_feed, list_feeds, articles_by_feed, get_setting, set_setting}`.
   Adding a URL that is already there is an error ("already added"). Removing a feed keeps its
   articles. `articles_by_feed` ignores topic, so older articles stay visible if a feed's topic changes.
2. **`fetch_one` / `fetch_all` + `FetchReport::record_outcome`.** Fetching and storing are separate so
   a caller holding a database lock can release it during the network step. Core never prints; failure
   lines go in `FetchReport::failure_messages`. `fetch_all` was sequential here; it is parallel now (see
   the parallel refresh section).
3. **Desktop loops converted** (separate commit): `main.rs` and `ui::refresh_topic` use the shared step.
   Smoke test with a throwaway `HOME` and a dead feed: the TUI started and `skipped Dead: ...` was printed
   as before.
- Tests: 110 pass (101 before, 9 new: 7 for feeds/settings, 2 for fetch outcomes); clippy and fmt clean.

## Article summaries in core (branch `feature/article-summary`, 2026-10-09)

1. **`core/src/text.rs`.** `html_to_text` (hand-written, no new dependency), `tidy_plain_text`, and
   `limit_to_sentences`: cuts only at a sentence end (`.`/`!`/`?`, optionally followed by closing
   quotes, then whitespace or the end), at most 2000 characters. A dot in `3.5` doesn't count;
   "e.g." can end a sentence early (a shorter summary, never a broken one). One sentence longer than
   the limit falls back to a word boundary plus `...`. A test caught list items getting blank lines
   between them (opening `<li>` and closing `</li>` both broke the line), fixed.
2. **Migration 007** (`articles.summary`, nullable) and `Storage::article_by_id`. `insert_article`
   fills a missing summary on re-fetch like `feed_url`; read/saved/note state is untouched. A test
   runs the migration on a version-6 database.
3. **RSS mapping.** Prefers the entry's short description, falls back to its full content. Text typed
   `plain` is only tidied. Tested with RSS (HTML, CDATA, `content:encoded`) and Atom (`type="text"`).
4. **Checked:** export output can't change (it lists fields explicitly). Smoke test with a throwaway
   `HOME` and a local feed: database at version 7, HTML description stored as clean plain text,
   an item with no description stored as NULL.
- Tests: 133 pass (110 before, 23 new); clippy and fmt clean; core still has no ratatui/crossterm.

## OPML in core (branch `feature/opml`, 2026-10-10)

1. **`core/src/opml.rs`.** `parse_opml(text) -> OpmlImport { feeds, ignored, repeated }` and
   `write_opml(&[SourceConfig]) -> String`. Hand-written; no new dependency. Limits: 2 MiB, 5000
   feeds, 32 folder levels, `http`/`https` only. A file that is not OPML, or is damaged, is an
   error with a message; unusable entries are counted, not fatal.
2. **Decisions (Shane):** loose feeds go under "Uncategorized"; innermost folder is the topic;
   Reddit exports as `https://www.reddit.com/r/NAME/.rss` (comes back as plain RSS); the terminal
   app keeps `sources.toml` and gets `import-opml FILE` / `export-opml FILE` commands later
   (active list, optional `--list NAME`). The saved plan is `tuxwire-opml-plan.md`.
3. **Checked:** a test feeds 5000 random garbled strings to the reader (no panics); write-then-read
   round-trips names with `&`, `<`, quotes. `text::decode_entities` became `pub(crate)` so the
   reader reuses it.
- Tests: 149 pass (133 before, 16 new); clippy and fmt clean; core still has no ratatui/crossterm.

## Parallel refresh in core (branch `feature/parallel-refresh`, 2026-10-10)

1. **`fetch_all`** now runs up to 6 downloads at once (`MAX_CONCURRENT_FETCHES`) with `futures-util`'s
   `stream::iter(..).map(fetch_one).buffered(6).collect()`. Same signature, results still in source
   order, so the Android bridge and the terminal app both get it with no changes. `futures-util` was
   already in the build through `reqwest`; it is now a direct dependency (`default-features = false`,
   `alloc`), approved by Shane.
2. **Timeout.** The shared HTTP client had none. `build_client(timeout)` now sets `FETCH_TIMEOUT`
   (30 s) on the whole request; a silent server fails with a timeout. `fetch_feed_with(client, url)`
   exists so a test can use a 300 ms limit.
3. **Tests** use a tiny local server (threads, `127.0.0.1`): 6 feeds with a 300 ms delay finish in
   about one delay and in order; 14 feeds never exceed the limit; a dead feed in the middle fails alone;
   a silent server is cut off. Checked the tests can fail: with the limit forced to 1 the overlap test
   took 1.8 s and failed. Run them with `NO_PROXY=127.0.0.1` if your environment sets a proxy.
- Tests: 153 pass (149 before, 4 new); clippy and fmt clean; core still has no ratatui/crossterm.

## User-Agent and honest fetch errors (branch `feature/fetch-user-agent`, 2026-10-10)

1. **Why.** A feed (`https://system76.com/blog/rss.xml`) failed in both apps with "no root element":
   the body that arrived was empty. The shared client sent no `User-Agent`. Now `build_client` sets
   `tuxwire/<core version>` (`USER_AGENT` in `fetchers/rss.rs`).
2. **Errors.** `fetch_feed_with` checks the status first (`answered HTTP 403`) and an empty body
   (`sent an empty reply`) before parsing. Redirects are still followed by reqwest.
3. **Tests** (local server, `serve_once`): header sent, 403 named, empty 200 named.
4. **Not confirmed from here:** the sandbox cannot reach system76.com, so whether the header alone fixes
   that feed must be tested on a real device. If it still fails, the new message will now say why.
- Tests: 156 pass (153 before, 3 new); clippy and fmt clean; core still has no ratatui/crossterm.

## Feed discovery (branch `feature/feed-discovery`, 2026-10-10)

1. **What.** `core/src/fetchers/discover.rs`, `discover_feed(input) -> Result<Discovered { url, feed, found_by }>`.
   Order: the address is a feed -> the page's `<link rel=alternate>` lines (first real feed, skipping comment
   feeds, up to 5 tried) -> up to 12 guessed addresses (`/feed /rss /rss.xml /atom.xml /feed.xml /index.xml`,
   page path first, then site root, 10 s each). Needs no new dependency: the HTML is scanned by hand with the
   tag helpers from `opml.rs` (now `pub(crate)`), and `reqwest::Url` resolves relative links.
2. **Shared download.** `fetchers/rss.rs::fetch_bytes` (status check, size cap, empty reply) now backs both
   refresh and discovery. Feeds are parsed from raw bytes, so ISO-8859-1 feeds work (test
   `a_latin_1_feed_is_read_correctly`). `FEED_MAX_BYTES` 20 MiB; a page is searched for links in its first 2 MiB.
3. **Comment feeds.** Link label / path+query contains the *word* "comments", or the feed's own title starts
   with "comments" / ends with "comments feed". Rule of thumb, tested with look-alikes ("Commentary").
   A feed typed directly is never rejected.
4. **Terminal.** `tui/src/ui/mod.rs`: `a` calls `discover_feed`; the confirm screen has a new `note` field
   ("found through the page's own feed link") under the URL, and saves the discovered address. Prompt reads
   "Feed or page address". Note: the call blocks the UI while it runs (same as the old fetch); the worst case
   (page advertises nothing, slow site) is bounded by 12 x 10 s.
5. **Tested.** 20 unit tests (local `127.0.0.1` server; run with `NO_PROXY=127.0.0.1`), and by hand in tmux
   with a throwaway `HOME` against a local test blog that listed a comments feed first: it chose the posts feed,
   showed the note, and wrote the posts address to `sources.toml`. Mutation check: disabling the comment filter
   fails 4 tests.
6. **Typing `example.com/blog`** (no scheme) assumes `https://`. A site that only speaks plain `http` must be
   typed with `http://`.
- Tests: 178 pass (156 before, 22 new); clippy and fmt clean; core still has no ratatui/crossterm.

## Fetch on add (branch `feature/refresh-on-add`, 2026-10-11)

1. **Problem.** Saving a source only wrote it to `sources.toml` (Android: the database); its articles
   arrived on the next refresh, so a new feed looked empty. The check step had already downloaded it, but
   that result was thrown away.
2. **Terminal.** `handle_add_source_key` has a new out-parameter `saved_url: &mut Option<String>`, set only
   when a source is saved (not on `Esc`). The caller then runs `fetch_added_source(storage, list_path, url)`:
   it looks the source up in the active list's file again (so it is fetched exactly like a refresh), runs
   `fetch_one`, and `record_outcome`s it. The footer uses `fetch_notice`, new articles get the `Icon::New`
   marker, and a failed download is counted in the report (feed stays saved), not raised.
3. **Android.** `AddFeedScreen`: after `addFeed`, `refreshFeed(address)`; success leaves the form, a failed
   download keeps the form open with a message ("saved, but its articles could not be downloaded yet ...")
   and a `saved` flag stops a second save. Kotlin is not compiled in the sandbox: test on a device.
4. **OPML import fetching:** done on Android (`feature/opml-import-refresh-android`, one refresh-all at the end). The terminal import does not download; the next start does.
5. **Tests:** two new terminal tests (articles stored at once; dead feed reported not raised), mutation-checked
   (breaking the source lookup fails the first); also run by hand in tmux against a local test blog.
- Tests: 180 pass (178 before, 2 new); clippy and fmt clean; core still has no ratatui/crossterm.

## Terminal OPML commands (branch `feature/tui-opml-commands`, 2026-10-11)

1. **`tui/src/opml_cli.rs`.** `parse_args` (no arguments = open the reader; a typo is an error, never
   a silent fallback), `pick_list`, `import_into`, `export_from`, `save_export`, `run`. `main.rs` calls
   it right after the lists are loaded, before anything else starts.
2. **`append_sources_at`** in `fetchers/config.rs` adds many `[[source]]` blocks in one read/write and
   replaces the file via a `.tmp` rename. `add_source_at` now calls it with one source.
3. **Shared with core:** `opml::feed_address` is now public so duplicates are compared the same way the
   export writes addresses. No new dependency; no argument-parsing crate (the arguments are hand-read).
4. **Try it without touching your config:** `env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME=$(mktemp -d)
   ./target/debug/tuxwire import-opml file.opml` (build first; do not put the fake HOME before `cargo`).
5. **Tests:** 11 in `opml_cli.rs` (argument reading, list choice, comment keeping, duplicates, bad file
   changes nothing, missing file created, round trip, no overwrite, size limit, Reddit address).

## Shared notes export (branch `feature/notes-export-core`, 2026-10-11)

1. **`core/src/export.rs`.** `render_saved_markdown(&[Article], &ExportOptions) -> String` and
   `format_article_markdown(&Article)`. Pure text; no paths. Two sections (no-note list first, then
   notes), newest first, counts in the headings, `---` between notes. Empty string when nothing to write.
2. **`ExportOptions { include_unnoted }`**, default true. TUI reads it from `export.toml`
   (`include_unnoted`, optional, so old files still load).
3. **TUI `export.rs`** now only decides the file location and calls core. Its formatting tests moved to
   core (10 tests there); 5 file-writing tests stay.
4. **Changes the look of the `E` file** (headings are now `###` under `##` sections). Documented in the
   changelog.
5. **Done since:** the Android bridge and screens (see the Android repo's notes). Un-save came after, below.

## Un-save and confirmations (branch `feature/unsave-tui`, 2026-10-11)

1. **Core (`storage/mod.rs`).** `unsave_article(id) -> bool` clears saved, saved_at, note, noted_at (true
   when there was a save to remove). `update_note` with text now also saves the article and marks it read,
   keeping an existing `saved_at`; `save_article` keeps `saved_at` when already saved. No schema change, so
   no migration and nothing to back up.
2. **TUI (`ui/mod.rs`).** `Mode::Confirm(Pending)`; `Pending` is `Unsave`, `DeleteNote` or `ReplaceExport`.
   The confirm branch runs before every other mode: only a plain `y` acts, anything else cancels and says
   what stayed. `u` opens the un-save question; the note popup's Enter on an emptied box (when a note
   exists) opens the delete-note question; `E` checks `export::export_file_path` and asks before replacing.
   `run_export` is the shared "write it now" helper.
3. **Not confirmed on purpose:** `x` (skip) and `Esc` in the note box. Easy to add as more `Pending`
   variants if they cause trouble.
4. **Try it without touching your config** (build first; HOME goes in front of the binary, not `cargo`):
   `env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME=(mktemp -d) ./target/debug/tuxwire`. Save with `s`, add a
   note with `n`, press `u` (note warning), press `n` to decline, `u` then `y` to confirm.
5. **Tests:** core +5 (repeat save keeps date, un-save clears everything, nothing-to-unsave is false,
   re-save after un-save is re-stamped, note saves and clearing keeps the save); TUI +1
   (`export_file_path`). Popups were checked by hand in a named tmux session.
