//! SQLite access and migrations (`docs/ARCHITECTURE.md` § Storage).
//!
//! Not implemented yet — this file exists so the module layout matches
//! the architecture doc from the start. When this is built out it will
//! own opening `~/.local/share/tuxwire/tuxwire.db` via `rusqlite`,
//! running any pending files from `migrations/` on startup, and providing
//! functions like `insert_article`, `mark_saved`, etc. for the rest of the
//! app to call — no other module should touch SQLite directly.
//!
//! ## Why `storage/mod.rs` instead of `storage.rs`
//!
//! Rust has two equivalent ways to write a module that will contain
//! submodules of its own: a `storage.rs` file next to a `storage/`
//! directory, or (the older, still very common style used here)
//! `storage/mod.rs` — the directory itself *is* the module, and `mod.rs`
//! is its "root" file. Either way, code elsewhere refers to this module
//! as `storage`, and anything declared `pub` in this file is reachable as
//! `storage::whatever`. Once schema/migration logic is split into its own
//! file (e.g. `storage/migrations.rs`), it'll be brought in with `pub mod
//! migrations;` right here — the same pattern used in `fetchers/mod.rs`.
