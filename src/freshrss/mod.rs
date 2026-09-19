//! Optional FreshRSS sync (`docs/ARCHITECTURE.md` § FreshRSS Sync).
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
//! Just `config`: parsing and validating `freshrss.toml`. The `client`
//! (HTTP/protocol), `mapping` (raw API shapes -> `Article`), and `sync`
//! (reconciling with `Storage`) submodules described in ARCHITECTURE.md
//! land in later build phases -- see that document's "Build phases"
//! section. Nothing in this module is called from `main.rs` yet; there is
//! no behavior to observe even with the feature enabled until a later
//! phase wires it in.
pub mod config;
