# lore-cli — Plan

A Rust terminal app (ratatui) to browse patches from a lore / public-inbox server
(e.g. lore.kernel.org), color-coded by merge/review status, with a tabbed thread reader.

## 1. Goals (from requirements)

- Written in Rust, terminal UI with `ratatui`.
- `config.toml` selects the lore server and the project (mailing list) to browse.
- Patch list view with status colors:
  - Green  = merged  (a thread email contains the text "Merged, thanks").
  - Yellow = reviewed but not merged (a "Reviewed-by:" trailer in the root patch
    or in any thread email).
  - Normal = neither merged nor reviewed.
- Selecting a subject opens the whole thread in a tab:
  - `q` closes the tab.
  - Up / Down scroll one line; Ctrl+u / Ctrl+d fast scroll (half page).
  - Multiple tabs allowed; Ctrl+n = next tab, Ctrl+p = previous tab.
- Everything lives under `~/Projects/lore-cli`.

## 2. Lore / public-inbox HTTP API (no official docs; verified endpoints)

- List patch roots, newest first, as an Atom feed:
  `/<list>/?x=A&q=rt:..+AND+NOT+s:Re:&o=<offset>`
  - `NOT s:Re:` drops replies, leaving series roots / standalone patches.
  - `rt:..` is a no-op time filter (the API rejects a bare `q`); pagination via
    `o=` in steps of 200 (max 200 entries per page).
  - Atom entry gives: subject (title), author name/email, updated time, and a
    link containing the Message-ID.
- Full thread as gzipped mbox (all emails + bodies):
  `/<list>/<message-id>/t.mbox.gz`
  - Used both to render the thread and to detect merged / reviewed status.
- Alternatives if needed: `/<list>/<message-id>/t.atom` (thread metadata),
  `/<list>/<message-id>/raw` (single message), `/<list>/_/text/help` (search help).

## 3. Tech stack (crates)

- `ratatui` + `crossterm` — TUI + backend/input (crossterm `event-stream`).
- `tokio` — async runtime (multi-thread, macros, sync, time).
- `reqwest` — HTTP client (rustls TLS).
- `flate2` — gunzip the `t.mbox.gz` body (not HTTP content-encoding).
- `feed-rs` — parse Atom feeds (list + thread metadata).
- `mailparse` — parse each mbox message (headers + body).
- `serde` + `toml` — config file.
- `anyhow` — error handling; `chrono` — date formatting.
- `directories` (optional) — locate a cache dir; otherwise cache under the project.

## 4. Config file — `config.toml`

```toml
[lore]
server  = "https://lore.kernel.org"   # base URL of the lore/public-inbox server
project = "amd-gfx"                    # mailing list / inbox name

[ui]
page_size          = 200   # patches fetched per page (max 200)
status_concurrency = 6     # parallel thread fetches for status detection
```

- Loaded from `--config <path>` or `./config.toml` (a `config.example.toml` is shipped).

## 5. Project structure

```
~/Projects/lore-cli/
├── Cargo.toml
├── config.example.toml
├── PLAN.md
├── README.md
└── src/
    ├── main.rs        # CLI args, load config, start tokio + app
    ├── config.rs      # Config structs + loader
    ├── model.rs       # PatchEntry, Email, ThreadTab, PatchStatus
    ├── lore/
    │   ├── mod.rs     # LoreClient (reqwest), URL builders
    │   ├── atom.rs    # Atom feed -> Vec<PatchEntry>
    │   ├── mbox.rs    # gunzip + split mbox -> Vec<Email>
    │   └── status.rs  # emails -> PatchStatus (merged / reviewed / normal)
    ├── cache.rs       # on-disk cache of thread mboxes + computed status
    ├── app.rs         # App state, event handling, update logic
    ├── event.rs       # input -> Action mapping (key bindings)
    └── ui.rs          # rendering: tab bar, list view, thread view, help bar
```

## 6. Data model

```rust
enum PatchStatus { Unknown, Normal, Reviewed, Merged }

struct PatchEntry {
    subject: String,
    author_name: String,
    author_email: String,
    message_id: String,       // no angle brackets
    updated: DateTime<Utc>,
    status: PatchStatus,      // starts Unknown, filled in async
}

struct Email {
    from: String,
    date: String,
    subject: String,
    message_id: String,
    in_reply_to: Option<String>,
    body: String,
}

struct ThreadTab {
    title: String,            // shortened subject
    message_id: String,
    emails: Vec<Email>,
    scroll: u16,
    loading: bool,
    error: Option<String>,
}
```

