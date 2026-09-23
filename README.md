# ferro

Ferro is a fast local code review tool for humans and AI — a Rust rival to px0.

- `ferro` CLI: single static Rust binary, browser UI, zero runtime deps
- `Ferro` desktop: Tauri 2 native app, same `ferro-core`, offline, no server
- Sub-ms startup goal, ~20MB RSS goal, background indexing
- Fuzzy file find, whole-tree regex search, file viewer
- Native git: status + diff vs HEAD
- Phase 2 (next): built-in agent with sandbox + apply/rollback
- Phase 3: team/enterprise — auth, TLS, shared reviews, MCP

## Run — CLI (browser)

```bash
cargo run -p ferro --release -- /path/to/repo --port 7778 --no-open
open http://127.0.0.1:7778/
```

Shortcuts: `Ctrl+P` fuzzy, `>query` search, `Ctrl+D` diff vs HEAD.

## Run — Desktop (Tauri)

```bash
pnpm --dir apps/desktop install
pnpm --dir apps/desktop dev
# opens native window, root = cwd, Ctrl+O to switch folder
```

Tauri commands (same core as CLI): `get_stats, list_files, fuzzy, grep, read_file, git_status, git_diff, set_root, reindex, pick_folder`.

## Layout

- `crates/ferro-core/`: index, fuzzy, search, git — shared by CLI + Tauri
- `crates/ferro-cli/`: `ferro` Axum server + embedded `web/`
- `apps/desktop/src-tauri/`: Tauri wrapper, `frontendDist = web/`
- `web/`: vanilla JS, dual backend (`window.__TAURI__.core.invoke` or `fetch /api/*`)

## API (CLI mode)

- `GET /api/health`, `/api/stats`, `/api/files`
- `GET /api/fuzzy?q=&limit=`
- `GET /api/search?q=&limit=`
- `GET /api/file?path=`
- `GET /api/git-status`, `/api/diff?path=`

## Benchmarks

See `BENCHMARKS.md`.

## Roadmap

- P1 local speed: DONE scaffold + Tauri shell
- P1b: syntax highlight windowed, 400k-line virtual render, persistent index cache
- P2 built-in agent: tool sandbox, patch apply, session log
- P3 team: TLS/OIDC, Postgres reviews, MCP server

License: MIT.
