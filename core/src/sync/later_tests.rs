//! Tests for the Read Later part of the sync cycle, run against the in-memory
//! fake CouchDB (`fake.rs`), the same way `tests.rs` tests saved articles. Each
//! "device" is its own in-memory database; several share one fake server, which
//! is how a phone, a computer and a tablet syncing through one CouchDB are
//! imitated.

#![warn(clippy::missing_docs_in_private_items)]

use super::fake::FakeCouch;
use super::*;
use serde_json::json;
use std::time::Duration;

/// The password used by every test.
const PASSWORD: &str = "hunter2-test-password";

/// A fresh device: an empty in-memory database behind a `Mutex`.
fn device() -> Mutex<Storage> {
    Mutex::new(Storage::open_in_memory().unwrap())
}

/// A config pointing at `server` with sync turned on.
fn config(server: &FakeCouch) -> SyncConfig {
    SyncConfig::new(
        true,
        server.url.clone(),
        "tuxwire".to_string(),
        "tuxwire".to_string(),
        PASSWORD.to_string(),
    )
}

/// Starts a fake server for the database "tuxwire".
async fn server() -> FakeCouch {
    FakeCouch::start("tuxwire", "tuxwire", PASSWORD).await
}

/// A short pause so a later change gets a later timestamp than an earlier one.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(5)).await;
}

/// Puts a link on `device`'s Read Later list.
fn add(device: &Mutex<Storage>, title: &str, url: &str) {
    device.lock().unwrap().add_read_later(title, url).unwrap();
}

/// The addresses on `device`'s list, sorted.
fn urls(device: &Mutex<Storage>) -> Vec<String> {
    let mut found: Vec<String> = device
        .lock()
        .unwrap()
        .list_read_later()
        .unwrap()
        .into_iter()
        .map(|i| i.url)
        .collect();
    found.sort();
    found
}

/// Whether the item at `url` on `device` is marked read.
fn is_read(device: &Mutex<Storage>, url: &str) -> bool {
    device
        .lock()
        .unwrap()
        .list_read_later()
        .unwrap()
        .into_iter()
        .find(|i| i.url == url)
        .map(|i| i.read)
        .unwrap()
}

/// Read Later rides on the sync switch: with sync off, nothing is sent.
#[tokio::test]
async fn read_later_does_nothing_while_sync_is_off() {
    let server = server().await;
    let a = device();
    add(&a, "A", "https://a.example/p");

    let mut cfg = config(&server);
    cfg.enabled = false;
    let error = sync_now(&a, &cfg).await.unwrap_err();
    assert!(error.to_string().contains("sync is turned off"));
    assert_eq!(server.request_count(), 0, "no request while switched off");
    assert_eq!(server.doc_count(), 0);
}

/// A link added on one device reaches another; a discard travels back.
#[tokio::test]
async fn a_link_and_its_removal_travel_between_two_devices() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());

    add(&a, "A post", "https://example.com/post");
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.later_sent, 1);
    assert!(server.doc("later:https://example.com/post").is_some());

    let report = sync_now(&b, &cfg).await.unwrap();
    assert_eq!(report.later_received, 1);
    assert_eq!(urls(&b), vec!["https://example.com/post".to_string()]);
    assert_eq!(report.later_sent, 0, "nothing to send back");

    // B discards it and syncs; A learns about it.
    tick().await;
    b.lock()
        .unwrap()
        .discard_read_later("https://example.com/post")
        .unwrap();
    sync_now(&b, &cfg).await.unwrap();
    sync_now(&a, &cfg).await.unwrap();
    assert!(urls(&a).is_empty(), "the removal reached the other device");
}

/// Three devices (two computers and a phone) all end up with the same list,
/// including an opened item and a removed one.
#[tokio::test]
async fn three_devices_end_up_in_step() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b, c) = (device(), device(), device());

    add(&a, "From A", "https://example.com/a");
    add(&b, "From B", "https://example.com/b");
    add(&c, "From C", "https://example.com/c");
    for d in [&a, &b, &c, &a, &b] {
        sync_now(d, &cfg).await.unwrap();
    }
    let all = vec![
        "https://example.com/a".to_string(),
        "https://example.com/b".to_string(),
        "https://example.com/c".to_string(),
    ];
    assert_eq!(urls(&a), all);
    assert_eq!(urls(&b), all);
    assert_eq!(urls(&c), all);

    // C opens A's link; B discards B's link.
    tick().await;
    c.lock()
        .unwrap()
        .mark_read_later_read("https://example.com/a")
        .unwrap();
    b.lock()
        .unwrap()
        .discard_read_later("https://example.com/b")
        .unwrap();
    for d in [&c, &b, &a, &c, &b] {
        sync_now(d, &cfg).await.unwrap();
    }
    let left = vec![
        "https://example.com/a".to_string(),
        "https://example.com/c".to_string(),
    ];
    assert_eq!(urls(&a), left);
    assert_eq!(urls(&b), left);
    assert_eq!(urls(&c), left);
    assert!(is_read(&a, "https://example.com/a"), "the open travelled");
    assert!(!is_read(&a, "https://example.com/c"));
}

