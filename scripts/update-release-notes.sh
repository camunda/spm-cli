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
#   --include-commits     Also append commits in the range that aren't tied to a
#                         merged PR (GitHub's notes only list PRs). Useful for the
#                         first tag / repos with direct-to-main commits. Requires
#                         local git history + the tag (in CI use fetch-depth: 0).
#   --dry-run             Print the generated notes to stdout; don't modify the release.
#   -h, --help            Show this help.
#
# Requires the GitHub CLI (`gh`) authenticated with write access to the repo.
# If no release exists for the tag yet, a non-draft one is created.
#
# Examples:
#   scripts/update-release-notes.sh v0.1.1
#   scripts/update-release-notes.sh v0.1.1 --previous-tag v0.1.0
#   scripts/update-release-notes.sh v0.1.0 --include-commits --dry-run
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

# Append a "Commits without a pull request" section listing commits in the
# release range that GitHub's PR-based notes omit. Reads TAG/PREV_TAG/REPO_SLUG
# as globals; takes the current notes body on $1 and prints the augmented body.
# Degrades gracefully (warns, returns the body unchanged) when local git history
# or the tag isn't available — e.g. a shallow CI checkout.
append_orphan_commits() {
  local notes="$1"

  if ! command -v git >/dev/null 2>&1 || ! git rev-parse --git-dir >/dev/null 2>&1; then
    log "warn: --include-commits needs a git repo; skipping the commits section"
    printf '%s' "$notes"
    return 0
  fi
  if ! git rev-parse -q --verify "refs/tags/$TAG^{commit}" >/dev/null 2>&1; then
    log "warn: tag '$TAG' not found locally (shallow checkout?); skipping the commits section"
    printf '%s' "$notes"
    return 0
  fi

  # Determine the commit range. Prefer an explicit --previous-tag; else the
  # nearest ancestor tag; else (first tag) everything reachable from TAG.
  local range base
  if [ -n "$PREV_TAG" ]; then
    if git rev-parse -q --verify "refs/tags/$PREV_TAG^{commit}" >/dev/null 2>&1; then
      range="$PREV_TAG..$TAG"
    else
      # The PR-based notes still honor --previous-tag (server-side), so warn that
      # the commits section is computed from a different base to avoid surprise.
      log "warn: --previous-tag '$PREV_TAG' not found locally; deriving the commits range from the nearest ancestor tag instead"
      base="$(git describe --tags --abbrev=0 "$TAG^" 2>/dev/null || true)"
      if [ -n "$base" ]; then range="$base..$TAG"; else range="$TAG"; fi
    fi
  else
    base="$(git describe --tags --abbrev=0 "$TAG^" 2>/dev/null || true)"
    if [ -n "$base" ]; then range="$base..$TAG"; else range="$TAG"; fi
  fi

  # PR numbers already covered by GitHub's notes, so we don't list them twice.
  # `|| true` keeps `set -o pipefail` from aborting when grep finds no PRs (e.g.
  # a first release with no merged PRs) — an empty set is a valid result here.
  local listed
  listed=" $(printf '%s\n' "$notes" | { grep -oE 'pull/[0-9]+' || true; } | { grep -oE '[0-9]+' || true; } | sort -u | tr '\n' ' ')"

  local orphans="" sha subject n
  while read -r sha subject; do
    [ -n "$sha" ] || continue
    # Skip commits that squash-merged a PR already in the list (subject ends "(#N)").
    if [[ "$subject" =~ \(#([0-9]+)\)[[:space:]]*$ ]]; then
      n="${BASH_REMATCH[1]}"
      case "$listed" in
        *" $n "*) continue ;;
      esac
    fi
    orphans+="* ${subject} ([\`${sha:0:7}\`](https://github.com/${REPO_SLUG}/commit/${sha}))"$'\n'
  done < <(git log --no-merges --format='%H %s' "$range" 2>/dev/null)

  if [ -z "$orphans" ]; then
    log "no orphan (non-PR) commits found in $range"
    printf '%s' "$notes"
    return 0
  fi

  local section="## Commits without a pull request"$'\n'"$orphans"
  # Slot the section in just before the "**Full Changelog**" footer if present,
  # otherwise append it to the end. The section is passed via the environment
  # (not awk -v) because BSD awk rejects newlines in -v assignments.
  if printf '%s\n' "$notes" | grep -q '^\*\*Full Changelog'; then
    SPM_SECTION="$section" awk '
      /^\*\*Full Changelog/ && !done { print ENVIRON["SPM_SECTION"]; print ""; done = 1 }
      { print }
    ' <<<"$notes"
  else
    printf '%s\n\n%s' "$notes" "$section"
  fi
}

TAG=""
REPO=""
PREV_TAG=""
TARGET=""
DRY_RUN=0
INCLUDE_COMMITS=0

while [ $# -gt 0 ]; do
  case "$1" in
    --repo)
      [ $# -ge 2 ] && [ -n "$2" ] || die "option '$1' requires a value (see --help)"
      REPO="$2"
      shift 2
      ;;
    --previous-tag)
      [ $# -ge 2 ] && [ -n "$2" ] || die "option '$1' requires a value (see --help)"
      PREV_TAG="$2"
      shift 2
      ;;
    --target)
      [ $# -ge 2 ] && [ -n "$2" ] || die "option '$1' requires a value (see --help)"
      TARGET="$2"
      shift 2
      ;;
    --include-commits)
      INCLUDE_COMMITS=1
      shift
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

# Resolve owner/repo once so the API path is well-formed and commit URLs (used
# by --include-commits) point at the right repo.
if [ -n "$REPO" ]; then
  REPO_SLUG="$REPO"
else
  REPO_SLUG="$(gh repo view --json nameWithOwner --jq '.nameWithOwner')" \
    || die "could not determine the repository; pass --repo <owner/repo>"
fi

# Ask GitHub to render the notes. Optional fields are only sent when set so gh
# falls back to its own detection (previous tag / target commitish).
NOTES_ARGS=(-f "tag_name=$TAG")
[ -n "$PREV_TAG" ] && NOTES_ARGS+=(-f "previous_tag_name=$PREV_TAG")
[ -n "$TARGET" ] && NOTES_ARGS+=(-f "target_commitish=$TARGET")

log "generating release notes for $TAG${PREV_TAG:+ (since $PREV_TAG)}"

NOTES="$(gh api --method POST "repos/$REPO_SLUG/releases/generate-notes" "${NOTES_ARGS[@]}" --jq '.body')" \
  || die "failed to generate release notes (does the tag '$TAG' exist on the remote?)"

[ -n "$NOTES" ] || die "GitHub returned empty release notes for '$TAG'"

# Optionally augment the PR-based notes with commits that have no associated
# merged PR — GitHub's generate-notes lists only PRs, so direct-to-main commits
# (common on the very first tag) would otherwise be invisible.
if [ "$INCLUDE_COMMITS" -eq 1 ]; then
  NOTES="$(append_orphan_commits "$NOTES")"
fi

if [ "$DRY_RUN" -eq 1 ]; then
  log "dry run: printing generated notes, release not modified"
  printf '%s\n' "$NOTES"
  exit 0
fi

# Persist to a temp file so newlines/markdown survive intact via --notes-file.
# Use an explicit template under $TMPDIR so it's portable across GNU and BSD
# (macOS) mktemp implementations.
tmp="$(mktemp "${TMPDIR:-/tmp}/spm-release-notes.XXXXXX")"
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
