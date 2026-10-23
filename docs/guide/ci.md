# CI & Automated Updates

spm ships two GitHub Actions from this repository:

| Action | What it does |
| --- | --- |
| `camunda/spm-cli@v0` | Installs `spm` from a GitHub Release, verifies it against the release's `SHA256SUMS`, and puts it on `PATH`. |
| `camunda/spm-cli/update@v0` | Opens **one pull request per outdated skill or plugin**, with the version change and the [content scan](/reference/cli-commands#spm-scan-path) of the new code in its body. |

Both run on Linux, macOS and Windows runners (x64; arm64 on Linux and macOS).

## Install spm in a workflow

```yaml
steps:
  - uses: actions/checkout@v5
  - uses: camunda/spm-cli@v0
  - run: spm install --locked   # fail if ai.lock is out of sync with ai.json
```

| Input | Default | |
| --- | --- | --- |
| `version` | the action's version | spm version to install (`X.Y.Z` or `vX.Y.Z`). `@v0` follows the newest 0.x release; pin `@vX.Y.Z` (or a commit SHA) for a fixed one. |
| `release-url` | `https://github.com/camunda/spm-cli/releases/download` | Where `v<version>/<asset>` and `v<version>/SHA256SUMS` are downloaded from — set it to use a mirror (an HTTP(S) proxy, or a `file://` directory on a self-hosted runner). |

| Output | |
| --- | --- |
| `version` | The installed version (no leading `v`). |
| `path` | Absolute path of the installed binary. |

The action refuses to install a binary that is missing from, or doesn't match,
the release's `SHA256SUMS`, and checks that the binary reports the requested
version. Releases before `SHA256SUMS` was published can't be installed with it.

## Open update pull requests on a schedule

Copy this into `.github/workflows/spm-update.yml`:

```yaml
name: spm update
on:
  schedule:
    - cron: "0 6 * * 1"   # Mondays 06:00 UTC
  workflow_dispatch:

permissions:
  contents: write        # push the spm/update-* branches
  pull-requests: write   # open and update the pull requests

jobs:
  update:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v5
      - uses: camunda/spm-cli@v0
      - uses: camunda/spm-cli/update@v0
```

Each run calls [`spm outdated`](/reference/cli-commands#spm-outdated-name) and, for
every outdated dependency, commits the change to `ai.json`/`ai.lock` on its own
`spm/update-<name>` branch and opens (or refreshes) a pull request titled
`chore(deps): update skill <name> to <version>`:

| Declared as | Proposed change |
| --- | --- |
| `"tag": "v1.0.0"` and a newer tag exists | `tag` re-pinned to the newest tag (as `spm add --tag … --force` does). |
| `"tag"` re-pointed upstream | `ai.lock` moved to the tag's new commit, with a warning in the PR. |
| `"branch": "main"` | `ai.lock` moved to the branch's new head (`spm update <name>`). |
| `"version": "^1"` | `ai.lock` moved to the highest tag in range. A newer tag **outside** the range is reported in the run summary but not proposed — widen the range by hand. |
| `"commit"` | Never outdated. |

Transitive dependencies are updated through the skill that requests them, not on
their own. Re-runs are idempotent: an unchanged update is not re-pushed, and a
dependency that moved again updates its existing pull request.

The pull request body shows the old and new version and commit, a compare link
(for GitHub-hosted sources), any other `ai.lock` entries that changed with it
(new transitive dependencies), and the `spm scan` findings for the new code.

### Blocking scan findings

The scan gate does not hide an update: when the new code has a **blocking**
(high or critical) finding, the pull request is still opened, carries a
`> [!CAUTION]` callout with the findings, and gets the `spm-scan-blocked` label
(removed once a later version scans clean). Protect your default branch with a
check on that label, or review those PRs with care: once merged, `spm install`
in a fresh clone stops at the same finding until `SPM_ALLOW_SUSPICIOUS=1` is set.

### Declining an update

Close an update pull request without merging to skip that version: the action
won't reopen it for the same commit. A newer upstream version opens a new pull
request.

### Inputs and outputs

| Input | Default | |
| --- | --- | --- |
| `token` | `github.token` | Token for the GitHub API. See below. |
| `working-directory` | `.` | Directory holding `ai.json`/`ai.lock`. |
| `base` | the checked-out branch | Branch the pull requests target. Required when the checkout is a detached HEAD. |
| `labels` | `dependencies` | Labels added to every update PR (comma- or newline-separated). |
| `blocked-label` | `spm-scan-blocked` | Label for PRs with blocking findings; empty to disable. |
| `dry-run` | `false` | Print the pull requests instead of pushing or calling the API. |

| Output | |
| --- | --- |
| `outdated` | Number of outdated dependencies reported by `spm outdated`. |
| `pull-requests` | JSON array `[{name, kind, branch, result, number, url, blocking}]` of the pull requests touched (`result`: `created`, `updated`, `unchanged`, `declined`). |

The run fails (after updating everything it could) when a dependency's remote
can't be reached or an update can't be pushed or opened.

### CI on update pull requests

Pull requests opened with the default `github.token` [don't trigger other
workflows](https://docs.github.com/en/actions/security-for-github-actions/security-guides/automatic-token-authentication#using-the-github_token-in-a-workflow),
so your CI won't run on them. To get CI, use a GitHub App token (or a
fine-grained personal access token with `contents` and `pull-requests` write)
for **both** the checkout (which `git push` uses) and the action:

```yaml
      - uses: actions/create-github-app-token@v2
        id: app
        with:
          app-id: ${{ vars.SPM_BOT_APP_ID }}
          private-key: ${{ secrets.SPM_BOT_PRIVATE_KEY }}
      - uses: actions/checkout@v5
        with:
          token: ${{ steps.app.outputs.token }}
      - uses: camunda/spm-cli@v0
      - uses: camunda/spm-cli/update@v0
        with:
          token: ${{ steps.app.outputs.token }}
```

Private skill repositories need the same: a token that can read them, e.g. via
`git config --global url."https://x-access-token:${TOKEN}@github.com/".insteadOf "https://github.com/"`
in a step before the update.

## Renovate

If you already run [Renovate](https://docs.renovatebot.com/), a
[regex manager](https://docs.renovatebot.com/modules/manager/regex/) can bump
**tag**-pinned skills in `ai.json`:

```json
{
  "customManagers": [
    {
      "customType": "regex",
      "description": "spm skills and plugins pinned to a tag",
      "fileMatch": ["(^|/)ai\\.json$"],
      "matchStrings": [
        "\"git\":\\s*\"(?<packageName>[^\"]+)\",\\s*\"tag\":\\s*\"(?<currentValue>[^\"]+)\""
      ],
      "datasourceTemplate": "git-tags",
      "depNameTemplate": "{{{packageName}}}"
    }
  ]
}
```

It matches the layout spm writes (`git` first, then `tag`). Renovate only edits
`ai.json`, so **`ai.lock` is left behind** and `spm install --locked` fails on
the PR until someone runs `spm install` and commits the lock. On self-hosted
Renovate you can do that in the PR with
[`postUpgradeTasks`](https://docs.renovatebot.com/configuration-options/#postupgradetasks)
(`"commands": ["spm install"]`, `"fileFilters": ["ai.lock"]`, with `spm` on the
Renovate host and the command in `allowedCommands`). Renovate also doesn't run
`spm scan`, track branches or version ranges, or follow transitive
dependencies — prefer the update action above when you can.
