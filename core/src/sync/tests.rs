//! Tests for the sync cycle, run against the in-memory fake CouchDB
//! (`fake.rs`). Each "device" is its own in-memory database, and several
//! devices can share one fake server, which is how two phones/computers
//! syncing through the same CouchDB are imitated.
//!
//! These need no real network: the fake listens on this computer only. (The
//! project runs tests with `NO_PROXY=127.0.0.1` so a proxy does not intercept.)

#![warn(clippy::missing_docs_in_private_items)]

use super::fake::FakeCouch;
use super::*;
use crate::models::Article;
use chrono::Utc;
use std::time::Duration;

/// The password used by every test. Tests check it never shows up in output.
const PASSWORD: &str = "hunter2-test-password";

/// A fresh device: an empty in-memory database behind a `Mutex`.
fn device() -> Mutex<Storage> {
    Mutex::new(Storage::open_in_memory().unwrap())
}

/// A config pointing at `server`, with sync turned on.
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

/// Stores a plain article at `url` on `device` and returns its id.
fn add(device: &Mutex<Storage>, url: &str) -> i64 {
    let article = Article::new(
        format!("Title of {url}"),
        url.to_string(),
        "Feed".to_string(),
        "tech".to_string(),
        Utc::now(),
    );
    device.lock().unwrap().insert_article(&article).unwrap()
}

/// Saves article `id` on `device`, with a note.
fn save_with_note(device: &Mutex<Storage>, id: i64, note: &str) {
    let storage = device.lock().unwrap();
    storage.save_article(id, None).unwrap();
    storage.update_note(id, Some(note.to_string())).unwrap();
}

/// The note on the first saved article of `device`, if any.
fn first_note(device: &Mutex<Storage>) -> Option<String> {
    device.lock().unwrap().saved_articles().unwrap()[0]
        .note
        .clone()
}

/// A short pause so a later change gets a later timestamp than an earlier one
/// on another "device".
async fn tick() {
    tokio::time::sleep(Duration::from_millis(5)).await;
}

/// One device saves an article with a note; another device receives it, edits
/// the note, and the first receives the edit.
#[tokio::test]
async fn two_devices_share_saved_articles_and_notes() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());

    // Device A saves and notes an article, then syncs.
    let id_a = add(&a, "https://example.com/1");
    save_with_note(&a, id_a, "from A");
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.pushed, 1);
    assert_eq!(server.doc_count(), 1);

    // Device B has never seen it: it arrives saved, with the note.
    let report = sync_now(&b, &cfg).await.unwrap();
    assert_eq!(report.pulled_added, 1);
    assert_eq!(first_note(&b).as_deref(), Some("from A"));

    // B edits the note (later), syncs; A syncs and receives the edit.
    tick().await;
    let id_b = b.lock().unwrap().saved_articles().unwrap()[0].id;
    b.lock()
        .unwrap()
        .update_note(id_b, Some("from B".into()))
        .unwrap();
    sync_now(&b, &cfg).await.unwrap();
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.pulled_updated, 1);
    assert_eq!(first_note(&a).as_deref(), Some("from B"));

    // Syncing again changes nothing and sends nothing.
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(
        (report.pushed, report.pulled_added, report.pulled_updated),
        (0, 0, 0)
    );
}

/// Un-saving on one device reaches the other (note and all), and does not
/// come back to life on later syncs.
#[tokio::test]
async fn unsaving_propagates_and_stays_unsaved() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());

    let id_a = add(&a, "https://example.com/1");
    save_with_note(&a, id_a, "temporary");
    sync_now(&a, &cfg).await.unwrap();
    sync_now(&b, &cfg).await.unwrap();
    assert_eq!(b.lock().unwrap().saved_articles().unwrap().len(), 1);

    // A un-saves, syncs. The server keeps a document that says "not saved".
    tick().await;
    a.lock().unwrap().unsave_article(id_a).unwrap();
    sync_now(&a, &cfg).await.unwrap();
    assert_eq!(
        server.doc("article:https://example.com/1").unwrap()["saved"],
        false
    );

    // B receives it: no longer saved, and the note went with the save.
    sync_now(&b, &cfg).await.unwrap();
    assert!(b.lock().unwrap().saved_articles().unwrap().is_empty());

    // More syncing on both sides does not bring it back.
    sync_now(&a, &cfg).await.unwrap();
    sync_now(&b, &cfg).await.unwrap();
    assert!(a.lock().unwrap().saved_articles().unwrap().is_empty());
    assert!(b.lock().unwrap().saved_articles().unwrap().is_empty());
}

