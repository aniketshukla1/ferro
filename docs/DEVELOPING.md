# Developing ferro

Everything you need to build, run and test ferro from source. The user guide is the [README](../README.md).

## Layout

| Path | What it is |
|---|---|
| `crates/ferro-core` | Indexing, fuzzy find, search engines, git, tree-sitter symbols and navigation, diff, checks (radar, test plans, security scan, coverage), team memory, self-update |
| `crates/ferro-server` | The HTTP API (`/api/v1`, routes in [routes.md](../crates/ferro-server/src/routes.md)), events, auth guards, jobs, language-server client, MCP |
| `crates/ferro-agent` | AI providers, the built-in agent, redaction, coding-agent harnesses |
| `crates/ferro-forge` | GitHub and GitLab pull requests: checkout, threads, drafts, review submission |
| `crates/ferro-cli` | The `ferro` binary (serves the embedded `web/`) plus `ferro ssh`, `ferro mcp`, `ferro ask` |
| `apps/desktop` | Tauri desktop shell around the same server |
| `web/` | The UI: vanilla ES modules, no build step |

## Build and run

```bash
cargo build --release -p ferro
./target/release/ferro /path/to/repo
```

While working on the UI, serve `web/` from disk so a browser reload picks up edits:

```bash
cargo run -p ferro -- . --dev-web web
```

`?mock=1` on the page URL runs the whole UI against an in-browser mock backend (`web/src/mock/`), which is what the end-to-end tests use.

### Desktop app

```bash
pnpm --dir apps/desktop install
pnpm --dir apps/desktop dev      # native window, root = current folder, Ctrl+O to switch
pnpm --dir apps/desktop build    # bundle
```

The bundle registers the `ferro://` URL scheme (declared in `apps/desktop/src-tauri/tauri.conf.json` under `plugins.deep-link.desktop.schemes`). `ferro://open?pr=<https-forge-url>` opens that pull request; malformed links, non-http(s) schemes, `file:` targets and shell metacharacters are rejected. On macOS, open the built app once (`target/release/bundle/macos/Ferro.app`) so Launch Services registers the scheme.

## Tests and gates

Run these before sending a change:

```bash
cargo fmt --check
CARGO_INCREMENTAL=0 cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
node web/tests/tools/check-syntax.mjs
node --test web/tests/unit/*.test.js
cd web/tests && pnpm install && npx playwright install chromium webkit && npx playwright test
```

Dependency policy (the exact version CI uses):

```bash
cargo install cargo-deny --version 0.20.2 --locked
cargo deny check
```

Release binary size (x86_64 Linux, stripped, 24 MiB budget):

```bash
./scripts/check-linux-binary-size.sh
```

Performance gate: clone the corpora once, build release, compare against the baseline. It reports `not comparable` on a different machine rather than a false pass.

```bash
./benchmark.sh --clone flask,redis
cargo build --release -p ferro
python3 bench/h2h.py --only ferro --repos flask,redis --gate bench/baseline.json
```

Timing budgets and how they are measured: [BENCHMARKS.md](../BENCHMARKS.md), `web/tests/e2e/budgets.spec.js` and `web/tests/unit/bootgraph.test.js`.

## Built-in agent from the command line

```bash
export ANTHROPIC_API_KEY=...        # or OPENAI_API_KEY / GEMINI_API_KEY / OLLAMA_MODEL
ferro ask "where is fuzzy scoring implemented?" --path .
```

## Routes

[routes.md](../crates/ferro-server/src/routes.md) lists every `/api/v1` route. It is parsed at build time to decide which routes are read-only-safe and audit-logged, so a new route must be listed there.
