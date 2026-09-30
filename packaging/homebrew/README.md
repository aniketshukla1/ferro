# Homebrew formula

`scripts/gen-homebrew-formula.sh <version> <https-download-base> <checksums.txt>` renders `ferro.rb` here from a release's `checksums.txt` (the release workflow publishes it next to the archives). macOS uses the native builds; Linux uses the static musl builds.

Publishing it to a tap repository (for example `<owner>/homebrew-ferro`) waits on the board's choice of release host. Nothing here is published automatically.
