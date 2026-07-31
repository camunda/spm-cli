#!/usr/bin/env bash
#
# Bump the crate version — the single source of truth for every release channel.
#
# The `[package].version` in Cargo.toml drives crates.io, the npm packages
# (derived by npm/build.mjs), and the GitHub Release. This script updates that
# version and keeps Cargo.lock's own entry in sync, exactly as documented in
# RELEASE.md. The npm/dist packages are generated at release time, so there is
# nothing else to bump.
#
# Usage:
#   scripts/bump-version.sh [patch|minor|major|X.Y.Z]
#
# With no argument it bumps the patch component. Passing an explicit semver
# (X.Y.Z) sets that exact version. The chosen version must be strictly greater
# than the current one (registries reject reused versions).
#
# Prints the new version to stdout (all diagnostics go to stderr) so callers can
# capture it, e.g. `new=$(scripts/bump-version.sh patch)`.

set -euo pipefail

# Resolve repo root from this script's location so it works from any CWD.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TOML="$ROOT/Cargo.toml"

log() { echo "$@" >&2; }
die() {
  log "error: $*"
  exit 1
}

[ -f "$CARGO_TOML" ] || die "Cargo.toml not found at $CARGO_TOML"

# Read the version only from the [package] section so an unrelated `version =`
# key elsewhere in the manifest can't be picked up (matches the release CI).
read_pkg_version() {
  awk '
    /^\[package\]/ { p = 1; next }
    /^\[/          { p = 0 }
    p && /^version[[:space:]]*=/ {
      gsub(/.*=[[:space:]]*"|".*/, ""); print; exit
    }
  ' "$CARGO_TOML"
}

current="$(read_pkg_version)"
[ -n "$current" ] || die "could not read [package].version from Cargo.toml"

case "$current" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) die "current version '$current' is not X.Y.Z semver" ;;
esac

IFS='.' read -r cur_major cur_minor cur_patch <<<"$current"

part="${1:-patch}"
case "$part" in
  major) new="$((cur_major + 1)).0.0" ;;
  minor) new="${cur_major}.$((cur_minor + 1)).0" ;;
  patch) new="${cur_major}.${cur_minor}.$((cur_patch + 1))" ;;
  [0-9]*.[0-9]*.[0-9]*) new="$part" ;;
  *) die "invalid argument '$part' (expected patch|minor|major or X.Y.Z)" ;;
esac

# Validate the target is strictly greater than the current version.
IFS='.' read -r new_major new_minor new_patch <<<"$new"
for n in "$new_major" "$new_minor" "$new_patch"; do
  case "$n" in
    '' | *[!0-9]*) die "target version '$new' is not X.Y.Z semver" ;;
  esac
done
is_greater() {
  [ "$new_major" -gt "$cur_major" ] && return 0
  [ "$new_major" -lt "$cur_major" ] && return 1
  [ "$new_minor" -gt "$cur_minor" ] && return 0
  [ "$new_minor" -lt "$cur_minor" ] && return 1
  [ "$new_patch" -gt "$cur_patch" ] && return 0
  return 1
}
is_greater || die "target version '$new' is not greater than current '$current' (never reuse a version)"

log "bumping $current -> $new"

# Update only the first `version =` inside the [package] section. A state flag
# ensures we never touch a `version =` in another section.
perl -0pi -e '
  s{
    (^\[package\][^\[]*?          # the [package] section body (up to the next [)
     ^version\s*=\s*")            # the version key
    [^"]*                          # old value
    (")
  }{${1}'"$new"'${2}}mxs;
' "$CARGO_TOML"

# Confirm the edit actually landed.
check="$(read_pkg_version)"
[ "$check" = "$new" ] || die "failed to update Cargo.toml (still reads '$check')"

# Keep Cargo.lock's own spm-cli entry in sync. Try `--offline` first (fast, and
# spm-cli is a local package so no network is needed in the common case); fall
# back to a normal `cargo update` if the registry index isn't already cached.
CARGO="${CARGO:-cargo}"
log "syncing Cargo.lock"
"$CARGO" update -p spm-cli --precise "$new" --offline >/dev/null 2>&1 \
  || "$CARGO" update -p spm-cli --precise "$new" >/dev/null

echo "$new"