/// When two devices change the same link, the later change wins everywhere.
#[tokio::test]
async fn the_later_change_to_the_same_link_wins() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());

    add(&a, "Shared", "https://example.com/s");
    sync_now(&a, &cfg).await.unwrap();
    sync_now(&b, &cfg).await.unwrap();

    // A opens it first, then B discards it later; the discard wins.
    tick().await;
    a.lock()
        .unwrap()
        .mark_read_later_read("https://example.com/s")
        .unwrap();
    tick().await;
    b.lock()
        .unwrap()
        .discard_read_later("https://example.com/s")
        .unwrap();
    for d in [&a, &b, &a, &b] {
        sync_now(d, &cfg).await.unwrap();
    }
    assert!(urls(&a).is_empty());
    assert!(urls(&b).is_empty());
}

/// Promoting a pasted link saves it as an ordinary saved article, which another
/// device receives, and the link leaves the list on both.
#[tokio::test]
async fn promoting_a_link_reaches_the_other_device_as_a_saved_article() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());

    add(&a, "Cool tool", "https://example.com/tool");
    sync_now(&a, &cfg).await.unwrap();
    sync_now(&b, &cfg).await.unwrap();
    assert_eq!(urls(&b).len(), 1);

    tick().await;
    a.lock()
        .unwrap()
        .promote_read_later("https://example.com/tool")
        .unwrap();
    sync_now(&a, &cfg).await.unwrap();
    sync_now(&b, &cfg).await.unwrap();

    assert!(urls(&a).is_empty());
    assert!(urls(&b).is_empty());
    let saved = b.lock().unwrap().saved_articles().unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].url, "https://example.com/tool");
    assert_eq!(saved[0].title, "Cool tool");
}

/// Documents from a newer version, unreadable ones and unsafe ones are skipped
/// and counted; a good one in the same batch still arrives.
#[tokio::test]
async fn odd_documents_are_skipped_and_counted() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();

    server.put_raw(
        "later:https://future.example/p",
        json!({"v":99,"url":"https://future.example/p","title":"T","added_at":5,
               "present":true,"updated_at":6}),
    );
    server.put_raw("later:https://bad.example/p", json!({"nonsense": true}));
    server.put_raw(
        "later:javascript:alert(1)",
        json!({"v":1,"url":"javascript:alert(1)","title":"T","added_at":5,
               "present":true,"updated_at":6}),
    );
    server.put_raw(
        "later:https://good.example/p",
        json!({"v":1,"url":"https://good.example/p","title":"Good","added_at":5,
               "present":true,"updated_at":6}),
    );

    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.skipped_newer_version, 1);
    assert_eq!(report.skipped_unreadable, 2);
    assert_eq!(report.later_received, 1);
    assert_eq!(urls(&a), vec!["https://good.example/p".to_string()]);
}

/// If another device writes the same link between our pull and our push, the
/// conflict is settled in the next round.
#[tokio::test]
async fn a_write_race_settles_in_the_next_round() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();
    add(&a, "Mine", "https://race.example/p");
    server.race_next_write(
        "later:https://race.example/p",
        json!({"v":1,"url":"https://race.example/p","title":"Other","added_at":1,
               "present":true,"updated_at":1}),
    );
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.later_conflicts_left, 0);
    assert_eq!(server.doc_count(), 1);
}

/// The summary says nothing about Read Later when it was not used, and the
/// password never shows up in it.
#[tokio::test]
async fn the_summary_is_unchanged_without_read_later_and_hides_the_password() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();

    let quiet = sync_now(&a, &cfg).await.unwrap();
    assert!(!quiet.summary().contains("Read Later"));

    add(&a, "A", "https://a.example/p");
    let busy = sync_now(&a, &cfg).await.unwrap();
    assert!(busy.summary().contains("Read Later: 1 sent, 0 received"));
    assert!(!busy.summary().contains(PASSWORD));
}

/// The Read Later checkpoint is separate from the saved-article checkpoint, so
/// a device updated later still reads documents written before it was.
#[tokio::test]
async fn the_read_later_checkpoint_is_its_own() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();
    assert_eq!(a.lock().unwrap().later_sync_since().unwrap(), None);

    // The saved-article checkpoint moves far ahead first...
    server.put_raw("article:https://old.example/x", json!({"v":1}));
    sync_now(&a, &cfg).await.unwrap();
    // ...then a Read Later document appears, written "earlier" in the sense
    // that this device never looked for it.
    server.put_raw(
        "later:https://late.example/p",
        json!({"v":1,"url":"https://late.example/p","title":"Late","added_at":5,
               "present":true,"updated_at":6}),
    );
    sync_now(&a, &cfg).await.unwrap();
    assert_eq!(urls(&a), vec!["https://late.example/p".to_string()]);
    assert!(a.lock().unwrap().later_sync_since().unwrap().is_some());
}
