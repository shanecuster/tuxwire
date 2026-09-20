# CLAUDE.md — tuxwire

Rust + ratatui terminal news reader. **Read `docs/ARCHITECTURE.md` first** —
it is the source of truth for structure and decisions. Current task: the
optional FreshRSS feature, specified in its "FreshRSS Sync (optional)"
section.

## Branch rules

- `master` is stable and must not change. **All FreshRSS work happens on
  `feature/freshrss`.**
- Before making any change, run `git branch --show-current`. If it isn't
  `feature/freshrss`, stop and say so.
- Never commit to `master`, merge, force-push, or delete branches. Merging
  is done by the human after testing.
- Commit in small, single-purpose steps with clear messages, so any one
  step is easy to test or revert.

## Hard constraints

- **Off means identical to `master`.** With the `freshrss` Cargo feature off,
  or no/disabled `freshrss.toml`, behavior must not change in any way.
- **Additive only.** No changes to existing tables or existing migrations,
  and no rewrites of existing feed-adding, storage, or sidebar logic. New
  migrations create new tables only. Touch existing files only at the small
  hook points the architecture doc describes.
- **`src/freshrss/` must not import `ratatui`, `crossterm`, or anything from
  `src/ui/`,** and must not hardcode paths. It has to stay extractable for
  a future Android app.
- **No new dependencies and no changes to existing dependency features**
  without asking first. The plan needs none.
- **Secrets:** never log, print, or commit the FreshRSS password or tokens;
  never include them in error messages; redact them in `Debug` output.
- Don't mark saved articles or notes as anything but local-only. They are
  permanent and never sent to FreshRSS.

## How to work

- Follow the build phases in the architecture doc **in order, and stop after
  each phase** with a short report: what changed, how to test it, anything
  surprising. Start with Phase 0 (read-only) and wait for the go-ahead.
- Ask before: adding a dependency, changing an existing file beyond a hook
  point, deviating from the architecture doc, or anything that could touch
  the person's real database or config.
- If the doc and the code disagree, say so and ask — don't silently pick
  one. When a real decision is made, update `docs/ARCHITECTURE.md` in the
  same commit.
- When something fails, find and explain the root cause rather than patching
  around the symptom.

## Documentation standard (hard requirement)

The person building this is learning Rust *through* this codebase and wants
**every section of code documented with what it does.** This applies to all
new code, and it is stricter than "non-trivial blocks only":

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
  (for example under `src/storage/`) and match their tone and depth.
- `src/freshrss/mod.rs` should start with
  `#![warn(clippy::missing_docs_in_private_items)]` so clippy flags any
  undocumented item. (Clippy can't check the inline `//` comments, so those
  are checked by the person reviewing each phase.)
- At the end of each phase, show one or two functions that best demonstrate
  the comment style, so the person can confirm it teaches.

`docs/ARCHITECTURE.md` "Documentation Standards" is the general policy; where
it says "non-trivial block", this file's stricter rule wins for FreshRSS code.

## Building and testing

Build and test **both** ways before calling anything done:

```bash
cargo build && cargo test                         # feature off: must match master
cargo build --features freshrss && cargo test --features freshrss
cargo clippy --all-targets --features freshrss
```

**Never develop against the real database or config.** Run the app with a
throwaway home directory so `~/.local/share/tuxwire/` and
`~/.config/tuxwire/` point somewhere disposable -- **build normally, then
run the already-built binary with `HOME` overridden**, not
`HOME=$(mktemp -d) cargo run`. `cargo`/`rustup` themselves resolve the
toolchain, the registry cache, and `~/.cargo` through `HOME` -- putting a
fake `HOME` in front of `cargo run` breaks *that* lookup, not just
tuxwire's own config/data paths. `tuxwire`'s compiled binary has no such
dependency on `HOME` (only tuxwire's own config/data-path code reads it),
so overriding it only for the already-built binary is what actually
isolates just tuxwire's paths:

```bash
cargo build --features freshrss
t=$(mktemp -d)
env HOME="$t" ./target/debug/tuxwire
```

Test against a throwaway FreshRSS server in Docker (`freshrss/freshrss`),
never the real instance. Unit tests should use saved JSON fixture files so
they need no network.

## Environment notes

- Primary machine: Linux (Hyprland). The app is also run on Android through
  Termux, where Rust builds must live in Termux's private filesystem (not
  shared/SAF storage). Don't introduce absolute or desktop-only paths.
- Config lives in `~/.config/tuxwire/`; data in `~/.local/share/tuxwire/`.
  Config-driven, no interactive path prompts.
- Keep `CHANGELOG.md` in the existing git-commit/changelog style, and add the
  README configuration notes for `freshrss.toml` in Phase 7.
