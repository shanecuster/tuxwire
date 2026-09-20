# tuxwire

A terminal-based RSS/Atom reader for people who take notes while they read.

tuxwire pulls articles from any feeds you choose — Linux news, security
advisories, your own blog, or anything else with an RSS feed — into a
single fast, themeable terminal interface. Save the articles that matter,
write a note explaining *why* they mattered, and find them again later —
without ever leaving the terminal.

<p align="center">
  <img src="docs/screenshots/full_app_with_articles.png" alt="tuxwire main view — sidebar, article list, banner" width="900">
</p>

---

## Why tuxwire

Most RSS readers are either a GUI app or a terminal reader with no memory
of *why* you saved something. tuxwire exists for the gap in between: a
fast, keyboard-driven reader that also lets you jot down a thought next
to an article — "try this on mangrove," "reference this for the blog
post" — and actually find that note again days or weeks later.

---

## Features

### Multi-source aggregation, fully editable
Pull from as many RSS/Atom feeds as you want, grouped into topics you
define yourself — not a fixed category list. Add a source by pressing `a`
inside the app (paste a feed URL, tuxwire validates it, pick or type a
topic), or edit `~/.config/tuxwire/sources.toml` directly in a text
editor. Either way, the file is yours to read and change at any time.

### Expandable topic tree
The sidebar isn't a flat list — each topic can be expanded to show the
individual sources feeding it. Select a topic to see everything in that
category, or drill into one specific source to see just its articles.

<p align="center">
  <img src="docs/screenshots/category_tree.png" alt="Expanded topic tree showing nested sources" width="500">
</p>

### Save articles with notes
Press `s` to save an article permanently (it also marks it read). Press
`n` to attach a note — a short reminder of why it mattered. Notes are
edited in a simple popup, pre-filled if one already exists. tuxwire
tracks both *when you saved* an article and *when you last wrote or
edited its note* as separate dates, since those are often different
moments.

<p align="center">
  <img src="docs/screenshots/notes_window.png" alt="Note-editing popup" width="700">
</p>

### A persistent, always-visible legend
No need to memorize what a key does or what a color means. The sidebar
permanently displays the active keybinds and a color swatch next to each
article state (unread, read, saved, skipped) — pulled live from your
actual theme, so it never drifts out of sync with what you're seeing.

<p align="center">
  <img src="docs/screenshots/sidebar.png" alt="Sidebar showing Topics / Keys / Colors sections" width="400">
</p>

### Saved view, History view, and search
- `S` — every saved article across all topics, with source, save date,
  and a note preview.
- `y` — every article tuxwire has ever fetched, regardless of state —
  useful for finding something you read but forgot to save.
- `/` — live substring search across titles and notes, filtering
  whichever view is currently open.

<p align="center">
  <img src="docs/screenshots/saved_view.png" alt="Saved view with note previews" width="900">
</p>

