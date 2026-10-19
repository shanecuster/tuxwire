//! Tests for the read-marks part of the sync cycle, run against the in-memory
//! fake CouchDB (`fake.rs`), the same way `later_tests.rs` tests Read Later.
//! Each "device" is its own in-memory database; several share one fake server.

#![warn(clippy::missing_docs_in_private_items)]

use super::fake::FakeCouch;
use super::*;
use rusqlite::params;
use serde_json::json;

/// The password used by every test.
const PASSWORD: &str = "hunter2-test-password";

/// A fresh device: an empty in-memory database behind a `Mutex`.
fn device() -> Mutex<Storage> {
    Mutex::new(Storage::open_in_memory().unwrap())
}

/// A config pointing at `server` with sync on and **read marks on**.
fn config(server: &FakeCouch) -> SyncConfig {
    SyncConfig::new(
        true,
        server.url.clone(),
        "tuxwire".to_string(),
        "tuxwire".to_string(),
        PASSWORD.to_string(),
    )
    .with_read_marks(true)
}

/// Starts a fake server for the database "tuxwire".
async fn server() -> FakeCouch {
    FakeCouch::start("tuxwire", "tuxwire", PASSWORD).await
}

/// Puts an unread article with address `url` on `device`.
fn add(device: &Mutex<Storage>, url: &str) {
    let article = crate::models::Article::new(
        format!("About {url}"),
        url.to_string(),
        "Feed".to_string(),
        "tech".to_string(),
        chrono::Utc::now(),
    );
    device.lock().unwrap().insert_article(&article).unwrap();
}

/// Opens the article at `url` on `device`, the way the apps do.
fn open(device: &Mutex<Storage>, url: &str) {
    let storage = device.lock().unwrap();
    let id: i64 = storage
        .connection_for_tests()
        .query_row(
            "SELECT id FROM articles WHERE url = ?1",
            params![url],
            |r| r.get(0),
        )
        .unwrap();
    storage.mark_read(id).unwrap();
}

/// Whether the article at `url` on `device` is read.
fn is_read(device: &Mutex<Storage>, url: &str) -> bool {
    device
        .lock()
        .unwrap()
        .connection_for_tests()
        .query_row(
            "SELECT read FROM articles WHERE url = ?1",
            params![url],
            |r| r.get(0),
        )
        .unwrap()
}

/// Read marks need their own switch: with only the main switch on, nothing
/// about reading is sent, however much was read.
#[tokio::test]
async fn nothing_is_sent_while_the_read_marks_switch_is_off() {
    let server = server().await;
    let a = device();
    add(&a, "https://a.example/1");
    open(&a, "https://a.example/1");

    let mut cfg = config(&server);
    cfg.read_marks = false;
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.marks_sent, 0);
    assert_eq!(server.doc_count(), 0, "no read: document was written");
    assert!(!report.summary().contains("Read marks"));
}

/// An article opened on one device is marked read on another that has it, and
/// the mark is not sent back.
#[tokio::test]
async fn opening_travels_to_another_device() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());
    add(&a, "https://a.example/post");
    add(&b, "https://a.example/post");
    add(&b, "https://a.example/other");

    open(&a, "https://a.example/post");
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.marks_sent, 1);
    assert!(server.doc("read:https://a.example/post").is_some());

    let report = sync_now(&b, &cfg).await.unwrap();
    assert_eq!(report.marks_received, 1);
    assert_eq!(report.marks_applied, 1);
    assert_eq!(report.marks_sent, 0, "not echoed back");
    assert!(is_read(&b, "https://a.example/post"));
    assert!(
        !is_read(&b, "https://a.example/other"),
        "others stay unread"
    );
    assert!(report.summary().contains("Read marks: 0 sent, 1 received"));
}

/// A mark for an article this device has not downloaded yet is applied when the
/// article arrives, even though the switch is not touched again.
#[tokio::test]
async fn a_mark_waits_for_an_article_that_has_not_arrived() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());
    add(&a, "https://a.example/new");
    open(&a, "https://a.example/new");
    sync_now(&a, &cfg).await.unwrap();

    // B has not fetched the article yet: the mark is received, nothing to mark.
    let report = sync_now(&b, &cfg).await.unwrap();
    assert_eq!(report.marks_received, 1);
    assert_eq!(report.marks_applied, 0);

    // Now B's feed refresh brings it in: it is read at once.
    add(&b, "https://a.example/new");
    assert!(is_read(&b, "https://a.example/new"));
}

