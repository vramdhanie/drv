# drv

[![License: MIT](https://img.shields.io/github/license/vramdhanie/drv?color=green)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.85%2B-DEA584?logo=rust)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-macOS-black?logo=apple)](https://github.com/vramdhanie/drv)

A fast command-line manager for Google Drive. List, share, copy, upload, and
download from the terminal; index your Drive's contents with a local
embedding model for semantic search; and ask Claude questions answered from
your own files. Multiple Google accounts supported.

```
$ drv ls Projects --long
   12.4 KB  2026-09-10 14:02  id:1AbC…  notes.md
        -   2026-09-08 09:15  id:1DeF…  Designs/
$ drv share "Projects/notes.md" someone@example.com --role editor
$ drv upload report.pdf photo.jpg --to "Projects/Archive"
$ drv download "Projects/notes.md" --out ~/Desktop
$ drv mv "Inbox/report.pdf" "Projects/Archive/"
$ drv cat "Projects/notes.md"
$ drv vim "Projects/notes.md"
$ drv do "find all empty files in this folder and delete them" --in Projects
$ drv index --in "Baha'i" --in MSc
$ drv search "the contract that mentions early termination fees"
$ drv prompt --in "Projects" "what did we decide about the launch date?"
```

## Install

```bash
brew install vramdhanie/tap/drv
```

Or build from source:

```bash
make install     # cargo install + code-sign (falls back to unsigned)
```

`make install` signs the binary with a local self-signed certificate
(`drv-signing`) when one exists, so macOS Keychain "Always Allow"
approvals survive rebuilds — an unsigned binary gets a new identity on
every build and re-prompts. One-time setup: in Keychain Access, create
a Self-Signed Root certificate of type Code Signing named `drv-signing`
and set its Code Signing trust to Always Trust.

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

### Multiple accounts

Run `drv auth login` once per Google account (add `--as work` to pick a
short alias; the default alias is the account's email). Then:

- `drv account list` — show accounts (● marks the active one)
- `drv account use work` — switch the default
- any command takes `-a`/`--account` to target a specific account:
  `drv -a personal ls`, `drv -a work search "quarterly report"`

Each account keeps its own Keychain tokens and its own local search index.

## Commands

| Command | What it does |
|---|---|
| `drv ls [PATH] [-r] [-l]` | List a folder; `-r` walks the tree, `-l` adds size/date/ID |
| `drv share PATH EMAIL [--role viewer\|commenter\|editor] [--notify]` | Grant access |
| `drv cp PATH [NEW_NAME] [--to FOLDER]` | Server-side duplicate |
| `drv upload FILE... [--to FOLDER]` | Upload with progress bars |
| `drv download PATH... [--out DIR]` | Download; Google-native docs export to docx/xlsx/pptx |
| `drv mv SOURCE DEST` | Move and/or rename (folder dest moves into it; new leaf renames) |
| `drv rm PATH...` | Move to Trash — never a permanent delete |
| `drv cat PATH` | Print contents; Docs/Sheets export as text/CSV, binary streams when redirected |
| `drv edit PATH` (alias `vim`) | Edit in $EDITOR, saved back to Drive only if changed |
| `drv browse` | Interactive browser: fuzzy navigation, multi-select + actions, shared/link markers |
| `drv do "INSTRUCTION" [--in FOLDER] [--dry-run]` | Natural-language tasks: plan shown, run on confirm |
| `drv index [--in FOLDER]... [--threads N] [--max-mem GB]` | Build/refresh the local semantic index |
| `drv search QUERY [--in FOLDER] [-n N]` | Semantic search over indexed content |
| `drv prompt QUESTION [--in FOLDER]` | Ask Claude, answered from your files with citations |
| `drv account list` / `use ALIAS` | Manage multiple Google accounts |
| `drv auth claude` | Store an Anthropic API key (optional — see below) |

Paths are resolved from your Drive root (`"Projects/Notes/todo.txt"`); use
`id:<fileId>` anywhere a path is accepted to address a file directly.

## Natural-language tasks

`drv do` turns an instruction — *"consolidate '/Personal - NEW/Nirav' into
'/Personal Home/Nirav'"*, *"prefix each file here with its created date"* —
into a concrete plan over a safe vocabulary (rename, move, copy, share,
download, trash, new folders). Claude explores folders it needs to see,
then the full plan is printed and **nothing runs until you confirm**
(`--dry-run` to only look, `--yes` to skip the prompt). Deletion only ever
means the recoverable Trash, and plans can only touch items in the folders
that were actually listed.

## Interactive browsing

`drv browse` starts at your Drive root: type to fuzzy-filter, Enter opens a
folder, `..` walks up, Esc leaves. Shared items are marked, and shortcuts
are labelled as links whose content lives elsewhere. `select multiple…`
ticks a set of files for a batch action — download, share, move (with an
interactive destination picker), or trash.

## How the semantic index works

`drv index` crawls your Drive's metadata (first run) or applies deltas from
the Drive **changes feed** (every later run — seconds, not minutes), then
extracts text from Google Docs/Sheets/Slides (native export), PDFs, and
text-like files, chunks it, and embeds each chunk with a **local** embedding
model (BGE-small, ~130 MB, downloaded once to `~/Library/Caches/drv`). The
index lives in SQLite under your data directory, one per account.

Indexing is scoped: name the folders whose *content* is worth embedding
with `drv index --in <folder>` (repeatable; the choice persists; `--all`
clears it). Without roots, only Google-native docs and PDFs are taken
drive-wide. The indexer runs at background priority with capped embedding
threads (`--threads`, default 2) and exits cleanly past a memory ceiling
(`--max-mem`, default 3 GB) — progress is saved and the next run resumes.

Your file contents are sent nowhere: embedding runs on-device, and search
(`drv search`) works entirely offline. Only `drv prompt` and `drv do` call
Claude — with your stored API key, or, when none is set, through your
locally installed **Claude Code CLI** (`claude -p`), riding your existing
subscription with no key at all. `--model` passes through to either
backend.

## Roadmap

- Recursive download/upload of folders; indexing of docx/xlsx uploads.

## License

[MIT](LICENSE)
