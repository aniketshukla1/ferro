# Architecture — ferro Phase 1

One process. No runtime. Axum JSON + embedded vanilla JS.

- `index.rs`: `ignore` crate walker, gitignore-aware, skips `.git`, skips >8MB in listing. Listen-first, index in background tokio task.
- `fuzzy.rs`: two-pass O(n) scorer. +40 verbatim in basename, +20 starts-with, +16 boundary, +14 camel hump, +14 in-basename, +12 consecutive, +4 exact case, -gap, -len/8, -2 per slash.
- `search.rs`: bytes.Contains fast reject + ASCII fold, 2MB file cap, 32-rune snippet lead, worker threads = NumCPU.
- `git.rs`: pure shell-out git (status porcelain, diff HEAD, merge-base). No libgit2 for portability.
- `server.rs`: routes under `/api/*`, static fallback via rust-embed `web/`. Traversal guard in `safe_join`.
- `web/`: ~60-row virtual concept, fuzzy + tree cache client-side. No npm.

Security Phase 1: path traversal block, no tunnel domain logic yet (P3 adds origin verify + TLS).

Next: syntect windowed highlight with 512KB window + 512MB LRU (behind feature flag to keep binary lean), persistent file-id cache (mtime+size), git watcher.
