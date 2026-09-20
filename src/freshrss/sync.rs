//! Ties `client`, `mapping`, and `Storage` together into one actual sync
//! (`docs/ARCHITECTURE.md` § FreshRSS Sync, Phase 4: "pull").
//!
//! This is the thin async I/O layer the module doc comments elsewhere in
//! this tree keep promising: every *decision* (which ids to fetch, what
//! topic an item belongs under, what an `Article` looks like) is made by
//! a pure function in `mapping`, tested there with no network involved.
//! What's left here is orchestration -- call `client` in the right order,
//! hand its results to `mapping`, and write what comes back through
//! `Storage` -- plus the bookkeeping (counts, the truncated flag) a
//! caller needs to show something meaningful once this returns.
//!
//! Unlike `client` and `config`, this module *does* know about `Storage`
//! -- ARCHITECTURE.md's own module layout says so explicitly ("`sync`
//! (reconciles FreshRSS state with `Storage`)"), and reconciling is
//! exactly what this file does. It still doesn't know anything about the
//! UI: `main.rs`/`ui/mod.rs` are the only places that decide *when* `run`
//! below gets called and *how* its result gets shown.

use super::client::Client;
use super::config::Password;
use super::mapping;
use crate::storage::Storage;
use std::collections::HashSet;
use std::time::Duration;

/// What one call to `run` accomplished -- everything a caller needs to
/// show a meaningful "here's what just happened" message, without
/// needing to inspect `Storage` itself afterwards.
#[derive(Debug, Default)]
pub struct SyncSummary {
    /// How many items were fetched, mapped, and successfully inserted as
    /// brand-new `articles` rows this round.
    pub new_articles: usize,
    /// How many of the server's unread items were already linked in
    /// `freshrss_items` from a previous sync -- filtered out by
    /// `mapping::select_ids_to_fetch` before `item_contents` was ever
    /// called for them, so these never cost a network request.
    pub already_synced: usize,
    /// How many fetched items were skipped because their URL already
    /// belonged to a different, pre-existing `articles` row -- per
    /// ARCHITECTURE.md's URL-conflict rule, that existing row is never
    /// touched or linked; the FreshRSS item is simply not stored. See
    /// `Storage::insert_article_if_new_url`'s doc comment for the exact
    /// mechanism.
    pub url_conflicts: usize,
    /// `true` if `Client::unread_item_ids` hit its own hard page cap
    /// this round, meaning the unread id list `run` worked from was
    /// known to be incomplete. A caller must not treat `true` here as
    /// "nothing more to do" -- it means the *opposite*: there's more on
    /// the server than this sync could even see. Phase 5's read-state
    /// reconcile step (`docs/ARCHITECTURE.md`'s "Pull read state") is
    /// the one place this flag actually changes behavior: that step
    /// must be skipped entirely for a sync where this is `true`, since
    /// "this item isn't in my unread list" is meaningless when the list
    /// itself is known to be missing items that are still genuinely
    /// unread.
    pub truncated: bool,
}

