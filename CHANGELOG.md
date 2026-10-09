# Changelog

Notable changes to tuxwire, in the same voice as the project's own commit
messages — what changed and why, not just what.

## Unreleased

### Added

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

- **Database migration 005** adds a `feed_url` column to articles (which
  feed each came from), filled in for older articles where possible.
  Nothing else in the database changes.
- **Default theme is now Everforest** (was Catppuccin Macchiato) when no
  `theme.toml` exists. Users with their own `theme.toml` see no change, and
  Macchiato is still one keypress away in the picker.

### Fixed

- A `theme.toml` hex color containing non-ASCII characters (e.g. `"€€"`)
  crashed tuxwire at startup. It now gives a clear error instead.
