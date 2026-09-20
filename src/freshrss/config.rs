//! Loading and validating `freshrss.toml` (`docs/ARCHITECTURE.md` §
//! Configuration: `freshrss.toml`).
//!
//! Mirrors the "normalize/validate at the boundary" shape every other
//! config loader in this codebase already uses (`theme::Theme::load`,
//! `fetchers::config::load_sources`, `export::ExportConfig::load`): this
//! module is the only place that knows `freshrss.toml`'s on-disk shape,
//! and everything else only ever sees the validated `FreshRssConfig` this
//! produces.

use anyhow::{Context, anyhow, bail};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// The two states a `freshrss.toml` can resolve into: either the feature
/// is off (nothing here is ever read, no network request is ever made), or
/// it's on with everything a sync needs already validated and in hand.
///
/// This is an `enum`, not a `bool enabled` field on a struct whose other
/// fields are all `Option`, for the same reason `models.rs` prefers
/// `Option<String>` over a sentinel value for `note`: it puts "there might
/// be nothing here" *in the type itself*. Every future call site that
/// wants to actually sync has to `match` on this (or an `if let
/// FreshRssConfig::Enabled { .. } = ...`) and can *only* reach `url`,
/// `username`, and `password` from inside the `Enabled` arm — there's no
/// way to accidentally read a `url` that was never validated because the
/// feature was off, the way there would be with a struct full of
/// `Option<String>` fields a caller could forget to check.
#[derive(Debug)]
pub enum FreshRssConfig {
    /// `freshrss.toml` doesn't exist, or has `enabled = false` — the
    /// runtime half of "off means identical to `master`"
    /// (docs/ARCHITECTURE.md's FreshRSS Sync section). No other field on a
    /// disabled config is even parsed as more than an optional string; see
    /// `RawFreshRss::into_config` below.
    Disabled,

    /// Everything a sync needs, already resolved and validated: a
    /// `password_file`/`TUXWIRE_FRESHRSS_PASSWORD` has actually been read
    /// into a real `Password`, `url`/`username` are guaranteed non-empty.
    /// Nothing downstream of this point needs to re-check "is this field
    /// actually there."
    Enabled {
        /// The FreshRSS server's base URL, exactly as `freshrss.toml`'s
        /// `url` key holds it (no `/api/greader.php` suffix) -- guaranteed
        /// non-empty by `into_config`'s validation below.
        url: String,
        /// The FreshRSS account name to log in as.
        username: String,
        /// The already-resolved API password -- see `Password`'s own doc
        /// comment for why this isn't a bare `String`.
        password: Password,
        /// Whether to run a sync automatically at startup, in addition to
        /// `r` on a `FreshRSS/*` topic — see ARCHITECTURE.md's "Refresh
        /// triggers." Defaults to `true` (see `default_sync_on_start`
        /// below) when the key is omitted from the file entirely.
        sync_on_start: bool,
        /// The most items any *one* sync will fetch full contents for and
        /// insert, even when there are more unread items than this
        /// waiting on the server. Already validated to a sensible range
        /// by `into_config` below -- see `validate_max_items_per_sync`.
        /// Defaults to 500 (see `default_max_items_per_sync`) when the
        /// key is omitted. Per ARCHITECTURE.md's Phase 4 design: a first
        /// sync against an account with thousands of unread items
        /// backfills gradually across several syncs instead of trying to
        /// pull everything in one pass.
        max_items_per_sync: usize,
        /// Whether a sync is allowed to send anything back to the server
        /// at all -- Phase 5's push step (`edit-tag`, marking items read
        /// remotely). Defaults to `true` (see `default_push_read_state`).
        /// `false` doesn't touch the *pull* direction: local reads still
        /// get queued (`pending_read_push` still gets set, per the same
        /// hook points as always) and the server's own read state still
        /// flows in locally, this only stops the outbound half -- for
        /// someone who wants tuxwire to mirror their FreshRSS reading
        /// list without ever writing back to it.
        push_read_state: bool,
    },
}

/// The FreshRSS API password, wrapped so it can't leave this type by
/// accident.
///
/// `docs/ARCHITECTURE.md`'s Security rules require the password to "never
/// [be] logged, printed, or committed... never included in error
/// messages... redact[ed] in `Debug` output." A bare `String` field
/// satisfies none of that on its own — anything holding one can be
/// `{:?}`-printed, interpolated into an error message, or handed to
/// `println!` without anyone noticing until the real password shows up in
/// a log file. Wrapping it in a dedicated type with a hand-written `Debug`
/// impl (below) and one narrowly-named accessor makes every one of those
/// mistakes either impossible or, at minimum, `rg`-able: every real use of
/// the password text has to call `expose_secret()` by name.
pub struct Password(String);

