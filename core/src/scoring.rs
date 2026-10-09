//! Skip-weighting: the "learning" system (`docs/ARCHITECTURE.md` §
//! The "Learning" System).
//!
//! Not implemented yet. When built, this module will own the
//! `skip_weights` table logic: incrementing a keyword's weight when an
//! article is skipped (`x` keybind), and scoring freshly-fetched articles
//! against those weights so frequently-skipped topics sort to the bottom
//! (or get filtered) on refresh. Deliberately simple and inspectable by
//! design — no black-box ML, just a keyword/weight lookup anyone can read
//! straight out of the database.
