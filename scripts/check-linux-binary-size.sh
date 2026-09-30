#!/usr/bin/env bash
# Build and gate the stripped x86_64 Linux ferro release binary.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="x86_64-unknown-linux-gnu"
LIMIT_MIB="${FERRO_BINARY_SIZE_LIMIT_MIB:-24}"
LIMIT_BYTES=$((LIMIT_MIB * 1024 * 1024))
BIN="$ROOT/target/$TARGET/release/ferro"

if [[ "$(uname -s)" != "Linux" || "$(uname -m)" != "x86_64" ]]; then
  echo "Host is not x86_64 Linux. Using docker..."
  if ! command -v docker >/dev/null 2>&1; then
    echo "error: docker is not installed. Run on an x86_64 Linux host or install docker." >&2
    exit 2
  fi
  # We use the rust image to build the binary in an x86_64 environment.
  # We also run the rest of the script inside the container so 'file' and 'wc' commands are available and check the right binary.
  docker run --rm --platform linux/amd64 -v "$ROOT:/usr/src/ferro" -w /usr/src/ferro rust:latest bash -c "
    cargo build --locked --release -p ferro --target \"$TARGET\"
    
    CONTAINER_BIN=\"/usr/src/ferro/target/$TARGET/release/ferro\"
    
    if ! command -v file >/dev/null 2>&1; then
      apt-get update && apt-get install -y file
    fi
    
    DESCRIPTION=\"\$(file -b \"\$CONTAINER_BIN\")\"
    if [[ \"\$DESCRIPTION\" != *ELF* || \"\$DESCRIPTION\" != *stripped* || \"\$DESCRIPTION\" == *\"not stripped\"* ]]; then
      echo \"error: expected a stripped ELF binary, got: \$DESCRIPTION\" >&2
      exit 1
    fi
    
    BYTES=\"\$(wc -c < \"\$CONTAINER_BIN\" | tr -d '[:space:]')\"
    MIB=\"\$(awk -v bytes=\"\$BYTES\" 'BEGIN { printf \"%.3f\", bytes / 1024 / 1024 }')\"
    PERCENT=\"\$(awk -v bytes=\"\$BYTES\" -v limit=\"$LIMIT_BYTES\" 'BEGIN { printf \"%.1f\", bytes * 100 / limit }')\"
    printf 'ferro x86_64-linux stripped binary: %s bytes (%s MiB / %s MiB, %s%%)\n' \
      \"\$BYTES\" \"\$MIB\" \"$LIMIT_MIB\" \"\$PERCENT\"
    
    if (( BYTES > LIMIT_BYTES )); then
      echo \"error: ferro exceeds the ${LIMIT_MIB} MiB binary-size budget\" >&2
      exit 1
    fi
  "
  exit $?
fi

# Fallback for native execution
cargo build --locked --release -p ferro --target "$TARGET"

if ! command -v file >/dev/null 2>&1; then
  echo "error: the 'file' command is required to verify that the binary is stripped" >&2
  exit 2
fi

DESCRIPTION="$(file -b "$BIN")"
# "not stripped" also contains "stripped", so reject it explicitly.
if [[ "$DESCRIPTION" != *ELF* || "$DESCRIPTION" != *stripped* || "$DESCRIPTION" == *"not stripped"* ]]; then
  echo "error: expected a stripped ELF binary, got: $DESCRIPTION" >&2
  exit 1
fi

BYTES="$(wc -c < "$BIN" | tr -d '[:space:]')"
MIB="$(awk -v bytes="$BYTES" 'BEGIN { printf "%.3f", bytes / 1024 / 1024 }')"
PERCENT="$(awk -v bytes="$BYTES" -v limit="$LIMIT_BYTES" 'BEGIN { printf "%.1f", bytes * 100 / limit }')"
printf 'ferro x86_64-linux stripped binary: %s bytes (%s MiB / %s MiB, %s%%)\n' \
  "$BYTES" "$MIB" "$LIMIT_MIB" "$PERCENT"

if (( BYTES > LIMIT_BYTES )); then
  echo "error: ferro exceeds the ${LIMIT_MIB} MiB binary-size budget" >&2
  exit 1
fi
