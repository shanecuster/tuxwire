//! Tests for the feed-list sync cycle, run against the in-memory fake CouchDB
//! (`fake.rs`), the same way `tests.rs` tests the saved-article sync. Each
//! "device" is its own in-memory database; several share one fake server.
//!
//! A device's feed list is just a `Vec<FeedEntry>` here, standing in for
//! `sources.toml` or the Android `feeds` table.

#![warn(clippy::missing_docs_in_private_items)]

use super::fake::FakeCouch;
use super::feeds::sync_feeds;
use super::*;
use crate::storage::FeedEntry;
use serde_json::json;
use std::time::Duration;

/// The password used by every test.
const PASSWORD: &str = "hunter2-test-password";

/// A fresh device: an empty in-memory database behind a `Mutex`.
fn device() -> Mutex<Storage> {
    Mutex::new(Storage::open_in_memory().unwrap())
}

/// A config pointing at `server` with sync on. `feeds` is the separate feed
/// switch.
fn config(server: &FakeCouch, feeds: bool) -> SyncConfig {
    SyncConfig::new(
        true,
        server.url.clone(),
        "tuxwire".to_string(),
        "tuxwire".to_string(),
        PASSWORD.to_string(),
    )
    .with_feeds(feeds)
}

/// Starts a fake server for the database "tuxwire".
async fn server() -> FakeCouch {
    FakeCouch::start("tuxwire", "tuxwire", PASSWORD).await
}

/// A feed list entry.
fn entry(name: &str, url: &str) -> FeedEntry {
    FeedEntry {
        name: name.to_string(),
        url: url.to_string(),
        topic: "tech".to_string(),
    }
}

/// A short pause so a later change gets a later timestamp than an earlier one.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(5)).await;
}

/// The addresses waiting to be added on `device`, sorted.
fn additions(device: &Mutex<Storage>) -> Vec<String> {
    let mut urls: Vec<String> = device
        .lock()
        .unwrap()
        .pending_feed_additions()
        .unwrap()
        .into_iter()
        .map(|f| f.url)
        .collect();
    urls.sort();
    urls
}

/// The addresses whose removal is waiting for an answer on `device`, sorted.
fn removals(device: &Mutex<Storage>) -> Vec<String> {
    let mut urls: Vec<String> = device
        .lock()
        .unwrap()
        .pending_feed_removals()
        .unwrap()
        .into_iter()
        .map(|f| f.url)
        .collect();
    urls.sort();
    urls
}

/// Feed sync does nothing, and makes no request, unless both switches are on.
#[tokio::test]
async fn feed_sync_refuses_while_either_switch_is_off() {
    let server = server().await;
    let a = device();

    // Sync on, feed switch off (the default).
    let error = sync_feeds(&a, &config(&server, false), vec![entry("A", "https://a/f")])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("feed sync is turned off"));

    // Sync itself off.
    let mut cfg = config(&server, true);
    cfg.enabled = false;
    let error = sync_feeds(&a, &cfg, vec![entry("A", "https://a/f")])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("sync is turned off"));

    assert_eq!(server.request_count(), 0, "no request while switched off");
    assert!(a.lock().unwrap().pending_feed_pushes().unwrap().is_empty());
}

/// A feed added on one device is offered to the other, which adds it and then
/// has nothing left to send.
#[tokio::test]
async fn a_feed_added_on_one_device_reaches_the_other() {
    let server = server().await;
    let cfg = config(&server, true);
    let (a, b) = (device(), device());

    // A follows two feeds and syncs.
    let list_a = vec![
        entry("One", "https://one.example/feed"),
        entry("Two", "https://two.example/feed"),
    ];
    let report = sync_feeds(&a, &cfg, list_a.clone()).await.unwrap();
    assert_eq!((report.found_here, report.pushed), (2, 2));
    assert_eq!(server.doc_count(), 2);

    // B follows nothing; both feeds are waiting to be added.
    sync_feeds(&b, &cfg, vec![]).await.unwrap();
    assert_eq!(
        additions(&b),
        vec!["https://one.example/feed", "https://two.example/feed"]
    );

    // B's app adds them to its list and says so.
    // Collect the list first: a `for` loop would otherwise hold the first
    // lock for its whole run, and locking again inside it would wait forever.
    let waiting = b.lock().unwrap().pending_feed_additions().unwrap();
    for feed in waiting {
        b.lock().unwrap().feed_added_locally(&feed.url).unwrap();
    }
    assert!(additions(&b).is_empty());

    // Syncing again with the new list sends nothing and finds nothing new.
    let report = sync_feeds(&b, &cfg, list_a.clone()).await.unwrap();
    assert_eq!(
        (report.pushed, report.found_here, report.removed_here),
        (0, 0, 0)
    );
    let report = sync_feeds(&a, &cfg, list_a).await.unwrap();
    assert_eq!(report.pushed, 0);
    assert!(additions(&a).is_empty());
    assert_eq!(server.doc_count(), 2, "still one document per feed");
}

