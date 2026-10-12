//! A tiny in-memory imitation of CouchDB, for tests only.
//!
//! It understands exactly the three requests sync makes (database info,
//! `_changes`, `_bulk_docs`), checks the login, hands out revision labels, and
//! answers a write with a stale revision as a **conflict**, as the real server
//! does. That is enough to test the whole sync cycle, including two devices
//! sharing one server, without a real CouchDB.
//!
//! Not a general HTTP server: one request per connection, no TLS.

#![warn(clippy::missing_docs_in_private_items)]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One stored document.
struct FakeDoc {
    /// Which revision number it is on (1, 2, 3...).
    rev_number: u64,
    /// The document's JSON, without `_rev`.
    value: Value,
    /// Whether it has been deleted.
    deleted: bool,
}

/// Everything the fake server remembers.
struct FakeState {
    /// The one database name it serves.
    database: String,
    /// The `Authorization` header value it accepts.
    expected_auth: String,
    /// The documents, by name.
    docs: HashMap<String, FakeDoc>,
    /// The sequence number of the latest change.
    seq: u64,
    /// The change log: (sequence number, document name), oldest first.
    log: Vec<(u64, String)>,
    /// How many requests have been answered (including refused logins).
    requests: usize,
    /// A document to write just before the next batch write is processed,
    /// imitating another device writing between our pull and our push.
    race: Option<(String, Value)>,
}

/// A running fake server.
pub(super) struct FakeCouch {
    /// Its address, like `http://127.0.0.1:50123`.
    pub url: String,
    /// The shared state, so tests can look inside and change it.
    state: Arc<Mutex<FakeState>>,
    /// The background task accepting connections.
    task: tokio::task::JoinHandle<()>,
}

impl FakeCouch {
    /// Starts a server for `database` that accepts `user` / `password`.
    pub(super) async fn start(database: &str, user: &str, password: &str) -> FakeCouch {
        // Port 0 means "any free port". The listener is made with the
        // standard library first so the address is known right away.
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let port = std_listener.local_addr().unwrap().port();
        let listener = TcpListener::from_std(std_listener).unwrap();

        let state = Arc::new(Mutex::new(FakeState {
            database: database.to_string(),
            expected_auth: format!("Basic {}", base64(format!("{user}:{password}").as_bytes())),
            docs: HashMap::new(),
            seq: 0,
            log: Vec::new(),
            requests: 0,
            race: None,
        }));

        // The accept loop: each connection is handled on its own task.
        let loop_state = state.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let connection_state = loop_state.clone();
                tokio::spawn(async move {
                    let _ = handle(stream, connection_state).await;
                });
            }
        });

        FakeCouch {
            url: format!("http://127.0.0.1:{port}"),
            state,
            task,
        }
    }

    /// Stops the server, so later connections are refused ("offline").
    pub(super) fn stop(&self) {
        self.task.abort();
    }

    /// The stored JSON of document `id` (with `_id`, without `_rev`), if it
    /// exists and is not deleted.
    pub(super) fn doc(&self, id: &str) -> Option<Value> {
        let state = self.state.lock().unwrap();
        state
            .docs
            .get(id)
            .filter(|d| !d.deleted)
            .map(|d| d.value.clone())
    }

    /// How many live (not deleted) documents there are.
    pub(super) fn doc_count(&self) -> usize {
        let state = self.state.lock().unwrap();
        state.docs.values().filter(|d| !d.deleted).count()
    }

    /// How many requests the server has answered.
    pub(super) fn request_count(&self) -> usize {
        self.state.lock().unwrap().requests
    }

    /// Deletes a document, as someone might by hand in the CouchDB web page.
    pub(super) fn delete_doc(&self, id: &str) {
        let mut state = self.state.lock().unwrap();
        state.seq += 1;
        let seq = state.seq;
        if let Some(doc) = state.docs.get_mut(id) {
            doc.rev_number += 1;
            doc.deleted = true;
            state.log.push((seq, id.to_string()));
        }
    }

    /// Puts a document on the server directly, whatever its shape. Used to
    /// plant documents from a newer tuxwire, unreadable ones, and others'.
    pub(super) fn put_raw(&self, id: &str, value: Value) {
        let mut state = self.state.lock().unwrap();
        write_doc(&mut state, id, value);
    }

    /// Makes the next batch write first insert `value` as `id`, imitating
    /// another device that wrote after our pull and before our push.
    pub(super) fn race_next_write(&self, id: &str, value: Value) {
        self.state.lock().unwrap().race = Some((id.to_string(), value));
    }
}

/// Stores `value` as document `id` on its next revision and logs the change.
/// Returns the new revision label.
fn write_doc(state: &mut FakeState, id: &str, mut value: Value) -> String {
    state.seq += 1;
    let seq = state.seq;
    // Work out the next revision number, then store without `_rev`.
    let rev_number = state.docs.get(id).map_or(1, |d| d.rev_number + 1);
    if let Some(object) = value.as_object_mut() {
        object.remove("_rev");
        object.insert("_id".to_string(), json!(id));
    }
    state.docs.insert(
        id.to_string(),
        FakeDoc {
            rev_number,
            value,
            deleted: false,
        },
    );
    state.log.push((seq, id.to_string()));
    format!("{rev_number}-fake")
}

