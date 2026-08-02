#!/usr/bin/env bash
# Usage: ./scripts/update-formula-checksums.sh <tag>   e.g. ./scripts/update-formula-checksums.sh v0.2.4
#
# Downloads the published release binaries and writes their real SHA-256 sums
# into Formula/recached.rb.
#
# Run this AFTER the release workflow has built and uploaded the artifacts for
# <tag>. The checksums cannot be known before then, which is why bump-version.sh
# leaves placeholders behind: a formula carrying a *stale but valid* checksum
# installs the previous release without complaining, and that is how 0.1.8 kept
# being served to `brew install` long after 0.2.x shipped.
set -euo pipefail

TAG="${1:?Usage: $0 <tag>  e.g. $0 v0.2.4}"
if ! [[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?$ ]]; then
    echo "error: '$TAG' is not a release tag (expected e.g. v0.2.4)" >&2
    exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FORMULA="$ROOT/Formula/recached.rb"
BASE="https://github.com/recached-dev/recached/releases/download/$TAG"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# The formula's version must already match the tag, or we would be pairing new
# checksums with old URLs — the same class of mismatch this script exists to end.
FORMULA_VERSION=$(grep -m1 '^  version ' "$FORMULA" | sed 's/.*"\(.*\)".*/\1/')
if [[ "v$FORMULA_VERSION" != "$TAG" ]]; then
    echo "error: formula is at v$FORMULA_VERSION but you asked for $TAG." >&2
    echo "       run scripts/bump-version.sh ${TAG#v} first." >&2
    exit 1
fi

update_one() {
    local asset="$1" placeholder="$2"
    echo "Fetching $asset..."
    if ! curl -fsSL --retry 3 -o "$TMP/$asset" "$BASE/$asset"; then
        echo "error: could not download $BASE/$asset" >&2
        echo "       has the release workflow finished uploading for $TAG?" >&2
        return 1
    fi
    local sum
    sum=$(shasum -a 256 "$TMP/$asset" | cut -d' ' -f1)
    python3 - "$FORMULA" "$placeholder" "$sum" <<'PYEOF'
import sys
path, placeholder, checksum = sys.argv[1], sys.argv[2], sys.argv[3]
content = open(path).read()
if placeholder not in content:
    sys.exit(f"error: placeholder {placeholder} not found in {path} — already filled?")
open(path, 'w').write(content.replace(placeholder, checksum))
PYEOF
    echo "  $asset -> $sum"
}

update_one "recached-macos-amd64" "REPLACE_WITH_AMD64_SHA256"
update_one "recached-macos-arm64" "REPLACE_WITH_ARM64_SHA256"

if grep -q "REPLACE_WITH_" "$FORMULA"; then
    echo "error: placeholders remain in $FORMULA" >&2
    grep -n "REPLACE_WITH_" "$FORMULA" >&2
    exit 1
fi

echo
echo "Formula/recached.rb updated for $TAG. Verify with:"
echo "  brew install --build-from-source $FORMULA"
