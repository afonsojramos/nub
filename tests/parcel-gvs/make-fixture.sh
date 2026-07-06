#!/usr/bin/env bash
# Build a minimal Parcel app whose build spins up @parcel/workers — the
# shape that exercises @parcel/core's module-scoped serializer registry
# (registerCoreWithSerializer.js) across the worker-farm boundary. That
# registry is what breaks with a DataCloneError when @parcel/core is
# materialized as two separate module instances (the GVS store-dir
# over-split this harness regression-tests).
#
# Usage: make-fixture.sh [dest] [parcel-version]
set -euo pipefail
DEST="${1:-/tmp/nub-parcel-gvs-fixture}"
VERSION="${2:-2.12.0}"
rm -rf "$DEST"
mkdir -p "$DEST/src"
cat > "$DEST/package.json" <<EOF
{
  "name": "nub-parcel-gvs-fixture",
  "version": "1.0.0",
  "source": "src/index.html",
  "scripts": { "build": "parcel build --no-cache" },
  "devDependencies": { "parcel": "$VERSION" }
}
EOF
cat > "$DEST/src/index.html" <<'EOF'
<!doctype html><html><body><script type="module" src="./index.js"></script></body></html>
EOF
cat > "$DEST/src/index.js" <<'EOF'
import { greet } from './greet.js';
console.log(greet('parcel'));
EOF
cat > "$DEST/src/greet.js" <<'EOF'
export const greet = (n) => `hello ${n}`;
EOF
echo "fixture at $DEST (parcel@$VERSION)"
