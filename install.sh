#!/usr/bin/env bash
# ferro installer: `curl -fsSL https://raw.githubusercontent.com/aniketshukla1/ferro/main/install.sh | sh`
set -euo pipefail

REPO="aniketshukla1/ferro"
BIN_DIR="${BIN_DIR:-$HOME/.local/bin}"
VERSION="${FERRO_VERSION:-latest}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$os/$arch" in
  darwin/arm64)  target="aarch64-apple-darwin" ;;
  darwin/x86_64) target="x86_64-apple-darwin" ;;
  linux/x86_64)  target="x86_64-unknown-linux-gnu" ;;
  linux/aarch64|linux/arm64) target="aarch64-unknown-linux-gnu" ;;
  *) echo "ferro: unsupported $os/$arch (build from source: cargo install --path crates/ferro-cli)" >&2; exit 1 ;;
esac

if [ "$VERSION" = "latest" ]; then
  base="https://github.com/$REPO/releases/latest/download"
else
  base="https://github.com/$REPO/releases/download/$VERSION"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "ferro: downloading $base/ferro-$target.tar.gz"
curl -fsSL "$base/ferro-$target.tar.gz" -o "$tmp/ferro.tar.gz"
curl -fsSL "$base/checksums.txt" -o "$tmp/checksums.txt"

# Verify the SHA-256 checksum (shasum on macOS, sha256sum on Linux).
if command -v shasum >/dev/null 2>&1; then
  (cd "$tmp" && shasum -a 256 -c checksums.txt --status 2>/dev/null || (cd "$tmp" && grep "ferro-$target.tar.gz" checksums.txt | shasum -a 256 -c -)) || { echo "ferro: checksum mismatch" >&2; exit 1; }
elif command -v sha256sum >/dev/null 2>&1; then
  (cd "$tmp" && sha256sum -c --status <(grep "ferro-$target.tar.gz" checksums.txt)) || { echo "ferro: checksum mismatch" >&2; exit 1; }
else
  echo "ferro: no shasum/sha256sum found, refusing to install unverified binary" >&2
  exit 1
fi
echo "ferro: checksum ok"

tar -xzf "$tmp/ferro.tar.gz" -C "$tmp"
# Accept both archive layouts: flat (ferro at root) and nested (ferro-<target>/ferro).
bin="$(find "$tmp" -maxdepth 2 -name ferro -type f | head -1)"
[ -n "$bin" ] || { echo "ferro: binary not found in archive" >&2; exit 1; }
mkdir -p "$BIN_DIR"
install -m 755 "$bin" "$BIN_DIR/ferro"
echo "ferro: installed to $BIN_DIR/ferro"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "ferro: add to PATH: export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac
"$BIN_DIR/ferro" --version
