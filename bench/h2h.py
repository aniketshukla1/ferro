#!/usr/bin/env python3
"""Ferro end-to-end performance gate from BACKEND.md sections 5.5 and 7."""

from __future__ import annotations

import argparse
import json
import os
import platform
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
DEFAULT_FERRO = Path(os.environ.get("CARGO_TARGET_DIR", ROOT / "target")) / "release" / "ferro"
DEFAULT_CORPUS = ROOT / "bench-repos"
METRICS = ("fuzzy", "scan", "open_warm")
SOURCE_EXTENSIONS = {
    ".c",
    ".cc",
    ".cpp",
    ".cs",
    ".go",
    ".h",
    ".hpp",
    ".java",
    ".js",
    ".jsx",
    ".kt",
    ".php",
    ".py",
    ".rb",
    ".rs",
    ".swift",
    ".ts",
    ".tsx",
}


class BenchError(RuntimeError):
    """A setup or benchmark failure that must fail the quality gate."""


class NotComparable(BenchError):
    """The current machine does not match the baseline machine."""


def command_output(argv: list[str], timeout: float = 10.0) -> str:
    try:
        run = subprocess.run(
            argv,
            check=True,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return ""
    return run.stdout.strip()


def processor_name() -> str:
    system = platform.system()
    if system == "Darwin":
        raw = command_output(["system_profiler", "SPHardwareDataType", "-json"])
        if raw:
            try:
                hardware = json.loads(raw)["SPHardwareDataType"][0]
                chip = hardware.get("chip_type")
                if chip:
                    return str(chip)
            except (KeyError, IndexError, TypeError, json.JSONDecodeError):
                pass
    elif system == "Linux":
        try:
            for line in Path("/proc/cpuinfo").read_text(encoding="utf-8").splitlines():
                if line.lower().startswith(("model name", "hardware")):
                    return line.split(":", 1)[-1].strip()
        except OSError:
            pass
    return platform.processor().strip() or "unknown"


def machine_info() -> dict[str, Any]:
    return {
        "system": platform.system(),
        "architecture": platform.machine(),
        "processor": processor_name(),
        "logical_cpus": os.cpu_count() or 0,
    }


def machine_label(machine: dict[str, Any]) -> str:
    return (
        f"{machine['system']} {machine['architecture']}, "
        f"{machine['processor']}, {machine['logical_cpus']} logical CPUs"
    )


def load_gate(path: Path) -> dict[str, Any]:
    try:
        gate = json.loads(path.read_text(encoding="utf-8"))
    except OSError as exc:
        raise BenchError(f"cannot read gate file {path}: {exc}") from exc
    except json.JSONDecodeError as exc:
        raise BenchError(f"invalid JSON in gate file {path}: {exc}") from exc
    if not isinstance(gate, dict):
        raise BenchError(f"gate file {path} must contain a JSON object")
    return gate


def require_comparable(gate: dict[str, Any], actual: dict[str, Any]) -> None:
    expected = gate.get("_meta", {}).get("machine")
    if not isinstance(expected, dict):
        raise NotComparable("not comparable: gate file has no _meta.machine fingerprint")
    mismatches = []
    for key in ("system", "architecture", "processor", "logical_cpus"):
        if expected.get(key) != actual.get(key):
            mismatches.append(f"{key}: expected {expected.get(key)!r}, got {actual.get(key)!r}")
    if mismatches:
        expected_label = machine_label(expected)
        actual_label = machine_label(actual)
        raise NotComparable(
            "not comparable: benchmark machine differs from the baseline\n"
            f"  baseline: {expected_label}\n"
            f"  current:  {actual_label}\n"
            "  mismatch: " + "; ".join(mismatches)
        )


def available_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def fetch_json(url: str, timeout: float = 2.0) -> Any:
    with urllib.request.urlopen(url, timeout=timeout) as response:
        if response.status != 200:
            raise BenchError(f"HTTP {response.status} from {url}")
        return json.load(response)


def wait_for_index(base_url: str, process: subprocess.Popen[Any], timeout: float = 60.0) -> dict[str, Any]:
    deadline = time.monotonic() + timeout
    last_error = "server did not answer"
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise BenchError(f"ferro exited before indexing (exit {process.returncode})")
        try:
            index = fetch_json(f"{base_url}/api/v1/meta").get("index") or {}
            if index.get("state") == "ready" and int(index.get("files", 0)) > 0:
                return index
        except (BenchError, OSError, ValueError) as exc:
            last_error = str(exc)
        time.sleep(0.05)
    raise BenchError(f"ferro did not finish indexing within {timeout:.0f}s: {last_error}")


def curl_ms(url: str) -> float:
    curl = shutil.which("curl")
    if not curl:
        raise BenchError("curl is required by the performance protocol")
    try:
        run = subprocess.run(
            [curl, "-fsS", "-o", os.devnull, "-w", "%{time_total}", url],
            check=True,
            capture_output=True,
            text=True,
            timeout=60,
        )
        return float(run.stdout.strip()) * 1000.0
    except OSError as exc:
        raise BenchError(f"cannot run curl: {exc}") from exc
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired, ValueError) as exc:
        detail = getattr(exc, "stderr", "") or str(exc)
        raise BenchError(f"curl benchmark failed for {url}: {detail.strip()}") from exc


