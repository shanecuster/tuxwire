# Changelog

Notable changes to tuxwire, in the same voice as the project's own commit
messages — what changed and why, not just what.

## Unreleased

### Added

- **Theme picker with eight built-in themes.** Press `t` to choose between
  Everforest (the new default), Dracula, Gruvbox, Catppuccin Mocha, Nord,
  Solarized Dark, Tokyo Night, and Catppuccin Macchiato (the original
  palette). The whole screen previews each one live, `Enter` keeps it,
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

- **Default theme is now Everforest** (was Catppuccin Macchiato) when no
  `theme.toml` exists. Users with their own `theme.toml` see no change, and
  Macchiato is still one keypress away in the picker.

### Fixed

- A `theme.toml` hex color containing non-ASCII characters (e.g. `"€€"`)
  crashed tuxwire at startup. It now gives a clear error instead.
