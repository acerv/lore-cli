# AGENTS.md

Guidance for AI agents (and humans pairing with them) working on this codebase.
Read this before editing. The user-facing project overview lives in `README.md`.

## What this is

`lore-cli` is a ratatui TUI for browsing patches on a lore/public-inbox server
(default: lore.kernel.org). It lists a project's thread roots newest-first,
colors them by merge/review status (probed async from each thread's mbox), and
opens threads in closable tabs. Rust 2021, ratatui 0.29, crossterm, tokio,
reqwest, feed-rs, flate2, mailparse.

## Commands

```sh
cargo build                       # build
cargo build --release             # optimized build
cargo test                        # unit + render tests (default)
cargo test -- --ignored           # also run the live network smoke test
cargo run --release               # run with ./config.toml
cargo run -- --config <path>      # run with a specific config
```

There is no configured linter or formatter; `cargo clippy` is available but
treat its style suggestions as advisory and match surrounding code.

## Source map

```
src/
├── main.rs        CLI args, tokio runtime, terminal setup, event loop, channel wiring
├── config.rs      Config TOML structs + loader
├── model.rs       PatchEntry, Email, PatchStatus enum
├── app.rs         App state, state machine, all key handling, async fetch coordination
├── event.rs       AppEvent enum (Input / Tick / PatchesLoaded / MoreLoaded / StatusUpdated / ThreadLoaded)
├── ui.rs          All rendering: tab bar, patch list, thread view, status bar
├── cache.rs       On-disk cache for decompressed thread mboxes
├── series.rs      Patch-set grouping + superseded-flags logic
└── lore/
    ├── mod.rs     LoreClient: HTTP fetch + URL builders
    ├── atom.rs    Atom feed -> Vec<PatchEntry>
    ├── mbox.rs    gunzip + mbox split -> Vec<Email>
    └── status.rs  threads -> PatchStatus (merged/reviewed/normal)
```

When you edit `ui.rs`, you almost always also need `app.rs`. When you change
data flow, check `event.rs` first.

## Architecture & key invariants

### Event-driven core

Everything flows through one `mpsc::unbounded_channel<AppEvent>`:
- A **blocking** `spawn_input_reader` thread forwards crossterm events as
  `AppEvent::Input`.
- A `spawn_ticker` task emits `AppEvent::Tick` every 250 ms (drives spinners).
- Async fetches become `PatchesLoaded` / `MoreLoaded` / `StatusUpdated` /
  `ThreadLoaded`.

`main.rs::run` processes one event, re-renders *unconditionally*, then calls
`app.probe_visible()`. **Do not** gate re-renders on "did something change";
the redraw after every event is intentional and cheap. State mutations in
event handlers (`on_*`, `handle_crossterm`) take effect on the next redraw.

### App state grouping

`App::patches: Vec<PatchEntry>` is the source of truth, newest first. From it
we derive:
- `groups: Vec<Group>` — patch-sets; a cover letter `[PATCH 0/N]` is the
  `head`, the parts `[PATCH 1/N..]` are `children`. Standalone patches are
  groups with empty `children`. Built by `series::group`.
- `superseded: Vec<bool>` — per-patch flag: a newer `[PATCH vN]` of the same
  title exists. Built by `series::superseded_flags`. Used both for styling
  (grey strikethrough) *and* for list filtering (see below).
- `rows: Vec<Row>` — the **flattened visible** list. One `Row` per visible
  line; expansion state comes from `expanded: HashSet<message_id>`.

The rebuild path is `rebuild_view_keeping(keep_id)`:
1. Recompute `superseded` + `groups` (cheap O(n); always from `patches`).
2. `rebuild_rows()` — respects expand/collapse, search, and `latest_only`.
3. `restore_selection(keep_id)` — keeps the same patch selected where possible
   and **always** clamps to `rows.len() - 1`.

**Invariant:** every code path that adds/removes patches or toggles a row must
go through `rebuild_view` / `rebuild_view_keeping` / explicit `rebuild_rows` +
`restore_selection`. Never mutate `rows` directly outside `rebuild_rows`.

### `rebuild_rows` filters, in this priority order

1. **Search active + non-empty query:** flat filtered list (subject substring,
   case-insensitive). Returns early — no group tree.
2. **Group tree:** for each group, push the head; if expanded, push children
   (depth=1) indented.

`latest_only` (toggled by the `N` key) is an additive filter applied *inside*
both paths via `is_hidden_by_latest(index)`. It hides a patch when:
- `superseded[index]` is true (an older version exists), OR
- `patches[index].status` is `Reviewed` or `Merged`.

When `latest_only` is on, groups are also forced collapsed (children never
shown regardless of `expanded`).

**Invariant:** `on_status_updated` must call `rebuild_view()` when
`latest_only && status changed`, so a patch whose status resolves to
Reviewed/Merged asynchronously drops out of the filtered list.

### Status probing

`probe_visible()` is called after every render. It schedules probes for rows
in the viewport + one screen of look-ahead whose status is still `Unknown` and
not already in `requested`. Concurrency is bounded by `status_sem` (size from
`config.ui.status_concurrency`). Probes retry up to `STATUS_FETCH_ATTEMPTS`
with `STATUS_RETRY_DELAY`. Decompressed threads live in `cache.rs`.

### Input routing

`handle_crossterm` filters to `KeyEventKind::Press`, then `handle_key`:
1. **Ctrl-modified** keys checked first; `Ctrl+n` / `Ctrl+p` switch tabs.
2. List tab (`active_tab == 0`) → `handle_list_key`, unless a search is being
   typed (`search_active`) → `handle_search_key`.
3. Thread tabs → `handle_thread_key`.

Note: `N` (uppercase, Shift+N) is the latest-only toggle; lowercase `n` is
unused. Patterns in `handle_list_key` match on `key.code`, so modifiers other
than Ctrl are ignored — `Shift+N` routes to `Char('N')` correctly.

### Rendering

`ui::render` lays out 4 vertical areas: tab bar, separator rule, body, status
bar. The status bar is hijacked by the search input while `search_active`.
Patch rows are built by `tree_row` with fixed-width columns (subject, 22-char
author, 10-char date). Colors come from `status_style`; the patch-set count
(`▸N` / `▾N`) renders red bold. `sanitize` strips control chars and expands
tabs to 8-column stops — never render raw control bytes (desyncs the display).

## Tests

`cargo test` runs unit tests in each module and render tests in `ui::tests`.
Render tests use `ratatui::backend::TestBackend` and assert on buffer colors /
modifiers / text via helpers like `row_has_fg`, `row_has_modifier`,
`buffer_text`. When adding UI-visible behavior, prefer adding a render test
to `ui::tests` **and** a state-machine test to `app::tests`.

`lore::live_tests::live_pipeline` is `#[ignore]` because it hits
lore.kernel.org; run it explicitly with `cargo test -- --ignored`.

## Conventions

- **Comments:** the codebase leans on doc comments and inline rationale
  comments; keep them when editing and add them for non-obvious logic.
- **Error handling:** `anyhow` everywhere; user-facing errors flow back as
  `Option<String>` on `App::error` / `ThreadTab::error`. Async fetch failures
  are non-fatal: `on_more_loaded` swallows errors and allows retry.
- **No file writes outside the cache dir** (`cache.rs`). Never write to the
  repo or home directory.
- **Naming:** `on_*` for event handlers, `spawn_*` for task launchers,
  `handle_*` for input dispatch, `select_*` for navigation, `rebuild_*` for
  row derivation.
- **Config:** anything user-tunable lives in `config.rs` with defaults via
  `#[serde(default)]`; the `[lore]` section is required, `[ui]` and `[status]`
  are optional.