def best_of_five(url: str) -> float:
    curl_ms(url)  # Untimed page-cache warm-up required by the protocol.
    return min(curl_ms(url) for _ in range(5))


def biggest_source(repo: Path) -> str:
    """Largest tracked source file (API v1 has no flat file list; the index follows .gitignore too)."""
    listed = command_output(["git", "-C", str(repo), "ls-files"], timeout=30).splitlines()
    sizes = []
    for rel in listed:
        if Path(rel).suffix.lower() in SOURCE_EXTENSIONS:
            try:
                sizes.append(((repo / rel).stat().st_size, rel))
            except OSError:
                pass
    if not sizes:
        raise BenchError("corpus contains no source file for the open-warm benchmark")
    return max(sizes)[1]


def terminate(process: subprocess.Popen[Any]) -> None:
    if process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def repo_revision(repo: Path) -> str:
    revision = command_output(["git", "-C", str(repo), "rev-parse", "HEAD"])
    return revision or "unknown"


def measure_repo(name: str, repo: Path, ferro: Path) -> dict[str, Any]:
    port = available_port()
    base_url = f"http://127.0.0.1:{port}"
    with tempfile.TemporaryDirectory(prefix=f"ferro-h2h-{name}-") as temp_dir:
        temp = Path(temp_dir)
        env = os.environ.copy()
        env["FERRO_HOME"] = str(temp / "ferro-home")
        env["RUST_LOG"] = "warn"
        with (temp / "ferro.log").open("w", encoding="utf-8") as log:
            process = subprocess.Popen(
                [
                    str(ferro),
                    str(repo),
                    "--port",
                    str(port),
                    "--no-open",
                    "--no-auth",
                    "--no-git",
                    "--quiet",
                    "--no-color",
                ],
                cwd=ROOT,
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
            )
            try:
                stats = wait_for_index(base_url, process)
                biggest = biggest_source(repo)
                fuzzy_url = f"{base_url}/api/v1/fuzzy?" + urllib.parse.urlencode(
                    {"q": "srv", "limit": 20}
                )
                scan_url = f"{base_url}/api/v1/search?" + urllib.parse.urlencode(
                    {"q": "zzqqxx_no_such_token", "maxFiles": 20}
                )
                open_url = f"{base_url}/api/v1/file/lines?" + urllib.parse.urlencode(
                    {"path": biggest, "from": 1, "count": 1000, "hl": 1}
                )
                return {
                    "revision": repo_revision(repo),
                    "files": int(stats["files"]),
                    "index": float(stats.get("ms", 0)),
                    "fuzzy": round(best_of_five(fuzzy_url), 3),
                    "scan": round(best_of_five(scan_url), 3),
                    "open_warm": round(best_of_five(open_url), 3),
                    "open_path": biggest,
                }
            except BenchError as exc:
                log.flush()
                try:
                    tail = (temp / "ferro.log").read_text(encoding="utf-8")[-4000:]
                except OSError:
                    tail = ""
                if tail:
                    raise BenchError(f"{exc}\nferro log:\n{tail}") from exc
                raise
            finally:
                terminate(process)


