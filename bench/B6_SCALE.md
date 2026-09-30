# B6 Kubernetes scale measurements

Measured on 2026-09-27 against Ferro `a94fd60` and Kubernetes
`dfd7b93a1783878be367e1fc4a780318330cb3bf` (a fresh `--depth 1` clone).

## Machine and build

- Apple M1 Pro, 8 cores, arm64; macOS/Darwin 27.0.0.
- Rust 1.98.0; `cargo build --release` (`opt-level = 3`, thin LTO,
  one codegen unit).
- Kubernetes file snapshot: 31,403 files.
- Symbol index: 207,412 symbols; its dedicated pool used 4 of the 8
  available cores, as specified by `SymbolIndex`.

These numbers are comparable with the M1 Pro baseline in
the backend spec.

## Symbol index build

The timer starts immediately before `SymbolIndex::ensure_built` on an already
completed file snapshot and stops when `SymbolState::Ready` is observed. Each
run uses a new empty symbol-cache directory, so persisted-index preload cannot
short-circuit extraction.

| Run | Build time |
|---:|---:|
| 1 | 7,948.492 ms |
| 2 | 8,050.043 ms |
| 3 | 8,485.958 ms |

Worst case: **8,485.958 ms — PASS** against the 10,000 ms budget (1,514.042 ms
headroom).

## Navigation latency

After the production server's symbol index was queryable, one measured pass
issued each `GET /api/v1/nav/{definition,references,hover}` request below over
loopback with `curl`. Values are client wall-clock times, including HTTP and
JSON serialization. Reference requests used `limit=1000`.

| Location / identifier | Definition | References | Hover |
|---|---:|---:|---:|
| `pkg/api/service/testing/make.go:34` / `MakeService` | 17.850 ms | 5,135.010 ms | 4.405 ms |
| `cmd/kube-scheduler/app/server.go:94` / `NewSchedulerCommand` | 2.768 ms | 1,423.274 ms | 7.330 ms |
| `cmd/kube-scheduler/app/server.go:183` / `Run` | 13.994 ms | **11,618.920 ms** | 2.378 ms |
| `pkg/probe/exec/exec.go:36` / `New` | 2.172 ms | 492.416 ms | 1.697 ms |
| `pkg/controller/podautoscaler/hpa_selector_store.go:90` / `Delete` | 2.474 ms | 1,813.738 ms | 1.530 ms |
| `pkg/controller/podautoscaler/replica_calculator.go:80` / `GetResourceReplicas` | 2.301 ms | 758.083 ms | 2.191 ms |

Worst cases by route:

- Definition: **17.850 ms — PASS** against 50 ms.
- References: **11,618.920 ms — FAIL** against 50 ms.
- Hover: **7.330 ms — PASS** against 50 ms.
- Overall `any nav query`: **FAIL**.

All 18 requests returned HTTP 200. A follow-up evidence request for the worst
identifier (`Run`) reported 2,393 ms of handler time and returned 628 references
with `truncated: true`, independently confirming that the failure is in the
request path rather than loopback overhead. This follow-up issue records the
failure only; it does not attempt an optimization.

## x86_64-linux grammar size

**Deferred; no x86_64-linux verdict in this report.** The host has only
`aarch64-apple-darwin` and `x86_64-apple-darwin` Rust targets, no Linux linker,
and the installed Docker client has no running daemon. The runnable stripped
x86_64-linux size gate is owned by the active DevOps toolchain follow-up
ORA-13. That gate must report the grammar delta against the 10 MiB budget and
drop languages if it exceeds the budget.

The earlier 9.267 MiB arm64 measurement is not substituted for this platform-
specific gate.
