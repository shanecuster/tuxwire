//! The CouchDB conversation: the few HTTP requests sync needs.
//!
//! CouchDB speaks plain HTTP and JSON, so this uses the `reqwest` client the
//! rest of tuxwire already has; no extra library is involved. Only three
//! requests are made: check the database is reachable, ask for changes since a
//! checkpoint, and write a batch of documents.
//!
//! Safety rules kept here:
//! - the password is sent only in the `Authorization` header, never in the web
//!   address, and never written to a log or an error message;
//! - errors are turned into short plain sentences (see [`describe`]) that name
//!   what is wrong without echoing the server address;
//! - replies are read with a size limit, so a broken server cannot make us read
//!   forever.

#![warn(clippy::missing_docs_in_private_items)]

use super::SyncConfig;
use super::doc::ArticleDoc;
use anyhow::{Context, anyhow};
use reqwest::{StatusCode, Url};
use serde::Deserialize;
use std::time::Duration;

/// How long to wait for a connection before deciding the server is
/// unreachable ("offline"). Short, so a missing network does not freeze the
/// app for long.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The longest a whole request, including reading the reply, may take.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The most bytes of one reply we will read: 32 MiB. A page of 200 saved
/// articles is a few hundred kilobytes; the limit only guards against a server
/// gone wrong.
const MAX_REPLY_BYTES: usize = 32 * 1024 * 1024;

/// A connection to one CouchDB database.
pub(crate) struct CouchClient {
    /// The HTTP client (certificates, timeouts, user agent).
    http: reqwest::Client,
    /// The database's address, always ending in `/` so paths join onto it.
    database_url: Url,
    /// The login name.
    username: String,
    /// The password. Private; used only to build the `Authorization` header.
    password: String,
}

/// One entry of CouchDB's list of changes.
#[derive(Debug, Deserialize)]
pub(crate) struct ChangeRow {
    /// The document's name.
    pub id: String,
    /// `true` if the document was deleted on the server (for example by hand
    /// in the CouchDB web page).
    #[serde(default)]
    pub deleted: bool,
    /// The document itself, as loose JSON. `None` for a deleted document.
    #[serde(default)]
    pub doc: Option<serde_json::Value>,
}

/// One page of changes.
#[derive(Debug)]
pub(crate) struct ChangesPage {
    /// The changed documents, oldest change first.
    pub rows: Vec<ChangeRow>,
    /// The checkpoint to resume from after this page. CouchDB 3 sends it as
    /// text and older versions as a number; it is kept as text either way.
    pub last_seq: String,
}

/// The raw shape of CouchDB's `_changes` reply.
#[derive(Debug, Deserialize)]
struct RawChanges {
    /// The page's rows.
    results: Vec<ChangeRow>,
    /// The checkpoint, text or number (hence loose JSON).
    last_seq: serde_json::Value,
}

/// CouchDB's answer for one document in a batch write.
#[derive(Debug, Deserialize)]
pub(crate) struct WriteResult {
    /// Which document this answer is about.
    pub id: String,
    /// The new revision label when the write worked.
    #[serde(default)]
    pub rev: Option<String>,
    /// An error code such as `"conflict"` when it did not.
    #[serde(default)]
    pub error: Option<String>,
}

impl CouchClient {
    /// Builds a client for `config`, which must already have passed
    /// [`SyncConfig::validate`] (that is where the address is parsed).
    pub(crate) fn new(config: &SyncConfig, base: Url) -> anyhow::Result<Self> {
        // Reuse the shared client settings (bundled certificates, user
        // agent), and add the two sync limits.
        let http = crate::fetchers::rss::client_builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("could not set up the network client")?;

        // `Url::join` replaces the last path segment unless the base ends in
        // `/`, so make sure it does before adding the database name.
        let mut base = base;
        if !base.path().ends_with('/') {
            let with_slash = format!("{}/", base.path());
            base.set_path(&with_slash);
        }
        let database_url = base
            .join(&format!("{}/", config.database))
            .context("the server address and database name do not make a valid address")?;

        Ok(CouchClient {
            http,
            database_url,
            username: config.username.clone(),
            password: config.password().to_string(),
        })
    }

