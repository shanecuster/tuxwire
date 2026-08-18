//! ratatui views/widgets (`docs/ARCHITECTURE.md` § 3. TUI).
//!
//! Not implemented yet. When built, this module will own the terminal
//! setup/teardown (via `crossterm`, raw mode + alternate screen), the main
//! draw loop, and the topic sidebar / article list / footer layout
//! described in ARCHITECTURE.md, likely split into submodules
//! (`ui/topics.rs`, `ui/articles.rs`, `ui/saved.rs`, ...) the same way
//! `fetchers/` is split by source type.
