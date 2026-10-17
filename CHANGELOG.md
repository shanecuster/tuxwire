# Changelog

Notable changes to tuxwire, in the same voice as the project's own commit
messages — what changed and why, not just what.

## Unreleased

### Fixed

- **A newly added feed showed no articles until the next refresh**, which made
  the app look broken. Adding a source (terminal `a`, Android "save") now
  fetches that one feed straight away. If the download fails the feed is still
  saved, and the message says why.

- **Feeds that sent an empty page to tuxwire** (System76's blog, for one)
  failed with "unable to parse feed: no root element". Requests now send a
  `User-Agent: tuxwire/<version>` header, which such sites expect. Failures are
  also reported honestly: "answered HTTP 403" or "sent an empty reply" instead
  of an XML parse error. Applies to the terminal app and the Android app alike.
- **Feeds in older encodings** (declared as ISO-8859-1 in the feed's XML line)
  keep their accents: the parser now gets the raw bytes. Downloads also have
  size limits (20 MiB per feed, 2 MiB of a page searched for feed links).

### Changed

- **Deleting or replacing anything now asks first.** Emptying the note box on an
  article that has a note asks "Delete note?" before deleting it, and `E` asks
  "Replace export?" when the export file already exists (the file is rewritten
  from scratch, so hand edits would be lost). A plain `y` confirms; any other
  key cancels.
- **Pressing `s` on an article that is already saved no longer moves its save
  date forward.**
- **`E` now tells you when you are in the wrong view.** Export only works from
  the Saved view (`S`); pressing `E` anywhere else used to do nothing, which
  looked like a broken key. It now shows "Export works from the Saved view:
  press S, then E". The sidebar legend reads "export (S view)".
- **The saved-articles export (`E`) now has two sections**: saved articles
  without a note, as a short list, then the notes in full, newest first, with
  counts. A new `include_unnoted` setting in `export.toml` (default on) leaves
  the first section out when you only want your notes. The text is now built by
  the shared core crate, ready for the Android app to write the same file.

### Added

- **Groundwork for optional feed-list sync (core only, nothing to see yet).**
  The shared core can now mirror your feed list between devices through the
  same CouchDB server, so a feed you add on your phone can appear in the
  terminal app and the other way round. It has its own switch, off by default,
  apart from saved-article sync, because a feed list shows what you read. Feeds
  another device added are offered to the app to add; a feed another device
  *removed* is only listed, for the app to ask you about: nothing is ever
  removed by sync on its own. Not synced: renames, topic changes, Reddit
  sources. Migration 009 adds one small bookkeeping table; back up your
  database before first running a build that contains it. The terminal app and
  Android parts follow.

- **Optional CouchDB sync in the terminal app.** `tuxwire sync` copies saved
  articles and notes to and from a CouchDB server you run; `tuxwire sync --test`
  only checks the connection; `c` in the reader runs one sync and refreshes the
  list. Set it up in `~/.config/tuxwire/sync.toml` (see `tui/sync.toml.example`).
  It is off by default, never runs by itself, and the password stays out of
  `sync.toml`: it comes from `TUXWIRE_SYNC_PASSWORD` or a `password_file` that
  only you can read (anything more open is refused).

- **Groundwork for optional CouchDB sync (core only, nothing to see yet).** The
  shared core can now share saved articles and notes between devices through a
  CouchDB server you run yourself. It is off by default and has no screen yet, so
  nothing changes for you until the Android and terminal parts land. It insists
  on `https://` (plain `http://` only to this computer), keeps the password out
  of the database, logs and error messages, and does nothing at all while turned
  off. Migration 008 adds a change stamp to articles and a small bookkeeping
  table; back up your database before first running a build that contains it.
- **Un-save (`u`), always with a "are you sure?" first.** Saved something by
  mistake? `u` asks "Un-save?" and only a `y` does it. A note and a save are
  locked together: an article with a note shows a stronger warning (un-saving
  also deletes the note), and answering no tells you the note keeps it saved.
  Writing a note now saves the article too, so a note can never be hidden from
  the Saved view and the export. Core gains `Storage::unsave_article`.
- **`tuxwire import-opml FILE` and `tuxwire export-opml FILE`.** Bring a feed
  list in from another reader, or save yours, from the terminal. `--list NAME`
  picks one of your feed lists; otherwise the list you used last. Import adds
  only feeds you do not have yet and keeps the comments in `sources.toml`;
  export never overwrites an existing file (`-` prints to the screen). Nothing
  is downloaded: the new feeds' articles arrive the next time you open tuxwire.
- **Android: importing an OPML file now downloads the new feeds' articles** once
  at the end, so imported feeds do not look empty.

- **Paste a web page, get its feed.** Adding a source (`a` in the terminal app)
  now accepts a blog's address, not only its feed address. tuxwire reads the
  feed link the page advertises, or, if there is none, tries the usual
  addresses (`/feed`, `/rss.xml`, `/index.xml` and so on, at most 12 guesses).
  Comment feeds are skipped, and the confirm screen shows which address was
  found and how. Lives in `tuxwire-core` (`discover_feed`), so the Android
  app gets it too.

- **Faster refresh, and a time limit per feed.** Refreshing now downloads up to
  six feeds at the same time instead of one after another (results are still
  stored in order), and each feed has a 30-second limit, so a server that goes
  silent fails that one feed instead of stalling the whole refresh. Applies to
  the terminal app and the Android app alike.
- **OPML reader and writer in core** (`core/src/opml.rs`, used by the Android
  app's import/export; the terminal commands come later). Folders become
  topics, loose feeds go under "Uncategorized", Reddit sources export as their
  `.rss` address. Files are treated as untrusted: 2 MiB, 5000 feeds, 32 folder
  levels, `http`/`https` addresses only. No new dependency.
- **Multiple feed lists.** An optional `~/.config/tuxwire/lists.toml`
  defines named lists (e.g. "main" and "casual"), each a file in the
  `sources.toml` format. `f` opens a picker showing each list's name,
  file, and unread count, and switches instantly. Only the active list is
  fetched, at startup and on `r`, and `a` adds sources to it. The choice
  is remembered across restarts. A missing or broken list file is a footer
  message, and tuxwire falls back to another list instead of failing.
  Read, saved, and note state are shared across lists, and Saved, History,
  search, and export cover all of them. With no `lists.toml` (or one list)
  nothing changes.
- **Feed list path in the footer.** "Feed List: ~/.config/tuxwire/…" on the
  right of the footer (the file icon in Nerd mode), cut from the left to
  keep the file name visible, and hidden when there's no room or the
  terminal is under 60 columns. Key hints and notices keep priority.

- **Theme picker with eight built-in themes.** Press `t` to choose between
  Everforest (the new default), Dracula, Gruvbox, Catppuccin Mocha, Nord,
  Solarized Dark, Tokyo Night, and Catppuccin Macchiato (the original
  palette), listed alphabetically in the picker; the default doesn't
  depend on that order. The whole screen previews each one live, `Enter` keeps it,
  `Esc` cancels. The choice is saved as `base = "..."` in `theme.toml` and
  restored on startup. Colors in `theme.toml`'s `[theme]` table now act as
  overrides on top of the chosen theme, so existing full `theme.toml` files
  keep working unchanged. Every built-in is tested for readable text
  contrast and clearly distinct article-state colors.
- **Nerd Font icons, with Unicode and ASCII fallbacks.** Article rows get
  a state icon (unread, new this session, read, saved, skipped), sidebar
  topics get a category icon (built-ins for linux, rust, security, news,
  sports, and more; a folder for anything else), and `r`/startup report
  new articles, failed sources, and offline status in the footer. `i`
  cycles Nerd → Unicode → ASCII live and saves the choice to a new
  `~/.config/tuxwire/icons.toml`, which also takes custom per-topic icons
  under `[categories]`. The first launch asks "Do you see an icon here?"
  and falls back to Unicode if not. Only single-cell (Nerd Font *Mono*)
  glyphs are used, so borders and columns stay aligned.

### Changed

- **Database migration 007** adds a `summary` column to articles: a short plain-text
  summary taken from each feed entry (HTML removed, cut at the end of a sentence).
  Older articles get theirs the next time their feed is fetched. The terminal app
  stores it but does not show it yet; the Android app's article screen does.
- **One shared fetch step.** The startup fetch and the `r` refresh used to each
  handle fetch results themselves. Both now use `fetchers::fetch_one` and
  `FetchReport::record_outcome` from `tuxwire-core`, which the Android app uses
  too. Nothing changes for users: same messages, same counts.
- **Database migration 006** adds two new tables, `feeds` and `settings`, for
  the Android app (the terminal app doesn't read them). Nothing existing changes.
- **The repository is now a two-crate Cargo workspace.** The shared, terminal-free
  logic (articles, SQLite storage and migrations, fetching, the fetch report, the
  icon table, and the eight built-in theme palettes) moved into `core/`
  (`tuxwire-core`); the terminal program stays in `tui/`. This is groundwork for a
  native Android app that reuses `core/`. Nothing changes for users: same
  commands (`cargo build`, `cargo run`), same config files, same database, same
  UI. Under the hood, `Storage::open()` became `Storage::open_at(path)` (the
  terminal app works out the XDG path itself) and theme palettes are plain RGB in
  `core/`, converted to ratatui colors in `tui/`.

- **Database migration 005** adds a `feed_url` column to articles (which
  feed each came from), filled in for older articles where possible.
  Nothing else in the database changes.
- **Default theme is now Everforest** (was Catppuccin Macchiato) when no
  `theme.toml` exists. Users with their own `theme.toml` see no change, and
  Macchiato is still one keypress away in the picker.

### Fixed

- A `theme.toml` hex color containing non-ASCII characters (e.g. `"€€"`)
  crashed tuxwire at startup. It now gives a clear error instead.