/// When both devices change the same note without syncing in between, the
/// later change wins on both.
#[tokio::test]
async fn the_later_edit_wins_a_disagreement() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());

    let id_a = add(&a, "https://example.com/1");
    save_with_note(&a, id_a, "start");
    sync_now(&a, &cfg).await.unwrap();
    sync_now(&b, &cfg).await.unwrap();
    let id_b = b.lock().unwrap().saved_articles().unwrap()[0].id;

    // Both edit offline; B's edit is later.
    tick().await;
    a.lock()
        .unwrap()
        .update_note(id_a, Some("A offline".into()))
        .unwrap();
    tick().await;
    b.lock()
        .unwrap()
        .update_note(id_b, Some("B offline".into()))
        .unwrap();

    sync_now(&a, &cfg).await.unwrap();
    sync_now(&b, &cfg).await.unwrap();
    sync_now(&a, &cfg).await.unwrap();
    assert_eq!(first_note(&a).as_deref(), Some("B offline"));
    assert_eq!(first_note(&b).as_deref(), Some("B offline"));
}

/// If another device writes the same document between our pull and our push,
/// the write conflicts, the next round settles it, and the newer copy wins.
#[tokio::test]
async fn a_write_conflict_is_settled_by_another_round() {
    let server = server().await;
    let cfg = config(&server);
    let b = device();

    // The other device's older copy appears just before B's push.
    let id_b = add(&b, "https://example.com/1");
    server.race_next_write(
        "article:https://example.com/1",
        serde_json::json!({
            "v": 1, "url": "https://example.com/1", "title": "t", "source": "s",
            "topic": "c", "published": 0, "saved": true, "saved_at": 1,
            "note": "other device", "noted_at": 1, "updated_at": 1
        }),
    );
    save_with_note(&b, id_b, "mine, newer");

    let report = sync_now(&b, &cfg).await.unwrap();
    assert_eq!(report.pushed, 1);
    assert_eq!(report.conflicts_left, 0);
    let stored = server.doc("article:https://example.com/1").unwrap();
    assert_eq!(stored["note"], "mine, newer");
}

/// More documents than one page are all transferred, and a second sync finds
/// nothing new.
#[tokio::test]
async fn many_articles_move_in_several_pages() {
    let server = server().await;
    let cfg = config(&server);
    let (a, b) = (device(), device());

    for n in 0..250 {
        let id = add(&a, &format!("https://example.com/{n}"));
        a.lock().unwrap().save_article(id, None).unwrap();
    }
    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.pushed, 250);

    let report = sync_now(&b, &cfg).await.unwrap();
    assert_eq!(report.pulled_added, 250);
    assert_eq!(b.lock().unwrap().saved_articles().unwrap().len(), 250);

    let report = sync_now(&b, &cfg).await.unwrap();
    assert_eq!((report.pulled_added, report.pushed), (0, 0));
}

/// A document deleted by hand on the server is written again next sync.
#[tokio::test]
async fn a_document_deleted_on_the_server_is_written_again() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();

    let id = add(&a, "https://example.com/1");
    save_with_note(&a, id, "keep");
    sync_now(&a, &cfg).await.unwrap();
    server.delete_doc("article:https://example.com/1");
    assert_eq!(server.doc_count(), 0);

    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.pushed, 1);
    assert_eq!(server.doc_count(), 1);
}

/// Documents from a newer tuxwire, unreadable ones and other apps' are
/// skipped; the readable ones still sync.
#[tokio::test]
async fn unknown_documents_are_skipped_not_fatal() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();

    server.put_raw(
        "article:https://z/new",
        serde_json::json!({
            "v": 2, "url": "https://z/new", "title": "t", "source": "s", "topic": "c",
            "published": 0, "saved": true, "updated_at": 5
        }),
    );
    server.put_raw("article:broken", serde_json::json!({"junk": true}));
    server.put_raw("other-app:thing", serde_json::json!({"hello": "world"}));
    server.put_raw(
        "article:https://z/ok",
        serde_json::json!({
            "v": 1, "url": "https://z/ok", "title": "ok", "source": "s", "topic": "c",
            "published": 0, "saved": true, "updated_at": 5
        }),
    );

    let report = sync_now(&a, &cfg).await.unwrap();
    assert_eq!(report.skipped_newer_version, 1);
    assert_eq!(report.skipped_unreadable, 1);
    assert_eq!(report.pulled_added, 1);
    // The other app's document is untouched.
    assert!(server.doc("other-app:thing").is_some());
}

/// Only saved/note state is sent: nothing about reading or skipping, and no
/// credential.
#[tokio::test]
async fn only_saved_state_is_sent() {
    let server = server().await;
    let cfg = config(&server);
    let a = device();

    let id = add(&a, "https://example.com/1");
    {
        let storage = a.lock().unwrap();
        storage.mark_skipped(id).unwrap();
    }
    save_with_note(&a, id, "note");
    sync_now(&a, &cfg).await.unwrap();

    let stored = server.doc("article:https://example.com/1").unwrap();
    let keys: Vec<&str> = stored
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert!(!keys.contains(&"read"));
    assert!(!keys.contains(&"skipped"));
    assert!(!stored.to_string().contains(PASSWORD));
}

