#!/usr/bin/env bash
# Usage: ./scripts/bump-version.sh <new-version>
# Updates the single version entry in [workspace.package] of the root Cargo.toml.
# All crates inherit from there, so this is the only file that ever needs editing.
set -euo pipefail

NEW_VERSION="${1:?Usage: $0 <new-version>  e.g. $0 0.2.0}"

# Basic semver guard (0.1.0 / 1.0.0-rc.1 / 2.3.4-beta.5)
if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.]+)?(\+[a-zA-Z0-9.]+)?$ ]]; then
    echo "error: '$NEW_VERSION' is not a valid semver string" >&2
    exit 1
fi

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CARGO_TOML="$ROOT/Cargo.toml"

CURRENT=$(grep '^version' "$CARGO_TOML" | head -1 | sed 's/version = "\(.*\)"/\1/')

# Replace the version line inside [workspace.package] — only the first occurrence.
python3 - "$CARGO_TOML" "$NEW_VERSION" <<'EOF'
import re, sys
path, version = sys.argv[1], sys.argv[2]
content = open(path).read()
updated, n = re.subn(r'^version\s*=\s*"[^"]+"', f'version = "{version}"', content, count=1, flags=re.MULTILINE)
if n == 0:
    print("error: could not find 'version = ...' in " + path, file=sys.stderr)
    sys.exit(1)
open(path, 'w').write(updated)
EOF

echo "Bumped $CURRENT → $NEW_VERSION in $CARGO_TOML"

# Update package.json files.
update_package_json() {
    local pkg="$1"
    python3 - "$pkg" "$NEW_VERSION" <<'PYEOF'
import json, sys
path, version = sys.argv[1], sys.argv[2]
with open(path) as f:
    data = json.load(f)
data['version'] = version
with open(path, 'w') as f:
    json.dump(data, f, indent=2)
    f.write('\n')
PYEOF
    echo "Bumped $NEW_VERSION in $pkg"
}

update_package_json "$ROOT/wasm-edge/package.json"
update_package_json "$ROOT/sdks/recached-react/package.json"
update_package_json "$ROOT/sdks/recached-vue/package.json"

# Verify the workspace resolves cleanly.
echo "Verifying workspace..."
cargo check --workspace --exclude wasm-edge --quiet
echo "OK — all crates resolved at $NEW_VERSION"
