# AGENTS.md

> This repo follows the central Camunda AGENTS guidelines:
> https://raw.githubusercontent.com/camunda/.github/refs/heads/main/AGENTS.md
> The instructions below extend those and take precedence on conflict.

## Role & boundary

This repo is **spm** (`spm-cli`), a Rust command-line tool: a *skill package
manager*. You declare AI skills as git dependencies in `ai.json`, and `spm`
materializes them for AI tools (Claude Code and GitHub Copilot CLI) **without
committing skills to your repo** — anything written into the working tree is
gitignored, and there are no symlinks. See `README.md` for the user-facing model
and `RELEASE.md` for distribution.

- **Project type**: single-binary CLI (`spm`).
- **Source language**: Rust (edition 2021, MSRV **1.97**, pinned in `.tool-versions`).
- **Build/dependency**: `cargo`, with `make` providing convenience shortcuts.
- **Key dependencies**: `clap` (CLI), `serde`/`serde_json` (config), `anyhow`
  (errors), `directories` (store paths), `jsonschema` (validating `ai.json`).
- **Testing**: `cargo test` — end-to-end tests in `tests/cli/` (one test binary,
  a module per domain) drive the real binary against throwaway git repos.
- **Distribution**: npm (`@camunda8/spm`), crates.io (`spm-cli`), and GitHub
  Release binaries — all driven from a single git tag (see `RELEASE.md`).

Everything under `src/` is hand-written and owned by this repo; there is no
generated source to regenerate.

## Path map