/// Sync does nothing at all while it is turned off: no request is made.
#[tokio::test]
async fn nothing_happens_while_sync_is_off() {
    let server = server().await;
    let mut cfg = config(&server);
    cfg.enabled = false;
    let a = device();

    let error = sync_now(&a, &cfg).await.unwrap_err();
    assert!(error.to_string().contains("turned off"));
    assert_eq!(server.request_count(), 0);
}

/// Each failure gets a plain sentence that names the problem and leaves out
/// the address and the password.
#[tokio::test]
async fn failures_are_explained_in_plain_words() {
    let server = server().await;
    let a = device();

    // Wrong password.
    let mut cfg = config(&server);
    cfg = SyncConfig::new(
        true,
        cfg.url.clone(),
        cfg.database.clone(),
        cfg.username.clone(),
        "wrong".into(),
    );
    let error = sync_now(&a, &cfg).await.unwrap_err().to_string();
    assert!(error.contains("wrong username or password"), "{error}");

    // Database that does not exist.
    let cfg = SyncConfig::new(
        true,
        server.url.clone(),
        "other".into(),
        "tuxwire".into(),
        PASSWORD.into(),
    );
    let error = test_connection(&cfg).await.unwrap_err().to_string();
    assert!(error.contains("database not found"), "{error}");

    // Good settings pass the connection test even while sync is off.
    let mut cfg = config(&server);
    cfg.enabled = false;
    test_connection(&cfg).await.unwrap();

    // Server gone: offline, and the message does not echo the address.
    server.stop();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let cfg = config(&server);
    let error = sync_now(&a, &cfg).await.unwrap_err().to_string();
    assert!(error.contains("offline"), "{error}");
    assert!(!error.contains("127.0.0.1"), "{error}");
    assert!(!error.contains(PASSWORD), "{error}");
}

/// The address rules: https (or this computer only), no credentials inside it,
/// a valid database name, and both login fields filled in.
#[test]
fn settings_are_checked_before_anything_is_sent() {
    let make = |url: &str, db: &str, user: &str, pw: &str| {
        SyncConfig::new(true, url.into(), db.into(), user.into(), pw.into())
    };

    assert!(
        make("https://couch.example.ts.net", "tuxwire", "u", "p")
            .validate()
            .is_ok()
    );
    assert!(
        make("http://127.0.0.1:5984", "tuxwire", "u", "p")
            .validate()
            .is_ok()
    );
    assert!(
        make("http://localhost:5984", "tuxwire", "u", "p")
            .validate()
            .is_ok()
    );

    // Plain http to another machine would expose the password.
    let error = make("http://couch.example", "tuxwire", "u", "p")
        .validate()
        .unwrap_err();
    assert!(error.to_string().contains("https://"));
    // Credentials do not belong in the address.
    assert!(
        make("https://u:p@couch.example", "tuxwire", "u", "p")
            .validate()
            .is_err()
    );
    assert!(
        make("ftp://couch.example", "tuxwire", "u", "p")
            .validate()
            .is_err()
    );
    assert!(make("not a url", "tuxwire", "u", "p").validate().is_err());
    // Database names.
    assert!(
        make("https://c.example", "Tuxwire", "u", "p")
            .validate()
            .is_err()
    );
    assert!(
        make("https://c.example", "1tux", "u", "p")
            .validate()
            .is_err()
    );
    assert!(
        make("https://c.example", "a/b", "u", "p")
            .validate()
            .is_err()
    );
    assert!(make("https://c.example", "", "u", "p").validate().is_err());
    // Login fields.
    assert!(
        make("https://c.example", "tuxwire", "", "p")
            .validate()
            .is_err()
    );
    assert!(
        make("https://c.example", "tuxwire", "u", "")
            .validate()
            .is_err()
    );
}

/// Printing a config (in a log, a crash report) never reveals the password or
/// the server address.
#[test]
fn debug_output_hides_the_password_and_address() {
    let cfg = SyncConfig::new(
        true,
        "https://private.tailnet.ts.net".into(),
        "tuxwire".into(),
        "me".into(),
        PASSWORD.into(),
    );
    let printed = format!("{cfg:?}");
    assert!(!printed.contains(PASSWORD));
    assert!(!printed.contains("private.tailnet"));
}

/// The one-line summary mentions only what happened.
#[test]
fn the_report_summary_reads_plainly() {
    let report = SyncReport {
        pulled_added: 2,
        pulled_updated: 1,
        pushed: 4,
        skipped_newer_version: 1,
        ..SyncReport::default()
    };
    assert_eq!(
        report.summary(),
        "4 sent, 3 received, 1 skipped (update tuxwire)"
    );
}
