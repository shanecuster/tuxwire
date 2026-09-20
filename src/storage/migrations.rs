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
/// `migrations()` below builds a fresh `Vec<Migration>` each time it's
/// called, but every *value* inside each one is still `&'static str`, not
/// `String`: `name` is a string literal, and `sql` comes from
/// `include_str!`, which splices a file's contents in as a string literal
/// at compile time (see `migrations()`'s own doc comment). A `&'static
/// str` is a reference guaranteed valid for the *entire* life of the
/// program -- typically because, like here, it points at data embedded
/// directly in the compiled binary rather than anything allocated on the
/// heap at runtime -- so there's no ownership question to resolve the way
/// there would be for borrowed data with a shorter lifetime, even though
/// the `Migration` struct instances themselves (and the `Vec` holding
/// them) are ordinary runtime values, rebuilt on every call.
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
/// number. Nothing about *this* function's caller (`run`, below) needs to
/// change.
///
/// ## Why a function instead of a `const` array
///
/// Every migration up through 003 is unconditional, but 004
/// (`freshrss_items`/`freshrss_meta`, see `migrations/004_freshrss_tables.sql`)
/// only exists when this crate is built with `--features freshrss` --
/// per docs/ARCHITECTURE.md's FreshRSS Sync section, a `master`/no-feature
/// build's schema must be identical to `master`'s, so that migration must
/// never run (or even be compiled in) without the feature. Attaching
/// `#[cfg(...)]` to one *element* of a `const` array literal isn't
/// something stable Rust supports; `#[cfg(...)]` on a *statement* (the
/// conditional `.push(...)` below) is fully stable and ordinary, so this
/// builds the list at runtime instead. The cost is negligible -- this
/// runs once per startup, building a `Vec` with three or four elements --
/// and `run` below doesn't care whether its migrations came from a
/// `const` or a freshly-built `Vec`; it just iterates them in order.
fn migrations() -> Vec<Migration> {
    // `mut` is only ever exercised by the `#[cfg(feature = "freshrss")]`
    // `.push(...)` below -- a no-feature build never mutates this `Vec`
    // after building it, which `rustc` would otherwise (correctly, in
    // that build) flag as an unused `mut`. `#[allow(unused_mut)]` is
    // scoped to just this one binding rather than the whole function,
    // and does nothing at all in a `--features freshrss` build, where the
    // `mut` is genuinely needed.
    #[allow(unused_mut)]
    let mut migrations = vec![
        Migration {
            version: 1,
            name: "001_init",
            sql: include_str!("../../migrations/001_init.sql"),
        },
        Migration {
            version: 2,
            name: "002_unique_article_url",
            sql: include_str!("../../migrations/002_unique_article_url.sql"),
        },
        Migration {
            version: 3,
            name: "003_add_saved_and_noted_at",
            sql: include_str!("../../migrations/003_add_saved_and_noted_at.sql"),
        },
    ];

    // `include_str!` here only ever runs at all when this whole line is
    // compiled in -- i.e. only with `--features freshrss` -- so a
    // no-feature build doesn't just skip *pushing* this migration, it
    // never embeds `004_freshrss_tables.sql`'s bytes into the binary in
    // the first place.
    #[cfg(feature = "freshrss")]
    migrations.push(Migration {
        version: 4,
        name: "004_freshrss_tables",
        sql: include_str!("../../migrations/004_freshrss_tables.sql"),
    });

    migrations
}

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

    for migration in migrations() {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A database whose `user_version` is already ahead of every migration
    /// *this binary* knows about -- exactly the situation `master` (or any
    /// build missing a feature-gated migration, e.g. a `freshrss`-only one)
    /// finds itself in when it opens a database that a different,
    /// newer-aware build already migrated further. `run` doesn't special-
    /// case this at all: it only ever compares each of *its own*
    /// `migrations()` entries against `current_version` and `continue`s past
    /// anything at or below it, so a `current_version` higher than every
    /// entry in the list just means every entry gets skipped -- nothing
    /// tries to re-run a migration this binary doesn't have, nothing
    /// errors, and (critically) nothing lowers `user_version` back down to
    /// what this binary *does* know about. The database opens exactly as-is.
    #[test]
    fn a_database_ahead_of_known_migrations_is_left_untouched() {
        let conn = Connection::open_in_memory().unwrap();
        // Bring a fresh database up to this binary's own latest known
        // version first, the same as any real startup.
        run(&conn).unwrap();

        // Simulate a build with one more migration than this one knows
        // about (e.g. a feature-gated migration this binary was compiled
        // without) having already run against this exact database file.
        let latest_known = migrations().last().unwrap().version;
        conn.execute_batch(&format!("PRAGMA user_version = {}", latest_known + 1)).unwrap();

        // Running this binary's own (comparatively older) migration list
        // against that now-ahead database must be a complete no-op --
        // every migration's version is <= the db's user_version, so every
        // one is skipped. If this instead tried to re-run migration 1's
        // `CREATE TABLE articles`, it would fail with "table already
        // exists" rather than silently succeeding the way it does here.
        run(&conn).unwrap();

        let version: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0)).unwrap();
        assert_eq!(
            version,
            latest_known + 1,
            "run must never lower a user_version that's already ahead of what it knows about"
        );
    }
}
