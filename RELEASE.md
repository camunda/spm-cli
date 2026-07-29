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
2. Bump the version (this drives all three channels):

   ```bash
   # edit `version` in Cargo.toml, then refresh the lockfile entry:
   cargo update -p spm-cli --precise X.Y.Z   # or edit Cargo.lock's spm-cli entry
   git add Cargo.toml Cargo.lock
   git commit -m "chore(release): vX.Y.Z"    # Conventional Commits (CI enforces it)
   ```
3. Tag and push (the tag is what triggers publishing):

   ```bash
   git tag vX.Y.Z
   git push origin main --tags
   ```

The tag push fans out to:

- **`release.yml`** → builds the 5 target binaries, attaches them to the GitHub
  Release, then the `npm` job packages and publishes all six npm packages.
- **`publish.yml`** → publishes the `spm-cli` crate to crates.io.

Both verify the tag equals the `Cargo.toml` version before publishing.

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