impl Password {
    /// The actual password text — named `expose_secret` rather than a
    /// plain getter (or implementing `Deref<Target = str>`, which would
    /// let it leak through ordinary-looking string operations without a
    /// trace) so every call site that reaches for the real secret says so
    /// in its own source. As of Phase 3, its one real caller is
    /// `freshrss::client::Client::login`, which reads it right at the
    /// point of building the login request body and nowhere else.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Builds a `Password` directly from plain text, bypassing every
    /// normal resolution path (`freshrss.toml`'s `password_file`, the
    /// `TUXWIRE_FRESHRSS_PASSWORD` env var). Test-only, `pub(crate)` (not
    /// `pub`) -- the one real caller outside this module is
    /// `freshrss::sync`'s manual live-server test, which needs a
    /// `Password` built straight from an environment variable it reads
    /// itself, without going through a real `freshrss.toml` file that
    /// test has no reason to create.
    #[cfg(test)]
    pub(crate) fn from_plain_text_for_tests(text: String) -> Self {
        Self(text)
    }
}

impl std::fmt::Debug for Password {
    /// Hand-written instead of `#[derive(Debug)]` for exactly one reason:
    /// a derived impl on `Password(String)` would print the real password
    /// verbatim the moment *anything* containing it — `FreshRssConfig`
    /// itself, or some future struct that embeds one — gets formatted with
    /// `{:?}`, including by an innocent `eprintln!("{config:?}")` written
    /// to debug something unrelated. Redacting it here, once, means that
    /// mistake is harmless everywhere `Password` ever ends up, rather than
    /// relying on every future caller to remember not to print it.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// The raw shape `freshrss.toml` deserializes into, before validation.
/// Every field but `enabled`/`sync_on_start` is `Option` — same reasoning
/// as `fetchers::config::RawSource`: a missing field is something this
/// module wants to detect and report clearly, not something serde itself
/// should reject with a generic "missing field" error.
#[derive(Deserialize)]
struct RawFreshRssFile {
    /// The `[freshrss]` table itself -- the file's only top-level key.
    freshrss: RawFreshRss,
}

/// The raw contents of the `[freshrss]` table before validation --
/// `RawFreshRss::into_config` below is what turns this into a real
/// `FreshRssConfig`.
#[derive(Deserialize)]
struct RawFreshRss {
    /// `#[serde(default)]` (defaulting to `bool::default()`, i.e. `false`)
    /// means a `freshrss.toml` that somehow omits `enabled` entirely is
    /// still safely disabled, rather than a missing-field error — matching
    /// the bundled default file, which spells it out explicitly as
    /// `enabled = false` for clarity but doesn't strictly need to.
    #[serde(default)]
    enabled: bool,
    /// The FreshRSS server's base URL, not yet validated as non-empty --
    /// `None` covers both "the key is missing" and lets `into_config` tell
    /// the person exactly which required field they left out.
    url: Option<String>,
    /// The FreshRSS account name, same "not yet validated" reasoning as
    /// `url`.
    username: Option<String>,
    /// Defaults to `true` when omitted (see `default_sync_on_start`) —
    /// per ARCHITECTURE.md, "most people who enable FreshRSS at all want
    /// it current as soon as tuxwire opens."
    #[serde(default = "default_sync_on_start")]
    sync_on_start: bool,
    /// Path to a file containing just the API password, before `~`
    /// expansion or the file itself has been read -- see
    /// `resolve_password`.
    password_file: Option<String>,
    /// Defaults to 500 when omitted (see `default_max_items_per_sync`) --
    /// not yet checked against `validate_max_items_per_sync`'s sensible
    /// range, since that check belongs in `into_config` alongside every
    /// other field's validation, not scattered into deserialization.
    #[serde(default = "default_max_items_per_sync")]
    max_items_per_sync: usize,
    /// Defaults to `true` when omitted (see `default_push_read_state`).
    #[serde(default = "default_push_read_state")]
    push_read_state: bool,
}

/// The default `sync_on_start` value when `freshrss.toml` omits the key
/// entirely -- a plain function (not a constant) because that's what
/// serde's `#[serde(default = "...")]` attribute requires: a path to a
/// function returning the field's type, called only when the key is
/// missing from the parsed TOML.
fn default_sync_on_start() -> bool {
    true
}

/// The default `max_items_per_sync` when `freshrss.toml` omits the key --
/// 500 is the number the plan settled on: generous enough that a normal,
/// regularly-synced account never notices the cap, small enough that a
/// first sync against an account with thousands of unread items doesn't
/// try to fetch and insert all of them in one pass (see
/// `Client::CONTENTS_CHUNK_SIZE`'s doc comment -- at 100 items per
/// request, 500 is 5 chunked requests, not 50+).
fn default_max_items_per_sync() -> usize {
    500
}

/// The default `push_read_state` when `freshrss.toml` omits the key --
/// `true`, matching ARCHITECTURE.md's original design (read state syncs
/// both directions by default); a person who wants a pull-only mirror
/// sets this to `false` explicitly.
fn default_push_read_state() -> bool {
    true
}

/// The range `max_items_per_sync` must fall in, checked by
/// `validate_max_items_per_sync` below. `0` would make every sync
/// silently fetch nothing forever, which is almost certainly a typo, not
/// an intentional choice -- so the minimum is 1, not 0. The maximum
/// (10,000) isn't a hard technical limit (there's no reason a larger
/// value would actually break anything), just a sanity ceiling: a value
/// far beyond it is more likely a mistyped extra zero than a deliberate
/// choice, and it keeps one misconfigured sync from trying to fetch tens
/// of thousands of items' full contents in a single pass.
const MAX_ITEMS_PER_SYNC_RANGE: std::ops::RangeInclusive<usize> = 1..=10_000;

impl RawFreshRss {
    /// Turns the raw, possibly-incomplete parse into a real
    /// `FreshRssConfig` — `Disabled` immediately if `enabled` is `false`
    /// (every other field is simply ignored in that case, even if it's
    /// present but malformed-looking, since nothing will ever read it),
    /// or `Enabled` with every field validated and the password actually
    /// resolved.
    ///
    /// Fields are validated one at a time (rather than, say, collecting
    /// every problem into one combined error) so a person fixing
    /// `freshrss.toml` sees one clear, specific complaint at a time — same
    /// pattern `RawSource::validate` uses for `sources.toml`. That's for
    /// hard *errors* -- fatal, `enabled = true` is unusable without this
    /// fixed. Non-fatal *warnings* (an insecure URL, a loosely-permissioned
    /// password file) are a different thing entirely: collected into the
    /// returned `Vec<String>` instead of stopping anything, since `main`
    /// now decides how to show them (the footer, once the TUI is up --
    /// see ARCHITECTURE.md's "Config warnings in the footer" note) rather
    /// than this module assuming stderr is always the right place.
    fn into_config(self) -> anyhow::Result<(FreshRssConfig, Vec<String>)> {
        if !self.enabled {
            return Ok((FreshRssConfig::Disabled, Vec::new()));
        }

        let url = self
            .url
            .ok_or_else(|| anyhow!("freshrss.toml has enabled = true but is missing a required 'url' field"))?;
        let username = self.username.ok_or_else(|| {
            anyhow!("freshrss.toml has enabled = true but is missing a required 'username' field")
        })?;
        validate_max_items_per_sync(self.max_items_per_sync)?;

        // Every warning collected below goes into this one `Vec` instead
        // of being printed as it's found -- `mut` because pushing onto it
        // happens in more than one place (here, and inside
        // `resolve_password`, which needs to add its own warning about
        // `password_file`'s permissions without also owning the *rest* of
        // this function's warnings).
        let mut warnings = Vec::new();
        if let Some(warning) = insecure_url_warning(&url) {
            warnings.push(warning);
        }

        let password = resolve_password(self.password_file.as_deref(), &mut warnings)?;

        let config = FreshRssConfig::Enabled {
            url,
            username,
            password,
            sync_on_start: self.sync_on_start,
            max_items_per_sync: self.max_items_per_sync,
            push_read_state: self.push_read_state,
        };
        Ok((config, warnings))
    }
}

/// Checks `value` against `MAX_ITEMS_PER_SYNC_RANGE`, returning a clear,
/// specific error naming both the bad value and the acceptable range when
/// it's out of bounds -- same "name what's wrong" contract as the
/// `url`/`username` missing-field checks in `into_config` above, just for
/// a value that's present but nonsensical rather than absent.
fn validate_max_items_per_sync(value: usize) -> anyhow::Result<()> {
    if MAX_ITEMS_PER_SYNC_RANGE.contains(&value) {
        return Ok(());
    }

    bail!(
        "freshrss.toml's max_items_per_sync ({value}) must be between {} and {} -- \
         0 would make every sync fetch nothing, and a much larger value risks one sync \
         trying to pull an unreasonable number of items at once",
        MAX_ITEMS_PER_SYNC_RANGE.start(),
        MAX_ITEMS_PER_SYNC_RANGE.end()
    );
}

/// The bundled fallback `freshrss.toml`, embedded into the compiled binary
/// via `include_str!` — same trick `theme.rs`/`fetchers::config.rs`/
/// `export.rs` all use for their own `.toml.example` files. Its `enabled =
/// false` is what makes "no `freshrss.toml` on disk yet" behave exactly
/// like a present-but-disabled one.
const DEFAULT_FRESHRSS_TOML: &str = include_str!("../../freshrss.toml.example");

impl FreshRssConfig {
    /// Loads `~/.config/tuxwire/freshrss.toml`, falling back to the
    /// bundled disabled-by-default file (see `DEFAULT_FRESHRSS_TOML`
    /// above) when it doesn't exist yet.
    ///
    /// Unlike `fetchers::config::load_sources` (which *writes out* its
    /// bundled default the first time it's needed), this follows
    /// `theme::Theme::load`'s pattern instead: fall back to the default
    /// in memory, but never write anything to disk. `sources.toml` is
    /// something every install needs from the first run; FreshRSS is an
    /// opt-in feature most people running tuxwire will never touch, so
    /// there's no reason to leave a `freshrss.toml` sitting in someone's
    /// config directory just because tuxwire happened to check whether the
    /// feature was on.
    ///
    /// Any I/O error besides "the file simply isn't there" — bad
    /// permissions, a malformed TOML file, a required field missing while
    /// `enabled = true` — is a real problem the person needs to see, so
    /// it's propagated with `?`/`with_context` rather than silently
    /// falling back, same reasoning as every other loader in this
    /// codebase.
    ///
    /// Returns the config *and* a `Vec<String>` of non-fatal warnings
    /// (insecure URL, loosely-permissioned password file) rather than
    /// printing them itself -- `main` is the one place that knows whether
    /// the TUI is up yet, so it's the one place that can decide whether a
    /// warning belongs on stderr (still nothing running) or in the
    /// footer (see ARCHITECTURE.md's "Config warnings in the footer"
    /// note). An empty `Vec` (not `None`) when there's nothing to warn
    /// about, or when the config is `Disabled` -- a plain, always-there
    /// `Vec` is simpler for every caller to iterate than an
    /// `Option<Vec<String>>` that's only ever `None` in the "nothing to
    /// say" case anyway.
    pub fn load() -> anyhow::Result<(FreshRssConfig, Vec<String>)> {
        let path = freshrss_toml_path()?;

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => DEFAULT_FRESHRSS_TOML.to_string(),
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read FreshRSS config {}", path.display()));
            }
        };

