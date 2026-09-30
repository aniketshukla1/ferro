#!/usr/bin/env bash
# Build the static live demo (GitHub Pages): the web UI with its in-browser mock backend, plus
# the repository files the mock shows as sample content. No server.
# Usage: scripts/build-demo-site.sh [out-dir]   (default: _site)
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
out="${1:-$root/_site}"
rm -rf "$out"
mkdir -p "$out/web"
cp -R "$root/web/assets" "$root/web/src" "$root/web/styles" "$root/web/index.html" "$out/web/"

# The mock reads file contents from `../<path>` next to web/, like the repository layout: copy
# the text files its file list names (small ones only; lockfiles and binaries are skipped).
python3 - "$root" "$out" <<'PY'
import json, os, re, shutil, sys
root, out = sys.argv[1], sys.argv[2]
src = open(os.path.join(root, "web/src/mock/files.js"), encoding="utf-8").read()
files = json.loads(re.search(r"FILES = (\[.*\]);", src, re.S).group(1))
skip = re.compile(r"(\.lock|lock\.yaml|\.png|\.icns|\.ico|\.jpg|\.gif|\.woff2?)$")
n = 0
for path, _size in files:
    full = os.path.join(root, path)
    if path.startswith("web/") or skip.search(path) or not os.path.isfile(full):
        continue
    if os.path.getsize(full) > 300_000:
        continue
    dest = os.path.join(out, path)
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    shutil.copyfile(full, dest)
    n += 1
print(f"sample files: {n}")
PY

# The entry page sends visitors to the app in demo mode (the app's CSP forbids inline scripts,
# so a meta refresh does the redirect).
cat > "$out/index.html" <<'HTML'
<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ferro · live demo</title>
<meta name="description" content="Try ferro in your browser: iron-clad code review, running on sample data.">
<meta http-equiv="refresh" content="0; url=web/index.html?mock=1&amp;demo=1">
<link rel="icon" href="web/assets/favicon.svg" type="image/svg+xml">
</head>
<body style="background:#0f0f11;color:#ededf0;font:16px system-ui,sans-serif;display:grid;place-items:center;height:100vh;margin:0">
<p>Opening the ferro demo… <a style="color:#ff8c2e" href="web/index.html?mock=1&amp;demo=1">Continue</a></p>
</body>
</html>
HTML
touch "$out/.nojekyll"
echo "demo site: $out ($(du -sh "$out" | cut -f1))"
