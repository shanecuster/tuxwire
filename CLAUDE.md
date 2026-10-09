# CLAUDE.md — tuxwire

Rust + ratatui terminal news reader: a standalone RSS reader. **Read
`docs/ARCHITECTURE.md` first** — it is the source of truth for structure and
decisions. The FreshRSS integration was dropped; `feature/freshrss` is
archived as the tag `archive/freshrss` and will not be merged.

## Branch rules

- Feature branches off master; master changes only by merging a branch
  whose tests pass.
- Before making any change, run `git branch --show-current`. If it's
  `master`, stop and create (or ask for) a feature branch first.
- Never merge, force-push, or delete branches or tags. Merging is done by
  the human after testing.
- Commit in small, single-purpose steps with clear messages, so any one
  step is easy to test or revert.

## Hard constraints

- **No new dependencies and no changes to existing dependency features**
  without asking first.
- **`core/` (the `tuxwire-core` crate) is shared with the Android app.** It
  must never depend on `ratatui` or `crossterm` (not even in tests), never
  import anything from `tui/`, and never read `$HOME`/`$XDG_*` or hardcode a
  path: the program using it decides where files live and passes the path in
  (e.g. `Storage::open_at`). Terminal-only things (the UI, key handling,
  `~/.config/tuxwire/*.toml` loading, launching w3m/browsers) stay in `tui/`.
  Check with `cargo tree -p tuxwire-core | grep -E 'ratatui|crossterm'`
  (it should print nothing).
- UI code never hard-codes a color or a glyph: colors come from `Theme`
  slots (`tui/src/theme.rs`, filled from the palettes in
  `core/src/theme.rs`), icons from `IconSettings` (`core/src/icons/`).
- Saved articles and notes are permanent and stay on the device by default.
  Syncing them (read state, saved articles, notes) to a user's own CouchDB
  is an optional feature that is off by default and tucked away in settings
  -- never sync anything unless the user turned it on.

## How to work

- Ask before: adding a dependency, deviating from the architecture doc, or
  anything that could touch the person's real database or config.
- If the doc and the code disagree, say so and ask — don't silently pick
  one. When a real decision is made, update `docs/ARCHITECTURE.md` in the
  same commit.
- When something fails, find and explain the root cause rather than patching
  around the symptom.
- At the end of a piece of work, give a short report: what changed, how to
  test it, anything surprising.

## Documentation standard (hard requirement)

The person building this is learning Rust *through* this codebase and wants
**every section of code documented with what it does.** This applies to all
new code:

- **Every module, struct, enum, field, function, and method** gets a `///`
  (or `//!` for modules) doc comment saying what it is for and what it
  returns or changes.
- **Inside function bodies, every logical section gets a `//` comment**
  explaining what that section does, in plain language, step by step — a
  short comment above each group of lines, not just at the top of the
  function. If a line uses a Rust concept that's new to a beginner, say
  what it does right there.
- **Also explain *why*,** not just *what*: especially ownership and borrowing
  (`&str` vs `String`, `.clone()`, `&mut`), lifetimes, traits, `Result` and
  `?` versus `unwrap`, `Option`, `match`, iterator chains (what they expand
  to conceptually), and async/`await`.
- Write for someone who has never seen Rust. Don't assume any concept is
  obvious. Terse idioms get a comment saying what they expand to.
- Comments must be accurate. If code changes, update its comments in the same
  commit. A misleading comment is worse than none.
- Before writing, reread one or two of the best-commented existing files
  (for example under `core/src/storage/`) and match their tone and depth.
- New modules start with `#![warn(clippy::missing_docs_in_private_items)]`
  (as `core/src/icons/mod.rs`, `core/src/theme.rs`, and
  `core/src/fetch_report.rs` do) so
  clippy flags any undocumented item. (Clippy can't check the inline `//`
  comments, so those are checked by the person reviewing.)
- At the end of each piece of work, show one or two functions that best
  demonstrate the comment style, so the person can confirm it teaches.

`docs/ARCHITECTURE.md` "Documentation Standards" is the general policy; where
it says "non-trivial block", this file's stricter rule wins.

## Building and testing

The repo is a Cargo workspace (`core/` + `tui/`). Run at the repo root, these
commands cover both crates. All three must pass before calling anything done
(and before a branch is merged into master):

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

**Never develop against the real database or config.** Run the app with a
throwaway home directory so `~/.local/share/tuxwire/` and
`~/.config/tuxwire/` point somewhere disposable -- **build normally, then
run the already-built binary with `HOME` overridden**, not
`HOME=$(mktemp -d) cargo run`. `cargo`/`rustup` themselves resolve the
toolchain, the registry cache, and `~/.cargo` through `HOME` -- putting a
fake `HOME` in front of `cargo run` breaks *that* lookup, not just
tuxwire's own config/data paths. Also unset `XDG_CONFIG_HOME` and
`XDG_DATA_HOME`, which take priority over `HOME` if they're set:

```bash
cargo build
t=$(mktemp -d)
env -u XDG_CONFIG_HOME -u XDG_DATA_HOME HOME="$t" ./target/debug/tuxwire
```

To drive the TUI from a script, use a **named** tmux session and kill only
that session by name (`tmux kill-session -t <name>`). Never kill tmux
sessions you didn't create, and never run `tmux kill-server`. See NOTES.md
for the exact commands.

Unit tests should need no network (a refused connection to `127.0.0.1` is
fine for testing offline handling).

## Environment notes

- Primary machine: Linux (Hyprland). The app is also run on Android through
  Termux, where Rust builds must live in Termux's private filesystem (not
  shared/SAF storage). Don't introduce absolute or desktop-only paths.
- A native Android app (Kotlin + Jetpack Compose, separate private repo
  `tuxwire-android`) will use `tuxwire-core` through UniFFI. Anything added
  to `core/` should make sense on a phone, not just in a terminal.
- Config lives in `~/.config/tuxwire/` (`sources.toml`, `theme.toml`,
  `icons.toml`, `export.toml`); data in `~/.local/share/tuxwire/`.
  Config-driven, no interactive path prompts.
- Keep `CHANGELOG.md` in the existing changelog style, and update
  `README.md`, `docs/ARCHITECTURE.md`, and `NOTES.md` when behavior or
  config changes.