        let raw: RawFreshRssFile = toml::from_str(&contents)
            .with_context(|| format!("failed to parse FreshRSS config {}", path.display()))?;

        raw.freshrss.into_config()
    }
}

/// Resolves the actual password text from whichever of the two sources
/// `docs/ARCHITECTURE.md` documents is available, preferring the
/// environment variable: "the `TUXWIRE_FRESHRSS_PASSWORD` environment
/// variable... takes precedence over `password_file` when both are set."
/// The env var is checked first specifically so a person can override a
/// committed/shared `freshrss.toml`'s `password_file` for one run (a
/// throwaway test server, say) without editing the file itself.
///
/// `warnings: &mut Vec<String>` rather than returning its own warning
/// alongside the password (e.g. as a tuple) -- this function already has
/// one job (produce a `Password` or a clear error), and threading the
/// same `Vec` `into_config` is already building through means the
/// person fixing `freshrss.toml` sees every warning together, in one
/// place, regardless of which internal function happened to notice it.
fn resolve_password(password_file: Option<&str>, warnings: &mut Vec<String>) -> anyhow::Result<Password> {
    if let Ok(from_env) = std::env::var("TUXWIRE_FRESHRSS_PASSWORD") {
        return Ok(Password(from_env));
    }

    let password_file = password_file.ok_or_else(|| {
        anyhow!(
            "freshrss.toml has enabled = true but no way to find the API password -- set \
             either password_file in freshrss.toml or the TUXWIRE_FRESHRSS_PASSWORD \
             environment variable"
        )
    })?;

    let path = expand_tilde(password_file)?;

    if let Some(warning) = loose_permissions_warning(&path) {
        warnings.push(warning);
    }

    // The file's contents *are* the secret, so unlike every other
    // `with_context`/`bail!` in this module, nothing below this line ever
    // interpolates `contents` (or the password derived from it) into a
    // message -- only `path`, which is just a filename, is safe to name.
    let contents = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read FreshRSS password file {}", path.display()))?;

    let password = contents.trim().to_string();
    if password.is_empty() {
        bail!("FreshRSS password file {} is empty", path.display());
    }

    Ok(Password(password))
}

