# Benchmarks

Methodology mirrors px0 `BENCHMARKS.md`: shallow clones, fastest of 5, language servers off.

Corpus: flask, redis, django, react, kubernetes, typescript, linux.

| metric | px0 claimed | ferro target P1 |
|---|---|---|
| cold start | <1ms | <5ms |
| RSS idle | ~20MB | <25MB |
| index linux 95k files | 370ms | <400ms |
| fuzzy linux | 6.0ms | <7ms |
| full scan django | 26.8ms | <35ms |

Run: `./benchmark.sh --clone && ./benchmark.sh`
