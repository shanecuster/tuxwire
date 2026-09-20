//! Talking to a FreshRSS server's Google Reader-compatible API
//! (`docs/ARCHITECTURE.md` § FreshRSS Sync, "Protocol"). This is Phase 3:
//! login, the subscription list, unread item IDs, and item contents.
//!
//! ## What this module deliberately does *not* know
//!
//! Per the module boundary described in `freshrss/mod.rs`: nothing here
//! touches `Storage`, the TUI, or the filesystem. `Client::new` takes
//! `url`/`username`/`password` as plain arguments — it never reads
//! `freshrss.toml` itself, that's `freshrss::config`'s job. Turning the raw
//! types below (`Subscription`, `Item`) into `Article` values is Phase 4's
//! `freshrss::mapping`, not this file.
//!
//! ## Why the HTTP client is a private module-level static, not a `Client`
//! field
//!
//! This mirrors `fetchers::rss::HTTP_CLIENT` exactly (see that file's own
//! comment for the full reasoning) rather than reusing it directly —
//! `src/freshrss/` stays self-contained and doesn't reach into a sibling
//! module, the same rule `freshrss::config`'s `expand_tilde` already
//! follows instead of calling `crate::export`'s version. `LazyLock` builds
//! the `reqwest::Client` exactly once, the first time any `Client` method
//! runs, and every `FreshRssClient` (there's usually only ever one, built
//! once per sync) shares that same underlying connection pool and TLS
//! setup.
use crate::freshrss::config::Password;
use anyhow::{Context, anyhow, bail};
use serde::Deserialize;
use std::sync::LazyLock;
use std::time::Duration;

/// How long any single request to the FreshRSS server is allowed to take
/// before `reqwest` gives up and returns a timeout error, instead of
/// hanging forever. Set once here, on the shared `HTTP_CLIENT` builder
/// below, which is what makes it apply to *every* request this module
/// makes without each call site having to remember to set it — per
/// ARCHITECTURE.md's Security rules, "All requests have explicit
/// timeouts."
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// How many item IDs to ask for per `stream/items/ids` page. This is a
/// request *hint*, not a hard promise from the server — see
/// `unread_item_ids` below for how the loop actually decides when it's
/// out of pages, which does not rely on getting exactly this many back.
const ITEMS_PER_PAGE: u32 = 1000;

/// A hard ceiling on how many `stream/items/ids` pages `unread_item_ids`
/// will ever request in one call, regardless of what the server sends
/// back. Verified against a live test server that a response can carry a
/// `continuation` value right up to (and including) the last page that
/// still has real items on it — see ARCHITECTURE.md's "Verified against a
/// live test server" note — so a client that only stops on "continuation
/// missing" is trusting the server to eventually say so. This constant is
/// the fallback that keeps a bug, or a FreshRSS instance that never
/// terminates the stream, from turning into an infinite loop: at
/// `ITEMS_PER_PAGE` × `MAX_ID_PAGES` = 50,000 IDs, `unread_item_ids` stops
/// and returns whatever it collected with `UnreadItemIds::truncated` set,
/// instead of hanging the app or failing the whole sync outright.
const MAX_ID_PAGES: usize = 50;

/// How many item IDs go into a single `stream/items/contents` request.
/// Verified that a batch of 20 works fine against a live server; this is
/// picked well under that as a conservative default for however many
/// thousands of IDs a real sync might have to work through (see the
/// Phase 3 report for the "first sync with thousands of unread items"
/// discussion) — one request per ID would be thousands of round trips,
/// and one request for everything risks an oversized POST body and a
/// slow, all-or-nothing response. Chunking keeps each request small and
/// bounded regardless of how large the unread list gets.
const CONTENTS_CHUNK_SIZE: usize = 100;

/// How many ids `sync::run`'s push step batches into a single
/// `edit-tag` call. `pub(crate)`, not private -- unlike
/// `CONTENTS_CHUNK_SIZE`, the chunking loop this sizes lives in
/// `freshrss::sync`, not in this module (see `Client::mark_read`'s doc
/// comment for exactly why). Verified a batch of 2 works against a live
/// server; picked the same value as `CONTENTS_CHUNK_SIZE` for
/// consistency rather than because 100 specifically was tested against
/// `edit-tag` -- both endpoints take the same repeated-`i=` form shape,
/// so there's no reason to expect a meaningfully different practical
/// limit between them.
pub(crate) const MARK_READ_CHUNK_SIZE: usize = 100;

/// The `reqwest::Client` every request in this module shares. See the
/// module doc comment above for why this exists instead of reusing
/// `fetchers::rss::HTTP_CLIENT`, and `fetchers::rss::HTTP_CLIENT`'s own
/// comment for why `.tls_certs_only(...)` is used instead of
/// `reqwest::Client::new()` (in short: it avoids a platform-verifier panic
/// on Android/Termux by handing `reqwest` a bundled root cert list
/// instead of asking the OS for one).
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    let root_certs = webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|der| {
            reqwest::Certificate::from_der(der.as_ref())
                .expect("webpki-root-certs' bundled certs are always valid DER")
        })
        .collect::<Vec<_>>();

    reqwest::Client::builder()
        .tls_certs_only(root_certs)
        // `.timeout(...)` here applies to every request this `Client`
        // ever sends -- there's no separate per-call `.timeout(...)` to
        // forget to add later.
        .timeout(REQUEST_TIMEOUT)
        .build()
        .expect("building the shared FreshRSS HTTP client should never fail")
});

/// The token `login` returns on success, sent back on every later request
/// as an `Authorization: GoogleLogin auth=<token>` header.
///
/// A bare `String` would work exactly the same way at the type-checker
/// level -- Rust doesn't distinguish "a string that happens to be a
/// secret" from any other string. Wrapping it in its own type is what
/// makes the hand-written `Debug` impl below possible: without a distinct
/// type to attach it to, there'd be nothing stopping some future
/// `eprintln!("{auth:?})")` (debugging something unrelated) from printing
/// the real token to a log file. Same reasoning as `freshrss::config::Password`.
pub struct AuthToken(String);

impl AuthToken {
    /// Builds the exact `Authorization` header value the Google
    /// Reader-compatible API expects. Private (no `pub`) -- the token
    /// text should never need to leave this module in any other shape.
    fn header_value(&self) -> String {
        format!("GoogleLogin auth={}", self.0)
    }
}

impl std::fmt::Debug for AuthToken {
    /// Hand-written for the same reason as `Password`'s: a
    /// `#[derive(Debug)]` here would print the real token the moment
    /// anything holding an `AuthToken` gets formatted with `{:?}`.
    /// ARCHITECTURE.md's Security rules are explicit that auth tokens are
    /// never printed by `Debug`, same as the password itself.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AuthToken(<redacted>)")
    }
}

