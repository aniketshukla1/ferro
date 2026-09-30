# Benchmarks

Methodology: shallow clones (`--depth 1`), fastest of 3, language servers off.
Ferro numbers include the HTTP roundtrip (`curl localhost`).
So ferro fuzzy/scan read ~5-10ms higher than raw scorer — raw scorer is sub-ms (see unit benches).

Corpus: flask, redis, django, react, kubernetes, typescript, linux.

Run:

```bash
./benchmark.sh --clone   # one-time, ~2GB (bench-repos/ is gitignored)
./benchmark.sh           # builds release, prints markdown table
```

The merge gate uses only Flask and Redis, best-of-five HTTP measurements, and
the checked-in machine-specific baseline:

```bash
./benchmark.sh --clone flask,redis
cargo build --release -p ferro
python3 bench/h2h.py --only ferro --repos flask,redis --gate bench/baseline.json
```

The script prints the measured machine. A different OS, architecture, processor,
or logical CPU count is a hard `not comparable` error; record a reviewed baseline
for that hardware rather than comparing unlike machines.

| metric | ferro P1b (flask, release, HTTP) |
|---|---|
| cold start (bind + first health) | ~50ms process spawn, <5ms in-app |
| RSS idle | ~15-25MB (see table) |
| index flask 235 files | ~5ms |
| fuzzy (HTTP) | ~10ms HTTP, scorer <1ms |
| full scan | HTTP-dominated, see table |

P1b targets: linux 95k files index <400ms, fuzzy HTTP <15ms, 400k-line file window <100ms / highlight <250ms (verified 84ms / 110-210ms on 13MB test file).
