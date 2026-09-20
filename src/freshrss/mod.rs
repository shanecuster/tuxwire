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
//! batch across the whole reading list, not one request per feed. A future
//! `sync` submodule here will own that whole batch directly (log in once,
//! fetch once, fan out to every feed's items) and produce `Article` values
//! itself via a `mapping` submodule, rather than pretending to be one more
//! `Fetcher` impl. Per ARCHITECTURE.md's "Refresh triggers," it's driven at
//! startup and from the `r` keybind while a `FreshRSS/*` topic is selected
//! -- not through `fetchers::configured_sources()`.
//!
//! ## What exists so far
//!
//! All four submodules ARCHITECTURE.md describes: `config` (parsing
//! `freshrss.toml`), `client` (HTTP/protocol -- Phase 3), `mapping` (raw
//! API shapes -> `Article`, plus the pure "which ids to fetch" decision
//! -- Phase 4), and `sync` (the async orchestration that ties `client`,
//! `mapping`, and `Storage` together for one sync -- Phase 4). `sync` is
//! wired into `main.rs`/`ui/mod.rs` (startup sync, and `r` on a
//! `FreshRSS/*` topic) as of Phase 4; see this module's own doc comment
//! and `sync`'s for exactly how.
pub mod client;
pub mod config;
pub mod mapping;
pub mod sync;
