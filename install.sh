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
  url="https://github.com/$REPO/releases/latest/download/ferro-$target.tar.gz"
else
  url="https://github.com/$REPO/releases/download/$VERSION/ferro-$target.tar.gz"
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
echo "ferro: downloading $url"
curl -fsSL "$url" -o "$tmp/ferro.tar.gz"
tar -xzf "$tmp/ferro.tar.gz" -C "$tmp"
mkdir -p "$BIN_DIR"
install -m 755 "$tmp/ferro" "$BIN_DIR/ferro"
echo "ferro: installed to $BIN_DIR/ferro"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "ferro: add to PATH: export PATH=\"\$HOME/.local/bin:\$PATH\"" ;;
esac
"$BIN_DIR/ferro" --version
