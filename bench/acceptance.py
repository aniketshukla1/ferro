#!/usr/bin/env python3
"""Timing acceptance checks from BACKEND.md § 6 that the unit tests cannot assert.

Starts the given ferro binary with --no-auth on a private port per check and prints one
PASS/FAIL line per budget. Run it on a quiet machine (load average well under the core count);
numbers taken under load say nothing about the budgets.

  python3 bench/acceptance.py --ferro target/release/ferro [--only b1,b2b,b3,rss]

Checks:
  b1   400k-line / ~13 MB file: first /file/lines (line-index build) <= 150 ms; warm window
       <= 5 ms; cold window (new region) <= 30 ms.
  b2b  trigram index on bench-repos/kubernetes and typescript: no-match and rare-literal
       queries <= 40 ms (k8s) / <= 100 ms (TS); common literal `return` (maxFiles 200) <= 60 ms.
  b3   5,000-row diff: <= 60 ms with hl=1, <= 15 ms with hl=0 (warm).
  rss  RSS 30 s after a search burst is within 10 % of the post-index baseline (kubernetes).
"""
from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CORPUS = ROOT / "bench-repos"
results: list[tuple[str, bool, str]] = []


def port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def get(srv: str, endpoint: str, **query) -> tuple[float, dict]:
    url = f"{srv}/api/v1/{endpoint}" + ("?" + urllib.parse.urlencode(query) if query else "")
    t0 = time.perf_counter()
    with urllib.request.urlopen(url, timeout=120) as r:
        body = r.read()
    ms = (time.perf_counter() - t0) * 1000
    return ms, json.loads(body) if body else {}


def best(srv: str, endpoint: str, n: int = 5, **query) -> float:
    get(srv, endpoint, **query)  # warm
    return min(get(srv, endpoint, **query)[0] for _ in range(n))


def check(name: str, value: float, budget: float, unit: str = "ms") -> None:
    ok = value <= budget
    results.append((name, ok, f"{value:.1f} {unit} (budget {budget:g} {unit})"))
    print(f"  {'PASS' if ok else 'FAIL'} {name}: {value:.1f} {unit} (budget {budget:g} {unit})", flush=True)


class Server:
    def __init__(self, ferro: Path, repo: Path):
        self.port = port()
        self.base = f"http://127.0.0.1:{self.port}"
        self.home = tempfile.TemporaryDirectory(prefix="ferro-acc-")
        env = dict(os.environ, FERRO_HOME=self.home.name, RUST_LOG="warn")
        self.proc = subprocess.Popen(
            [str(ferro), str(repo), "--port", str(self.port), "--no-open", "--no-auth", "--quiet"],
            env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )

    def wait(self, search_index: bool = False, index: bool = True, timeout: float = 180) -> dict:
        t0 = time.monotonic()
        while time.monotonic() - t0 < timeout:
            try:
                idx = get(self.base, "meta")[1].get("index") or {}
                if not index or (idx.get("state") == "ready" and (not search_index or idx.get("searchIndex") == "ready")):
                    return idx
            except OSError:
                pass
            time.sleep(0.1)
        raise SystemExit(f"ferro not ready within {timeout:.0f}s")

    def close(self) -> None:
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        self.home.cleanup()


def b1(ferro: Path) -> None:
    print("B1 /file/lines on a 400k-line file", flush=True)
    with tempfile.TemporaryDirectory(prefix="ferro-b1-") as d:
        # ~13.6 MB: over the index's 8 MiB listing limit, so this exercises opening by path.
        (Path(d) / "big.txt").write_text("".join(f"line {i:06d}: the quick brown fox {i}\n" for i in range(400_000)))
        s = Server(ferro, Path(d))
        try:
            s.wait(index=False)
            first, _ = get(s.base, "file/lines", path="big.txt", **{"from": 200_000, "count": 100, "hl": 0})
            check("B1 first window (line-index build)", first, 150)
            warm = best(s.base, "file/lines", path="big.txt", **{"from": 200_000, "count": 100, "hl": 0})
            check("B1 warm window", warm, 5)
            cold = get(s.base, "file/lines", path="big.txt", **{"from": 350_000, "count": 100, "hl": 0})[0]
            check("B1 cold window (new region)", cold, 30)
        finally:
            s.close()