/// A feed both devices already follow is not duplicated or offered again.
#[tokio::test]
async fn a_feed_both_devices_follow_is_one_feed() {
    let server = server().await;
    let cfg = config(&server, true);
    let (a, b) = (device(), device());
    let shared = vec![entry("Same", "https://same.example/feed")];

    sync_feeds(&a, &cfg, shared.clone()).await.unwrap();
    // B has it too, added by hand with different capitals.
    let report = sync_feeds(&b, &cfg, vec![entry("Same", "HTTPS://Same.example/Feed")])
        .await
        .unwrap();
    assert_eq!(report.pushed, 0, "nothing new to send");
    assert!(additions(&b).is_empty());
    assert_eq!(server.doc_count(), 1);
}

/// A removal on one device is only listed on the other. The other device keeps
/// its feed until the person agrees, and agreeing sends nothing back.
#[tokio::test]
async fn a_removal_is_listed_and_waits_for_a_yes() {
    let server = server().await;
    let cfg = config(&server, true);
    let (a, b) = (device(), device());
    let both = vec![
        entry("Keep", "https://keep.example/f"),
        entry("Drop", "https://drop.example/f"),
    ];

    // Both devices follow both feeds.
    sync_feeds(&a, &cfg, both.clone()).await.unwrap();
    sync_feeds(&b, &cfg, both.clone()).await.unwrap();
    assert!(additions(&b).is_empty());

    // B stops following "Drop" and syncs.
    tick().await;
    let only_keep = vec![both[0].clone()];
    let report = sync_feeds(&b, &cfg, only_keep.clone()).await.unwrap();
    assert_eq!((report.removed_here, report.pushed), (1, 1));
    assert_eq!(
        server.doc("feed:https://drop.example/f").unwrap()["present"],
        false
    );

    // A syncs with its list unchanged: the removal is only listed.
    let report = sync_feeds(&a, &cfg, both.clone()).await.unwrap();
    assert_eq!(report.removed_here, 0);
    assert_eq!(removals(&a), vec!["https://drop.example/f"]);
    // Asking again does not change the answer, and nothing was removed.
    sync_feeds(&a, &cfg, both.clone()).await.unwrap();
    assert_eq!(removals(&a), vec!["https://drop.example/f"]);

    // The person says yes: the app removes it and says so. Nothing is sent.
    a.lock()
        .unwrap()
        .feed_removed_locally("https://drop.example/f")
        .unwrap();
    let report = sync_feeds(&a, &cfg, only_keep).await.unwrap();
    assert_eq!((report.pushed, report.removed_here), (0, 0));
    assert!(removals(&a).is_empty());
}

/// Saying no keeps the feed and stops the question, and it is not removed on
/// the next syncs.
#[tokio::test]
async fn keeping_a_feed_stops_the_question() {
    let server = server().await;
    let cfg = config(&server, true);
    let (a, b) = (device(), device());
    let both = vec![
        entry("Keep", "https://keep.example/f"),
        entry("Drop", "https://drop.example/f"),
    ];

    sync_feeds(&a, &cfg, both.clone()).await.unwrap();
    sync_feeds(&b, &cfg, both.clone()).await.unwrap();
    tick().await;
    sync_feeds(&b, &cfg, vec![both[0].clone()]).await.unwrap();
    sync_feeds(&a, &cfg, both.clone()).await.unwrap();
    assert_eq!(removals(&a).len(), 1);

    // The person keeps it.
    a.lock()
        .unwrap()
        .dismiss_feed_removal("https://drop.example/f")
        .unwrap();
    let report = sync_feeds(&a, &cfg, both).await.unwrap();
    assert!(removals(&a).is_empty());
    assert_eq!(report.pushed, 0, "keeping a feed sends nothing");
}

/// Feed documents and saved-article documents share one database without
/// bothering each other: article sync skips feed documents silently, and feed
/// sync skips article documents.
#[tokio::test]
async fn feed_and_article_sync_ignore_each_others_documents() {
    let server = server().await;
    let cfg = config(&server, true);
    let a = device();
    sync_feeds(&a, &cfg, vec![entry("A", "https://a.example/f")])
        .await
        .unwrap();
    server.put_raw(
        "article:https://x/1",
        json!({"v":1,"url":"https://x/1","title":"t","source":"s","topic":"c",
               "published":0,"saved":true,"updated_at":5}),
    );

    // Saved-article sync on another device: gets the article, ignores the feed.
    let b = device();
    let report = sync_now(&b, &cfg).await.unwrap();
    assert_eq!(report.pulled_added, 1);
    assert_eq!(
        (report.skipped_newer_version, report.skipped_unreadable),
        (0, 0)
    );

    // Feed sync on that device: gets the feed, ignores the article.
    let report = sync_feeds(&b, &cfg, vec![]).await.unwrap();
    assert_eq!(report.skipped_unusable, 0);
    assert_eq!(additions(&b), vec!["https://a.example/f"]);
}