/// Checks whether `url` is worth warning about — plain `http://` talking
/// to a host that isn't the local machine, meaning the FreshRSS API
/// password would cross the network unencrypted on every login. Returns
/// the warning message rather than printing it directly (unlike
/// `main.rs`'s `eprintln!`-on-fetch-failure precedent) purely so this
/// logic itself is unit-testable without capturing stderr — the one
/// caller above still just `eprintln!`s whatever comes back.
///
/// Deliberately narrow: only `http://` (never `https://`, which is always
/// fine) with a host that isn't `localhost`/`127.0.0.1`/`::1` triggers a
/// warning. A URL this crate can't even parse the scheme/host out of
/// isn't flagged here — a genuinely malformed `url` will fail loudly
/// enough once the client (a later build phase) actually tries to use it.
fn insecure_url_warning(url: &str) -> Option<String> {
    let host = url.strip_prefix("http://")?.split(['/', ':']).next().unwrap_or("");

    if matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return None;
    }

    Some(format!(
        "freshrss.toml's url ({url}) uses plain http to a non-local host -- the FreshRSS \
         API password will be sent unencrypted. Use https:// unless this really is a \
         local-only server."
    ))
}

/// Checks whether `path` (the resolved `password_file`) is readable by
/// anyone besides its owner, per ARCHITECTURE.md's "On Unix, warn if
/// `password_file` is readable by group or others." Same
/// return-a-message-instead-of-printing shape as `insecure_url_warning`,
/// for the same testability reason.
///
/// A file that can't be `stat`ed yet (doesn't exist, permission denied
/// just checking metadata) isn't warned about here — the read in
/// `resolve_password` right after this call is what surfaces that as a
/// real, specific error.
#[cfg(unix)]
fn loose_permissions_warning(path: &Path) -> Option<String> {
    use std::os::unix::fs::PermissionsExt;

    let mode = std::fs::metadata(path).ok()?.permissions().mode();
    // `0o077` masks just the group and other permission bits -- any of
    // read/write/execute set for either means "not owner-only."
    if mode & 0o077 == 0 {
        return None;
    }

    Some(format!(
        "FreshRSS password file {} is readable by group or others (mode {:o}) -- consider \
         `chmod 600 {}`",
        path.display(),
        mode & 0o777,
        path.display()
    ))
}

