# Jian

A native macOS Markdown editor built with GPUI Kit.

Jian uses a source-first document model. Inactive blocks render as Markdown;
clicking a block edits its original source. A full-document source mode is also
available.

## Run

Requires macOS and stable Rust.

```sh
cargo run
cargo run -- path/to/note.md
```

## Features

- Native open-file and open-folder dialogs.
- Block-level and full-document Markdown editing with autosave.
- Nearest-ancestor `.jian` workspace discovery and a collapsible file sidebar.
- Filename filtering and on-demand backlinks for standard Markdown links.
- Native print dialog with Save as PDF support and optional print CSS.
- Native titlebar filename and dirty marker, close/quit saving, and external
  modification detection.

## Workspace configuration

Attachments default to `assets/`. Override the directory in the optional
`.jian/config.toml`:

```toml
assets_dir = "Attachments"
```

Print uses `.jian/print.css` when present and falls back to the built-in
stylesheet otherwise.

## License

MIT OR Apache-2.0. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).