/// Reads older than 90 days are not sent, and unread articles never are.
#[tokio::test]
async fn only_recent_reads_are_sent() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();
    for url in [
        "https://a.example/old",
        "https://a.example/new",
        "https://a.example/unread",
    ] {
        add(&a, url);
    }
    open(&a, "https://a.example/old");
    open(&a, "https://a.example/new");
    // Make "old" 100 days old.
    let hundred_days = 100 * 24 * 60 * 60 * 1000;
    a.lock()
        .unwrap()
        .connection_for_tests()
        .execute(
            "UPDATE articles SET read_at = ?1 WHERE url = 'https://a.example/old'",
            params![chrono::Utc::now().timestamp_millis() - hundred_days],
        )
        .unwrap();

    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.marks_sent, 1);
    assert!(server.doc("read:https://a.example/new").is_some());
    assert!(server.doc("read:https://a.example/old").is_none());
    assert!(server.doc("read:https://a.example/unread").is_none());
}

/// Both devices open the same article before syncing: it ends up sent once,
/// read on both, with nothing left to retry.
#[tokio::test]
async fn opening_the_same_article_on_two_devices_settles() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());
    for d in [&a, &b] {
        add(d, "https://a.example/same");
        open(d, "https://a.example/same");
    }
    for d in [&a, &b, &a] {
        let report = sync_now(d, &cfg).await.unwrap();
        assert_eq!(report.marks_conflicts_left, 0);
    }
    assert_eq!(server.doc_count(), 1);
    assert!(is_read(&a, "https://a.example/same") && is_read(&b, "https://a.example/same"));
}

/// Documents from a newer version, unreadable ones and unsafe ones are skipped
/// and counted; a good one in the same batch still arrives.
#[tokio::test]
async fn odd_documents_are_skipped_and_counted() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();
    add(&a, "https://good.example/p");

    server.put_raw(
        "read:https://future.example/p",
        json!({"v":99,"url":"https://future.example/p","read_at":6}),
    );
    server.put_raw("read:https://bad.example/p", json!({"nonsense": true}));
    server.put_raw(
        "read:javascript:alert(1)",
        json!({"v":1,"url":"javascript:alert(1)","read_at":6}),
    );
    server.put_raw(
        "read:https://good.example/p",
        json!({"v":1,"url":"https://good.example/p","read_at":6}),
    );

    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.skipped_newer_version, 1);
    assert_eq!(report.skipped_unreadable, 2);
    assert_eq!(report.marks_received, 1);
    assert!(is_read(&a, "https://good.example/p"));
}

/// If another device writes the same mark between our pull and our push, the
/// conflict is settled in the next round.
#[tokio::test]
async fn a_write_race_settles_in_the_next_round() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();
    add(&a, "https://race.example/p");
    open(&a, "https://race.example/p");
    server.race_next_write(
        "read:https://race.example/p",
        json!({"v":1,"url":"https://race.example/p","read_at":1}),
    );
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.marks_conflicts_left, 0);
    assert_eq!(server.doc_count(), 1);
}

/// A document deleted on the server by hand is written again next time.
#[tokio::test]
async fn a_mark_deleted_on_the_server_is_sent_again() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();
    add(&a, "https://a.example/1");
    open(&a, "https://a.example/1");
    sync_now(&a, &cfg).await.unwrap();
    server.delete_doc("read:https://a.example/1");

    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.marks_sent, 1);
    assert!(server.doc("read:https://a.example/1").is_some());
}

/// The checkpoint is its own, and the password never shows up in the summary.
#[tokio::test]
async fn the_read_marks_checkpoint_is_its_own_and_the_summary_hides_the_password() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();
    assert_eq!(a.lock().unwrap().read_marks_sync_since().unwrap(), None);

    server.put_raw("article:https://old.example/x", json!({"v":1}));
    sync_now(&a, &cfg).await.unwrap();
    server.put_raw(
        "read:https://late.example/p",
        json!({"v":1,"url":"https://late.example/p","read_at":6}),
    );
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.marks_received, 1);
    assert!(a.lock().unwrap().read_marks_sync_since().unwrap().is_some());
    assert!(!report.summary().contains(PASSWORD));
}