/// Runs one full sync against `client`: log in, fetch the subscription
/// list and unread item ids, fetch contents for up to `max_items_per_sync`
/// not-already-synced items (newest first), map and insert each one.
///
/// Takes a `&Client` (already constructed with a URL/username/password)
/// rather than building one itself -- the caller (`main.rs` for the
/// startup sync, `ui/mod.rs` for the `r`-triggered one) is what has the
/// loaded `FreshRssConfig::Enabled` fields in hand, and each of those two
/// call sites has a different reason to construct its `Client`
/// differently (the startup sync's lives on its own background thread;
/// `r`'s lives on the same thread as the rest of the TUI) -- see
/// ARCHITECTURE.md's "How the background sync shares Storage with the
/// UI." Logging in fresh *inside* `run` either way, rather than also
/// accepting an already-logged-in `AuthToken`, matches ARCHITECTURE.md's
/// Security rule directly: "Tokens live in memory only; log in again per
/// refresh session" -- every call to `run` is its own session.
pub async fn run(storage: &Storage, client: &Client, max_items_per_sync: usize) -> anyhow::Result<SyncSummary> {
    // Step 1 of ARCHITECTURE.md's "Sync behavior": log in and get a
    // token. Every request below needs this, so a login failure here
    // (`?`) stops the whole sync immediately -- there's nothing useful
    // any later step could do without it.
    let auth = client.login().await?;

    // Step 2: the subscription list. Kept around for the rest of this
    // function (not just fetched and discarded) because mapping each
    // item's topic needs to look its feed up by id -- see the
    // `subscriptions.iter().find(...)` below.
    let subscriptions = client.subscriptions(&auth).await?;

    // Step 3 (part one): every currently-unread item id, newest first,
    // possibly truncated -- see `UnreadItemIds`'s own doc comment for
    // exactly what `truncated` means and why hitting the page cap isn't
    // treated as an error.
    let unread = client.unread_item_ids(&auth).await?;

    // Which of those ids this app has already synced before, per
    // ARCHITECTURE.md's dedupe rule ("fetch contents only for IDs not
    // already in freshrss_items"). Built as a plain loop (one query per
    // id) rather than a single batch query -- `remote_id` is a `UNIQUE`
    // indexed column, so each lookup is cheap, and `Storage` doesn't
    // (and per its own "additive only" design, shouldn't need to) expose
    // a batch version of this check just for this one caller.
    let mut already_synced_ids = HashSet::new();
    for id in &unread.ids {
        if storage.freshrss_article_id_by_remote_id(id)?.is_some() {
            already_synced_ids.insert(id.clone());
        }
    }

    // Step 3 (part two): the pure decision of which ids to actually spend
    // a network request fetching, made entirely by `mapping` -- see its
    // own doc comment for the "newest first, capped at max_items_per_sync"
    // reasoning.
    let to_fetch = mapping::select_ids_to_fetch(&unread.ids, &already_synced_ids, max_items_per_sync);

    let mut summary =
        SyncSummary { new_articles: 0, already_synced: already_synced_ids.len(), url_conflicts: 0, truncated: unread.truncated };

    // Nothing new to fetch -- a legitimate, common outcome (every unread
    // item was already synced last time), not a failure. Returning early
    // here skips an `item_contents` call with an empty id list, which
    // `Client::item_contents` would otherwise happily make (and get
    // nothing useful back from).
    if to_fetch.is_empty() {
        return Ok(summary);
    }

    let items = client.item_contents(&auth, &to_fetch).await?;

    // Step 3 (part three): map and insert each fetched item.
    for item in &items {
        // A `client::Item` only carries its *feed's* id (`origin.stream_id`)
        // -- resolving that feed's FreshRSS category (and therefore its
        // tuxwire topic) means looking the matching `Subscription` up by
        // that id. `.find(...)` over a `Vec` is a linear search, not a
        // `HashMap` lookup -- fine here since a sync's subscription list
        // is realistically a handful to a few dozen feeds, not thousands.
        let category_label = subscriptions
            .iter()
            .find(|subscription| subscription.id == item.origin.stream_id)
            .and_then(|subscription| subscription.categories.first())
            .map(|category| category.label.as_str());
        let topic = mapping::topic_for_category(category_label);
        let article = mapping::map_item_to_article(item, &topic);

        // `insert_article_if_new_url` is the one piece of this loop that
        // actually enforces ARCHITECTURE.md's URL-conflict rule -- see
        // its own doc comment in `storage/mod.rs`. `match` on its
        // `Option<i64>` result is what makes the two outcomes (a real
        // new row vs. a silently-skipped conflict) impossible to
        // confuse with each other: there's no shared code path that
        // could accidentally link a `freshrss_items` row to the wrong
        // article.
        match storage.insert_article_if_new_url(&article)? {
            Some(article_id) => {
                storage.insert_freshrss_item(article_id, &item.id, &item.origin.stream_id)?;
                summary.new_articles += 1;
            }
            None => {
                summary.url_conflicts += 1;
            }
        }
    }

    Ok(summary)
}