### In-terminal reading via w3m, with automatic fallback
If [w3m](https://w3m.sourceforge.net/) is installed, pressing `Enter`
opens the article right inside your terminal — no context switch to a
browser. If w3m isn't installed, tuxwire automatically falls back to
your regular default browser instead. Nothing breaks either way.

<p align="center">
  <img src="docs/screenshots/article_open_w3m.png" alt="Reading an article in-terminal via w3m" width="900">
</p>

### Export your saved articles to Markdown
Press `E` from the Saved view to export everything you've saved — title,
source link, save/note dates, and your note — into a single Markdown
file. Configurable output location, useful for feeding notes straight
into a blog post or personal wiki.

### Catppuccin Macchiato theming, fully editable
Dark, low-saturation, easy on the eyes by default — and every color is a
line in `~/.config/tuxwire/theme.toml`, not hardcoded. Don't like a
shade? Change the hex value and restart.

### Cross-platform
Runs anywhere Rust runs — tested on desktop Linux and on Android via
[Termux](https://termux.dev/). Same codebase, same database format,
same features.

---

## Installation

tuxwire isn't published to any package manager yet — for now, build it
from source. This is more approachable than it sounds; the steps below
assume no prior experience.

### 1. Install Rust

If you don't already have Rust installed:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Follow the prompts (the defaults are fine), then restart your terminal
or run:

```bash
source "$HOME/.cargo/env"
```

Confirm it worked:

```bash
cargo --version
```

### 2. Install a C compiler toolchain

Needed because some dependencies compile C code as part of the build.

| Distro | Command |
|---|---|
| Debian / Ubuntu | `sudo apt install build-essential pkg-config` |
| Arch / CachyOS / Manjaro | `sudo pacman -S base-devel pkg-config` |
| Fedora | `sudo dnf groupinstall "Development Tools"` |
| macOS | `xcode-select --install` |

### 3. (Optional but recommended) Install w3m

For the best experience — in-terminal article reading instead of
switching to a browser:

| Distro | Command |
|---|---|
| Debian / Ubuntu | `sudo apt install w3m` |
| Arch / CachyOS / Manjaro | `sudo pacman -S w3m` |
| Fedora | `sudo dnf install w3m` |
| macOS (Homebrew) | `brew install w3m` |

**Not required.** tuxwire works completely fine without it — articles
just open in your regular browser instead.

### 4. Clone and build

```bash
git clone https://github.com/shanecuster/tuxwire.git
cd tuxwire
cargo build --release
```

The first build compiles every dependency from scratch and may take a
few minutes. The compiled binary lands at `target/release/tuxwire`.

### 5. Make it runnable from anywhere

```bash
mkdir -p ~/.local/bin
ln -s "$(pwd)/target/release/tuxwire" ~/.local/bin/tuxwire
```

Make sure `~/.local/bin` is on your `$PATH` (most modern distros include
it by default). Check with:

```bash
echo $PATH
```

If it's missing, add this to your shell config (`~/.bashrc`, `~/.zshrc`,
or `~/.config/fish/config.fish`):

```bash
export PATH="$HOME/.local/bin:$PATH"
```

After that, restart your terminal and just run:

```bash
tuxwire
```

---

## Running on Android (Termux)

tuxwire runs natively inside [Termux](https://termux.dev/) — a real
Linux environment on Android, not a special Android app.

```bash
pkg update && pkg upgrade
pkg install git rust sqlite pkg-config openssl
```

Clone and build the same way as desktop:

```bash
git clone https://github.com/shanecuster/tuxwire.git
cd tuxwire
cargo build --release
cargo run
```

**To open articles in your phone's browser**, install the
[Termux:API](https://wiki.termux.com/wiki/Termux:API) companion app
(from the same source — F-Droid or Play Store — as Termux itself), then:

```bash
pkg install termux-api
echo 'export BROWSER=termux-open-url' >> ~/.bashrc
source ~/.bashrc
```

`w3m` isn't recommended on a phone screen — the fallback to your regular
browser is the better fit for Android.

---

## Usage

Launch it:

```bash
tuxwire
```

<p align="center">
  <img src="docs/screenshots/main_topic_view.png" alt="Browsing topics and articles" width="900">
</p>

### Keybindings

| Key | Action |
|---|---|
| `j` / `k` | Navigate the article list |
| `Up` / `Down` / `Tab` | Navigate the topic sidebar |
| `Right` / `l` | Expand the selected topic (show its sources) |
| `Left` / `h` | Collapse the selected topic |
| `Enter` | Open the selected article (`w3m`, or your browser as fallback) |
| `s` | Save the current article (also marks it read) |
| `n` | Add or edit a note on a saved article |
| `x` | Mark an article as skipped |
| `r` | Refresh the current topic's sources |
| `S` | View saved articles |
| `y` | View history (every article ever fetched) |
| `/` | Search titles and notes in the current view |
| `a` | Add a new source |
| `E` | Export all saved articles to Markdown |
| `q` | Quit |

---

## Configuration

tuxwire reads three config files from `~/.config/tuxwire/`, generating
sensible defaults on first run if they don't exist yet.

### `sources.toml` — your feeds

```toml
[[source]]
name = "Arch Linux News"
type = "rss"
url = "https://archlinux.org/feeds/news/"
topic = "linux"
```

- `topic` is required on every source, and each source belongs to
  **exactly one** topic — keeps the sidebar unambiguous.
- Add sources by editing this file directly, or press `a` in-app to add
  one interactively (it validates the feed URL before saving).

### `theme.toml` — colors

```toml
[theme]
background   = "#24273a"
accent_unread   = "#f5a97f"
accent_read     = "#8bd5ca"
accent_saved    = "#eed49f"
accent_skipped  = "#c6a0f6"
```

Ships with [Catppuccin Macchiato](https://github.com/catppuccin/catppuccin)
by default. Change any value and restart — nothing is hardcoded.

### `export.toml` — where exports go

```toml
[export]
path = "~/tuxwire-notes/"
```

Change `path` to wherever you'd like your exported Markdown notes to
land — a synced folder, a notes vault, anywhere you have write access.

---

## FreshRSS Sync (optional)

If you already run a [FreshRSS](https://freshrss.org/) server, tuxwire
can pull your subscriptions in, keep read state in sync with the server,
and add new feeds to FreshRSS instead of (or alongside) your local
`sources.toml`. Entirely optional and off by default — with the feature
not compiled in, or `freshrss.toml` absent/disabled, tuxwire behaves
exactly as described everywhere else in this README.

### Building with FreshRSS support

```bash
cargo build --release --features freshrss
```

A plain `cargo build --release` (no flags) never compiles any of this
in — the `freshrss` feature is off by default.

### Server setup

FreshRSS's sync API is off by default and needs a password separate from
your normal login:

1. In FreshRSS, go to **Settings → Authentication** and enable **Allow
   API access**.
2. Under **Settings → Profile**, set an **API password**. This is not
   the same as your account login password — it's a second, API-only
   credential.

### `freshrss.toml`

Create `~/.config/tuxwire/freshrss.toml` (see
[`freshrss.toml.example`](freshrss.toml.example) for a template):

```toml
[freshrss]
enabled       = true
url           = "https://freshrss.example.com"   # base URL only, no /api/greader.php
username      = "your-username"
sync_on_start = true
max_items_per_sync = 500
push_read_state    = true
password_file = "~/.config/tuxwire/freshrss.pass"
```

| Key | Meaning |
|---|---|
| `enabled` | Master on/off switch. `false` (or the file missing entirely) behaves identically to a build without `--features freshrss`. |
| `url` | Your FreshRSS server's base URL — no `/api/greader.php` suffix. |
| `username` | Your FreshRSS account name. |
| `sync_on_start` | Sync automatically in the background when tuxwire opens, in addition to `r` on a `FreshRSS/*` topic. Defaults to `true`. |
| `max_items_per_sync` | The most items any one sync fetches and stores, even if more are waiting — a first sync against thousands of unread items backfills gradually across several syncs instead of pulling everything at once. Defaults to `500`, must be between `1` and `10,000`. |
| `push_read_state` | Whether a sync sends anything back to FreshRSS at all (marking items you've read locally as read on the server). `false` leaves the pull direction untouched — the server's own read state still flows in — it only stops tuxwire from writing back. Defaults to `true`. |
| `password_file` | Path to a file containing *only* the API password (`chmod 600` — tuxwire warns if it's readable by group/others). |

**The API password never goes directly in `freshrss.toml`.** Provide it
one of two ways:
- `password_file`, above, or
- the `TUXWIRE_FRESHRSS_PASSWORD` environment variable, which takes
  precedence over `password_file` when both are set (handy for
  overriding a shared/committed `freshrss.toml` for one run, e.g.
  against a throwaway test server).

### Using it

Once configured, FreshRSS feeds show up in the sidebar under
`FreshRSS/<category>` topics (`FreshRSS/Uncategorized` for anything
without one) after the first sync. `r` on a `FreshRSS/*` topic triggers a
sync the same way `r` refreshes a local topic's sources.

Pressing `a` to add a feed gains a **Destination: Local / FreshRSS**
choice on the confirm screen when FreshRSS is enabled — choosing
FreshRSS subscribes the feed on the server (via `quickadd`) instead of
writing to `sources.toml`, and skips the name/topic prompts entirely
(FreshRSS assigns neither; the feed lands in `FreshRSS/Uncategorized`
once it has synced articles). **Adding a feed this way is not affected
by `push_read_state`** — it's a direct action you just took, not an
automatic side effect of read-state changes, so it always goes through
regardless of that setting.

---

## Uninstalling

```bash
rm ~/.local/bin/tuxwire
rm -rf ~/.config/tuxwire ~/.local/share/tuxwire
rm -rf /path/to/cloned/tuxwire
```

The middle command removes your config and saved-article database —
skip it if you want to keep your data around.

---

## Roadmap

tuxwire is under active development. See
[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) for the full design
history, what's built, what's planned, and what was deliberately left
out (and why).

---

## Contributing

This project is under active development and personal use. Issues and
pull requests are welcome, but expect things to be in flux.

## License

Licensed under the [MIT License](LICENSE).
