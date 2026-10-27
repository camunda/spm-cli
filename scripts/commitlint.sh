#!/usr/bin/env bash
#
# Lint commit messages with the same commitlint CI runs (the `commitlint` job in
# .github/workflows/ci.yml installs @commitlint/cli + @commitlint/config-conventional
# and reads commitlint.config.js), so a bad message fails before it is pushed.
# The rules are not re-implemented here: this only picks the input.
#
#   scripts/commitlint.sh --edit <file>   lint one message (the commit-msg hook)
#   scripts/commitlint.sh [<base>]        lint every commit in <base>..HEAD
#                                         (default base: merge-base with origin/main)
#
# Needs Node (npx); the packages are fetched once into the npx cache.
set -euo pipefail

if ! command -v npx >/dev/null 2>&1; then
  echo "commitlint: npx not found — install Node to lint commit messages locally" >&2
  echo "commitlint: (CI runs the same check on every PR; see AGENTS.md › Commit messages)" >&2
  exit 1
fi

cd "$(git rev-parse --show-toplevel)"

commitlint() {
  npx --yes -p @commitlint/cli -p @commitlint/config-conventional -- commitlint "$@"
}

if [ "${1:-}" = "--edit" ]; then
  commitlint --edit "${2:?usage: $0 --edit <commit-msg-file>}"
else
  base="$(git rev-parse --verify "${1:-$(git merge-base origin/main HEAD)}^{commit}")"
  # commitlint rejects an empty range (--from == --to), e.g. on main itself.
  if [ "$base" = "$(git rev-parse HEAD)" ]; then
    echo "commitlint: no commits in ${1:-origin/main}..HEAD"
    exit 0
  fi
  commitlint --from "$base" --to HEAD --verbose
fi