/// Non-Unix platforms (nothing tuxwire targets today, per ARCHITECTURE.md,
/// but `#[cfg(unix)]` needs a matching fallback for the function to exist
/// at all when compiled elsewhere) have no POSIX permission bits to check,
/// so there's nothing to warn about.
#[cfg(not(unix))]
fn loose_permissions_warning(_path: &Path) -> Option<String> {
    None
}

/// Expands a leading `~`/`~/...` in `password_file` to `$HOME` — the same
/// gap (path expansion is a shell feature, not something the filesystem or
/// `PathBuf` understands on its own) `export.rs`'s `expand_tilde` exists
/// to fill, duplicated here rather than shared: each config loader in this
/// codebase already owns its own tiny bit of path-resolution logic
/// (`theme_path`, `sources_path`, `export_config_path` all separately
/// re-implement the same XDG-fallback shape too), and `src/freshrss/`
/// specifically must not reach into `crate::export` or any other sibling
/// module to stay the self-contained, extractable unit ARCHITECTURE.md
/// describes.
fn expand_tilde(raw: &str) -> anyhow::Result<PathBuf> {
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = std::env::var("HOME")
            .context("HOME environment variable is not set -- can't expand '~' in freshrss.toml's password_file")?;
        return Ok(PathBuf::from(home).join(rest));
    }

    if raw == "~" {
        let home = std::env::var("HOME")
            .context("HOME environment variable is not set -- can't expand '~' in freshrss.toml's password_file")?;
        return Ok(PathBuf::from(home));
    }

    Ok(PathBuf::from(raw))
}