| Path | Ownership and intent |
| --- | --- |
| `src/main.rs` | Binary entry point; wires up modules and maps errors to exit codes. |
| `src/cli.rs` | `clap` command definitions and the top-level command dispatch (`init`, `add`, `remove`, `update`, `install`, `list`, `clean`, `prune`). Primary edit surface for CLI behavior. |
| `src/cli/session.rs` | `ScopeSession`: the only way a command takes the state lock (private `StateLock`, re-entry guarded) and loads `ai.json`/`ai.lock`/links/ownership, in canonical order via `snapshot`. |
| `src/cli/index.rs` | `DependencyIndex`: the one classification of `ai.json` + `ai.lock` entries (direct per kind, transitive, undeclared, `requested_by`) shared by `list`, `status`, `outdated` and `doctor`. |
| `src/cli/sync/mod.rs` | The sync pipeline shared by add/remove/update/install/link: `plan` (a short orchestrator over the phases below) and `LockMode`/`Run`/`Plan`. `resolve.rs` pins direct deps, fetches plugins/skills and walks transitive ones; `validate.rs` holds the pre-resolution checks (policy, targets, links) and post-resolution ones (collisions, `--locked` backstop); `guard.rs` the overlap guard protecting linked working copies and state files; `apply.rs` writes a `Plan` into the targets. |
| `src/cli/verdict.rs` | `LockVerdict`: the one judgement of `ai.lock` against `ai.json` (missing, drift, stale, unresolved, rewritten, unverified) and every message/fix hint about it, rendered as the `install --locked`/`link` error or as `doctor`'s lockfile checks. |
| `src/cli/outdated/` | `spm outdated` (read-only). `mod.rs` is the command; `check.rs` runs one `ls-remote` per repository and compares every selector (primary and `also_satisfies`) with its pin (`newest_tag`, range resolution via the resolver's rule); `upgrade.rs` plans how a transitive bump climbs the requester chain; `report.rs` is the `Row`/`Status` model, which *is* the `--json` document the update Action consumes, plus the shared exit status; `render.rs` is the text table, warnings and summary. |
| `src/manifest/` | The user-authored `ai.json`. `model.rs` holds the `Manifest`/`SkillSpec` types and the kind-agnostic view over the skill and plugin maps (`DependencyKind` lives in `mod.rs`); `de.rs` the serde helpers that reject explicit `null`s, non-object specs and duplicate names; `validate.rs` the semantic spec checks (one selector, contained `path`, full-SHA `commit`); `load.rs` loading, parsing (schema + model) and saving of the root manifest; `dependency.rs` the lenient, size-bounded nested `DependencyManifest`. `schema_parity.rs` (tests) holds the model against `schema/ai.schema.json`: same fields, same accept/reject over a generated corpus, with model-only rules in an explicit allow-list. |
| `src/version.rs` | The version selector (`Version`: tag/branch/commit/semver range) and the one semver requirement parser and tag-to-version rule; a leaf shared by `ids`, `manifest`, `lockfile`, `resolver` and `cli`. |
| `src/lockfile.rs` | The generated, VCS-committed `ai.lock` pin file for reproducible installs. |
| `src/schema.rs` | Embedded JSON Schema validation for `ai.json`. |
| `src/resolver.rs` | Resolves a skill spec (tag/branch/commit) to an immutable commit SHA + store key. |
| `src/store.rs` | Manages the global store (`~/.spm/store/<repo>@<sha>`) — one clone per commit, shared across projects. |
| `src/git/` | Thin wrapper over the system `git` binary (no libgit2), public API re-exported from `mod.rs`: `command.rs` spawns git (hardened env, failure capture, option-injection guard), `transport.rs` applies the `--protocol` rewrite and fallback, `repository.rs` holds the remote/checkout operations. |
| `src/remote.rs` | Remote URL equivalence: `Protocol`, the SSH/HTTPS `rewrite` and the protocol-independent `remote_identity` — the one rule shared by `git` (transport), `policy` (source matching) and `transitive` (dedup). A leaf, so `policy` does not depend on `git`. |
| `src/paths.rs` | Resolves the store (fetch-cache) root; honors the `SPM_HOME` override. Vendor output is project-local, not under this root. |
| `src/fsutil.rs` | `walk_tree`: the single boundary-aware traversal (skips `.git`, follows symlinks only inside the checkout, breaks cycles) behind both the safe copy (`copy_tree`) and the scan, so the gate reads exactly what lands. |
| `src/scan/` | Deterministic content scanner flagging prompt injection, secret exfiltration, encoded/obfuscated payloads and auto-run triggers. `rules.rs` is the rule table (rule id → category → severity, via the `rules!` macro), the pattern lists and one triggering + one benign fixture per rule (a rule without both fails its test); `engine.rs` walks the tree and applies the file-level and line rules; `decode.rs` spots base64/hex blobs decoding to shell; `finding.rs` holds `Severity`/`Category`/`Finding` and its rendering; `gate.rs` is `enforce` (the pre-materialize gate in `sync`) and the `SPM_ALLOW_SUSPICIOUS` override. Also backs `spm scan`. |
| `src/transitive/` | Transitive skill-dependency resolution behind `resolveTransitive`: `expand` fetches, scans and materializes every skill and walks the nested `ai.json` graph. `identity.rs` is the normalized `(git, path)` identity (`normalize_git`) and the synthesized `{requester}__{declared}-{hash}` names; `walk.rs` is the DFS (scan-before-recurse, cycle guard, diamond dedup, version conflicts, depth/node/edge caps); `lock.rs` reuses `ai.lock` pins for children, records `--locked` misses and sorts lock provenance; `diagnostics.rs` renders requester chains and conflict errors. |
| `src/shell.rs` | Quotes names/paths interpolated into suggested commands (fix hints) so they paste as single shell arguments. |
| `src/jsonfile.rs` | The one way spm reads/writes its JSON files (`load`/`load_or_default`/`save`, pretty + trailing newline, path-named errors) and the single atomic-replace implementation (`save_atomic`, `Staged`). |
| `src/jsonutil.rs` | Merge-patches user-owned JSON config without clobbering user keys. |
| `src/materialized.rs` | `MaterializedSkill`/`MaterializedPlugin`: the resolved dependencies handed to the vendors, shared by their producers (`transitive`, `plugin`, `cli` sync) and the `vendor` adapters, so `plugin` does not depend on `vendor`. |
| `src/vendor/mod.rs` | The `Vendor` trait (plus the optional `Registration` and `PluginHost` capabilities) + `for_target` dispatch — the extension point for new targets. |
| `src/vendor/claude.rs` | Claude adapter: writes a marketplace pointer into `.claude/settings.local.json`. |
| `src/vendor/shareddir.rs` | `SharedDirVendor` presets for every "skills one dir deep" tool (Copilot, Gemini, Codex, Cursor, Cline, Windsurf, Amp); an `Ownership` mode says whether the project dir is wholly spm's (Copilot) or shared with the user's own skills. |
| `src/vendor/contract_tests.rs` | The generic `Vendor` contract suite (a `#![cfg(test)]` file): runs every `ALL_TARGETS` entry in both scopes in a throwaway sandbox (global roots redirected per thread by `paths::sandbox`) and asserts stable re-materialize, complete + idempotent clean, user content untouched, no writes outside declared outputs, and exact `status`. A new target is covered without being listed. |
| `skills/spm/SKILL.md` | User-facing agent skill that teaches an agent to operate `spm`, installable with `spm add ... --path skills/spm`. **Update it in the same PR whenever a command, flag, message or behavior it documents changes** (check it against `spm --help`). |
| `tests/cli/` | End-to-end tests against the real binary with an isolated `SPM_HOME` per test: a single `cli` test binary (`main.rs`) with one module per domain. `common.rs` holds the shared harness (`Sandbox`, git fixtures, helpers used by more than one module); a helper used by one module stays in it. `source.rs` is the shared `syn`-based, `#[cfg(test)]`-blind view of `src/` (the `skip_test_only!` visitor list, `cfg` predicates and `shipped_files`, the module files a non-test build compiles) behind the source guards: `lint_policy.rs` (no lint suppressions), `layout.rs` (file-size budget), `gateways.rs` (single-gateway guards over shipped tokens: process spawning only in `src/git/command.rs`, file writes (`fs::write`/`copy`/`rename`, imports of them grouped or not, `File::create`/`create_new`, `OpenOptions`) only via `jsonfile` plus a reasoned allow-list — a guarded module or type may not be renamed (`use std::fs as disk`, `File as Out`, `type Out = File`) or glob-imported (`fs::{self, *}`), macro bodies' imports included (an unparseable one naming a guarded module is rejected), and qualified paths (`<File>::create`) read as their type, OS file locks only in `src/cli/session.rs` (which rejects `fs::File` aliases on its own), `read_dir` pinned to an allow-list: recursive walks go through `fsutil::walk_tree` except three allow-listed ones that must see links rather than follow them (`store.rs` `dir_size`, `cli/sync/guard.rs`, `cli/target.rs`); each list is file-level, shrink-only — a stale entry fails too) and `architecture.rs` — the module layering test, whose `ALLOWED_EDGES` table is the single source of truth for the top-level module graph (plus `LEAVES`, the `cli`-is-top rule and an acyclic graph, cycles reported as strongly connected components); a new or stale edge, any cycle, a table naming a module that does not exist, or a crate-root glob/alias that would hide edges fails until fixed or the table is updated. It also pins the module-layering diagram in `docs/guide/architecture.md` to exactly `ALLOWED_EDGES` and every top-level module (`SPM_BLESS=1` rewrites it). `docs_drift.rs` checks that `skills/spm/SKILL.md` documents every command and flag, and generates the command overview blocks in `README.md` and `docs/reference/cli-commands.md` (between `<!-- cli-overview:start -->`/`<!-- cli-overview:end -->`) from each command's `spm … -h` — its summary line (the first paragraph of the clap doc comment), arguments, options and possible values, plus the required choices of its `Usage:` line (`<NAME|--all>`, from a required clap `ArgGroup`) and the option pairs clap refuses together (probed on the binary, rendered as `[a|b]`) — failing when the committed blocks differ (`SPM_BLESS=1 cargo test cli_overview` rewrites them). |
| `schema/ai.schema.json` | Source of truth for the `ai.json` shape (draft-07); embedded by `src/schema.rs`. |
| `docs/` | VitePress documentation site (published to GitHub Pages). User-facing docs live here — keep it in sync with `README.md` when behavior changes (`reference/` for CLI/`ai.json`/`ai.lock`/schema, `guide/` for concepts/targets). `guide/architecture.md` diagrams the module layering, command flow and install data flow; its module-layering diagram is checked against `ALLOWED_EDGES` by `tests/cli/architecture.rs` (regenerate with `SPM_BLESS=1 cargo test module_graph_diagram`). `package.json` overrides `katex` (pulled in by mermaid) past GHSA-238p-pmpm-9mq7; the docs render no math, so the major bump is safe — drop the override once mermaid ships a fixed katex. |
| `npm/` | npm distribution: `build.mjs` generates the launcher + 5 platform packages from the crate version. `targets.mjs` (release targets + asset names) and `crate.mjs` (crate version) are the single sources shared with the GitHub Actions. |
| `action.yml`, `action/` | Setup GitHub Action (`camunda/spm-cli@v0`): `action/lib/setup.mjs` downloads a release binary and verifies it against `SHA256SUMS`. Dependency-free Node 24 (`action/lib/core.mjs` is the runner toolkit). `action/stage-release.mjs` writes `SHA256SUMS` in `release.yml`. |
| `update/` | Update GitHub Action (`camunda/spm-cli/update@v0`): entry point for `action/lib/update.mjs`, which opens one PR per outdated dependency (`action/lib/report.mjs` builds branch names + PR bodies, `action/lib/github.mjs` is the REST client). |
| `action/test/` | `node --test` suites for both actions, against the real debug `spm`, throwaway git repos and a fake GitHub API; `release.test.mjs` guards release-workflow drift (targets, tag triggers, action inputs/outputs). |
| `Makefile` | Convenience shortcuts over `cargo` (`make check` mirrors the CI gate). |
| `.cargo-husky/hooks/` | Pre-commit hook (fmt + clippy) auto-installed via `cargo-husky` on first `cargo test`/`build`. |
| `.github/workflows/` | `ci.yml` (fmt + clippy + test + MSRV), `release.yml`, `publish.yml`. |

## Build / test / lint

`cargo` is the source of truth; the `Makefile` wraps it. **`make check` mirrors
the CI gate exactly** — run it before pushing.

```bash
make build        # debug build (cargo build)
make release      # optimized (LTO, stripped) build
make test         # cargo test --all (end-to-end suite)
make test-action  # cargo build + node --test the GitHub Actions (Node 24)
make fmt          # cargo fmt --all
make fmt-check    # fail if not formatted
make lint         # cargo clippy --all-targets -- -D warnings
make check        # fmt-check + lint + test + test-action  ← the full CI gate
make run ARGS=".." # cargo run -- $(ARGS)
make install      # release build → $(PREFIX)/bin/spm  (default /usr/local)
```

CI (`.github/workflows/ci.yml`) runs the same gate on Linux, macOS, and Windows
(`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --all`, `node --test "action/test/*.test.mjs"`), an `action-e2e`
job that runs both actions with `uses: ./` and `uses: ./update`, plus an
**MSRV** job that runs `cargo check --locked
--all-targets` on Rust **1.97** to guard the published minimum toolchain. When
you touch dependencies or use newer std/APIs, keep the MSRV job green (or bump
`rust-version` in `Cargo.toml` and `.tool-versions` deliberately).

### Always-green policy

`src/` must be warning- and clippy-clean. Do not silence lints (`#[allow(...)]`,
`--cap-lints`, etc.) in `src/` just to make a build pass — fix the underlying
issue. Formatting, clippy, and tests must all be green before merging.

Both are enforced: `tests/cli/lint_policy.rs` fails on any `#[allow(..)]`,
`#![allow(..)]`, `#[expect(..)]` or `cfg_attr`-wrapped one in `src/` outside
`#[cfg(test)]` items, and `[lints.clippy]` in `Cargo.toml` denies
`unwrap_used`/`expect_used` — propagate errors with `anyhow` context instead.
Test code may unwrap (`clippy.toml`); the `tests/cli` crate is `#![cfg(test)]`
so its shared helpers count as test code.


## No Such Thing as "Flaky Tests"

Intermittently failing tests must always be root-caused and addressed as a product defect (code) or a production-line defect (test). We do not acknowledge the existence of such a thing as "flaky tests".

## No Test Retries

There are to be no test retries. A test must pass or fail deterministically on a
single run — no per-test retry attributes and no CI re-run-on-failure to coax a
green. A test that only passes on a later attempt is non-deterministic, which is
a production-line defect: root-cause and fix it (in the product or the test),
never paper over it with a retry.

## Red/Green Discipline

All bug fixes must have a test that reproduces the defect before modifying code. Red/Green—always.

## Fix the Failure Mode, Don't Just Squash the Bug

Whenever we detect an issue, reason broadly about the defect class and write a test guard for the defect class. Prefer securing surfaces — including suggesting an architectural refactor to eliminate the failure mode categorically — over squashing individual bugs.

## Feature Test Coverage

When adding new features, ensure test coverage over the new surface to prevent undetected regressions.

## Derivation Over Duplication: No Drift Surfaces

Identify and eliminate drift surfaces — duplicate sources of truth. Ensure that everything that can be derived is derived from a single source of truth and has a single canonical implementation. Do not introduce duplication.

## No Task Without a Tracked Issue or PR

Before starting any planned task, first check whether an issue or PR already exists for it. This keeps work visible and traceable at our velocity.

- **Already being worked on → stop and flag.** If a matching issue already has the `in progress` label (or is assigned, or carries a "working on this" comment), or an open PR already exists, do not start work. Stop and flag it to the user with a link.
- **Nothing tracked yet → create and claim it.** If neither is found, create an issue before writing any code, then mark it as claimed and in progress (apply the `in progress` label and/or set Project Status to "In Progress") so nobody else picks it up.
- **Claiming the issue.** Assign it to the requesting user and mark it in progress (apply the `in progress` label and/or set Project Status to "In Progress") before starting work. If assignment isn't possible (the requester isn't a repo collaborator or lacks assign permission), fall back to assigning the working owner/agent and post a comment naming the requester — e.g. `@username is working on this` — so ownership is still recorded. Treat such a comment as an equivalent "In Progress" signal when checking whether a task is already claimed.

Only work on a task once it is tracked and claimed as in progress — never in parallel with an untracked, unclaimed, or already-in-progress item.

### Auto-close issues on merge with closing keywords

To make a merged PR automatically close its tracked issue, the PR body (or a
commit message) **must** use a GitHub closing keyword followed by the issue
number: `Closes #N`, `Fixes #N`, or `Resolves #N` (also their `close`/`closed`,
`fix`/`fixed`, `resolve`/`resolved` variants).

- **`Refs #N` / `Ref #N` / `See #N` do not close anything** — they only create a
  reference link. A PR that says `Refs #16` will merge without closing #16, and
  the issue must then be closed by hand.
- Reserve `Refs #N` for deliberate link-only references (e.g. a partial step
  toward a larger tracking issue that should stay open).
- If a PR merged without auto-closing its issue because it used a non-closing
  keyword (or omitted the reference entirely), close the issue manually with a
  comment pointing at the merged PR.

## Automated Verification Over Human Review Gates

When a property can be verified deterministically and programmatically, encode it as an automated gate — e.g. type-check or CI job — instead of a required human review or approval. Pull a human in for judgement and to be alerted on failure, not to hand-verify what a machine can prove.

- **Encode the check.** Express deterministic properties (correctness, safety, migration ordering, presence, version-serving, no-drift) as a gate that fails the build or deploy; keep required-reviewers lists empty.
- **Escalate on failure only.** Route a human in when a gate fails or a call needs judgement.
- **Reserve human review for the non-deterministic.** Including: design trade-offs, ambiguous requirements, and irreversible actions.
- **Meet a manual gate for a deterministic check? Replace it.** Convert it to an automated gate (or flag it).

## Zero Tolerance for Warnings, Errors, and Test Failures

We do not tolerate warnings, errors, or test failures in this project.

There are no pre-existing failures or warnings, and you will not allow any to enter the codebase.

## Commit messages

We use [Conventional Commits](https://www.conventionalcommits.org/), enforced on
every PR by CI (the `commitlint` job in `.github/workflows/ci.yml`, using
`commitlint.config.js` → `@commitlint/config-conventional`).

Format:

```
<type>(optional scope): <subject>

<body>
```

Common `type` values: `feat`, `fix`, `chore`, `docs`, `style`, `refactor`,
`test`, `ci`, `build`, `perf`. Rules:

- Imperative, lowercase subject ("add SSH support", not "Added SSH support").
- Keep the subject concise; put rationale, links, and detail in the body.
- Mark breaking changes with `BREAKING CHANGE:` in the body/footer.

Attribute the AI author with a **single** `Co-authored-by` trailer: the Copilot
co-author line, with the model name as the author name. Do not add a second,
separate co-author line for the model. Example:
`Co-authored-by: Copilot Opus 4.8 <223556219+Copilot@users.noreply.github.com>`

## Releasing

Releases are driven from a **single git tag** and fan out to npm, crates.io, and
GitHub Release binaries. The crate version in `Cargo.toml` is the single source
of truth (the npm package versions are derived from it). See `RELEASE.md` for the
full process, one-time trusted-publisher setup, and dry-run instructions — do not
hand-edit npm package versions or reuse a released version.