# Changelog

Notable changes to tuxwire, in the same voice as the project's own commit
messages — what changed and why, not just what.

## Unreleased

### Added

- **FreshRSS sync (optional, off by default).** Pull your subscriptions
  from a self-hosted [FreshRSS](https://freshrss.org/) server, keep read
  state in sync in both directions, and add new feeds to FreshRSS
  straight from the `a` flow — all gated behind the `freshrss` Cargo
  feature and a `freshrss.toml` config file, so a plain `cargo build`
  (no flags) and a normal run with no `freshrss.toml` behave exactly as
  they always have. See the README's "FreshRSS Sync" section for setup,
  and `docs/ARCHITECTURE.md`'s "FreshRSS Sync" section for the full
  design.
  - FreshRSS feeds appear in the sidebar under `FreshRSS/<category>`
    topics (`FreshRSS/Uncategorized` for anything uncategorized),
    grouped in alongside local `sources.toml` topics.
  - `r` on a `FreshRSS/*` topic triggers a sync the same way `r` already
    refreshes a local topic; an optional startup sync
    (`sync_on_start`, on by default) runs in the background without
    delaying the TUI opening.
  - A sync pulls new articles (deduped by FreshRSS's own item id),
    reconciles read state in both directions (a locally-read article
    gets pushed to the server; an article read elsewhere on the server
    gets marked read locally), and reports a summary in the footer.
  - A FreshRSS item whose URL already belongs to an existing local
    article is skipped, never duplicated or linked — the existing
    article's topic, source, read/saved state, and note are never
    touched.
  - `push_read_state = false` turns off the outbound half only — the
    server's own read state still flows in locally.
  - Pressing `a` gains a **Destination: Local / FreshRSS** choice on the
    confirm screen when FreshRSS is enabled; choosing FreshRSS
    subscribes the feed on the server instead of writing to
    `sources.toml`, skipping the name/topic prompts (FreshRSS assigns
    neither). Not gated by `push_read_state` — adding a feed is a direct
    action, not an automatic side effect.
  - Saved articles and notes stay exactly what they've always been:
    local-only, never sent to or read from FreshRSS.