/// The write token `token` (below) returns -- a second, separate secret
/// from `AuthToken`, required on every request that *changes* something
/// server-side (`edit-tag`, `subscription/quickadd`) as basic protection
/// against cross-site request forgery. Its own type, not a reused
/// `AuthToken`, because the two are genuinely different credentials with
/// different lifetimes (ARCHITECTURE.md: "Tokens live in memory only; log
/// in again per refresh session" applies to both, but they're fetched by
/// two different calls and are never interchangeable) -- keeping them as
/// distinct types is what makes it a compile error to accidentally pass
/// one where the other belongs, rather than a subtle runtime bug.
pub struct WriteToken(String);

impl WriteToken {
    /// The raw token text, exactly as `edit-tag`'s `T=` form field
    /// expects it (no `GoogleLogin auth=`-style wrapping, unlike
    /// `AuthToken::header_value` -- the write token travels in the
    /// request *body*, not a header).
    fn as_form_value(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for WriteToken {
    /// Same reasoning as `AuthToken`'s hand-written `Debug` -- this is a
    /// credential too, and ARCHITECTURE.md's Security rules don't carve
    /// out an exception for "the write token specifically."
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WriteToken(<redacted>)")
    }
}

/// One subscribed FreshRSS feed, as `subscription/list` describes it.
/// `pub` because Phase 4's `freshrss::mapping` needs to read these fields
/// to build `Article`/source names -- this module only fetches and
/// parses, it doesn't decide what a `Subscription` becomes.
#[derive(Debug, Deserialize)]
pub struct Subscription {
    /// FreshRSS's own feed identifier, e.g. `"feed/2"` -- stable across
    /// syncs, used later as `freshrss_items.feed_id`.
    pub id: String,
    /// The feed's human-readable name (e.g. "Hacker News: Newest"),
    /// mapped onto `Article.source` in Phase 4.
    pub title: String,
    /// Which FreshRSS categories this feed belongs to. A `Vec` because
    /// the API allows a feed to sit in more than one category, even
    /// though ARCHITECTURE.md's mapping (one FreshRSS category → one
    /// `FreshRSS/<category>` topic) only ever uses the first one.
    #[serde(default)]
    pub categories: Vec<Category>,
    /// The feed's own URL (what tuxwire's own `sources.toml` would call
    /// `url`) -- e.g. `"https://hnrss.org/newest"`.
    pub url: String,
    /// The feed's *site* URL (its homepage, not the feed XML itself) --
    /// e.g. `"https://news.ycombinator.com/newest"`. `#[serde(rename =
    /// "htmlUrl")]` is needed because the JSON key is camelCase
    /// (`htmlUrl`) but this codebase's Rust field naming convention is
    /// snake_case (`html_url`) -- `rename` tells serde "the wire name and
    /// the Rust name are allowed to differ, here's the mapping."
    #[serde(rename = "htmlUrl")]
    pub html_url: String,
}

/// One category a `Subscription` belongs to.
#[derive(Debug, Deserialize)]
pub struct Category {
    /// FreshRSS's internal category ID, e.g.
    /// `"user/-/label/Uncategorized"`.
    pub id: String,
    /// The human-readable category name, e.g. `"Uncategorized"` -- this
    /// is what Phase 4 namespaces into `FreshRSS/Uncategorized`.
    pub label: String,
}

/// The raw shape of a `subscription/list?output=json` response body --
/// `subscriptions` is the only field this API returns, so there's nothing
/// else to model. Not `pub`: callers only ever see the `Vec<Subscription>`
/// `subscriptions` (the method) hands back, never this wrapper.
#[derive(Deserialize)]
struct SubscriptionListResponse {
    /// Every feed the account is subscribed to.
    subscriptions: Vec<Subscription>,
}

/// One item's ID as `stream/items/ids` reports it, before normalization.
/// Not `pub` -- `unread_item_ids` immediately unwraps and normalizes
/// every one of these into a plain `String`, so this shape never needs to
/// escape this module.
#[derive(Deserialize)]
struct ItemRef {
    /// The raw ID text, in whichever shape this particular endpoint
    /// happened to use -- not yet run through `normalize_item_id`.
    id: String,
}

/// The raw shape of a `stream/items/ids?output=json` response body.
#[derive(Deserialize)]
struct ItemRefsPage {
    /// This page's item IDs, in server-given order (verified: newest
    /// first). Empty exactly when there are no more unread items left to
    /// page through -- see `Client::unread_item_ids`'s doc comment.
    #[serde(rename = "itemRefs")]
    item_refs: Vec<ItemRef>,
    /// `Option` because the field is genuinely sometimes absent (see
    /// `unread_item_ids`'s doc comment for exactly when) -- not a
    /// sentinel like an empty string, which would leave "no more pages"
    /// and "the empty string happens to be a valid continuation token"
    /// ambiguous. `#[serde(default)]` means a JSON object that omits this
    /// key entirely deserializes to `None`, rather than serde treating a
    /// missing field as a parse error.
    #[serde(default)]
    continuation: Option<String>,
}

/// What `Client::unread_item_ids` hands back: every id it managed to
/// collect, and whether it stopped early because `MAX_ID_PAGES` was hit
/// rather than because the server actually ran out of items. Not a
/// `Result`/error for the truncated case -- see `unread_item_ids`'s doc
/// comment for why a truncated set is still useful data, not a failure.
#[derive(Debug)]
pub struct UnreadItemIds {
    /// Every id collected before this call stopped, newest first.
    pub ids: Vec<String>,
    /// `true` if pagination stopped because `MAX_ID_PAGES` was reached,
    /// not because the server signaled an empty final page. A caller
    /// (`freshrss::sync`) must treat `ids` as an incomplete view of the
    /// server's real unread set when this is `true` -- see
    /// `unread_item_ids`'s doc comment.
    pub truncated: bool,
}

/// One synced article's full content, as `stream/items/contents` returns
/// it. `pub` for the same reason as `Subscription` -- Phase 4 reads these
/// fields to build an `Article`.
#[derive(Debug, Deserialize)]
pub struct Item {
    /// The item's ID, **already normalized** by the time a caller of
    /// `Client::item_contents` sees it -- see `normalize_item_id` below.
    /// The raw JSON value here is the long `tag:google.com,2005:reader/item/<hex>`
    /// form; this module converts it before this `Item` is ever handed
    /// back, so every other part of the app only ever sees the plain
    /// decimal form `stream/items/ids` already uses.
    pub id: String,
    /// The article's headline, maps onto `Article.title`.
    pub title: String,
    /// Unix timestamp (seconds), maps onto `Article.timestamp`.
    pub published: i64,
    /// The article's own URL. A `Vec` because the API technically allows
    /// more than one, but real responses have exactly zero or one --
    /// `#[serde(default)]` means a response that omits `canonical`
    /// entirely becomes an empty `Vec` instead of a parse error.
    #[serde(default)]
    pub canonical: Vec<Href>,
    /// An alternate link (occasionally an AMP or syndicated copy of the
    /// same article) -- Phase 4's mapping falls back to this if
    /// `canonical` is empty.
    #[serde(default)]
    pub alternate: Vec<Href>,
    /// Which feed this item came from, and that feed's own display
    /// details -- see `Origin` below.
    pub origin: Origin,
    /// The item's body/description as HTML. `Option` because a feed
    /// entry can genuinely have no description at all, not just an empty
    /// one.
    pub summary: Option<Summary>,
    /// The article's byline, when the feed provides one. Also `Option`
    /// for the same reason as `summary`.
    pub author: Option<String>,
}

/// A single `{"href": "..."}` link object -- `canonical` and `alternate`
/// on `Item` are both arrays of this same tiny shape.
#[derive(Debug, Deserialize)]
pub struct Href {
    /// The link's actual URL.
    pub href: String,
}

/// Which feed an `Item` came from, embedded directly in every item so a
/// batch of items from several different feeds can each be attributed
/// correctly without a second lookup.
#[derive(Debug, Deserialize)]
pub struct Origin {
    /// The feed's ID, e.g. `"feed/2"` -- matches `Subscription::id`, and
    /// becomes `freshrss_items.feed_id`.
    #[serde(rename = "streamId")]
    pub stream_id: String,
    /// The feed's site URL, matching `Subscription::html_url`.
    #[serde(rename = "htmlUrl")]
    pub html_url: String,
    /// The feed's display name, matching `Subscription::title`.
    pub title: String,
}

/// An item's body, wrapped in its own struct because the JSON wraps it
/// the same way (`{"summary": {"content": "..."}}`, not a bare string).
#[derive(Debug, Deserialize)]
pub struct Summary {
    /// The article body/description as raw HTML, e.g.
    /// `"<p>Article URL: ...</p>"` -- not sanitized or stripped here,
    /// that's Phase 4/the TUI's concern, not this client's.
    pub content: String,
}

/// The raw shape of a `stream/items/contents` response body. Not `pub`
/// for the same reason as `SubscriptionListResponse`.
#[derive(Deserialize)]
struct ContentsResponse {
    /// The requested batch's items, in no particular guaranteed order.
    items: Vec<Item>,
}

/// A connection to one FreshRSS server, holding just enough to log in and
/// make authenticated requests. Deliberately dumb: it doesn't cache a
/// token across calls (see `AuthToken`'s doc comment on why tokens are
/// passed in explicitly instead), doesn't retry, and doesn't know
/// anything about `Storage` -- Phase 5's `freshrss::sync` is what owns a
/// `Client`, calls `login` once per sync, and threads the resulting
/// `AuthToken` through the other calls.
///
/// `#[derive(Debug)]` is safe here specifically *because* `password` is a
/// `Password`, which has its own redacting `Debug` impl -- printing a
/// whole `Client` with `{:client?}` shows the real `url`/`username` (not
/// secret) and `Password(<redacted>)`, never the real password text.
#[derive(Debug)]
pub struct Client {
    /// The server's base URL, e.g. `"https://freshrss.example.com"` --
    /// exactly what `freshrss.toml`'s `url` field holds, no
    /// `/api/greader.php` suffix. `api_url` below is what appends that.
    url: String,
    /// The FreshRSS account name -- exactly `freshrss.toml`'s `username`.
    username: String,
    /// The FreshRSS API password (separate from the account's login
    /// password). See `Password`'s own doc comment for why this is never
    /// a bare `String`.
    password: Password,
}

impl Client {
    /// Builds a `Client` for one FreshRSS server. Takes ownership of
    /// `password` (a `Password`, not a `&str`) so the caller can't
    /// accidentally keep a second copy of the secret sitting around after
    /// handing it off -- once it's moved in here, this `Client` is the
    /// only thing holding it (aside from whatever `Password` was cloned
    /// from, if anything was; in practice `freshrss::config::load` builds
    /// exactly one and this is where it ends up).
    pub fn new(url: String, username: String, password: Password) -> Self {
        Self { url, username, password }
    }

