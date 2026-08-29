#!/usr/bin/env bash
# Dependency update routine — run locally, on demand.
#
# This replaces Dependabot, which was removed: it opened one pull request per
# dependency per ecosystem and queued a full CI matrix behind each one. Batching
# the same work into a single local pass costs one CI run instead of sixteen,
# and lets a bump that breaks the build be fixed before it is ever pushed.
#
# Usage:
#   scripts/update-deps.sh            # compatible updates + audit (the routine)
#   scripts/update-deps.sh --audit    # security check only, changes nothing
#   scripts/update-deps.sh --majors   # also report the majors it will not take
#   scripts/update-deps.sh --actions  # re-resolve pinned GitHub Action SHAs
#
# The order matters: security first, because that is the half that cannot wait.
#
# What this deliberately does NOT do: take semver-incompatible upgrades. Those
# need a human reading a changelog. `--majors` lists them so they are visible
# rather than invisible; taking one is a separate, deliberate commit.

set -euo pipefail

cd "$(dirname "$0")/.."

bold() { printf '\n\033[1m%s\033[0m\n' "$1"; }
have() { command -v "$1" >/dev/null 2>&1; }

audit_only=false
show_majors=false
actions_only=false
case "${1:-}" in
    --audit)   audit_only=true ;;
    --majors)  show_majors=true ;;
    --actions) actions_only=true ;;
    "")        ;;
    *) echo "unknown flag: $1 (see the header of this script)" >&2; exit 2 ;;
esac

# ── GitHub Actions ────────────────────────────────────────────────────────────
# Actions are pinned to commit SHAs, which is why this exists: a pin is immutable,
# so it never picks up an upstream security fix on its own. Nothing else moves
# them now that Dependabot is gone.
if $actions_only; then
    bold "GitHub Action pins — current vs. what the tag points at today"
    if ! have gh; then
        echo "needs the gh CLI (brew install gh)" >&2
        exit 1
    fi
    grep -rhoE 'uses: [^@]+@[0-9a-f]{40} # \S+' .github/workflows/ | sort -u |
    while read -r _ ref _ tag; do
        repo="${ref%@*}"
        pinned="${ref#*@}"
        meta=$(gh api "repos/$repo/git/ref/tags/$tag" 2>/dev/null) || {
            echo "  ?  $repo — could not resolve tag $tag"; continue; }
        typ=$(printf '%s' "$meta" | jq -r '.object.type')
        sha=$(printf '%s' "$meta" | jq -r '.object.sha')
        # An annotated tag points at a tag object, which points at the commit.
        # Dereference it, or every annotated tag looks like it has moved.
        [ "$typ" = "tag" ] && sha=$(gh api "repos/$repo/git/tags/$sha" --jq '.object.sha')
        if [ "$sha" = "$pinned" ]; then
            echo "  ok $repo@$tag"
        else
            echo "  UPDATE $repo@$tag"
            echo "      $pinned"
            echo "   -> $sha"
        fi
    done
    echo
    echo "Apply by editing the SHA in .github/workflows/*.yml, keeping the trailing tag comment."
    exit 0
fi

# ── Security ──────────────────────────────────────────────────────────────────
bold "Advisories, licences, bans, sources"
if have cargo-deny; then
    cargo deny --all-features check
elif have cargo-audit; then
    echo "cargo-deny not installed; falling back to cargo-audit (advisories only)"
    cargo audit
else
    echo "install one: cargo install cargo-deny --locked" >&2
    exit 1
fi

if $audit_only; then
    exit 0
fi

# ── Rust ──────────────────────────────────────────────────────────────────────
bold "Rust — semver-compatible updates"
# Updates Cargo.lock only. Cargo.toml requirements are untouched, so nothing
# here can be a breaking change.
cargo update

bold "Rust — verifying"
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --workspace --all-features

# ── JavaScript ────────────────────────────────────────────────────────────────
for dir in docs sdks/recached-react sdks/recached-vue; do
    [ -f "$dir/package.json" ] || continue
    bold "npm — $dir"
    (cd "$dir" && npm update && npm audit --omit=dev || true)
done

# ── What was left behind ──────────────────────────────────────────────────────
if $show_majors; then
    bold "Rust — semver-INCOMPATIBLE updates available (not taken)"
    if have cargo-outdated; then
        cargo outdated --workspace --root-deps-only
    else
        echo "install to see these: cargo install cargo-outdated --locked"
    fi
    for dir in docs sdks/recached-react sdks/recached-vue; do
        [ -f "$dir/package.json" ] || continue
        bold "npm — outdated in $dir (not taken)"
        (cd "$dir" && npm outdated || true)
    done
    echo
    echo "Each of these needs a changelog read and its own commit."
fi

bold "Done"
echo "Review with: git diff Cargo.lock '**/package-lock.json'"
echo "Majors were not taken. See: scripts/update-deps.sh --majors"