/// Answers one HTTP request on `stream`.
async fn handle(mut stream: TcpStream, state: Arc<Mutex<FakeState>>) -> std::io::Result<()> {
    // Step 1: read until the blank line that ends the headers.
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();

    // Step 2: read the request line and the headers we care about.
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default().to_string();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    let mut auth = String::new();
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("authorization:") {
            // Keep the original capitalisation of the value.
            auth = line["authorization:".len()..].trim().to_string();
        }
    }

    // Step 3: read the body, if there is one.
    while buffer.len() < header_end + content_length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..n]);
    }
    let body = &buffer[header_end..buffer.len().min(header_end + content_length)];

    // Step 4: decide the answer.
    let (status, reply) = {
        let mut state = state.lock().unwrap();
        state.requests += 1;
        if auth != state.expected_auth {
            (401, json!({"error": "unauthorized"}))
        } else {
            route(&mut state, &method, &target, body)
        }
    };

    // Step 5: send it and close.
    let text = reply.to_string();
    let response = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
        text.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// Works out the status and JSON reply for one authorised request.
fn route(state: &mut FakeState, method: &str, target: &str, body: &[u8]) -> (u16, Value) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

    // Anything for another database is "not found".
    if segments.first().copied() != Some(state.database.as_str()) {
        return (404, json!({"error": "not_found"}));
    }

    match (method, segments.get(1).copied()) {
        ("GET", None) => (200, json!({"db_name": state.database})),
        ("GET", Some("_changes")) => (200, changes(state, query)),
        ("POST", Some("_bulk_docs")) => (201, bulk(state, body)),
        _ => (400, json!({"error": "bad_request"})),
    }
}

/// The reply to `_changes`: documents changed after `since`, newest change per
/// document, oldest first, at most `limit`.
fn changes(state: &FakeState, query: &str) -> Value {
    // Read `since` (its leading digits) and `limit` from the query string.
    let mut since = 0u64;
    let mut limit = usize::MAX;
    for pair in query.split('&') {
        match pair.split_once('=') {
            Some(("since", value)) => {
                let digits: String = value.chars().take_while(|c| c.is_ascii_digit()).collect();
                since = digits.parse().unwrap_or(0);
            }
            Some(("limit", value)) => limit = value.parse().unwrap_or(usize::MAX),
            _ => {}
        }
    }

    // The latest change number for each document after `since`.
    let mut latest: HashMap<&str, u64> = HashMap::new();
    for (seq, id) in &state.log {
        if *seq > since {
            latest.insert(id.as_str(), *seq);
        }
    }
    let mut ordered: Vec<(&str, u64)> = latest.into_iter().collect();
    ordered.sort_by_key(|(_, seq)| *seq);
    ordered.truncate(limit);

    let last_seq = ordered.last().map_or(since, |(_, seq)| *seq);
    let results: Vec<Value> = ordered
        .iter()
        .map(|(id, seq)| {
            let doc = &state.docs[*id];
            let rev = format!("{}-fake", doc.rev_number);
            if doc.deleted {
                json!({"seq": format!("{seq}-fake"), "id": id, "deleted": true,
                       "changes": [{"rev": rev}],
                       "doc": {"_id": id, "_rev": rev, "_deleted": true}})
            } else {
                let mut value = doc.value.clone();
                value["_rev"] = json!(rev);
                json!({"seq": format!("{seq}-fake"), "id": id,
                       "changes": [{"rev": rev}], "doc": value})
            }
        })
        .collect();
    json!({"results": results, "last_seq": format!("{last_seq}-fake"), "pending": 0})
}

/// The reply to `_bulk_docs`: one answer per document, a conflict for any
/// whose `_rev` does not match the current one.
fn bulk(state: &mut FakeState, body: &[u8]) -> Value {
    // A planted write from "another device", if one is waiting.
    if let Some((id, value)) = state.race.take() {
        write_doc(state, &id, value);
    }

    let request: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let mut answers = Vec::new();
    for doc in request["docs"].as_array().cloned().unwrap_or_default() {
        let id = doc["_id"].as_str().unwrap_or_default().to_string();
        let given = doc["_rev"].as_str().map(str::to_string);
        let current = state.docs.get(&id);

        // The revision the server would accept: the current one for a live
        // document; none (or the current one) for a deleted or unknown one.
        let accepted = match current {
            Some(d) if !d.deleted => given.as_deref() == Some(&format!("{}-fake", d.rev_number)),
            Some(d) => {
                given.is_none() || given.as_deref() == Some(&format!("{}-fake", d.rev_number))
            }
            None => given.is_none(),
        };

        if accepted {
            let rev = write_doc(state, &id, doc);
            answers.push(json!({"ok": true, "id": id, "rev": rev}));
        } else {
            answers.push(json!({"id": id, "error": "conflict",
                                "reason": "Document update conflict."}));
        }
    }
    Value::Array(answers)
}

/// Standard base64 text for `bytes`, for building the expected login header.
/// (A few lines here keep the tests free of an extra library.)
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}