    /// Builds a full request URL by joining this client's base `url` with
    /// `/api/greader.php` and whatever path-plus-query-string the caller
    /// wants. `.trim_end_matches('/')` guards against `freshrss.toml`
    /// having a trailing slash on `url` (e.g.
    /// `"https://freshrss.example.com/"`) producing a doubled `//` in the
    /// final URL -- a small normalization so every other method here can
    /// just pass a plain `"/reader/api/0/..."` path without worrying
    /// about it.
    fn api_url(&self, path_and_query: &str) -> String {
        format!("{}/api/greader.php{path_and_query}", self.url.trim_end_matches('/'))
    }

    /// Logs in and returns the token every later request needs.
    ///
    /// `async fn` means calling `login()` doesn't run any of this
    /// immediately -- it returns a `Future` that does nothing until
    /// something `.await`s it (see `fetchers::rss::fetch_feed`'s doc
    /// comment for the same idea in more depth). The caller controls
    /// *when* the actual network request happens, which is what lets
    /// tokio run this concurrently with other work instead of blocking
    /// the whole program on one HTTP round trip.
    pub async fn login(&self) -> anyhow::Result<AuthToken> {
        // `.form(&[...])` sends this as a normal HTML form POST body
        // (`Email=...&Passwd=...`, URL-encoded), which is what
        // `accounts/ClientLogin` expects -- not JSON. The password is
        // read via `expose_secret()` right here, at the one place it
        // actually needs to leave the `Password` wrapper to go out on the
        // wire, and nowhere else in this function.
        let response = HTTP_CLIENT
            .post(self.api_url("/accounts/ClientLogin"))
            .form(&[("Email", self.username.as_str()), ("Passwd", self.password.expose_secret())])
            .send()
            .await
            // `.with_context` only ever names `self.url` (never the
            // request body) -- see the module doc comment: nothing here
            // ever interpolates a secret into an error.
            .with_context(|| format!("failed to reach FreshRSS login endpoint at {}", self.url))?;

        // A non-2xx status (verified: a bad password comes back as plain
        // `401 Unauthorized` with a `Google-Bad-Token: true` header) is a
        // real failure. The response body is deliberately *not* read or
        // included in this error -- today it's just the harmless text
        // "Unauthorized!", but nothing guarantees a future FreshRSS
        // version won't echo something more sensitive back on failure,
        // and there's no reason to take that risk for an error message.
        if !response.status().is_success() {
            bail!(
                "FreshRSS login to {} failed with HTTP {} -- check the configured username and password",
                self.url,
                response.status()
            );
        }

        let body = response.text().await.with_context(|| "failed to read FreshRSS login response body")?;

        parse_login_response(&body)
    }

