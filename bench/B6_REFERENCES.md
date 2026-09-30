# B6 Kubernetes reference-navigation measurements

Measured on 2026-09-28 against the ORA-52 worktree (parent `774966f`) and
Kubernetes `dfd7b93a1783878be367e1fc4a780318330cb3bf`.

Previous worst case, from `bench/B6_SCALE.md` on `main` (Ferro `a94fd60`):
**11,618.920 ms** for `Run` references. This report is the follow-up.

## Machine and build

- Apple M1 Pro, 8 cores, 16 GiB, arm64; Darwin 27.0.0.
- Rust 1.98.0; `cargo build --release` (`opt-level = 3`, thin LTO, one codegen unit).
- This host's load average during the runs was 140–188. The B6 baseline was taken on a quiet machine. Wall-clock samples here include that contention.
- Kubernetes snapshot walked by the symbol-index test: 31,100 files.
- Symbol index: 207,448 symbols. The symbol pool used 4 of 8 cores.

## What changed

References no longer scan the workspace on the request. The symbol-index build
records, per identifier, the files that contain it, and packed line/column
positions for names that occur in 12 or more files (capped so the table stays
inside the heap budget). A query is a lookup. Names that are not packed are
re-lexed from their file list only.

Comments, strings, and any path with a `vendor` component are excluded, matching
the previous textual reference rules. Preload of `symbols.bin` does not mark the
index Ready: a warm symbol table has no reference postings until a full build
finishes.

## Symbol-index build

Timer: immediately before `SymbolIndex::ensure_built` on an in-memory snapshot,
until `SymbolState::Ready`. Release build. Two samples:

| Run | Build time | Load average |
|---:|---:|---:|
| 1 | 23,528.106 ms | ~180 |
| 2 | 12,751.905 ms | 142.16 149.75 123.01 |

Run 2 process time for the whole test, including the file walk before the
timer: real 17.50 s, user 28.15 s, sys 2.63 s.

**Not a pass against 10,000 ms on this host.** Both samples were taken while
other work had the machine at load average above 140. The quiet-machine outline
baseline in `bench/B6_SCALE.md` was 8,485.958 ms worst. The identifier postings
add one scan of the same files on the existing 4-thread pool (previously ~1.1 s
of wall when that pool was not stalled) and about 150 ms to pack the wide
names. A quiet M1 should land under 10 s. QA should remeasure the build with
the same timer on a machine whose load is comparable to the baseline, and
should wait for `SymbolState::Ready` rather than the file-index event. Ready is
not exposed on the HTTP API.

## Heap

Published table after run 2: **37,933,088 bytes (36.2 MiB)**. Budget 40 MiB.
**PASS.**

| Part | Bytes |
|---|---:|
| Reference index | 16,325,059 |
| Paths | 1,522,157 |
| Symbol names | 8,383,352 |
| Lowercased names | 8,383,352 |
| Symbol metas | 3,319,168 |

Reference index: 140,044 names, 682 names with packed positions, 599,917 packed
locations.

## Navigation latency over HTTP

After `symbols.bin` was republished (index Ready), each
`GET /api/v1/nav/{references,definition,hover}` below was issued with `curl`
against `ferro --no-auth` on `127.0.0.1`. Reference requests used `limit=1000`
and were repeated three times. Times are client wall-clock, including HTTP and
JSON. Load average during the pass: 150.94 151.48 121.39.

| Location / identifier | Definition | References (3 runs) | Hover |
|---|---:|---|---:|
| `pkg/api/service/testing/make.go:34:6` / `MakeService` | 6.335 ms (1 def) | 10.530, 9.972, 7.622 ms; 1000 refs, truncated | 3.855 ms |
| `cmd/kube-scheduler/app/server.go:94:6` / `NewSchedulerCommand` | 1.872 ms (1 def) | 2.623, 1.702, 1.666 ms; 4 refs, not truncated | 4.722 ms |
| `cmd/kube-scheduler/app/server.go:183:6` / `Run` | 6.869 ms (10 defs, handler cap) | 2.985, 2.569, 6.185 ms; 1000 refs, truncated | 2.695 ms |
| `pkg/probe/exec/exec.go:36:6` / `New` | 3.910 ms (10 defs, handler cap) | 6.036, 2.897, 2.175 ms; 1000 refs, truncated | 1.912 ms |
| `pkg/controller/podautoscaler/hpa_selector_store.go:90:28` / `Delete` | 1.876 ms (10 defs, handler cap) | 2.775, 2.475, 1.887 ms; 1000 refs, truncated | 1.678 ms |
| `pkg/controller/podautoscaler/replica_calculator.go:80:29` / `GetResourceReplicas` | 2.634 ms (1 def) | 3.505, 3.372, 2.121 ms; 7 refs, not truncated | 2.461 ms |

All 30 requests returned HTTP 200.

Worst cases:

- References, worst of 18: **10.530 ms — PASS** against 50 ms (was 11,618.920 ms).
- Definition: **6.869 ms — PASS** against 50 ms.
- Hover: **4.722 ms — PASS** against 50 ms.

In-process lookups on the same release binary, same six names, were 0.150–2.441 ms
on the second build sample.

## Intended difference from the baseline responses

The old path searched at most 200 trigram candidate files, 50 hits per file,
then parsed each hit file. `Run` came back with 628 references and
`truncated: true` because that scan stopped early.

The new path returns identifier matches in path order from the index, skipping
comments, strings, and vendor trees, up to `limit=1000`. `Run`, `New`,
`Delete`, and `MakeService` now return 1000 references with `truncated: true`.
`NewSchedulerCommand` returns 4 and `GetResourceReplicas` returns 7, with
`truncated: false`. A larger, path-ordered set is the intended result. The
route shape is unchanged.

## Gates

- `cargo fmt --check`: pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo test --workspace`: pass (the Kubernetes scale test stays ignored).
- `cargo test -p ferro-core --lib`: 93 passed, 1 ignored.
- `cargo deny check`: fails on pre-existing advisories, MPL-2.0 license
  rejections, and wildcard bans. This change does not touch `Cargo.lock`.
- `bench/h2h.py`: not in this worktree, and `bench-repos` has no flask or
  redis checkout, so the perf gate was not run here.
- x86_64-linux stripped size: not run. This host has no Linux linker. Owned by
  ORA-13.
