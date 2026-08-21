# Releasing spm

`spm` is distributed through three channels, all driven from a single git tag:

| Channel | Package / artifact | Workflow | Auth |
| --- | --- | --- | --- |
| **npm** (recommended for users) | `@camunda8/spm` + `@camunda8/spm-<os>-<cpu>` (×5) | [`release.yml`](.github/workflows/release.yml) → `npm` job | OIDC Trusted Publishing |
| **GitHub Release** | `spm-<target>[.exe]` binaries | [`release.yml`](.github/workflows/release.yml) → `build` job | `GITHUB_TOKEN` |
| **crates.io** | `spm-cli` crate (binary `spm`) | [`publish.yml`](.github/workflows/publish.yml) | OIDC Trusted Publishing |

**The crate version in `Cargo.toml` is the single source of truth.** The npm
package version is derived from it by [`npm/build.mjs`](npm/build.mjs); the
release workflows refuse to publish if the pushed tag doesn't match it. There is
nothing else to bump.

## The npm distribution model

`npm i -g @camunda8/spm` must give a working `spm` on `PATH` with no extra setup,
including on Windows. To do that without a postinstall download (which would fail
against this internal repo's authenticated release assets), spm uses the
esbuild/biome pattern:

- **`@camunda8/spm`** — a tiny launcher package. Its `bin/spm.js` is what npm
  links onto `PATH`. At runtime it resolves the native binary from the matching
  platform package and execs it, forwarding argv, stdio, and the exit code.
- **`@camunda8/spm-<os>-<cpu>`** — five packages, one per platform, each carrying
  a single prebuilt binary and gated by npm's `os`/`cpu` fields. They are listed
  as `optionalDependencies` of the launcher, so npm installs **only** the one
  that fits the host.

All six manifests are generated from one config in `npm/build.mjs`, so they can't
drift from each other or from the crate version.

## One-time setup

Publishing via OIDC requires each package to **already exist** on its registry
(you configure the trusted publisher in the package's settings), so the very
first publish of each is a manual bootstrap. This is tracked in the distribution
issue; the summary:

### crates.io (`spm-cli`)

1. Bootstrap: set a temporary `CARGO_REGISTRY_TOKEN` repo secret and cut a
   release (or `cargo publish` locally once). `publish.yml` prefers the token.
2. On crates.io, register a **Trusted Publisher** for `spm-cli` → repo
   `camunda/spm-cli`, workflow `publish.yml`.
3. Delete the `CARGO_REGISTRY_TOKEN` secret. Every later release uses OIDC.

### npm (`@camunda8/spm` and the 5 platform packages)

1. Bootstrap: either
   - set a temporary `NPM_TOKEN` repo secret (an automation token for the
     `@camunda8` scope) and run the release — `release.yml`'s `npm` job prefers
     it; **or**
   - publish once locally: build the six packages and `npm publish` each
     (platform packages first, then the root):

     ```bash
     # build release binaries for all targets into artifacts/<target>/spm[.exe]
     node npm/build.mjs --bin-dir artifacts
     for d in npm/dist/@camunda8/spm-*; do npm publish "$d" --access public; done
     npm publish npm/dist/@camunda8/spm --access public
     ```
2. On npmjs.com, for **each of the six packages**, add a **Trusted Publisher** →
   GitHub Actions, repo `camunda/spm-cli`, workflow `release.yml`. Each package
   has its own trusted-publisher setting.
3. Delete the `NPM_TOKEN` secret. Every later release authenticates via OIDC.

> Provenance is intentionally not enabled: npm provenance requires a **public**
> source repo, and `camunda/spm-cli` is internal. OIDC Trusted Publishing itself
> works regardless.

## Cutting a release

1. Update the changelog / confirm `main` is green.
2. Bump the version (this drives all three channels). Use the helper — it edits
   `Cargo.toml` and refreshes `Cargo.lock`'s entry for you:

   ```bash
   make bump                 # bump the patch component (default)
   make bump PART=minor      # or minor / major
   make bump VERSION=X.Y.Z   # or an exact version
   git add Cargo.toml Cargo.lock
   git commit -m "chore(release): vX.Y.Z"    # Conventional Commits (CI enforces it)
   ```

   `main` is protected, so land the bump via a PR. `make bump-pr` automates that
   — it bumps on a `chore/bump-vX.Y.Z` branch, pushes, and opens the PR with
   `gh` (same `PART=`/`VERSION=` options). Prefer it over pushing to `main`:

   ```bash
   make bump-pr              # patch bump → branch → PR (needs gh, clean tree)
   ```

   Under the hood this runs `scripts/bump-version.sh`; the equivalent manual
   steps are editing `version` in `Cargo.toml` and
   `cargo update -p spm-cli --precise X.Y.Z`.
3. Tag and push (the tag is what triggers publishing):

   ```bash
   git tag vX.Y.Z
   git push origin main --tags
   ```

The tag push fans out to:

- **`release.yml`** → builds the 5 target binaries, attaches them to the GitHub
  Release, then the `npm` job packages and publishes all six npm packages, and
  finally the `release-notes` job fills in the release description (see below).
- **`publish.yml`** → publishes the `spm-cli` crate to crates.io.

Both verify the tag equals the `Cargo.toml` version before publishing.

## Who can release

Publishing is gated on two independent layers, so only repository
**admins** can cut a release:

1. **Tag ruleset (`restrict-release-tag-creation`)** — a repository ruleset
   restricts creation, update, and deletion of `refs/tags/v*` to repo admins
   (bypass list = the Admin role). Non-admins — including any org member with
   plain write access — simply cannot create a `v*` tag, so the release
   workflow never even starts. Fork-based external contributors already cannot
   push tags upstream.
2. **`authorize` job in `release.yml`** — a defense-in-depth backstop that runs
   first and re-checks, at run time, that the actor who triggered the workflow
   has `admin`/`maintain` permission (via the collaborator-permission API). It
   gates every other job, so a stray tag from a ruleset bypass or future
   settings drift can never build binaries or publish to npm. If the actor is
   unauthorized the run fails immediately.

## Release notes ("What's Changed")

The GitHub Release description is generated from the PRs/commits in the release
by [`scripts/update-release-notes.sh`](scripts/update-release-notes.sh), which
delegates to GitHub's own `releases/generate-notes` API — the same engine as the
"Generate release notes" button — so the body is the canonical
`* <PR title> by @author in <url>` list with no locally-maintained formatting to
drift.

- **Automatic**: the `release-notes` job in `release.yml` runs after `build` on
  every tag and writes the notes onto the just-created release.
- **Backfill / regenerate**: Actions → **release** → *Run workflow* with
  `notes_tag: vX.Y.Z`. This skips the build/npm path and only (re)writes the
  notes for that already-published tag.
- **Locally** (needs an authenticated `gh`):

  ```bash
  scripts/update-release-notes.sh vX.Y.Z            # write notes onto the release
  scripts/update-release-notes.sh vX.Y.Z --dry-run  # preview only, don't modify
  ```

- **Grouping** (optional): add a `.github/release.yml` with `changelog`
  categories and the generated PR list is grouped by label automatically.

- **`--include-commits`**: GitHub's notes list only *merged PRs*. Commits pushed
  directly to `main` (no PR) are omitted — most visible on the first tag, whose
  early history predates the PR workflow. Passing `--include-commits` appends a
  **Commits without a pull request** section for those, deduped against the PRs
  already listed. The `release-notes` job enables this by default (with a full
  `fetch-depth: 0` checkout); it's a no-op when every commit came through a PR.

  ```bash
  scripts/update-release-notes.sh v0.1.0 --include-commits --dry-run
  ```

## Dry runs

- **npm**: Actions → **release** → *Run workflow* with `dry_run: true` (default).
  Builds everything and runs `npm publish --dry-run` for all six packages —
  nothing is uploaded.
- **crates.io**: Actions → **Publish** → *Run workflow* with `dry_run: true`.
  Runs `cargo publish --dry-run`.

## Verifying a release

```bash
# npm — installs and runs on the host with no extra setup
npm i -g @camunda8/spm && spm --version

# crates.io
cargo install spm-cli && spm --version

# GitHub Release (internal repo → authenticated download)
gh release download vX.Y.Z --repo camunda/spm-cli \
  --pattern 'spm-aarch64-apple-darwin' --output spm && ./spm --version
```

Check all six npm packages resolved to the new version:

```bash
npm view @camunda8/spm version
for p in linux-x64 linux-arm64 darwin-x64 darwin-arm64 win32-x64; do
  echo "@camunda8/spm-$p -> $(npm view @camunda8/spm-$p version)"
done
```

## Notes

- **Publish order matters** on npm: platform packages are published before the
  root launcher, because the launcher's `optionalDependencies` pin their exact
  version. The workflow does this automatically.
- **A failed npm job** can be re-run from the Actions tab; `npm publish` is
  idempotent per version (already-published packages are skipped/error and can
  be ignored). To ship a fix, bump to a new patch version and re-tag.
- **Never reuse a version.** Both registries reject republishing an existing
  version; always bump.
