#!/usr/bin/env bash
# Cut a Shortlist release: bump the workspace version, commit, tag, and
# optionally push. Pushing the tag builds both macOS assets on GitHub Actions
# and publishes them; installed apps then see the update on their next check.
#
#   ./scripts/release.sh patch
#   ./scripts/release.sh minor --push
#   ./scripts/release.sh major --push
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BUMP="${1:-patch}"
PUSH="${2:-}"

case "$BUMP" in
  patch|minor|major) ;;
  *)
    echo "usage: $0 [patch|minor|major] [--push]" >&2
    exit 1
    ;;
esac

if [[ "$PUSH" != "" && "$PUSH" != "--push" ]]; then
  echo "usage: $0 [patch|minor|major] [--push]" >&2
  exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "Working tree is not clean. Commit or stash changes before releasing." >&2
  exit 1
fi

CURRENT="$(awk -F'"' '/^version = / { print $2; exit }' Cargo.toml)"
IFS='.' read -r MAJOR MINOR PATCH <<< "$CURRENT"
case "$BUMP" in
  major) MAJOR=$((MAJOR + 1)); MINOR=0; PATCH=0 ;;
  minor) MINOR=$((MINOR + 1)); PATCH=0 ;;
  patch) PATCH=$((PATCH + 1)) ;;
esac
NEXT="$MAJOR.$MINOR.$PATCH"
TAG="v$NEXT"

if git rev-parse -q --verify "refs/tags/$TAG" >/dev/null; then
  echo "Tag $TAG already exists." >&2
  exit 1
fi

echo "Releasing $CURRENT → $TAG"
awk -v old="$CURRENT" -v new="$NEXT" '
  !done && $0 == "version = \"" old "\"" {
    print "version = \"" new "\""
    done = 1
    next
  }
  { print }
' Cargo.toml > Cargo.toml.release-tmp
mv Cargo.toml.release-tmp Cargo.toml

# Refresh the workspace package versions recorded in Cargo.lock.
cargo metadata --format-version 1 >/dev/null

git add Cargo.toml Cargo.lock
git commit -m "Release $TAG"
git tag -a "$TAG" -m "Shortlist $TAG"

BRANCH="$(git rev-parse --abbrev-ref HEAD)"

echo
echo "Created commit and tag $TAG."
if [[ "$PUSH" == "--push" ]]; then
  git push origin "$BRANCH" "$TAG"
  echo "Pushed. GitHub Actions is building and publishing $TAG."
  echo "Installed apps will offer the update after the release finishes."
else
  echo "Next:"
  echo "  git push origin $BRANCH $TAG"
  echo "The tag triggers .github/workflows/release.yml, which publishes the app assets."
fi
