# ferro-server routes

Every `/api/v1` route with its HTTP methods. `contract.rs` parses this table at build time:
any method other than GET/HEAD is a mutation, so `--read-only` refuses it and the audit log
records it. A new route must be listed here.

## Endpoint index

| Method | Path | Milestone | Feature flag |
|---|---|---|---|
| GET | `/api/v1/meta` | B1 | `v1` |
| GET | `/api/v1/events` (SSE) | B1 | `events` |
| GET/PUT | `/api/v1/settings` | B1 | `settings` |
| GET | `/api/v1/settings/schema` | B1 | `settings` |
| GET/PUT | `/api/v1/session` | B1 | `session` |
| PUT | `/api/v1/credentials` | B8 | `credentials` |
| GET | `/api/v1/tree` | B1 | `tree` |
| GET | `/api/v1/file` | B1 | `file` |
| GET | `/api/v1/file/lines` | B1 | `file` (+`hl.classes`, `hl.exact`) |
| GET | `/api/v1/file/raw` | B1 | `file` |
| POST | `/api/v1/file/edit` | gap | `file.edit` |
| GET | `/api/v1/file/markdown` | B1 | `markdown.v2` |
| POST | `/api/v1/markdown/render` | B1 | `markdown.v2` |
| POST | `/api/v1/highlight` | B1 | `hl.classes` |
| GET | `/api/v1/file/outline` | B1 (regex) → B6 (tree-sitter) | `outline` / `outline.treesitter` |
| GET/POST | `/api/v1/jobs`, `/api/v1/jobs/{id}`, `/api/v1/jobs/{id}/cancel` | B1 | `jobs` |
| POST | `/api/v1/index/rebuild` | B1 | `jobs` |
| POST | `/api/v1/workspace/open` | B1 | `workspace.open` |
| POST | `/api/v1/desktop/pick-folder`, `/api/v1/desktop/open-external` | B1 | `desktop` |
| GET | `/api/v1/metrics` | B1 | `metrics` |
| GET | `/api/v1/fuzzy` | B2a | `fuzzy.v2` |
| GET | `/api/v1/search` | B2a | `search.v2` (+`search.regex`) |
| GET | `/api/v1/search/stream` (SSE) | B2a | `search.stream` |
| GET | `/api/v1/file/find` | B2a | `file.find` |
| POST | `/api/v1/paths/resolve` | B2a | `paths.resolve` |
| GET | `/api/v1/git/status` | B3 | `git.status.v2` |
| GET | `/api/v1/git/changes` | B3 | `git.changes` |
| GET | `/api/v1/git/diff` | B3 | `git.diff.v2` |
| GET | `/api/v1/git/blob/lines`, `/api/v1/git/blob/raw` | B3 | `git.blob` |
| GET | `/api/v1/git/gutter` | B3 | `git.gutter` |
| POST | `/api/v1/git/stage`, `/unstage`, `/discard`, `/commit`, `/push`, `/pull` | B3 | `git.write` |
| GET | `/api/v1/git/log` | B3 | `git.log` (+`git.history`: paging, any ref, search) |
| GET | `/api/v1/git/refs`, `/api/v1/git/show` | gap | `git.history` |
| POST | `/api/v1/git/checkout`, `/api/v1/git/fetch` | gap | `git.history` |
| POST | `/api/v1/ai/explain` | gap | `ai.explain` |
| POST | `/api/v1/ai/edit` (stream) | gap | `ai.edit` |
| GET | `/api/v1/checks/breaking` | gap | `checks.breaking` |
| GET | `/api/v1/checks/tests/plan` | gap | `checks.tests` |
| POST | `/api/v1/checks/tests/run` | gap | `checks.tests` |
| GET | `/api/v1/checks/security` | gap | `checks.security` |
| POST | `/api/v1/checks/security/deep` | gap | `checks.security` |
| GET | `/api/v1/checks/coverage` | gap | `checks.coverage` |
| GET | `/api/v1/memory` | gap | `memory` |
| POST | `/api/v1/memory/rules`, `/api/v1/memory/signals`, `/api/v1/memory/suggestions/dismiss` | gap | `memory` |
| POST | `/api/v1/memory/learn` | gap | `memory.learn` |
| PATCH/DELETE | `/api/v1/memory/rules/{id}` | gap | `memory` |
| GET | `/api/v1/pr` | B4 | `pr.github` / `pr.gitlab` |
| POST | `/api/v1/pr/open`, `/api/v1/pr/refresh` | B4 | `pr.open` |
| GET | `/api/v1/pr/threads` | B4 | `pr.threads` |
| POST | `/api/v1/pr/threads/{id}/reply`, `/api/v1/pr/conversation` | B4 | `pr.threads` |
| GET/POST/PATCH/DELETE | `/api/v1/review/drafts[/{id}]` | B4 | `review.drafts` |
| POST | `/api/v1/review/submit` | B4 | `review.drafts` |
| GET/PUT | `/api/v1/review/viewed` | B4 | `review.viewed` |
| GET | `/api/v1/review/rounds` | B4 | `review.rounds` |
| GET | `/api/v1/ai/status` | B5 | `ai` |
| POST | `/api/v1/ai/ask` (stream) | B5 | `ai.ask` |
| POST | `/api/v1/ai/review` | B5 | `ai.review` |
| POST | `/api/v1/ai/findings/{id}/accept`, `/dismiss` | B5 | `ai.review` |
| POST | `/api/v1/git/commit-message` | B5 | `ai.commit` |
| GET | `/api/v1/symbols` | B6 | `symbols` |
| GET | `/api/v1/nav/definition`, `/nav/references`, `/nav/hover` | B6 | `nav` |
| GET/PUT | `/api/v1/harness` | B7 | `harness` |
| POST | `/api/v1/harness/edit`, `/api/v1/harness/revert` | B7 | `harness` |
| POST | `/api/v1/git/hunk` | B7 | `git.hunk` |
| GET | `/api/v1/lsp/open`, `/api/v1/diagnostics` | gap | `lsp.diagnostics` |
| GET/POST | `/api/v1/harness/threads` | gap | `harness.threads` |
| GET/DELETE | `/api/v1/harness/threads/{id}` | gap | `harness.threads` |
| POST | `/api/v1/harness/threads/{id}/turns` | gap | `harness.threads` |
| GET | `/api/v1/update` | gap | `update.auto` |
| POST | `/api/v1/update/check`, `/api/v1/update/install`, `/api/v1/update/restart` | gap | `update.auto` |
| POST | `/mcp` | B7 | `mcp` — MCP transport for coding agents (Bearer auth); not used by the frontend |
| — | (CLI) `--tls-cert`/`--tls-key`, `--tls self-signed` | B8 | `tls` |
| — | (CLI) `--read-only` | B8 | `readOnly` |
| — | `<state_dir>/audit.jsonl` mutation log | B8 | `auditLog` |

---
