set shell := ["bash", "-euo", "pipefail", "-c"]

export PATH := `brew --prefix rustup` + "/bin:" + env_var("PATH")

# Command Line Tools' SwiftPM fails the first build of Swift Testing targets after any
# configuration change ("plugin for module 'TestingMacros' not found"); Xcode's toolchain does not.
export DEVELOPER_DIR := "/Applications/Xcode.app/Contents/Developer"

default: check

# Everything that must pass before a commit.
check: rust swift web types

# Tauri's build script requires the bundled sidecar to exist, so Rust checks build it first.
rust: build-sidecar
    cargo fmt --all --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace --quiet
    just api-check

swift:
    if [ -f native/sotto-audio/Package.swift ]; then \
      cd native/sotto-audio && swift build --build-path .build/strict -Xswiftc -warnings-as-errors && swift test; \
    fi

web:
    if [ -f apps/desktop/package.json ]; then \
      cd apps/desktop && pnpm install --frozen-lockfile --silent && pnpm run typecheck && pnpm run lint; \
    fi

# Release build of the audio sidecar, copied where Tauri's `externalBin` expects it. Not committed.
build-sidecar:
    cd native/sotto-audio && swift build -c release
    mkdir -p apps/desktop/src-tauri/binaries
    cp "$(cd native/sotto-audio && swift build -c release --show-bin-path)/sotto-audio" apps/desktop/src-tauri/binaries/sotto-audio-aarch64-apple-darwin
    if security find-identity -v -p codesigning | grep -q '"Sotto Dev Signing"'; then \
      codesign --force --sign "Sotto Dev Signing" --options runtime apps/desktop/src-tauri/binaries/sotto-audio-aarch64-apple-darwin; \
    fi

# One-time: local code-signing identity so Keychain and privacy grants survive rebuilds.
dev-signing:
    scripts/dev-signing-setup.sh

# Desktop app with a freshly built sidecar; debug builds run the copy in src-tauri/binaries.
dev: build-sidecar
    cd apps/desktop && pnpm tauri dev

# Local Postgres (host port 54329) and Mailpit (SMTP 1025, UI http://localhost:8025) for the API.
api-up:
    docker compose up -d --wait postgres mailpit

# The API runs in its own Cargo workspace because sqlx and rusqlite both link sqlite3 (LOG.md, Lessons).
api-check: api-up
    cd services/api && cargo fmt --check
    cd services/api && cargo clippy --all-targets -- -D warnings
    cd services/api && DATABASE_URL=postgres://sotto:sotto@localhost:54329/sotto cargo test --quiet

# Runs the API on http://localhost:8080 against the compose services; login mails show in Mailpit.
api-dev: api-up
    cd services/api && \
    DATABASE_URL=postgres://sotto:sotto@localhost:54329/sotto \
    BIND_ADDR=127.0.0.1:8080 PUBLIC_BASE_URL=http://localhost:8080 \
    SMTP_URL=smtp://localhost:1025 MAIL_FROM='Sotto <login@sotto.local>' \
    REALTIME_PROVIDER=fake cargo run

# Generated TypeScript types must match the Rust contract.
types:
    #!/usr/bin/env bash
    set -euo pipefail
    tmp="$(mktemp -d)"
    trap 'rm -rf "${tmp:?}"' EXIT
    TS_RS_EXPORT_DIR="$tmp" cargo test -p sotto-protocol --quiet > /dev/null
    diff -r "$tmp" apps/desktop/src/generated || { echo "generated types are stale: run cargo test -p sotto-protocol"; exit 1; }

# Fast subset for the pre-commit hook.
pre-commit:
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --quiet -- -D warnings

install-hooks:
    printf '#!/usr/bin/env bash\nexec just pre-commit\n' > .git/hooks/pre-commit
    chmod +x .git/hooks/pre-commit

latency-spike:
    cd spikes/latency && mkdir -p .build && swiftc -O -parse-as-library main.swift -o .build/spike
    spikes/latency/.build/spike fixtures/audio/pt-BR-question.wav pt-BR 20 | tail -6
    spikes/latency/.build/spike fixtures/audio/en-US-question.wav en-US 20 | tail -6
