//! Optional FreshRSS sync (`docs/ARCHITECTURE.md` § FreshRSS Sync).
//!
//! `#![warn(clippy::missing_docs_in_private_items)]` below makes
//! `cargo clippy` flag any struct/enum/field/function in this module tree
//! that's missing a `///` doc comment -- the compile-time half of
//! docs/CLAUDE.md's Documentation standard ("every function, struct,
//! field... gets a doc comment"). `warn`, not `deny`, so a lapse shows up
//! loudly in `cargo clippy --all-targets --features freshrss` output
//! without turning into a hard build failure. Clippy can't check the
//! *inline* `//` comments the same standard requires inside function
//! bodies -- those are reviewed by hand each phase.
#![warn(clippy::missing_docs_in_private_items)]
//!
//! Compiled only when the `freshrss` Cargo feature is enabled (see the
//! `#[cfg(feature = "freshrss")] mod freshrss;` line in `main.rs`) --
//! without that feature, this whole directory is invisible to the
//! compiler, which is the compile-time half of "off means identical to
//! `master`."
//!
//! ## The hard boundary
//!
//! Nothing under this module may import `ratatui`, `crossterm`, or
//! anything from `crate::ui`, and nothing here may hardcode a filesystem
//! path -- every path this module needs (like `freshrss.toml`'s own
//! location) is resolved the same XDG-fallback way every other config
//! loader in this codebase already does (see `config::freshrss_toml_path`),
//! never assumed. Per ARCHITECTURE.md: this keeps `src/freshrss/`
//! extractable as-is into the shared Rust core a future native Android app
//! would move fetchers/storage into, without a rewrite.
//!
//! ## Why this doesn't implement the `Fetcher` trait
//!
//! `crate::fetchers::Fetcher` (see its own doc comment) is shaped around
//! "one source, one independent `.fetch()`" -- exactly what `main`'s
//! initial fetch loop and `ui::refresh_topic` both assume when they call
//! `.fetch()` once per `ConfiguredSource`. The Google Reader-compatible API
//! FreshRSS exposes doesn't fit that shape: one login and one write token
//! cover *every* subscribed feed, and unread items come back as a single
//! batch across the whole reading list, not one request per feed. `sync`
//! owns that whole batch directly (log in once, fetch once, fan out to
//! every feed's items) and produces `Article` values itself via `mapping`,
//! rather than pretending to be one more `Fetcher` impl. Per
//! ARCHITECTURE.md's "Refresh triggers," it's driven at startup and from
//! the `r` keybind while a `FreshRSS/*` topic is selected -- not through
//! `fetchers::configured_sources()`.
//!
//! ## What's here (all seven build phases done)
//!
//! - `config` -- parsing and validating `freshrss.toml` (Phase 1).
//! - `client` -- the Google Reader-compatible HTTP protocol: login,
//!   subscriptions, unread ids, item contents (Phase 3), read-state push
//!   (`token`/`mark_read`, Phase 5), and adding a feed (`quickadd`,
//!   Phase 6). Knows nothing about `Storage` or the UI.
//! - `mapping` -- pure functions only: raw `client` shapes -> `Article`,
//!   which ids a sync should fetch, and sanitizing server-supplied text
//!   before it's ever shown in the TUI (Phase 4/6). No I/O, no `Storage`.
//! - `sync` -- the async orchestration that ties `client`, `mapping`, and
//!   `Storage` together: one full sync (pull + read-state reconcile +
//!   push, Phase 4/5), the startup sync's own background thread (Phase
//!   4), and adding a feed (Phase 6).
//!
//! Wired into `main.rs` (the startup sync, when `sync_on_start` is on)
//! and `ui/mod.rs` (`r` on a `FreshRSS/*` topic, the sidebar merge, the
//! three local read-marking hook points, and the `a` flow's FreshRSS
//! destination) -- see each of those call sites' own doc comments for
//! exactly how.
pub mod client;
pub mod config;
pub mod mapping;
pub mod sync;