def print_table(results: dict[str, dict[str, Any]]) -> None:
    print("| repo | files | index | fuzzy | full scan (miss) | open big (warm) |")
    print("|---|---:|---:|---:|---:|---:|")
    for repo, row in results.items():
        print(
            f"| {repo} | {row['files']} | {row['index']:.1f} ms | "
            f"{row['fuzzy']:.3f} ms | {row['scan']:.3f} ms | "
            f"{row['open_warm']:.3f} ms |"
        )


def gate_results(
    results: dict[str, dict[str, Any]], gate: dict[str, Any]
) -> list[str]:
    meta = gate.get("_meta", {})
    regression = float(meta.get("max_regression_percent", 25.0))
    multiplier = 1.0 + regression / 100.0
    failures: list[str] = []
    print(f"\nGate: no more than {regression:g}% slower than baseline")
    for repo, actual in results.items():
        expected = gate.get(repo)
        if not isinstance(expected, dict):
            failures.append(f"{repo}: missing baseline")
            continue
        for metric in METRICS:
            if metric not in expected:
                continue
            limit = float(expected[metric]) * multiplier
            measured = float(actual[metric])
            passed = measured <= limit
            print(
                f"  {'PASS' if passed else 'FAIL'} {repo}.{metric}: "
                f"{measured:.3f} ms (baseline {float(expected[metric]):.3f} ms, "
                f"limit {limit:.3f} ms)"
            )
            if not passed:
                failures.append(
                    f"{repo}.{metric} {measured:.3f} ms exceeds {limit:.3f} ms"
                )
    return failures


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--only",
        default="ferro",
        help="comma-separated tools to run; this gate currently supports ferro",
    )
    parser.add_argument(
        "--repos", default="flask,redis", help="comma-separated directories under bench-repos"
    )
    parser.add_argument("--gate", type=Path, help="baseline JSON used as a 25%% regression gate")
    parser.add_argument("--ferro", type=Path, default=DEFAULT_FERRO, help="ferro binary")
    parser.add_argument("--corpus", type=Path, default=DEFAULT_CORPUS, help="corpus directory")
    parser.add_argument("--json", action="store_true", help="emit machine and measurements as JSON")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    tools = [item.strip() for item in args.only.split(",") if item.strip()]
    if tools != ["ferro"]:
        raise BenchError("--only must be 'ferro'")
    repos = [item.strip() for item in args.repos.split(",") if item.strip()]
    if not repos:
        raise BenchError("--repos must name at least one corpus repository")

    machine = machine_info()
    gate = load_gate(args.gate.resolve()) if args.gate else None
    if gate is not None:
        require_comparable(gate, machine)

    ferro = args.ferro.resolve()
    if not ferro.is_file() or not os.access(ferro, os.X_OK):
        raise BenchError(f"ferro binary is missing or not executable: {ferro}; run cargo build -p ferro --release")

    results: dict[str, dict[str, Any]] = {}
    for name in repos:
        repo = (args.corpus / name).resolve()
        if not repo.is_dir():
            raise BenchError(
                f"missing corpus repository: {repo}; run ./benchmark.sh --clone {','.join(repos)}"
            )
        results[name] = measure_repo(name, repo, ferro)

    report = {
        "_meta": {
            "machine": machine,
            "samples": 5,
            "warmup_samples": 1,
        },
        **results,
    }
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(f"Machine: {machine_label(machine)}")
        print_table(results)

    if gate is not None:
        failures = gate_results(results, gate)
        if failures:
            print("\nPerformance gate failed:", file=sys.stderr)
            for failure in failures:
                print(f"- {failure}", file=sys.stderr)
            return 1
        print("\nPerformance gate passed.")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchError as exc:
        print(f"performance gate error: {exc}", file=sys.stderr)
        raise SystemExit(2)