    /// Fetches the list of feeds the account is subscribed to.
    pub async fn subscriptions(&self, auth: &AuthToken) -> anyhow::Result<Vec<Subscription>> {
        let body = self
            .authorized_get(auth, "/reader/api/0/subscription/list?output=json")
            .await
            .with_context(|| "failed to fetch FreshRSS subscription list")?;

        parse_subscriptions(&body)
    }

    /// Fetches every unread item's ID across all subscribed feeds,
    /// following FreshRSS's `c=`/`continuation` pagination until the
    /// server reports it's out of items -- newest item first, oldest
    /// last (verified against a live server: item IDs are assigned in
    /// increasing crawl order, and `stream/items/ids` returns them
    /// largest-ID-first).
    ///
    /// ## Why this isn't just "loop until `continuation` is missing"
    ///
    /// Verified against a live server (see ARCHITECTURE.md's "Verified
    /// against a live test server" note) that a page can come back both
    /// full of real items *and* carrying a `continuation` value, right up
    /// through the actual last page of results -- the only response that
    /// reliably means "there is nothing more" is one with an *empty*
    /// `itemRefs` array. So this loop's real stopping condition is
    /// "empty page", and it treats a missing `continuation` on a
    /// non-empty page as *also* meaning "stop after this one" (belt and
    /// suspenders -- if a server ever does omit `continuation` on the
    /// true last page, this still terminates correctly instead of making
    /// one pointless extra request).
    ///
    /// ## The hard cap doesn't fail the sync
    ///
    /// `for _ in 0..MAX_ID_PAGES` bounds the loop unconditionally: even if
    /// every single response somehow kept sending a `continuation`
    /// forever (a server bug, or two servers disagreeing about the
    /// protocol), this function gives up after `MAX_ID_PAGES` requests
    /// rather than hanging the app. Unlike an earlier version of this
    /// function, hitting the cap is **not** treated as an error -- it
    /// returns everything collected so far with `truncated: true` instead
    /// of a `bail!`. A caller (`freshrss::sync`) that got a full,
    /// untruncated set can safely reconcile "this item is no longer in
    /// the unread set, so it must have been read elsewhere" (Phase 5); a
    /// caller that got a truncated set cannot draw that conclusion at
    /// all, since a truncated set is missing an unknown number of ids
    /// that are still genuinely unread -- but there's no reason to fail
    /// the *whole* sync (new items, read-state pushes) just because that
    /// one downstream reconcile step has to be skipped for this round.
    pub async fn unread_item_ids(&self, auth: &AuthToken) -> anyhow::Result<UnreadItemIds> {
        let mut ids = Vec::new();
        let mut continuation: Option<String> = None;

        for _page in 0..MAX_ID_PAGES {
            // Build this page's query string. `c=` is only appended once
            // a previous page has handed back a continuation token to
            // resume from -- the very first request has none.
            let mut path = format!(
                "/reader/api/0/stream/items/ids?output=json\
                 &s=user/-/state/com.google/reading-list\
                 &xt=user/-/state/com.google/read\
                 &n={ITEMS_PER_PAGE}"
            );
            if let Some(token) = &continuation {
                path.push_str("&c=");
                path.push_str(token);
            }

            let body = self.authorized_get(auth, &path).await.with_context(|| "failed to fetch FreshRSS unread item ids")?;
            let page = parse_item_refs_page(&body)?;

            // An empty page is the one unambiguous "done" signal -- see
            // the doc comment above.
            if page.item_refs.is_empty() {
                return Ok(UnreadItemIds { ids, truncated: false });
            }

            // Normalize every ID right here, at the one place raw
            // `ItemRef`s get turned into the plain `String`s this
            // function returns -- `normalize_item_id` is the single
            // function that understands both ID shapes FreshRSS uses (see
            // its own doc comment), so nothing downstream of this loop
            // ever has to think about which one it's looking at.
            for item_ref in page.item_refs {
                ids.push(normalize_item_id(&item_ref.id)?);
            }

            match page.continuation {
                Some(token) => continuation = Some(token),
                // No continuation on a non-empty page: treat it the same
                // as an empty next page would have -- see the doc comment
                // above for why this belt-and-suspenders check exists.
                None => return Ok(UnreadItemIds { ids, truncated: false }),
            }
        }

        // Every iteration above returns early the moment the server signals
        // "done" -- reaching here means it never did, across MAX_ID_PAGES
        // requests. `truncated: true` is this function's way of telling
        // the caller "here's everything I got, but I stopped before the
        // server said to" -- see the doc comment above for why that's not
        // the same as failing outright.
        Ok(UnreadItemIds { ids, truncated: true })
    }

    /// Fetches full item contents (title, URL, body, feed of origin...)
    /// for every ID in `ids`, in batches of `CONTENTS_CHUNK_SIZE` rather
    /// than one request per ID or one request for all of them -- see
    /// `CONTENTS_CHUNK_SIZE`'s doc comment for why both of those extremes
    /// are worse.
    pub async fn item_contents(&self, auth: &AuthToken, ids: &[String]) -> anyhow::Result<Vec<Item>> {
        let mut items = Vec::with_capacity(ids.len());

        // `.chunks(CONTENTS_CHUNK_SIZE)` is a slice method that yields
        // successive sub-slices of at most that length -- e.g. 250 ids
        // sliced into chunks of 100 yields three chunks (100, 100, 50).
        // No allocation of a new `Vec` per chunk; each `chunk` below just
        // borrows a piece of `ids`.
        for chunk in ids.chunks(CONTENTS_CHUNK_SIZE) {
            // `stream/items/contents` expects one repeated `i=<id>` form
            // field per item, not a JSON array -- `.form(&form)` with a
            // `Vec` of `("i", id)` pairs is how `reqwest` produces
            // `i=1&i=2&i=3` instead of one `i=1,2,3`.
            let form: Vec<(&str, &str)> = chunk.iter().map(|id| ("i", id.as_str())).collect();

            let response = HTTP_CLIENT
                .post(self.api_url("/reader/api/0/stream/items/contents?output=json"))
                .header("Authorization", auth.header_value())
                .form(&form)
                .send()
                .await
                .with_context(|| "failed to reach FreshRSS stream/items/contents")?;

            if !response.status().is_success() {
                bail!("FreshRSS stream/items/contents failed with HTTP {}", response.status());
            }

            let body =
                response.text().await.with_context(|| "failed to read FreshRSS item contents response body")?;

            // `.extend(...)` appends every element of the parsed chunk's
            // `Vec<Item>` onto `items` in place, rather than building a
            // `Vec<Vec<Item>>` of per-chunk results and flattening it
            // afterwards.
            items.extend(parse_item_contents(&body)?);
        }

        Ok(items)
    }

