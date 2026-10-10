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
- Next migration number is **007** (004 was the FreshRSS one, never merged; 006 added `feeds` and `settings`).

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
   lines go in `FetchReport::failure_messages`. `fetch_all` is sequential.
3. **Desktop loops converted** (separate commit): `main.rs` and `ui::refresh_topic` use the shared step.
   Smoke test with a throwaway `HOME` and a dead feed: the TUI started and `skipped Dead: ...` was printed
   as before.
- Tests: 110 pass (101 before, 9 new: 7 for feeds/settings, 2 for fetch outcomes); clippy and fmt clean.
