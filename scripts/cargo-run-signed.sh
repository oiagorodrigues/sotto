#!/usr/bin/env bash
# Cargo runner for macOS (see .cargo/config.toml). Signs the desktop app binary with the local
# "Sotto Dev Signing" identity before running it, so Keychain and privacy grants survive
# rebuilds. Other binaries (tests, tools) run unchanged.
set -euo pipefail

bin="$1"
shift

if [[ "$(basename "$bin")" == "sotto-desktop" ]] \
  && security find-identity -v -p codesigning 2>/dev/null | grep -q '"Sotto Dev Signing"'; then
  root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  /usr/bin/codesign --force --sign "Sotto Dev Signing" --options runtime \
    --entitlements "$root/apps/desktop/src-tauri/entitlements.plist" "$bin"
fi

exec "$bin" "$@"
