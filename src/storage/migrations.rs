//! Discovers and applies the numbered SQL files under `migrations/`
//! (`docs/ARCHITECTURE.md` § Migrations).
//!
//! This module has exactly one job: given an open SQLite connection, bring
//! its schema up to date by running every migration it hasn't already run,
//! in order, and never touching one it's already run before.

use anyhow::Context;
use rusqlite::Connection;

/// One migration: a version number, a short name (used only in error
/// messages, to make a failure easy to trace back to a specific file), and
/// the raw SQL to execute.
///
/// This struct only ever exists as `'static` data baked into the binary
/// (see `MIGRATIONS` below) -- nothing ever constructs one at runtime --
/// which is why every field here can be a borrowed `&'static str` instead
/// of an owned `String`. A `&'static str` is a reference guaranteed valid
/// for the *entire* life of the program (typically because, like here, it
/// points at data embedded directly in the compiled binary rather than
/// anything allocated on the heap at runtime), so there's no ownership
/// question to resolve the way there would be for borrowed data with a
/// shorter lifetime.
struct Migration {
    version: u32,
    name: &'static str,
    sql: &'static str,
}

/// The full migration history, oldest first.
///
/// `include_str!` is a macro that runs at *compile* time: it reads the
/// named file (path is relative to this source file, like a `mod`
/// declaration) and splices its entire contents in as a string literal,
/// as if you'd typed the SQL directly into this Rust file yourself. The
/// practical effect is that `migrations/001_init.sql` becomes part of the
/// compiled binary -- tuxwire never reads that file from disk at runtime,
/// so it doesn't need `migrations/` to exist wherever the binary is
/// installed or run from. That matches ARCHITECTURE.md's "single static
/// binary" rationale for choosing Rust in the first place.
///
/// Adding a new migration later means: write `migrations/00N_whatever.sql`,
/// then add one more `Migration { .. }` entry here with the next version
/// number. Nothing about *this* function (`run`, below) needs to change.
const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "001_init",
    sql: include_str!("../../migrations/001_init.sql"),
}];

/// Brings `conn`'s schema up to date by running every migration whose
/// version is greater than the database's current `PRAGMA user_version`,
/// in ascending order, then advancing `user_version` to match.
///
/// ## Why `PRAGMA user_version` instead of a `migrations` log table
///
/// SQLite reserves four bytes in every database file's header specifically
/// for application use, exposed as `PRAGMA user_version` -- a plain
/// integer that starts at `0` in a freshly created database and otherwise
/// means whatever the application wants it to mean. ARCHITECTURE.md names
/// this as one of two options for tracking "how many migrations have
/// already run"; it's the one used here because it needs no schema of its
/// own to exist before it can be queried (a log table would have to be
/// created by some migration *before* migration tracking could start,
/// which is a bit of a chicken-and-egg problem for migration #1
/// specifically). A log table would earn its cost back if migrations ever
/// needed to run out of strict order or carry their own metadata (e.g. the
/// exact timestamp each one ran) -- not a need here, since this list is
/// always applied top-to-bottom, in full, every startup.
///
/// Called once, right after opening the connection, before any other
/// query touches the database -- see `Storage::open` in `storage/mod.rs`.
pub fn run(conn: &Connection) -> anyhow::Result<()> {
    // `query_row` runs a query expected to return exactly one row and
    // hands it to the closure to decode. `PRAGMA` statements always
    // return their value this way (as if `SELECT`ed), so this reads back
    // whatever integer was last written by a previous run of this
    // function (or `0`, for a database that has never had this function
    // run against it before).
    let current_version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

    for migration in MIGRATIONS {
        if migration.version <= current_version {
            // Already applied on a previous run -- skip it. This is what
            // makes `run` safe to call unconditionally on every startup,
            // rather than only on a database's very first creation.
            continue;
        }

        // `execute_batch` (as opposed to `execute`) runs a string that may
        // contain more than one `;`-separated statement -- exactly what a
        // migration file full of multiple `CREATE TABLE`s needs, and not
        // something the single-statement `execute` supports.
        conn.execute_batch(migration.sql)
            .with_context(|| format!("failed to run migration {}", migration.name))?;

        // `PRAGMA user_version = N` cannot take a bound parameter (`?1`)
        // the way ordinary SQL statements can -- PRAGMAs are a
        // SQLite-specific extension with their own, simpler grammar that
        // only accepts a literal value, not a placeholder. Formatting
        // `migration.version` directly into the SQL string is safe here
        // (unlike formatting *user input* into SQL, which is exactly the
        // SQL-injection mistake `params![...]` elsewhere in this module
        // exists to prevent) because it's a `u32` we wrote ourselves in
        // the `MIGRATIONS` array above, never data from outside the
        // program.
        conn.execute_batch(&format!("PRAGMA user_version = {}", migration.version))
            .with_context(|| format!("failed to record migration {} as applied", migration.name))?;
    }

    Ok(())
}