/// The full path to the user's FreshRSS config:
/// `~/.config/tuxwire/freshrss.toml`, per ARCHITECTURE.md's "Config lives
/// separately at `~/.config/tuxwire/`." Mirrors `theme::theme_path` /
/// `fetchers::config::sources_path` / `export::export_config_path`
/// exactly — same XDG-fallback pattern, one more filename joined onto the
/// same `tuxwire` config directory those already agree on.
fn freshrss_toml_path() -> anyhow::Result<PathBuf> {
    if let Ok(xdg_config_home) = std::env::var("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(xdg_config_home).join("tuxwire").join("freshrss.toml"));
    }

    let home =
        std::env::var("HOME").context("HOME environment variable is not set -- can't locate the FreshRSS config")?;

    Ok(PathBuf::from(home).join(".config/tuxwire/freshrss.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disabled config never even looks at `url`/`username`/
    /// `password_file` — all three can be missing (or, as tested here,
    /// simply left out of the TOML) without producing an error, since
    /// nothing downstream will ever read them.
    #[test]
    fn disabled_config_ignores_missing_fields() {
        let raw: RawFreshRssFile = toml::from_str("[freshrss]\nenabled = false\n").unwrap();
        let (config, warnings) = raw.freshrss.into_config().unwrap();

        assert!(matches!(config, FreshRssConfig::Disabled));
        assert!(warnings.is_empty());
    }

    /// A `freshrss.toml` with no `[freshrss]` section at all is a parse
    /// error (there's no `#[serde(default)]` on the whole file, only on
    /// individual fields inside it) -- unlike a merely-incomplete
    /// `[freshrss]` block, a file missing the table entirely is malformed
    /// enough to be worth a clear TOML-level error rather than silently
    /// treating it as disabled.
    #[test]
    fn missing_freshrss_table_fails_to_parse() {
        let result: Result<RawFreshRssFile, _> = toml::from_str("");
        assert!(result.is_err());
    }

    /// The bundled `freshrss.toml.example` (embedded via `include_str!` as
    /// `DEFAULT_FRESHRSS_TOML`, used as the fallback when no real
    /// `freshrss.toml` exists yet) must itself parse and resolve cleanly
    /// -- a comment-only edit to that file that accidentally breaks its
    /// TOML syntax, or sets a `max_items_per_sync` outside the valid
    /// range, would otherwise only surface the first time someone with no
    /// `freshrss.toml` ran a `--features freshrss` build.
    #[test]
    fn bundled_default_freshrss_toml_parses_and_resolves() {
        let raw: RawFreshRssFile = toml::from_str(DEFAULT_FRESHRSS_TOML).unwrap();
        let (config, warnings) = raw.freshrss.into_config().unwrap();

        assert!(matches!(config, FreshRssConfig::Disabled), "the bundled default ships with enabled = false");
        assert!(warnings.is_empty());
    }

    /// `enabled = true` with no `url` fails with a message naming the
    /// specific missing field -- same "name what's missing" contract
    /// `fetchers::config::RawSource::validate` guarantees for
    /// `sources.toml`.
    #[test]
    fn enabled_without_url_fails_with_clear_message() {
        let raw: RawFreshRssFile = toml::from_str(
            r#"
            [freshrss]
            enabled = true
            username = "me"
            password_file = "/nonexistent"
            "#,
        )
        .unwrap();

        let err = raw.freshrss.into_config().unwrap_err();
        assert!(err.to_string().contains("url"), "error should name the missing field: {err}");
    }

    /// `max_items_per_sync` defaults to 500 when the key is entirely
    /// absent -- same "defaults are honored" contract as
    /// `sync_on_start_defaults_to_true_when_omitted` above.
    #[test]
    fn max_items_per_sync_defaults_to_500_when_omitted() {
        let file_path = temp_password_file("test-password");
        let raw: RawFreshRssFile = toml::from_str(&format!(
            r#"
            [freshrss]
            enabled = true
            url = "https://freshrss.example.com"
            username = "me"
            password_file = {:?}
            "#,
            file_path.to_str().unwrap()
        ))
        .unwrap();

        let (config, _warnings) = raw.freshrss.into_config().unwrap();
        std::fs::remove_file(&file_path).ok();

        match config {
            FreshRssConfig::Enabled { max_items_per_sync, .. } => assert_eq!(max_items_per_sync, 500),
            FreshRssConfig::Disabled => panic!("expected an enabled config"),
        }
    }

    /// `max_items_per_sync = 0` is rejected with a clear error rather than
    /// silently producing a sync that never fetches anything -- see
    /// `validate_max_items_per_sync`'s doc comment for why 0 specifically
    /// is treated as almost certainly a mistake.
    #[test]
    fn max_items_per_sync_of_zero_fails_with_clear_message() {
        let raw: RawFreshRssFile = toml::from_str(
            r#"
            [freshrss]
            enabled = true
            url = "https://freshrss.example.com"
            username = "me"
            password_file = "/nonexistent"
            max_items_per_sync = 0
            "#,
        )
        .unwrap();

        let err = raw.freshrss.into_config().unwrap_err();
        assert!(err.to_string().contains("max_items_per_sync"), "error should name the field: {err}");
    }

    /// An absurdly large `max_items_per_sync` (almost certainly a typo,
    /// e.g. an extra zero) is rejected the same way as zero, not silently
    /// accepted just because it's technically a positive number.
    #[test]
    fn max_items_per_sync_above_the_sensible_range_fails_with_clear_message() {
        let raw: RawFreshRssFile = toml::from_str(
            r#"
            [freshrss]
            enabled = true
            url = "https://freshrss.example.com"
            username = "me"
            password_file = "/nonexistent"
            max_items_per_sync = 5000000
            "#,
        )
        .unwrap();

        let err = raw.freshrss.into_config().unwrap_err();
        assert!(err.to_string().contains("max_items_per_sync"), "error should name the field: {err}");
    }

    /// `sync_on_start` defaults to `true` when the key is entirely absent
    /// from the file -- per ARCHITECTURE.md, "most people who enable
    /// FreshRSS at all want it current as soon as tuxwire opens." Uses
    /// `password_file` (a real temp file), not the
    /// `TUXWIRE_FRESHRSS_PASSWORD` env var, specifically so this test has
    /// no process-wide state to race against every other test in this
    /// module -- see `env_lock` below for the tests that genuinely do need
    /// to touch the env var.
    #[test]
    fn sync_on_start_defaults_to_true_when_omitted() {
        let file_path = temp_password_file("test-password");
        let raw: RawFreshRssFile = toml::from_str(&format!(
            r#"
            [freshrss]
            enabled = true
            url = "https://freshrss.example.com"
            username = "me"
            password_file = {:?}
            "#,
            file_path.to_str().unwrap()
        ))
        .unwrap();

        let (config, _warnings) = raw.freshrss.into_config().unwrap();
        std::fs::remove_file(&file_path).ok();

        match config {
            FreshRssConfig::Enabled { sync_on_start, .. } => assert!(sync_on_start),
            FreshRssConfig::Disabled => panic!("expected an enabled config"),
        }
    }

    /// An explicit `sync_on_start = false` is honored, not silently
    /// overridden by the default. Same `password_file`-not-env-var
    /// reasoning as the test above.
    #[test]
    fn sync_on_start_can_be_disabled_explicitly() {
        let file_path = temp_password_file("test-password");
        let raw: RawFreshRssFile = toml::from_str(&format!(
            r#"
            [freshrss]
            enabled = true
            url = "https://freshrss.example.com"
            username = "me"
            sync_on_start = false
            password_file = {:?}
            "#,
            file_path.to_str().unwrap()
        ))
        .unwrap();

        let (config, _warnings) = raw.freshrss.into_config().unwrap();
        std::fs::remove_file(&file_path).ok();

        match config {
            FreshRssConfig::Enabled { sync_on_start, .. } => assert!(!sync_on_start),
            FreshRssConfig::Disabled => panic!("expected an enabled config"),
        }
    }

    /// A process-wide lock the three tests below hold for their entire
    /// body, so they can never interleave their `set_var`/`remove_var`
    /// calls on the one environment variable all three share
    /// (`TUXWIRE_FRESHRSS_PASSWORD`) -- `cargo test` runs tests on separate
    /// threads by default, and an environment variable is global state no
    /// amount of `&mut` borrowing in *this* process can protect on its
    /// own. `.unwrap_or_else(|poisoned| poisoned.into_inner())` recovers
    /// the lock even if an earlier test panicked while holding it (leaving
    /// the `Mutex` "poisoned") -- a stale env var from a panicked test is
    /// far less confusing to debug than every subsequent test in this file
    /// mysteriously failing to acquire the lock at all.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The environment variable wins over `password_file` when both are
    /// set -- per ARCHITECTURE.md, this is the documented precedence, not
    /// an implementation detail.
    #[test]
    fn env_var_password_takes_precedence_over_password_file() {
        let _guard = env_lock();
        let file_path = temp_password_file("password-from-file");

        unsafe {
            std::env::set_var("TUXWIRE_FRESHRSS_PASSWORD", "password-from-env");
        }
        let mut warnings = Vec::new();
        let password = resolve_password(Some(file_path.to_str().unwrap()), &mut warnings).unwrap();
        unsafe {
            std::env::remove_var("TUXWIRE_FRESHRSS_PASSWORD");
        }
        std::fs::remove_file(&file_path).ok();

        assert_eq!(password.expose_secret(), "password-from-env");
    }

    /// With no environment variable set, the password is read from
    /// `password_file`, trimmed of the trailing newline a text editor or
    /// `echo` would normally leave behind.
    #[test]
    fn reads_password_from_file_when_no_env_var_is_set() {
        let _guard = env_lock();
        unsafe {
            std::env::remove_var("TUXWIRE_FRESHRSS_PASSWORD");
        }

        let file_path = temp_password_file("real-password-with-trailing-newline\n");
        let mut warnings = Vec::new();
        let password = resolve_password(Some(file_path.to_str().unwrap()), &mut warnings).unwrap();
        std::fs::remove_file(&file_path).ok();

        assert_eq!(password.expose_secret(), "real-password-with-trailing-newline");
    }

    /// Neither source available (`enabled = true`, no `password_file`, no
    /// env var) is a clear error, not a silent "sync with an empty
    /// password."
    #[test]
    fn resolve_password_fails_clearly_with_no_source() {
        let _guard = env_lock();
        unsafe {
            std::env::remove_var("TUXWIRE_FRESHRSS_PASSWORD");
        }

        let mut warnings = Vec::new();
        let err = resolve_password(None, &mut warnings).unwrap_err();
        assert!(err.to_string().contains("password"), "error should mention the password: {err}");
    }

    /// The whole point of `Password`'s hand-written `Debug` impl: no
    /// `{:?}` of a `Password` — however it got constructed, however deep
    /// inside some other struct it's embedded — ever contains the actual
    /// password text.
    #[test]
    fn password_debug_output_never_contains_the_real_password() {
        let password = Password("super-secret-value".to_string());
        let debug_output = format!("{password:?}");

        assert!(!debug_output.contains("super-secret-value"));
        assert!(debug_output.contains("redacted"));
    }

    /// `FreshRssConfig` as a whole must be just as safe to `{:?}` as
    /// `Password` alone -- this is what actually matters in practice,
    /// since a config value (not a bare `Password`) is what a future
    /// `eprintln!("{config:?}")` debugging some unrelated field would
    /// print.
    #[test]
    fn freshrss_config_debug_output_never_contains_the_real_password() {
        let config = FreshRssConfig::Enabled {
            url: "https://freshrss.example.com".to_string(),
            username: "me".to_string(),
            password: Password("super-secret-value".to_string()),
            sync_on_start: true,
            max_items_per_sync: 500,
            push_read_state: true,
        };

        assert!(!format!("{config:?}").contains("super-secret-value"));
    }

    /// Plain `http://` to a real remote host is flagged; `https://` to the
    /// same host, and plain `http://` to `localhost`, are both fine.
    #[test]
    fn insecure_url_warning_flags_only_plain_http_to_a_remote_host() {
        assert!(insecure_url_warning("https://freshrss.example.com").is_none());
        assert!(insecure_url_warning("http://localhost:8080").is_none());
        assert!(insecure_url_warning("http://127.0.0.1:8080").is_none());

        let warning = insecure_url_warning("http://freshrss.example.com").unwrap();
        assert!(warning.contains("freshrss.example.com"));
    }

    /// `into_config` actually surfaces `insecure_url_warning`'s result in
    /// its returned `Vec<String>` -- this is the integration point that
    /// matters: the pure function above being correct doesn't by itself
    /// guarantee `into_config` is threading its result anywhere, since it
    /// used to just `eprintln!` it directly instead of collecting it.
    #[test]
    fn into_config_collects_the_insecure_url_warning() {
        let file_path = temp_password_file("test-password");
        // `chmod 600` so `loose_permissions_warning` stays quiet -- this
        // test is only about the URL warning; a stray second warning from
        // whatever the OS umask happened to leave the temp file at would
        // make the length assertion below flaky/misleading.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let raw: RawFreshRssFile = toml::from_str(&format!(
            r#"
            [freshrss]
            enabled = true
            url = "http://freshrss.example.com"
            username = "me"
            password_file = {:?}
            "#,
            file_path.to_str().unwrap()
        ))
        .unwrap();

        let (_config, warnings) = raw.freshrss.into_config().unwrap();
        std::fs::remove_file(&file_path).ok();

        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("freshrss.example.com"));
    }

    /// `chmod 600` (owner-only) is fine; anything readable by group or
    /// others gets flagged. Unix-only, matching `loose_permissions_warning`
    /// itself.
    #[cfg(unix)]
    #[test]
    fn loose_permissions_warning_flags_group_or_other_readable_files() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_password_file("whatever");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(loose_permissions_warning(&path).is_none());

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(loose_permissions_warning(&path).is_some());

        std::fs::remove_file(&path).ok();
    }

    /// A fresh temp file under the OS temp dir containing exactly
    /// `contents`, unique per call so parallel tests never collide on the
    /// same path -- the same hand-rolled approach `fetchers::config`'s
    /// tests use (`tempfile` isn't a dependency of this project).
    fn temp_password_file(contents: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);

        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("tuxwire-test-freshrss-password-{}-{n}", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path
    }
}
