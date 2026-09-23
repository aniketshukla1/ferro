#!/usr/bin/env bash
# ferro benchmark — px0 parity table. Shallow clones, fastest of 3, no LSP.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
BIN="$ROOT/target/release/ferro"
BENCH_DIR="$ROOT/bench-repos"
PORT=7891

repos() {
  echo "flask https://github.com/pallets/flask.git"
  echo "redis https://github.com/redis/redis.git"
  echo "django https://github.com/django/django.git"
  echo "react https://github.com/facebook/react.git"
  echo "kubernetes https://github.com/kubernetes/kubernetes.git"
  echo "typescript https://github.com/microsoft/TypeScript.git"
  echo "linux https://github.com/torvalds/linux.git"
}

if [ "${1:-}" = "--clone" ]; then
  mkdir -p "$BENCH_DIR"
  while read -r name url || [ -n "${name:-}" ]; do
    [ -z "${name:-}" ] && continue
    if [ -d "$BENCH_DIR/$name" ]; then echo "exists $name"; continue; fi
    echo "cloning $name..."
    git clone --depth 1 "$url" "$BENCH_DIR/$name" 2>&1 | tail -1
  done < <(repos)
  exit 0
fi

cargo build --release -p ferro 2>&1 | tail -1
echo ""
echo "| repo | files | index | fuzzy | full scan | rss |"
echo "|---|---|---|---|---|---|"

while read -r name url || [ -n "${name:-}" ]; do
  [ -z "${name:-}" ] && continue
  dir="$BENCH_DIR/$name"
  if [ ! -d "$dir" ]; then echo "| $name | missing (run --clone) | | | | |"; continue; fi
  # cold start: time to first health response
  "$BIN" "$dir" --port $PORT --no-open > /tmp/ferro_bench.log 2>&1 &
  pid=$!
  for i in $(seq 1 100); do curl -sf "localhost:$PORT/api/health" > /dev/null 2>&1 && break; sleep 0.05; done
  sleep 0.4
  stats=$(curl -sf "localhost:$PORT/api/stats" || echo '{"files":0,"indexed_ms":0}')
  files=$(python3 -c "import sys,json; print(json.load(sys.stdin).get('files',0))" <<< "$stats")
  index_ms=$(python3 -c "import sys,json; print(json.load(sys.stdin).get('indexed_ms',0))" <<< "$stats")
  # fastest of 3: fuzzy "server" + full-scan no-match string
  fuzzy_ms=$(PORT=$PORT python3 - <<'PY'
import time, urllib.request, json, os
port = os.environ.get("PORT", "7891")
best = 10**9
for _ in range(3):
    t0 = time.perf_counter()
    urllib.request.urlopen(f"http://localhost:{port}/api/fuzzy?q=server&limit=20").read()
    best = min(best, (time.perf_counter() - t0) * 1000)
print(f"{best:.1f}")
PY
)
  scan_ms=$(PORT=$PORT python3 - <<'PY'
import time, urllib.request, os
port = os.environ.get("PORT", "7891")
best = 10**9
for _ in range(3):
    t0 = time.perf_counter()
    urllib.request.urlopen(f"http://localhost:{port}/api/search?q=zzzz_no_match_qqqq_ferro&limit=20").read()
    best = min(best, (time.perf_counter() - t0) * 1000)
print(f"{best:.1f}")
PY
)
  rss_kb=$(ps -o rss= -p $pid 2>/dev/null | tr -d ' ' || echo 0)
  rss_mb=$(python3 -c "print(f'{(int($rss_kb or 0))/1024:.0f}')")
  echo "| $name | $files | ${index_ms}ms | ${fuzzy_ms}ms | ${scan_ms}ms | ${rss_mb} MB |"
  kill $pid 2>/dev/null || true
  wait $pid 2>/dev/null || true
  PORT=$((PORT + 1))
done < <(repos)
