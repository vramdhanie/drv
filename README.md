# drv

[![License: MIT](https://img.shields.io/github/license/vramdhanie/drv?color=green)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-DEA584?logo=rust)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-macOS-black?logo=apple)](https://github.com/vramdhanie/drv)

A fast command-line manager for Google Drive. List, share, copy, upload, and
download from the terminal — with local semantic search over your Drive's
contents and Claude-powered folder Q&A on the roadmap.

```
$ drv ls Projects --long
   12.4 KB  2026-09-10 14:02  id:1AbC…  notes.md
        -   2026-09-08 09:15  id:1DeF…  Designs/
$ drv share "Projects/notes.md" someone@example.com --role editor
$ drv upload report.pdf photo.jpg --to "Projects/Archive"
$ drv download "Projects/notes.md" --out ~/Desktop
```

## Install

Homebrew distribution is planned. For now, build from source:

```bash
cargo install --path .
```

## Authentication

`drv` talks directly to Google's API as **you** — it has no server, and your
files never pass through anything but your machine and Google.

```bash
drv auth login
```

This opens your browser for Google sign-in and stores the resulting refresh
token in the macOS Keychain. Two modes:

- **Built-in client** (release builds): just run `drv auth login`. You'll see
  Google's *"unverified app"* warning once — click *Advanced → continue*.
  This is normal for personal tools using restricted Drive scopes.
- **Bring your own client** (always available, and required when building
  from source without baked-in credentials):
  1. [console.cloud.google.com](https://console.cloud.google.com) → create a
     project → enable the **Google Drive API**
  2. *OAuth consent screen* → External → **publish to production**
     (apps left in "testing" mode expire their tokens every 7 days)
  3. *Credentials* → *Create credentials* → *OAuth client ID* → **Desktop app**
  4. `drv auth login --client-id <ID> --client-secret <SECRET>`

  The client ID is saved to `~/.config/drv/config.toml`; the secret and
  tokens go to the Keychain.

Distribution builds embed a default client at compile time:

```bash
DRV_CLIENT_ID=… DRV_CLIENT_SECRET=… cargo build --release
```

Check who you're signed in as (and storage usage) with `drv auth status`;
remove credentials with `drv auth logout`.

## Commands

| Command | What it does |
|---|---|
| `drv ls [PATH] [-r] [-l]` | List a folder; `-r` walks the tree, `-l` adds size/date/ID |
| `drv share PATH EMAIL [--role viewer\|commenter\|editor] [--notify]` | Grant access |
| `drv cp PATH [NEW_NAME] [--to FOLDER]` | Server-side duplicate |
| `drv upload FILE... [--to FOLDER]` | Upload with progress bars |
| `drv download PATH... [--out DIR]` | Download; Google-native docs export to docx/xlsx/pptx |
| `drv auth claude` | Store an Anthropic API key for the v0.2 features |

Paths are resolved from your Drive root (`"Projects/Notes/todo.txt"`); use
`id:<fileId>` anywhere a path is accepted to address a file directly.

## Roadmap

- **v0.2** — `drv index`: local semantic index of your Drive's contents
  (on-device embeddings — nothing leaves your machine; incremental updates
  via the Drive changes API). `drv search`: semantic search across the whole
  Drive. `drv prompt`: ask Claude questions scoped to a folder's contents.
- **v0.3** — Homebrew tap (`brew install vramdhanie/tap/drv`), recursive
  download/upload of folders.

## License

[MIT](LICENSE)
