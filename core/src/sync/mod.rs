//! Optional sync of saved articles and notes to the person's own CouchDB.
//!
//! **Off by default.** Nothing here runs, and nothing is sent anywhere, unless
//! the person has turned sync on and asked for it. See `docs/ARCHITECTURE.md`
//! ("Optional CouchDB sync") for the full design and the privacy rules.

#![warn(clippy::missing_docs_in_private_items)]

pub mod doc;