    /// Fetches a write token -- required on `edit-tag` and
    /// `subscription/quickadd` as basic CSRF protection for any request
    /// that changes server state (verified against a live server that
    /// `quickadd` doesn't actually *enforce* this the way `edit-tag`
    /// does -- a garbage token still succeeded -- but a real token is
    /// sent regardless, since that's the documented contract and a
    /// different FreshRSS version may enforce it). Plain-text body, the
    /// whole response *is* the token (no `Key=Value` parsing the way
    /// `login`'s response needs).
    pub async fn token(&self, auth: &AuthToken) -> anyhow::Result<WriteToken> {
        let body =
            self.authorized_get(auth, "/reader/api/0/token").await.with_context(|| "failed to fetch FreshRSS write token")?;
        Ok(WriteToken(body))
    }

    /// Marks every id in `remote_ids` read on the server, in **one**
    /// `edit-tag` request -- unlike `item_contents`, this does *not*
    /// chunk internally. That's deliberate: `freshrss::sync`'s push step
    /// needs to know exactly which chunk of ids actually succeeded before
    /// it's safe to clear their local `pending_read_push` flags (per
    /// ARCHITECTURE.md's push step: "clear the flag only after the server
    /// confirms"), and it can only know that at the granularity it
    /// controls its own chunking at -- if this function silently
    /// chunked a 500-id call into five requests and the third one failed,
    /// the caller would have no way to tell which 100 of the 500 actually
    /// went through. So the caller (`sync::run`) owns the `.chunks(...)`
    /// loop and calls this once per chunk; `MARK_READ_CHUNK_SIZE` is
    /// still defined here (client-protocol sizing belongs with the rest
    /// of `client`'s constants) for `sync` to chunk by.
    ///
    /// Verified against a live server that a genuine success is `HTTP
    /// 200` with a body of exactly `"OK"` -- checked here explicitly
    /// (not just the status code), since nothing about a 2xx status alone
    /// guarantees the server actually understood and applied the tag
    /// edit rather than, say, silently ignoring a malformed `a=` value.
    pub async fn mark_read(&self, auth: &AuthToken, write_token: &WriteToken, remote_ids: &[String]) -> anyhow::Result<()> {
        let mut form: Vec<(&str, &str)> = remote_ids.iter().map(|id| ("i", id.as_str())).collect();
        form.push(("a", "user/-/state/com.google/read"));
        form.push(("T", write_token.as_form_value()));

        let response = HTTP_CLIENT
            .post(self.api_url("/reader/api/0/edit-tag"))
            .header("Authorization", auth.header_value())
            .form(&form)
            .send()
            .await
            .with_context(|| "failed to reach FreshRSS edit-tag")?;

        if !response.status().is_success() {
            bail!("FreshRSS edit-tag failed with HTTP {}", response.status());
        }

        let body = response.text().await.with_context(|| "failed to read FreshRSS edit-tag response body")?;

        parse_mark_read_response(&body)
    }

    /// Subscribes to `feed_url` (`docs/ARCHITECTURE.md`'s "Adding feeds,"
    /// Phase 6). Unlike every other method here, a *declined* feed isn't
    /// a `client`-level failure at all -- the HTTP request itself
    /// succeeds (verified against a live server: every case, success or
    /// rejection, comes back `HTTP 200`), and FreshRSS's own JSON body is
    /// what actually says which one happened. That's why this returns a
    /// nested `Result`: the outer `anyhow::Result` is for genuine
    /// transport failures (network unreachable, a non-200 status, a body
    /// that isn't even valid JSON) via `?`, same as every other method;
    /// the inner `Result<QuickaddResult, String>` is FreshRSS's own
    /// verdict on the URL itself, which a caller has to handle either way
    /// -- there's no version of "the request worked" that also means
    /// "and the feed was actually added," so collapsing this into one
    /// flat `Result` would make it too easy for a caller to `?` straight
    /// past a plain rejection (an unreachable domain, an
    /// already-subscribed feed, a private/loopback URL FreshRSS's own
    /// SSRF check declined) as if it were a bug instead of an expected
    /// outcome.
    pub async fn quickadd(
        &self,
        auth: &AuthToken,
        write_token: &WriteToken,
        feed_url: &str,
    ) -> anyhow::Result<Result<QuickaddResult, String>> {
        let response = HTTP_CLIENT
            .post(self.api_url("/reader/api/0/subscription/quickadd"))
            .header("Authorization", auth.header_value())
            .form(&[("quickadd", feed_url), ("T", write_token.as_form_value())])
            .send()
            .await
            .with_context(|| "failed to reach FreshRSS subscription/quickadd")?;

        if !response.status().is_success() {
            bail!("FreshRSS subscription/quickadd failed with HTTP {}", response.status());
        }

        let body =
            response.text().await.with_context(|| "failed to read FreshRSS subscription/quickadd response body")?;

        parse_quickadd_response(&body)
    }