    /// Checks that the database can be reached and these credentials may use
    /// it. Writes nothing.
    pub(crate) async fn check(&self) -> anyhow::Result<()> {
        let response = self.send(self.http.get(self.database_url.clone())).await?;
        read_reply(response).await.map(|_| ())
    }

    /// Asks for the changes since `since` (`None` means from the beginning),
    /// at most `limit` of them, with each document included.
    pub(crate) async fn changes(
        &self,
        since: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<ChangesPage> {
        // `query_pairs_mut` adds `?key=value&...` and handles the escaping.
        let mut url = self.database_url.join("_changes")?;
        url.query_pairs_mut()
            .append_pair("include_docs", "true")
            .append_pair("limit", &limit.to_string())
            .append_pair("since", since.unwrap_or("0"));

        let response = self.send(self.http.get(url)).await?;
        let bytes = read_reply(response).await?;
        let raw: RawChanges = serde_json::from_slice(&bytes)
            .context("the server's list of changes was not in the expected format")?;

        // Turn the checkpoint into text whichever form it arrived in.
        let last_seq = match raw.last_seq {
            serde_json::Value::String(text) => text,
            other => other.to_string(),
        };
        Ok(ChangesPage {
            rows: raw.results,
            last_seq,
        })
    }

    /// Writes a batch of documents in one request and returns CouchDB's
    /// answer for each. A document whose revision is out of date comes back
    /// with `error: "conflict"` rather than failing the whole batch.
    pub(crate) async fn write_batch(
        &self,
        docs: &[ArticleDoc],
    ) -> anyhow::Result<Vec<WriteResult>> {
        let url = self.database_url.join("_bulk_docs")?;
        let body = serde_json::to_string(&serde_json::json!({ "docs": docs }))?;
        let request = self
            .http
            .post(url)
            .header("Content-Type", "application/json")
            .body(body);

        let response = self.send(request).await?;
        let bytes = read_reply(response).await?;
        serde_json::from_slice(&bytes)
            .context("the server's reply to the write was not in the expected format")
    }

    /// Adds the login and sends the request, turning a network failure into a
    /// plain sentence.
    async fn send(&self, request: reqwest::RequestBuilder) -> anyhow::Result<reqwest::Response> {
        request
            .basic_auth(&self.username, Some(&self.password))
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|error| anyhow!(describe(&error)))
    }
}

/// Reads a reply body with the size limit, or explains what went wrong when
/// the status is not a success.
async fn read_reply(mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    let status = response.status();

    // Read the body in pieces, stopping if it grows past the limit.
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| anyhow!(describe(&error)))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_REPLY_BYTES {
            return Err(anyhow!("the server's reply was too large"));
        }
    }

    if status.is_success() {
        Ok(body)
    } else {
        Err(anyhow!(describe_status(status)))
    }
}

/// A short plain sentence for an HTTP status that is not a success. None of
/// them include the address or any credential.
fn describe_status(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED => "wrong username or password".to_string(),
        StatusCode::FORBIDDEN => {
            "this user is not allowed to use that database (check its members list)".to_string()
        }
        StatusCode::NOT_FOUND => {
            "database not found (tuxwire never creates databases; create it on the server first)"
                .to_string()
        }
        other => format!("the server answered HTTP {}", other.as_u16()),
    }
}

/// A short plain sentence for a network failure. Only fixed phrases are used,
/// never the error's own text, because that text contains the server address
/// and a private hostname must not be echoed into messages or logs.
fn describe(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "offline: timed out".to_string()
    } else if error.is_connect() {
        "offline: could not connect to the server".to_string()
    } else if error.is_redirect() {
        "the server redirected too many times".to_string()
    } else if error.is_body() || error.is_decode() {
        "the server's reply could not be read".to_string()
    } else {
        "network error while talking to the server".to_string()
    }
}
