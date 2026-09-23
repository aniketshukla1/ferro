# ferro

Ferro is a fast local code review tool for humans and AI — a Rust rival to px0.

- Single static Rust binary, browser UI, zero runtime deps
- Sub-ms startup goal, ~20MB RSS goal, background indexing
- Fuzzy file find, whole-tree regex search, file viewer with virtual rows
- Native git: status + diff vs HEAD
- Phase 2 (next): built-in agent with sandbox + apply/rollback
- Phase 3: team/enterprise — auth, TLS, shared reviews, MCP

## Run (Phase 1: local speed)

```bash
cargo run --release -- /path/to/repo --port 7778 --no-open
open http://127.0.0.1:7778/
```

Shortcuts: `Ctrl+P` fuzzy, `>query` search, `Ctrl+D` diff vs HEAD.

## API

- `GET /api/health`, `/api/stats`, `/api/files`
- `GET /api/fuzzy?q=&limit=`
- `GET /api/search?q=&limit=`
- `GET /api/file?path=`
- `GET /api/git-status`, `/api/diff?path=`

## Benchmarks

See `BENCHMARKS.md`. Run:

```bash
./benchmark.sh --clone
./benchmark.sh
```

## Roadmap

- P1 local speed: DONE scaffold — index, fuzzy, search, viewer, git diff
- P1b: syntax highlight windowed, 400k-line virtual render, persistent index cache
- P2 built-in agent: tool sandbox, patch apply, session log
- P3 team: TLS/OIDC, Postgres reviews, MCP server

License: MIT.