    /// Shared plumbing for every simple `GET`-and-return-the-body call
    /// (`subscriptions`, each page inside `unread_item_ids`, and `token`)
    /// -- `item_contents` and `mark_read` don't use this since they're
    /// both `POST`s with a form body instead of a bare authenticated
    /// `GET`.
    async fn authorized_get(&self, auth: &AuthToken, path_and_query: &str) -> anyhow::Result<String> {
        let response = HTTP_CLIENT
            .get(self.api_url(path_and_query))
            .header("Authorization", auth.header_value())
            .send()
            .await
            .with_context(|| format!("failed to reach FreshRSS at {}", self.url))?;

        if !response.status().is_success() {
            bail!("FreshRSS request to {} failed with HTTP {}", self.url, response.status());
        }

        response.text().await.with_context(|| "failed to read FreshRSS response body")
    }
}

/// Checks whether an `edit-tag` response body actually confirms success --
/// verified against a live server that a genuine success is the literal
/// text `"OK"` (`.trim()`ed first, in case a trailing newline sneaks in).
/// Pulled out of `Client::mark_read` for the same fixture-testability
/// reason as `parse_login_response` below: this is the one place that
/// decides "did the server really confirm this," and it should be
/// checkable against a plain string without a real HTTP round trip.
fn parse_mark_read_response(body: &str) -> anyhow::Result<()> {
    if body.trim() != "OK" {
        bail!("FreshRSS edit-tag did not confirm success (response body was {body:?})");
    }
    Ok(())
}

/// What `Client::quickadd` returns on a genuine success -- just enough to
/// confirm what happened and name the feed; nothing here is stored
/// anywhere (unlike `subscription/list`, `quickadd` doesn't return
/// category/topic info, since it never assigns one -- see
/// ARCHITECTURE.md's "Adding feeds" for the `FreshRSS/Uncategorized`
/// consequence of that).
#[derive(Debug)]
pub struct QuickaddResult {
    /// The new feed's FreshRSS id, e.g. `"feed/8"`.
    pub stream_id: String,
    /// The feed's own title, as FreshRSS guessed it from the feed itself
    /// -- `quickadd` never takes a name as input, this is purely
    /// informational (e.g. for a confirmation message), not something
    /// `freshrss::sync` needs to store.
    pub stream_name: String,
}

/// The raw shape of a `subscription/quickadd` response body -- verified
/// against a live server for four real cases, summarized in
/// `parse_quickadd_response`'s own doc comment below. Every field but
/// `num_results` is optional because which of them are actually present
/// depends entirely on whether `num_results` was `1` or `0` -- serde
/// can't express "these three fields are mutually exclusive with those
/// two" any more precisely than "everything but the one field always
/// present is `Option`," so `parse_quickadd_response` is what actually
/// enforces the real shape.
#[derive(Deserialize)]
struct QuickaddResponse {
    /// `1` (or, in principle, more) on success, `0` on any rejection --
    /// the one field always present, and the one `parse_quickadd_response`
    /// actually branches on.
    #[serde(rename = "numResults")]
    num_results: u32,
    /// The new feed's FreshRSS id, e.g. `"feed/8"` -- present on success,
    /// absent on rejection.
    #[serde(rename = "streamId")]
    stream_id: Option<String>,
    /// The feed's own title, as FreshRSS guessed it -- present on
    /// success, absent on rejection.
    #[serde(rename = "streamName")]
    stream_name: Option<String>,
    /// FreshRSS's own explanation for a rejection (e.g. `"Already
    /// subscribed! <url>"`) -- present on rejection, absent on success.
    error: Option<String>,
}

/// Parses a `subscription/quickadd` response body into FreshRSS's own
/// verdict on the feed URL -- verified against a live server, every case
/// below is a real response, not a guess:
///
/// - Success: `{"numResults":1,"query":"...","streamId":"feed/8","streamName":"..."}`
/// - Already subscribed: `{"numResults":0,"error":"Already subscribed! <url>"}`
/// - Rejected (private/loopback -- FreshRSS's own SSRF protection):
///   `{"numResults":0,"error":"URL is not allowed to be resolved: <url> [<url>]"}`
/// - Unresolvable domain: `{"numResults":0,"error":"Failed to resolve domain: <url> [<url>]"}`
///
/// `num_results >= 1` is treated as success (not `== 1` specifically) --
/// nothing in the verified responses ever showed a value above `1`, but
/// "at least one" is the more defensible reading of a field literally
/// named `numResults` than "exactly one," and costs nothing extra to
/// check for.
fn parse_quickadd_response(body: &str) -> anyhow::Result<Result<QuickaddResult, String>> {
    let parsed: QuickaddResponse =
        serde_json::from_str(body).with_context(|| "failed to parse FreshRSS subscription/quickadd response")?;

    if parsed.num_results >= 1 {
        // A genuine success without `streamId`/`streamName` present would
        // mean FreshRSS's own response shape changed since this was
        // verified -- a real problem worth a clear error, not something
        // to paper over with `.unwrap_or_default()`.
        let stream_id = parsed
            .stream_id
            .ok_or_else(|| anyhow!("FreshRSS quickadd reported success but the response had no streamId"))?;
        let stream_name = parsed
            .stream_name
            .ok_or_else(|| anyhow!("FreshRSS quickadd reported success but the response had no streamName"))?;
        return Ok(Ok(QuickaddResult { stream_id, stream_name }));
    }

    Ok(Err(parsed.error.unwrap_or_else(|| "FreshRSS declined this feed (no reason given)".to_string())))
}

/// Parses a `ClientLogin` success body (plain text, three `Key=Value`
/// lines: `SID=`, `LSID=`, `Auth=`, in no guaranteed order -- verified
/// against a live server) into an `AuthToken`.
///
/// Pulled out of `Client::login` as its own free function (taking a plain
/// `&str` instead of a whole `Client`) specifically so it's unit-testable
/// against a saved fixture string with no network involved -- see the
/// `tests` module below.
fn parse_login_response(body: &str) -> anyhow::Result<AuthToken> {
    // `.lines()` splits on newlines without needing to know how many
    // lines there are or in what order -- `.find_map(...)` walks them and
    // keeps the first one where the closure returns `Some(...)`,
    // discarding every line that returns `None`. `.strip_prefix("Auth=")`
    // itself returns `Option<&str>`: `Some(rest)` if the line actually
    // started with `"Auth="`, `None` otherwise -- so this one expression
    // both finds the right line *and* strips its prefix in one step.
    body.lines()
        .find_map(|line| line.strip_prefix("Auth="))
        .map(|token| AuthToken(token.to_string()))
        .ok_or_else(|| anyhow!("FreshRSS login response had no Auth= line"))
}

/// Parses a `subscription/list?output=json` body into the `Vec<Subscription>`
/// callers actually want, discarding the one-field wrapper object
/// (`SubscriptionListResponse`) the JSON itself uses.
fn parse_subscriptions(body: &str) -> anyhow::Result<Vec<Subscription>> {
    let parsed: SubscriptionListResponse =
        serde_json::from_str(body).with_context(|| "failed to parse FreshRSS subscription list response")?;
    Ok(parsed.subscriptions)
}

/// Parses one `stream/items/ids?output=json` response body. Returns the
/// raw `ItemRefsPage` (not yet normalized) -- `Client::unread_item_ids`
/// is the one caller, and it does the normalizing itself so this function
/// stays a pure "turn JSON text into a Rust value" step with nothing else
/// mixed in.
fn parse_item_refs_page(body: &str) -> anyhow::Result<ItemRefsPage> {
    serde_json::from_str(body).with_context(|| "failed to parse FreshRSS unread item ids response")
}

/// Parses a `stream/items/contents` response body into `Item`s with their
/// `id` fields already normalized (see `normalize_item_id`) -- unlike
/// `parse_item_refs_page`, this one *does* finish normalization before
/// returning, since `Item` is the `pub` type Phase 4 will consume
/// directly, and it should never have to know the raw long-form `id`
/// shape exists at all.
fn parse_item_contents(body: &str) -> anyhow::Result<Vec<Item>> {
    let parsed: ContentsResponse =
        serde_json::from_str(body).with_context(|| "failed to parse FreshRSS item contents response")?;

    // `into_iter()` consumes `parsed.items` (we don't need `parsed`
    // afterwards) and `.map(...)` rebuilds each `Item` with a normalized
    // `id`. `mut item` inside the closure makes `item` a locally mutable
    // binding -- this doesn't change anything about the `Vec` it came
    // from, it's just enough mutability to overwrite one field before
    // handing the value onward.
    parsed
        .items
        .into_iter()
        .map(|mut item| {
            item.id = normalize_item_id(&item.id)?;
            Ok(item)
        })
        .collect()
}

/// Converts any FreshRSS item ID into the one plain-decimal form the rest
/// of the app uses (and `freshrss_items.remote_id` stores) -- the single
/// place this codebase's ID normalization happens, per ARCHITECTURE.md's
/// "normalize them in exactly one place in the client."
///
/// FreshRSS hands back two different shapes for what is otherwise the
/// exact same ID, depending which endpoint answered:
/// - `stream/items/ids` gives a plain decimal number as a string, e.g.
///   `"1789934360946639"`.
/// - `stream/items/contents` gives back `items[].id` in the long form
///   `"tag:google.com,2005:reader/item/00065bef91c167cf"` instead.
///
/// Verified against a live server that these really are the same number:
/// the hex suffix after the last `/` is that same decimal value, just
/// written in base 16 and zero-padded to 16 digits
/// (`u64::from_str_radix("00065bef91c167cf", 16) == Ok(1789934360946639)`).
/// This function detects which shape it was given and always returns the
/// short decimal form, so every other function in this module -- and
/// every caller outside it -- only ever has to reason about one ID shape.
fn normalize_item_id(raw: &str) -> anyhow::Result<String> {
    if let Some(hex) = raw.strip_prefix("tag:google.com,2005:reader/item/") {
        // `u64::from_str_radix(hex, 16)` parses `hex` as a base-16
        // (hexadecimal) number instead of the usual base-10 `.parse()`
        // would assume. It returns a `Result`, since `hex` might not
        // actually be valid hex if some future FreshRSS version changes
        // this format -- `.with_context` turns that into a clear error
        // naming the offending ID rather than an opaque parse failure.
        let value = u64::from_str_radix(hex, 16)
            .with_context(|| format!("item id '{raw}' had the long tag:google.com form but its suffix wasn't valid hex"))?;
        return Ok(value.to_string());
    }

    // Not the long form -- confirm it's actually a plain number before
    // accepting it as-is, so a genuinely malformed ID (a future API
    // change, a corrupted response) fails loudly right here instead of
    // silently becoming a wrong `remote_id` deep inside `Storage` later.
    // The parsed `u64` itself is thrown away (`raw.to_string()` is
    // returned, not `value.to_string()`) since `raw` is already in the
    // exact form this function is supposed to produce -- parsing it here
    // is purely a validation step, not a conversion.
    raw.parse::<u64>()
        .map(|_value| raw.to_string())
        .with_context(|| format!("item id '{raw}' wasn't a recognized FreshRSS id shape"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `include_str!` bakes each fixture file's contents into the test
    /// binary at compile time (the same trick `config.rs` uses for
    /// `freshrss.toml.example`) -- no filesystem access, and therefore no
    /// network, needed at test-run time. Every fixture here was captured
    /// from a real FreshRSS test server (see the module doc comment and
    /// ARCHITECTURE.md's "Verified against a live test server" note), not
    /// hand-written, with the real login fixture's `Auth=`/`SID=` values
    /// replaced by obvious placeholders -- see `fixtures/login_success.txt`.
    const LOGIN_SUCCESS: &str = include_str!("fixtures/login_success.txt");
    const LOGIN_FAILURE: &str = include_str!("fixtures/login_failure.txt");
    const SUBSCRIPTION_LIST: &str = include_str!("fixtures/subscription_list.json");
    const ITEMS_IDS_PAGE1: &str = include_str!("fixtures/items_ids_page1.json");
    const ITEMS_IDS_FINAL_PAGE: &str = include_str!("fixtures/items_ids_final_page.json");
    const ITEMS_CONTENTS: &str = include_str!("fixtures/items_contents.json");

    /// A successful login body's `Auth=` line is found and extracted,
    /// regardless of the other two lines around it.
    #[test]
    fn parses_auth_token_from_login_success_fixture() {
        let token = parse_login_response(LOGIN_SUCCESS).unwrap();
        assert_eq!(token.0, "FIXTURE-AUTH-TOKEN-NOT-REAL-0000000000000000000000");
    }

    /// A `Debug`-printed `AuthToken` never contains the real token text --
    /// the same guarantee `freshrss::config::Password` makes, checked here
    /// for this type specifically since it has its own hand-written impl.
    #[test]
    fn auth_token_debug_output_never_contains_the_real_token() {
        let token = parse_login_response(LOGIN_SUCCESS).unwrap();
        let debug_output = format!("{token:?}");

        assert!(!debug_output.contains("FIXTURE-AUTH-TOKEN-NOT-REAL"));
        assert!(debug_output.contains("redacted"));
    }

    /// Same guarantee as `AuthToken`'s, checked for `WriteToken`
    /// specifically since it has its own separate hand-written `Debug`.
    #[test]
    fn write_token_debug_output_never_contains_the_real_token() {
        let token = WriteToken("super-secret-write-token".to_string());
        let debug_output = format!("{token:?}");

        assert!(!debug_output.contains("super-secret-write-token"));
        assert!(debug_output.contains("redacted"));
    }

    /// The real success body verified against a live server.
    #[test]
    fn parses_a_real_edit_tag_success_response() {
        assert!(parse_mark_read_response("OK").is_ok());
        // A trailing newline (which some servers add) shouldn't matter.
        assert!(parse_mark_read_response("OK\n").is_ok());
    }

    /// Anything else -- including the exact failure body verified against
    /// a live server for a bad write token -- is not a confirmed success.
    #[test]
    fn rejects_anything_that_is_not_a_literal_ok() {
        assert!(parse_mark_read_response("Unauthorized!").is_err());
        assert!(parse_mark_read_response("").is_err());
    }

    /// The real success body verified against a live server.
    #[test]
    fn parses_a_real_quickadd_success_response() {
        let body = r#"{"numResults":1,"query":"https://shanetalkstech.com/index.xml","streamId":"feed/4","streamName":"shanetalkstech"}"#;

        let result = parse_quickadd_response(body).unwrap();
        let added = result.unwrap();
        assert_eq!(added.stream_id, "feed/4");
        assert_eq!(added.stream_name, "shanetalkstech");
    }

    /// The three real rejection bodies verified against a live server --
    /// all `numResults: 0` with an `error` string, and all three read
    /// back out through the inner `Err` rather than a transport-level
    /// failure.
    #[test]
    fn parses_real_quickadd_rejection_responses() {
        let already_subscribed = r#"{"numResults":0,"error":"Already subscribed! https://shanetalkstech.com/index.xml"}"#;
        let err = parse_quickadd_response(already_subscribed).unwrap().unwrap_err();
        assert!(err.contains("Already subscribed"));

        let ssrf_rejected =
            r#"{"numResults":0,"error":"URL is not allowed to be resolved: http://127.0.0.1:9099/nope.xml [http://127.0.0.1:9099/nope.xml]"}"#;
        let err = parse_quickadd_response(ssrf_rejected).unwrap().unwrap_err();
        assert!(err.contains("not allowed to be resolved"));

        let unresolvable = r#"{"numResults":0,"error":"Failed to resolve domain: https://nonexistent.invalid/feed.xml [https://nonexistent.invalid/feed.xml]"}"#;
        let err = parse_quickadd_response(unresolvable).unwrap().unwrap_err();
        assert!(err.contains("Failed to resolve domain"));
    }

    /// A `numResults: 0` response with no `error` field at all (not
    /// observed against a live server, but nothing in the JSON shape
    /// guarantees it can't happen) still produces a usable `Err`, not a
    /// panic or a confusing empty string.
    #[test]
    fn quickadd_rejection_with_no_error_field_still_produces_a_message() {
        let body = r#"{"numResults":0}"#;
        let err = parse_quickadd_response(body).unwrap().unwrap_err();
        assert!(!err.is_empty());
    }

    /// The real failure body FreshRSS sends back for a bad password
    /// (`"Unauthorized!"`, verified against a live server) has no `Auth=`
    /// line, so parsing it fails -- this is the body-shape half of
    /// rejecting a bad login; `Client::login`'s HTTP-status check (not
    /// unit-testable without a mock server) is the other half.
    #[test]
    fn login_failure_fixture_has_no_auth_line() {
        assert!(parse_login_response(LOGIN_FAILURE).is_err());
    }

    /// The subscription list fixture has two real feeds; check every
    /// field this module's `Subscription`/`Category` types expose.
    #[test]
    fn parses_subscription_list_fixture() {
        let subscriptions = parse_subscriptions(SUBSCRIPTION_LIST).unwrap();

        assert_eq!(subscriptions.len(), 2);

        let hn = subscriptions.iter().find(|s| s.id == "feed/2").unwrap();
        assert_eq!(hn.title, "Hacker News: Newest");
        assert_eq!(hn.url, "https://hnrss.org/newest");
        assert_eq!(hn.html_url, "https://news.ycombinator.com/newest");
        assert_eq!(hn.categories.len(), 1);
        assert_eq!(hn.categories[0].label, "Uncategorized");
    }

    /// A page with items still has a `continuation` -- confirming the
    /// exact shape `unread_item_ids` relies on (see its doc comment for
    /// why a present `continuation` on a non-empty page is *not* treated
    /// as "keep going" being safe to assume forever).
    #[test]
    fn parses_item_refs_page_with_continuation() {
        let page = parse_item_refs_page(ITEMS_IDS_PAGE1).unwrap();

        assert_eq!(page.item_refs.len(), 5);
        assert_eq!(page.item_refs[0].id, "1789934360946639");
        assert_eq!(page.continuation.as_deref(), Some("1789934360946635"));
    }

    /// The exhausted-stream response: empty `itemRefs`, no `continuation`
    /// at all -- the one unambiguous "stop" signal `unread_item_ids`
    /// checks for first.
    #[test]
    fn empty_item_refs_page_has_no_continuation() {
        let page = parse_item_refs_page(ITEMS_IDS_FINAL_PAGE).unwrap();

        assert!(page.item_refs.is_empty());
        assert_eq!(page.continuation, None);
    }

    /// The full round trip this module exists to get right: parsing real
    /// `stream/items/contents` JSON *and* normalizing the long-form
    /// `tag:google.com,...` ids it contains back down to the same plain
    /// decimal form `stream/items/ids` uses.
    #[test]
    fn parses_item_contents_fixture_and_normalizes_long_form_ids() {
        let items = parse_item_contents(ITEMS_CONTENTS).unwrap();

        assert_eq!(items.len(), 2);

        let first = &items[0];
        // Verified this is the same item: the raw fixture's id was
        // "tag:google.com,2005:reader/item/00065bef91c167cf", and its own
        // timestampUsec field (not stored on `Item`, but captured
        // alongside it when this fixture was recorded) was
        // "1789934360946639" -- this assertion is that normalization
        // reaches the same value going through the hex id instead.
        assert_eq!(first.id, "1789934360946639");
        assert_eq!(first.title, "Show HN: A browser MMO designed to be played by scripts, not people");
        assert_eq!(first.origin.stream_id, "feed/2");
        assert_eq!(first.origin.title, "Hacker News: Newest");
        assert_eq!(first.canonical[0].href, "https://space-core.at/");
        // The fixture's `summary.content` is a trimmed placeholder (the
        // repo is public; see the fixtures' own commit for why), not the
        // real scraped HTML this response originally carried -- this just
        // confirms the field round-trips into `Item.summary` at all.
        assert_eq!(first.summary.as_ref().unwrap().content, "<p>Fixture summary content one.</p>");
    }

    /// `normalize_item_id` directly: both real shapes it has to handle,
    /// plus a garbage input it should reject rather than silently pass
    /// through.
    #[test]
    fn normalize_item_id_handles_both_forms() {
        assert_eq!(normalize_item_id("1789934360946639").unwrap(), "1789934360946639");
        assert_eq!(
            normalize_item_id("tag:google.com,2005:reader/item/00065bef91c167cf").unwrap(),
            "1789934360946639"
        );
        assert!(normalize_item_id("not-an-id").is_err());
    }
}