/// Turning feed sync on after saved-article sync has already run still finds
/// the feeds: the two use separate checkpoints.
#[tokio::test]
async fn turning_feed_sync_on_later_still_finds_earlier_feeds() {
    let server = server().await;
    let (a, b) = (device(), device());
    sync_feeds(
        &a,
        &config(&server, true),
        vec![entry("A", "https://a.example/f")],
    )
    .await
    .unwrap();

    // B syncs saved articles only, several times (the feed switch is off).
    sync_now(&b, &config(&server, false)).await.unwrap();
    sync_now(&b, &config(&server, false)).await.unwrap();
    assert!(additions(&b).is_empty());

    // Now the switch goes on: the feed is still found.
    sync_feeds(&b, &config(&server, true), vec![])
        .await
        .unwrap();
    assert_eq!(additions(&b), vec!["https://a.example/f"]);
}

/// Documents that are not usable feeds are skipped and counted, never offered:
/// a dangerous address, a control character in the name, a newer format.
#[tokio::test]
async fn unusable_documents_are_skipped_and_counted() {
    let server = server().await;
    let cfg = config(&server, true);
    let b = device();
    server.put_raw(
        "feed:file:///etc/passwd",
        json!({"v":1,"url":"file:///etc/passwd","name":"x","topic":"t","present":true,"updated_at":5}),
    );
    server.put_raw(
        "feed:https://evil.example/f",
        json!({"v":1,"url":"https://evil.example/f","name":"bad\u{1b}[2J","topic":"t","present":true,"updated_at":5}),
    );
    server.put_raw(
        "feed:https://future.example/f",
        json!({"v":2,"url":"https://future.example/f","name":"n","topic":"t","present":true,"updated_at":5}),
    );
    server.put_raw(
        "feed:https://good.example/f",
        json!({"v":1,"url":"https://good.example/f","name":"Good","topic":"t","present":true,"updated_at":5}),
    );

    let report = sync_feeds(&b, &cfg, vec![]).await.unwrap();
    assert_eq!(
        (report.skipped_unusable, report.skipped_newer_version),
        (2, 1)
    );
    assert_eq!(additions(&b), vec!["https://good.example/f"]);
}

/// An empty list while feeds were expected is not taken as "everything was
/// removed": nothing is sent, and the report says so.
#[tokio::test]
async fn an_empty_list_does_not_wipe_the_feeds() {
    let server = server().await;
    let cfg = config(&server, true);
    let (a, b) = (device(), device());
    let list = vec![
        entry("A", "https://a.example/f"),
        entry("B", "https://b.example/f"),
    ];
    sync_feeds(&a, &cfg, list.clone()).await.unwrap();

    tick().await;
    let report = sync_feeds(&a, &cfg, vec![]).await.unwrap();
    assert!(report.removals_skipped);
    assert_eq!((report.removed_here, report.pushed), (0, 0));

    // Another device sees no removals.
    sync_feeds(&b, &cfg, list).await.unwrap();
    assert!(removals(&b).is_empty());
}

/// If another device writes between our pull and our push, the conflict is
/// settled by going round again and the feed still arrives.
#[tokio::test]
async fn a_write_race_settles_in_the_next_round() {
    let server = server().await;
    let cfg = config(&server, true);
    let a = device();
    // Another device writes the same feed just before our push.
    server.race_next_write(
        "feed:https://race.example/f",
        json!({"v":1,"url":"https://race.example/f","name":"Other","topic":"t","present":true,"updated_at":1}),
    );
    let report = sync_feeds(&a, &cfg, vec![entry("Mine", "https://race.example/f")])
        .await
        .unwrap();
    assert_eq!(report.conflicts_left, 0);
    assert_eq!(server.doc_count(), 1);
}

/// The password never appears in the report text.
#[tokio::test]
async fn the_report_never_shows_the_password() {
    let server = server().await;
    let cfg = config(&server, true);
    let a = device();
    let report = sync_feeds(&a, &cfg, vec![entry("A", "https://a.example/f")])
        .await
        .unwrap();
    assert!(!report.summary().contains(PASSWORD));
    assert!(!format!("{cfg:?}").contains(PASSWORD));
}