/// How long a background sync's own `Storage` connection will wait for
/// SQLite's write lock before giving up, if it happens to collide with a
/// write from the UI's own connection -- see
/// `Storage::set_busy_timeout`'s doc comment for the full reasoning.
/// Five seconds is generous relative to how long a single `INSERT`
/// actually takes (well under a millisecond in practice); the point
/// isn't to tune this finely, just to turn "immediate `SQLITE_BUSY`
/// error" into "wait a moment, then proceed," since a real collision
/// between two connections to a single-person's local SQLite file is
/// expected to be rare and brief, not sustained contention.
const STARTUP_SYNC_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Starts the FreshRSS startup sync (ARCHITECTURE.md's "Refresh
/// triggers," item 1) on its own OS thread and returns immediately with a
/// channel the caller can poll for the result -- this is the whole answer
/// to "how does the background sync share `Storage` with the UI" (see
/// ARCHITECTURE.md's section of that name):
///
/// - **A plain `std::thread::spawn`, not `tokio::spawn`.** `run` above
///   needs to interleave `.await`s (the `client` calls) with plain
///   synchronous `Storage` calls on a *borrowed* `&Storage` -- and
///   `rusqlite::Connection` is `Send` but not `Sync`, so a future that
///   captured a `&Storage` across an `.await` point would need
///   `&Storage: Send`, which needs `Storage: Sync`, which it isn't.
///   `tokio::spawn` requires its future to be `Send`; rather than fight
///   that bound (or wrap `Storage` in an `Arc<Mutex<_>>` that every other
///   caller of `Storage`'s methods would then also have to route through),
///   this spawns a genuine OS thread with no `Send` requirement on
///   anything it captures, and builds a throwaway single-threaded tokio
///   runtime (`Builder::new_current_thread()`) *inside* that thread just
///   to drive `run`'s `.await`s with `.block_on(...)`. `tokio::main`'s own
///   runtime never has to schedule this work at all.
/// - **Its own, independent `Storage::open()` connection**, never the
///   UI's. `rusqlite::Connection` not being `Sync` also means a bare
///   `&Storage` genuinely cannot be *shared* between this thread and the
///   UI's thread -- there is no safe way to hand this thread a reference
///   into the caller's own `Storage`. Opening a second connection to the
///   same `tuxwire.db` file sidesteps that entirely: SQLite itself
///   arbitrates access between multiple connections to one file, and
///   `set_busy_timeout` (called right after opening, below) is what
///   turns a rare moment of write contention between the two connections
///   into a short wait instead of an immediate error.
///
/// The `Sender` half of the channel is moved into the thread; the
/// `Receiver` half comes back to the caller. Sending can fail if the
/// caller already dropped its `Receiver` (e.g. the TUI exited before the
/// sync finished) -- `let _ = tx.send(...)` discards that specific error
/// deliberately, since there's nothing useful left to do with a result
/// nobody is listening for anymore.
pub fn spawn_startup_sync(
    url: String,
    username: String,
    password: Password,
    max_items_per_sync: usize,
) -> std::sync::mpsc::Receiver<anyhow::Result<SyncSummary>> {
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        // An immediately-invoked closure is what lets the `?` operator
        // be used freely for every fallible step below, while still
        // capturing whatever error resulted (rather than this whole
        // thread panicking on the first `?` failure) -- `result` ends up
        // as one `anyhow::Result<SyncSummary>` either way, exactly the
        // shape the channel carries.
        let result = (|| -> anyhow::Result<SyncSummary> {
            let storage = Storage::open()?;
            storage.set_busy_timeout(STARTUP_SYNC_BUSY_TIMEOUT)?;

            let client = Client::new(url, username, password);

            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            runtime.block_on(run(&storage, &client, max_items_per_sync))
        })();

        let _ = tx.send(result);
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::config::Password;

    /// A real, opt-in end-to-end check against a live FreshRSS server --
    /// **not** part of the normal `cargo test` run (see `#[ignore]`
    /// below), since it needs network access and real credentials,
    /// exactly the kind of test `docs/CLAUDE.md`'s "Unit tests should use
    /// saved JSON fixture files so they need no network" rule is about.
    /// This is the manual verification step instead -- run explicitly,
    /// on demand, against a genuine (never the real production) FreshRSS
    /// instance:
    ///
    /// ```text
    /// FRESHRSS_TEST_URL=http://localhost:8080 \
    /// FRESHRSS_TEST_USERNAME=your-test-username \
    /// FRESHRSS_TEST_PASSWORD=your-test-password \
    ///   cargo test --features freshrss -- --ignored manual_sync_against_a_live_server --nocapture
    /// ```
    ///
    /// What it checks, in order, matching the three scenarios asked for
    /// in review:
    /// 1. **First sync** against a server with at least one real unread
    ///    item succeeds and reports at least one new article.
    /// 2. **Second sync**, run immediately after with nothing new on the
    ///    server in between, reports **zero** new articles and at least
    ///    one "already synced" -- proving `remote_id` dedupe actually
    ///    works against a real server's real id shapes, not just the
    ///    fixture-driven unit tests already covering the same logic in
    ///    isolation.
    /// 3. **A URL conflict**: manually inserts a local (non-FreshRSS)
    ///    article at the exact URL of an article the first sync already
    ///    pulled in, deletes that FreshRSS-origin row so its URL is free
    ///    again, re-runs the sync, and confirms the conflict is skipped
    ///    and counted -- the local row is never touched.
    #[test]
    #[ignore = "needs network access and real FRESHRSS_TEST_* credentials -- see this test's own doc comment"]
    fn manual_sync_against_a_live_server() {
        let url = std::env::var("FRESHRSS_TEST_URL").expect("set FRESHRSS_TEST_URL to run this test");
        let username = std::env::var("FRESHRSS_TEST_USERNAME").expect("set FRESHRSS_TEST_USERNAME to run this test");
        let password = std::env::var("FRESHRSS_TEST_PASSWORD").expect("set FRESHRSS_TEST_PASSWORD to run this test");

        // `Storage::open()` resolves its path from `XDG_DATA_HOME`/`HOME`
        // -- pointing `XDG_DATA_HOME` at a fresh temp directory is the
        // same "throwaway home directory" technique
        // `docs/CLAUDE.md`'s "Building and testing" section documents for
        // running the whole app against disposable state, just applied to
        // this one test instead of a full `HOME=$(mktemp -d) cargo run`.
        let data_dir = std::env::temp_dir().join(format!("tuxwire-manual-sync-test-{}", std::process::id()));
        std::fs::create_dir_all(&data_dir).unwrap();
        unsafe {
            std::env::set_var("XDG_DATA_HOME", &data_dir);
        }

        let storage = Storage::open().unwrap();
        let client = Client::new(url, username, Password::from_plain_text_for_tests(password));

        // Scenario 1: first sync.
        let first = tokio::runtime::Runtime::new().unwrap().block_on(run(&storage, &client, 500)).unwrap();
        println!("first sync: {first:?}");
        assert!(first.new_articles > 0, "expected at least one new article on a first sync -- is the test server empty?");

        // Scenario 2: second sync, immediately after, nothing new on the
        // server in between.
        let second = tokio::runtime::Runtime::new().unwrap().block_on(run(&storage, &client, 500)).unwrap();
        println!("second sync: {second:?}");
        assert_eq!(second.new_articles, 0, "a second sync with nothing new on the server must insert nothing");
        assert!(second.already_synced > 0, "a second sync should recognize everything from the first as already synced");

        // Scenario 3: URL conflict. Steals the URL of whatever the first
        // sync actually inserted, freeing that FreshRSS item's own id to
        // be re-fetched, and confirms the conflict is skipped and
        // counted rather than silently adopted.
        let conflicting_url = storage
            .articles_by_topic(&{
                let topics = storage.topics().unwrap();
                topics.into_iter().find(|t| t.starts_with("FreshRSS/")).expect("a FreshRSS/* topic should exist by now")
            })
            .unwrap()
            .into_iter()
            .next()
            .expect("at least one FreshRSS article should exist by now")
            .url;

        // Delete that article (and its freshrss_items link, via the
        // foreign key) so the same URL -- and the same remote item -- can
        // be seen as "new" again, then immediately re-occupy the URL with
        // a plain local article before syncing again.
        storage.delete_article_for_tests(&conflicting_url).unwrap();
        let local = crate::models::Article::new(
            "A local article that happens to share a URL with a FreshRSS item".to_string(),
            conflicting_url,
            "Local Test Source".to_string(),
            "kernel".to_string(),
            chrono::Utc::now(),
        );
        let local_id = storage.insert_article(&local).unwrap();

        let third = tokio::runtime::Runtime::new().unwrap().block_on(run(&storage, &client, 500)).unwrap();
        println!("third sync (URL conflict): {third:?}");
        assert!(third.url_conflicts > 0, "expected the freed-up URL to collide with the local article and be skipped");

        let still_local = storage.articles_by_topic("kernel").unwrap();
        assert!(still_local.iter().any(|a| a.id == local_id), "the local article must still exist, untouched");

        unsafe {
            std::env::remove_var("XDG_DATA_HOME");
        }
        std::fs::remove_dir_all(&data_dir).ok();
    }
}
