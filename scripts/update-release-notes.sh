#!/usr/bin/env bash
#
# Generate a GitHub Release description ("What's Changed") from the PRs/commits
# that make up a release, and write it onto the release.
#
# The body is produced by GitHub's own `releases/generate-notes` API — the exact
# engine behind the "Generate release notes" button in the Releases UI — so the
# output matches GitHub's canonical
#
#     ## What's Changed
#     * <PR title> by @<author> in <PR url>
#     ...
#     **Full Changelog**: <compare url>
#
# format with zero locally-maintained formatting logic to drift (see AGENTS.md,
# "Derivation Over Duplication"). GitHub picks the previous tag automatically
# unless one is given, and groups PRs using the repo's
# `.github/release.yml` categories if present.
#
# Usage:
#   scripts/update-release-notes.sh <tag> [options]
#
# Options:
#   --repo <owner/repo>   Target repo (default: auto-detected by gh from CWD).
#   --previous-tag <tag>  Force the comparison base (default: gh auto-detects).
#   --target <commitish>  Commit/branch the tag points at (default: gh decides).
#   --dry-run             Print the generated notes to stdout; don't modify the release.
#   -h, --help            Show this help.
#
# Requires the GitHub CLI (`gh`) authenticated with write access to the repo.
# If no release exists for the tag yet, a non-draft one is created.
#
# Examples:
#   scripts/update-release-notes.sh v0.1.1
#   scripts/update-release-notes.sh v0.1.1 --previous-tag v0.1.0
#   scripts/update-release-notes.sh v0.1.1 --repo camunda/spm-cli --dry-run

set -euo pipefail

log() { echo "$@" >&2; }
die() {
  log "error: $*"
  exit 1
}

usage() {
  # Print the leading comment block (the lines starting with `#`) as help text.
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^#\{0,1\} \{0,1\}//'
}

command -v gh >/dev/null 2>&1 || die "the GitHub CLI (gh) is required but was not found on PATH"

TAG=""
REPO=""
PREV_TAG=""
TARGET=""
DRY_RUN=0

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)
      REPO="${2:-}"
      shift 2
      ;;
    --previous-tag)
      PREV_TAG="${2:-}"
      shift 2
      ;;
    --target)
      TARGET="${2:-}"
      shift 2
      ;;
    --dry-run)
      DRY_RUN=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    -*)
      die "unknown option '$1' (see --help)"
      ;;
    *)
      [ -z "$TAG" ] || die "unexpected extra argument '$1' (tag already set to '$TAG')"
      TAG="$1"
      shift
      ;;
  esac
done

[ -n "$TAG" ] || die "missing required <tag> argument (see --help)"

# gh auto-detects the repo from the CWD's git remote when --repo is omitted.
REPO_ARGS=()
[ -n "$REPO" ] && REPO_ARGS=(--repo "$REPO")

# Ask GitHub to render the notes. Optional fields are only sent when set so gh
# falls back to its own detection (previous tag / target commitish).
NOTES_ARGS=(-f "tag_name=$TAG")
[ -n "$PREV_TAG" ] && NOTES_ARGS+=(-f "previous_tag_name=$PREV_TAG")
[ -n "$TARGET" ] && NOTES_ARGS+=(-f "target_commitish=$TARGET")

log "generating release notes for $TAG${PREV_TAG:+ (since $PREV_TAG)}"

NOTES=""
if [ -n "$REPO" ]; then
  NOTES="$(gh api --method POST "repos/$REPO/releases/generate-notes" "${NOTES_ARGS[@]}" --jq '.body')" \
    || die "failed to generate release notes (does the tag '$TAG' exist on the remote?)"
else
  # Resolve owner/repo from gh so the API path is well-formed.
  RESOLVED="$(gh repo view --json nameWithOwner --jq '.nameWithOwner')" \
    || die "could not determine the repository; pass --repo <owner/repo>"
  NOTES="$(gh api --method POST "repos/$RESOLVED/releases/generate-notes" "${NOTES_ARGS[@]}" --jq '.body')" \
    || die "failed to generate release notes (does the tag '$TAG' exist on the remote?)"
fi

[ -n "$NOTES" ] || die "GitHub returned empty release notes for '$TAG'"

if [ "$DRY_RUN" -eq 1 ]; then
  log "dry run: printing generated notes, release not modified"
  printf '%s\n' "$NOTES"
  exit 0
fi

# Persist to a temp file so newlines/markdown survive intact via --notes-file.
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
printf '%s\n' "$NOTES" >"$tmp"

if gh release view "$TAG" "${REPO_ARGS[@]}" >/dev/null 2>&1; then
  log "updating existing release $TAG"
  gh release edit "$TAG" "${REPO_ARGS[@]}" --notes-file "$tmp"
else
  log "no release found for $TAG; creating one"
  gh release create "$TAG" "${REPO_ARGS[@]}" --title "$TAG" --notes-file "$tmp"
fi

log "done: release notes updated for $TAG"
