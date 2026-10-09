# tuxwire — Notes

A plain-language recap of each piece of work: what exists, how to run it, and what's worth
remembering.

---

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

**Tests:** 66 (master had 29). They cover glyph widths in every mode, icon names, category
lookup and overrides, `icons.toml` parsing and saving, offline detection against a refused
connection on `127.0.0.1:1`, article state icons, footer messages, theme lookup, override
merging, `theme.toml` saving, and per-theme contrast and distinctness for all 8 themes.

### Making Catppuccin Macchiato the default

The default lives in two places, and the tests fail if they disagree:

1. **`theme.toml.example`: change `base = "Everforest"` to `base = "Catppuccin Macchiato"`.**
   This is what a fresh install actually loads, since there's no `theme.toml` yet and the
   bundled example is used. Its comment says which theme is "(default)", and its
   commented-out override values are Everforest's (cosmetic only), so update those too.
2. **`src/theme.rs`: move the Macchiato block to the top of `BUILTIN_THEMES`.** Index 0 is
   what a `theme.toml` *without* a `base` line gets. Then set the test constant
   `DEFAULT_THEME = "Catppuccin Macchiato"`, and rename the test
   `bundled_default_is_everforest_without_overrides`.

Then update the docs that say "Everforest (default)": README, ARCHITECTURE, and the CHANGELOG
"Changed" entry, which goes away (Macchiato was the old default too). The picker would then
list Macchiato first. Users with an old full `theme.toml` see no difference either way.

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
