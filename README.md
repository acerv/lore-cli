# lore-cli

A terminal UI for browsing patches from a [lore](https://lore.kernel.org) /
[public-inbox](https://public-inbox.org) server, written in Rust with
[ratatui](https://ratatui.rs).

## Features

- Browse the patches of a configured mailing list.
- Color-coded status:
  - **green** — merged (a thread reply contains "Merged, thanks").
  - **yellow** — reviewed but not merged (a `Reviewed-by:` trailer exists).
  - **default** — neither merged nor reviewed.
- Open a patch to read the whole thread in a tab.

## Configuration

Copy `config.example.toml` to `config.toml` and set the server and project:

```toml
[lore]
server  = "https://lore.kernel.org"
project = "amd-gfx"
```

## Usage

```sh
cargo run --release            # uses ./config.toml
cargo run --release -- --config /path/to/config.toml
```

## Key bindings

| Key                 | Action                          |
| ------------------- | ------------------------------- |
| Up / Down           | move selection / scroll a line  |
| Enter               | open the selected thread        |
| q                   | close tab (or quit on the list) |
| Ctrl+n / Ctrl+p     | next / previous tab             |
| Ctrl+d / Ctrl+u     | fast scroll (half page)         |

## Status

Work in progress. See `PLAN.md` for the design and roadmap.
