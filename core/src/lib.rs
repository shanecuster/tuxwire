//! `tuxwire-core`: everything in tuxwire that is not about drawing a terminal.
//!
//! This crate is the shared "engine" behind every tuxwire front end. The
//! terminal app (`tui/`) uses it today; the Android app (a separate
//! repository) uses it through a thin foreign-function wrapper. Keeping the
//! logic here, in one place, means a feed-parsing or database fix is made
//! once and benefits both apps.
//!
//! ## What belongs here, and what does not
//!
//! Belongs here: anything that would be identical on a desktop and a phone --
//! the `Article` model, the SQLite database and its migrations, fetching and
//! parsing feeds, the fetch report, the icon table, and the built-in colour palettes.
//!
//! Does not belong here: anything that assumes a terminal (ratatui,
//! crossterm, key handling) or a Linux desktop (reading `$HOME`, `~/.config`
//! TOML files, launching `w3m`). Those stay in `tui/`. The rule of thumb:
//! this crate never decides *where* a file lives -- the program using it
//! passes paths in.
//!
//! ## Modules
//!
//! A Rust *library crate* is a tree of modules starting at this file
//! (`lib.rs`). Each `pub mod name;` line below tells the compiler to include
//! `name.rs` (or `name/mod.rs`) and makes it reachable by other crates as
//! `tuxwire_core::name` -- `pub` is what exposes it outside this crate.

pub mod export;
pub mod feed_import;
pub mod fetch_report;
pub mod fetchers;
pub mod icons;
pub mod models;
pub mod opml;
pub mod scoring;
pub mod stats;
pub mod storage;
pub mod sync;
pub mod text;
pub mod theme;
