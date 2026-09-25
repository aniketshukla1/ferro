# ferro

**Iron-clad code review.** Ferro is a local-first workbench for reading, searching and
reviewing code and pull requests on your own machine, with an optional AI agent as a
second reviewer.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/aniketshukla1/ferro/main/install.sh | sh
```

Or build from source (Rust stable): `cargo build --release -p ferro`.
Docker: `docker build -t ferro . && docker run -p 7777:7777 -v "$(pwd):/src:ro" ferro`

- `ferro` CLI: single static Rust binary, browser UI (needs `git` for repo features)
- `Ferro` desktop: Tauri 2 native app, same core, offline
- Background indexing with persistent cache, fuzzy file find, full-text search, file viewer
- Native git: status + diff vs HEAD, PR review with inline drafts
- Built-in agent with sandbox + patch apply (needs an LLM key, never committed)

## Security

Every serve prints a URL with a one-time token (`http://127.0.0.1:<port>/?token=…`).
The browser keeps it in an `HttpOnly; SameSite=Strict` cookie; scripts and other
origins get `401/403`. For automation use `Authorization: Bearer <token>` or
`--token`/`FERRO_TOKEN`. `--no-auth` works on loopback binds only.

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

## Ask the built-in agent (P2)

```bash
export GEMINI_API_KEY=...            # or OPENAI_API_KEY / OLLAMA_MODEL
# optional: export GEMINI_MODEL=gemini-2.0-flash
cargo run -p ferro -- ask "where is fuzzy scoring implemented?" --path .
```

Resolution order: `--api-key/--base-url/--model` flags > `GEMINI_API_KEY` >
`OPENAI_API_KEY` (+`OPENAI_BASE_URL`, `FERRO_MODEL`) > `OLLAMA_MODEL`
(localhost:11434, no key). Never commit keys — env only.

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
