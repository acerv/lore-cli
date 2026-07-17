# lore-cli

A terminal UI for browsing patches from a [lore](https://lore.kernel.org) /
[public-inbox](https://public-inbox.org) server, written in Rust with
[ratatui](https://ratatui.rs).

It lists a mailing list's patches, colors them by merge/review status, and opens
whole threads in closable, scrollable tabs.

## Features

- Browse the patches (thread roots) of a configured mailing list, newest first.
- Status colors, derived from each patch's thread:
  - **green** — merged (a non-quoted reply contains the configured merge marker,
    `"Merged, thanks"` by default).
  - **yellow** — reviewed but not merged (a `Reviewed-by:` trailer appears in the
    root patch or any reply).
  - **default** — neither merged nor reviewed.
  - **dim** — status still being probed (or undetermined).
- Open a patch to read the entire thread in its own tab, with reply nesting and
  light syntax coloring (quotes, trailers, diff lines).
- Multiple tabs; the patch list is always tab 0.
- Patch-set grouping: a cover letter (`[PATCH 0/N]`) folds its series patches
  beneath it (Space to expand/collapse); standalone patches open with Enter.
- Live search: press `/` to filter the list by subject as you type.
- Background status probing (visible rows first) with bounded concurrency and an
  on-disk cache.
- Incremental pagination: more patches load as you scroll to the bottom.

## Build

```sh
cargo build --release
```

Requires a recent stable Rust toolchain.

## Configuration

Copy `config.example.toml` to `config.toml` and adjust it:

```toml
[lore]
# Base URL of the lore / public-inbox server.
server  = "https://lore.kernel.org"
# Mailing list / inbox name (the path segment after the server).
project = "amd-gfx"

[ui]
page_size          = 200   # patches per page (200 = server maximum)
status_concurrency = 6     # parallel thread fetches for status detection

[status]
# Case-insensitive texts that mark a patch as merged (shown green). List every
# phrase your subsystem uses; a single string also works.
merged_markers = ["Merged, thanks", "Applied, thanks"]
```

Only the `[lore]` section is required; `[ui]` and `[status]` fall back to the
defaults shown above.

## Usage

```sh
cargo run --release                       # uses ./config.toml
cargo run --release -- --config PATH      # use a specific config file
./target/release/lore-cli --config PATH
```

## Key bindings

Patch list (tab 0):

| Key                       | Action                          |
| ------------------------- | ------------------------------- |
| Up / Down (k / j)         | move selection                  |
| Home / End (g / G)        | first / last                    |
| PageUp / PageDown         | jump 10                         |
| Ctrl+d / Ctrl+u           | half-page down / up             |
| Enter / → / l             | open the selected thread        |
| /                         | live search (Esc clears)        |
| R                         | refresh (check for new patches) |
| Space                     | expand / collapse a patch-set   |
| m                         | load more patches (auto at end) |
| Ctrl+n / Ctrl+p           | next / previous tab             |
| q / Esc                   | quit                            |

Thread tab:

| Key                       | Action                          |
| ------------------------- | ------------------------------- |
| Up / Down (k / j)         | scroll one line                 |
| Ctrl+d / Ctrl+u           | fast scroll (half page)         |
| PageDown / PageUp / Space | scroll a page                   |
| Home / End (g / G)        | top / bottom                    |
| Ctrl+n / Ctrl+p           | next / previous tab             |
| q / Esc                   | close the tab                   |

## How it works

- The patch list is the Atom feed
  `\<server\>/\<project\>/?x=A&q=rt:..+AND+NOT+s:Re:&o=\<offset\>`, which returns
  thread roots newest-first (`NOT s:Re:` drops replies), paginated by `o=`.
- A thread is the gzipped mbox `\<server\>/\<project\>/\<message-id\>/t.mbox.gz`,
  decompressed and parsed into individual emails for display and status
  detection.
- Decompressed threads are cached under the OS cache directory, e.g.
  `~/.cache/lore-cli/\<host\>/\<project\>/`.

### Note on merge detection

Whether a patch shows as merged depends entirely on maintainers writing the
configured `merged_markers`. Conventions vary widely between subsystems (some say
"Applied, thanks", some nothing at all), so list the phrases your list uses.
`Reviewed-by:` is a standard git trailer and is detected everywhere.

## Development

```sh
cargo test                 # unit + render tests
cargo test -- --ignored    # also run the live network smoke test
```

See `PLAN.md` for the original design and roadmap.
