#!/usr/bin/env bash
# The non-owner staged-copy test (item 3), which the host cannot reproduce: it needs
# a root-owned 0o644 binary + a non-root runner. Builds a Linux image (root install,
# drop to USER app, postinstall NOT run), then runs `nub` as the non-root user and
# asserts ensureExecutable staged a user-owned 0o755 copy under ~/.cache/nub/bin/ and
# exec'd it. Mirrors the canonical `npm i -g --ignore-scripts` container pattern.
#
# Requires Docker (Linux containers). On a macOS/arm64 host the image runs as
# linux/arm64 — fine, the heal is arch-independent.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"

CTX="$(mktemp -d)"; trap 'rm -rf "$CTX"' EXIT

# Build the fixture into the Docker context (symlink style — npm-global's shape).
bash "$SCRIPT_DIR/make-fixture.sh" "$CTX/fixture" symlink >/dev/null
cp "$SCRIPT_DIR/Dockerfile.non-owner" "$CTX/Dockerfile"
cp "$SCRIPT_DIR/assert.sh" "$CTX/assert.sh"

echo "== building non-owner image =="
docker build -q -t nub-launcher-nonowner "$CTX" >/dev/null

echo "== running first \`nub\` as non-root, postinstall NOT run =="
docker run --rm nub-launcher-nonowner