def b2b(ferro: Path) -> None:
    budgets = {"kubernetes": 40, "typescript": 100}
    rare = {"kubernetes": "ErrImageNeverPull", "typescript": "isJSDocSatisfiesTag"}
    for repo, budget in budgets.items():
        path = CORPUS / repo
        if not path.is_dir():
            print(f"B2b {repo}: skipped (no bench-repos/{repo})")
            continue
        print(f"B2b trigram index on {repo}", flush=True)
        s = Server(ferro, path)
        try:
            t0 = time.monotonic()
            s.wait(search_index=True)
            print(f"  search index ready after {time.monotonic() - t0:.1f} s", flush=True)
            ms, res = get(s.base, "search", q="zzqqxx_no_such_token")
            print(f"  engine={res.get('engine')}", flush=True)
            check(f"B2b {repo} no-match", best(s.base, "search", q="zzqqxx_no_such_token"), budget)
            check(f"B2b {repo} rare literal", best(s.base, "search", q=rare[repo]), budget)
            if repo == "kubernetes":
                check("B2b kubernetes common literal `return` (maxFiles 200)", best(s.base, "search", q="return", maxFiles=200), 60)
        finally:
            s.close()


def b3(ferro: Path) -> None:
    print("B3 5,000-row diff", flush=True)
    with tempfile.TemporaryDirectory(prefix="ferro-b3-") as d:
        repo = Path(d)
        git = lambda *a: subprocess.run(["git", "-C", str(repo), *a], check=True, capture_output=True)  # noqa: E731
        git("init", "-q")
        git("config", "user.email", "bench@example.invalid")
        git("config", "user.name", "bench")
        src = repo / "big.rs"
        src.write_text("".join(f"fn f{i}() -> u32 {{ let x = {i}; x + 1 }}\n" for i in range(2_500)))
        git("add", ".")
        git("commit", "-qm", "base")
        src.write_text("".join(f"fn f{i}() -> u64 {{ let y = {i} * 2; y + 1 }}\n" for i in range(2_500)))
        s = Server(ferro, repo)
        try:
            s.wait()
            _, diff = get(s.base, "git/diff", path="big.rs", base="HEAD", hl=1)
            rows = sum(len(h.get("rows", [])) for h in diff.get("hunks", []))
            print(f"  rows={rows}", flush=True)
            check("B3 5k-row diff hl=1", best(s.base, "git/diff", path="big.rs", base="HEAD", hl=1), 60)
            check("B3 5k-row diff hl=0", best(s.base, "git/diff", path="big.rs", base="HEAD", hl=0), 15)
        finally:
            s.close()


def rss(ferro: Path) -> None:
    path = CORPUS / "kubernetes"
    if not path.is_dir():
        print("RSS: skipped (no bench-repos/kubernetes)")
        return
    print("B1 RSS after a search burst (kubernetes)", flush=True)
    s = Server(ferro, path)
    try:
        s.wait(search_index=True)
        time.sleep(20)  # let the post-index scavenger settle
        base_rss = get(s.base, "metrics")[1]["rssBytes"]
        for q in ["return", "func", "error", "context", "string", "Pod", "Node", "nil", "err", "struct"] * 3:
            get(s.base, "search", q=q, maxFiles=200)
        peak = get(s.base, "metrics")[1]["rssBytes"]
        time.sleep(30)
        after = get(s.base, "metrics")[1]["rssBytes"]
        mib = lambda b: b / 1048576  # noqa: E731
        print(f"  baseline {mib(base_rss):.1f} MiB, after burst {mib(peak):.1f} MiB, 30 s later {mib(after):.1f} MiB", flush=True)
        check("B1 RSS 30 s after burst vs baseline", (after / base_rss - 1) * 100, 10, "%")
    finally:
        s.close()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--ferro", type=Path, default=ROOT / "target/release/ferro")
    ap.add_argument("--only", default="b1,b2b,b3,rss")
    args = ap.parse_args()
    load = os.getloadavg()[0]
    print(f"load average {load:.1f} on {os.cpu_count()} CPUs{'  (too busy: results are not meaningful)' if load > (os.cpu_count() or 8) else ''}")
    checks = {"b1": b1, "b2b": b2b, "b3": b3, "rss": rss}
    for name in args.only.split(","):
        checks[name.strip()](args.ferro)
    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)}/{len(results)} timing budgets met")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
