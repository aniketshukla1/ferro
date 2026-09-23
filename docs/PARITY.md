# Parity vs px0 (v0.1.8) — living matrix

Source: px0.ai docs reference (keyboard-shortcuts, cli-flags), guides, changelog.
Legend: DONE · PART (partial) · TODO · SKIP (deferred with reason) · EDGE (ferro-only advantage).

## CLI

| px0 | ferro |
|---|---|
| `px0 [path\|file:line\|pr-url]` | PART — path only; `file:line` unparsed; no pr-url |
| `-port` (0 = random) | PART — fixed port, no 0 |
| `-host`, `-no-open`, `-no-lsp` | DONE (`--host`, `--no-open`, `--no-lsp` accepted) |
| `-y` (skip merged-PR confirm) | TODO (with PR mode) |
| `-no-git` | TODO |
| `-agent <harness>` / `-no-agent` | EDGE instead — built-in agent, no external harness needed |
| `-no-color`, `-quiet`, `-verbose` | TODO |
| `-update`, `-v`/`-version` | TODO |
| `install.sh`, 15 targets, releases | TODO |
| `-no-telemetry` | SKIP — ferro has no telemetry to disable |

## Keyboard / editor

| px0 | ferro |
|---|---|
| Ctrl+P / Ctrl+K palette, fuzzy files | DONE |
| Ctrl+Shift+F workspace search | DONE (`>`, blended while typing) |
| Ctrl+Shift+O outline | DONE (panel; regex, no LSP) |
| Ctrl+F find in file | DONE |
| Ctrl+G jump to line | DONE (palette `:`) |
| Ctrl+, settings | TODO (with settings system) |
| Ctrl+B sidebar toggle | DONE |
| Ctrl+Shift+R refresh | DONE |
| Tabs: W / Shift+T reopen / Ctrl+Tab / Alt+1-9 | DONE |
| Alt+Z word wrap, Home/End, Alt+Left/Right history | DONE (wrap ≤2k lines; history; Home/End native scroll) |
| Shift+arrows selection model in viewer | PART (click + shift-click ranges; no keyboard expansion yet) |
| Alt+C copy ref, Alt+A copy w/ context, Alt+U usages | DONE |
| Right-click selection menu | DONE |
| Alt+M markdown preview, image viewer | TODO |
| Alt+R inline comment, batch apply (Ctrl+Enter) | TODO (with PR mode) |
| Alt+E harness dispatch | EDGE instead — built-in `ask` + `apply_patch` |
| F12 / Shift+F12 / call trails / hover | SKIP for now — needs LSP; regex outline covers 80% |
| Vim mode | SKIP — large; revisit on demand |
| `?` help sheet | DONE (palette `?`) |

## Review flow (flagship gap)

| px0 | ferro |
|---|---|
| `px0 <pr-url>` ephemeral worktree + merge-base diff | TODO |
| Inline draft comments + submit (approve/changes) | TODO |
| Stage / commit / push panel, AI commit message | TODO |
| Per-file stage ticks, ff-only pull | TODO |

## Systems

| px0 | ferro |
|---|---|
| Single binary, bg index, fuzzy, virtual rows | DONE |
| 14 themes | PART (3: forge/paper/mocha) |
| Memory scavenger (15s idle release) | TODO (cheap: drop highlight LRU on idle) |
| Settings JSON + visual manager | TODO |
| Docker guide + image | TODO |
| Benchmarks vs editors table | PART (own table; no VS Code run) |

## Ferro edges (px0 lacks)

- Built-in agent (`ask`, tools, sandbox) — no external CLI needed
- Persistent sqlite index cache + session audit log
- Token-AND content search; blended file+content palette
- Symbol outline with zero config (no LSP install)
- Patch normalize (missing `new file mode` headers)
