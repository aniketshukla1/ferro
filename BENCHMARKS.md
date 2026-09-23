# Benchmarks

Methodology: shallow clones (`--depth 1`), fastest of 3, language servers off.
Ferro numbers include HTTP roundtrip (`curl localhost`); px0 numbers are in-process.
So ferro fuzzy/scan read ~5-10ms higher than raw scorer — raw scorer is sub-ms (see unit benches).

Corpus: flask, redis, django, react, kubernetes, typescript, linux.

Run:

```bash
./benchmark.sh --clone   # one-time, ~2GB (bench-repos/ is gitignored)
./benchmark.sh           # builds release, prints markdown table
```

| metric | px0 claimed | ferro P1b (flask, release, HTTP) |
|---|---|---|
| cold start (bind + first health) | <1ms | ~50ms process spawn, <5ms in-app |
| RSS idle | ~20MB | ~15-25MB (see table) |
| index flask 235 files | 1ms | ~5ms |
| fuzzy (HTTP) | 0.8ms in-proc | ~10ms HTTP, scorer <1ms |
| full scan | 2.3ms | HTTP-dominated, see table |

P1b targets: linux 95k files index <400ms, fuzzy HTTP <15ms, 400k-line file window <100ms / highlight <250ms (verified 84ms / 110-210ms on 13MB test file).