## 7. Status detection (`lore/status.rs`)

For the emails of a thread, in order:

- If any body contains "Merged, thanks" (case-insensitive) => `Merged` (green).
- Else if any body has a line trailer `Reviewed-by:` (case-insensitive, line start)
  => `Reviewed` (yellow).
- Else => `Normal`.

Note: this is a text heuristic; a quoted "Merged, thanks" could produce a false
positive. Acceptable for v1; can be refined later (e.g. ignore quoted `>` lines).

## 8. UI & interaction

Layout: a top tab bar, a main body, and a bottom help/status line.

- Tab 0 is always the **Patches** list; thread tabs are appended after it.
  Ctrl+n / Ctrl+p cycle through all tabs (list + threads). This satisfies
  "move between tabs" and gives a natural way back to the list.

Patch list view:
- Table: status marker, subject, author, relative date.
- Row/subject color: green=Merged, yellow=Reviewed, default=Normal,
  dim/gray=Unknown (status still loading).
- Up/Down move selection; PgUp/PgDn jump; Enter opens the thread in a new tab
  (or focuses an already-open tab for that thread).
- `q` (on the list tab) quits the app.

Thread view (tab):
- Renders each email: a colored header (From / Date / Subject) then the body,
  with quoted `>` lines dimmed. Scrollable `Paragraph`.
- Up/Down scroll 1 line; Ctrl+d / Ctrl+u scroll half a page; PgDn/PgUp full page;
  Home/End jump to top/bottom.
- `q` closes the current thread tab (focus falls back to the previous tab).

Global:
- Bottom help bar shows the active key bindings.
- `?` toggles a help overlay (optional, nice-to-have).

## 9. Async data flow

- Runtime: `tokio`; the UI loop uses `tokio::select!` over:
  - crossterm `EventStream` (keyboard),
  - an mpsc `Rx` for async results (patch list loaded, status updates,
    thread loaded / failed),
  - a periodic tick for redraws.
- Startup: fetch page 0 of the list -> populate `Vec<PatchEntry>` (status=Unknown)
  -> render immediately.
- Status fill-in: spawn bounded tasks (semaphore = `status_concurrency`) to fetch
  each patch's `t.mbox.gz`, compute status, and send `(message_id, status)` back;
  rows recolor as results arrive.
- Open thread: reuse cached emails if present; otherwise show a "loading" tab and
  fetch in the background, then populate.
- Caching (`cache.rs`): store thread mbox bytes + computed status keyed by
  Message-ID to avoid refetching within/between runs; bounds request volume and
  is polite to the server (proper User-Agent, bounded concurrency).

## 10. Implementation phases

1. Scaffold Cargo project, dependencies, `config.example.toml`, `README.md`.
2. `config.rs` — load/validate config.
3. `lore` client — build URLs, fetch + parse the Atom list into `PatchEntry`s.
4. `lore/mbox.rs` — gunzip + parse thread mbox into `Email`s.
5. `lore/status.rs` — merged / reviewed / normal detection (+ unit tests).
6. `app.rs` + `event.rs` — state, key bindings, async event loop (no network yet).
7. `ui.rs` — patch list view with status colors + help bar.
8. Thread tabs — open/close, Ctrl+n/Ctrl+p, scrolling (Up/Down, Ctrl+u/Ctrl+d).
9. Background status fetching (bounded concurrency) + on-disk cache.
10. Polish — error/loading states, empty states, pagination ("load more").
11. Manual test against a real list; write `README.md` usage.

## 11. Risks / edge cases

- Rate limiting: mitigate with bounded concurrency, caching, and a clear
  User-Agent. Fetching status for every visible row is the heaviest cost.
- Merge/review heuristic false positives (quoted text) — documented; refine later.
- mbox parsing quirks: `From ` line escaping, MIME/multipart bodies, non-UTF-8
  charsets — decode defensively, fall back to lossy UTF-8.
- Very long threads: cap rendered lines or lazily render; ensure smooth scroll.
- Terminal resize and Unicode width handling in ratatui.

## 12. Decisions to confirm

- Default server/project for `config.example.toml` (proposal: lore.kernel.org +
  a moderate-traffic list such as `amd-gfx` or `linux-kselftest`).
- "List as first tab" model for tab navigation (recommended) vs. a separate
  full-screen list with an overlay for tabs.
- Case-insensitive matching for "Merged, thanks" and `Reviewed-by:` (recommended)
  vs. exact case.
