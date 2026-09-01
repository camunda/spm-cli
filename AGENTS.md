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
- **Testing**: `cargo test` — end-to-end tests in `tests/cli.rs` drive the real
  binary against throwaway git repos.
- **Distribution**: npm (`@camunda8/spm`), crates.io (`spm-cli`), and GitHub
  Release binaries — all driven from a single git tag (see `RELEASE.md`).

Everything under `src/` is hand-written and owned by this repo; there is no
generated source to regenerate.

## Path map

| Path | Ownership and intent |
| --- | --- |
| `src/main.rs` | Binary entry point; wires up modules and maps errors to exit codes. |
| `src/cli.rs` | `clap` command definitions and the top-level command dispatch (`init`, `add`, `remove`, `update`, `install`, `list`, `clean`, `prune`). Primary edit surface for CLI behavior. |
| `src/manifest.rs` | The user-authored `ai.json` model (targets + skill specs) and its (de)serialization. |
| `src/lockfile.rs` | The generated, VCS-committed `ai.lock` pin file for reproducible installs. |
| `src/schema.rs` | Embedded JSON Schema validation for `ai.json`. |
| `src/resolver.rs` | Resolves a skill spec (tag/branch/commit) to an immutable commit SHA + store key. |
| `src/store.rs` | Manages the global store (`~/.spm/store/<repo>@<sha>`) — one clone per commit, shared across projects. |
| `src/git.rs` | Thin wrapper over the system `git` binary (no libgit2); handles SSH/HTTPS and non-interactive auth. |
| `src/paths.rs` | Resolves the store (fetch-cache) root; honors the `SPM_HOME` override. Vendor output is project-local, not under this root. |
| `src/fsutil.rs` | Safe recursive copy (skips `.git`, does not follow symlinks to avoid exfiltration). |
| `src/jsonutil.rs` | Merge-patches user-owned JSON config without clobbering user keys. |
| `src/vendor/mod.rs` | The `Vendor` trait + `for_target` dispatch — the extension point for new targets. |
| `src/vendor/claude.rs` | Claude adapter: writes a marketplace pointer into `.claude/settings.local.json`. |
| `src/vendor/copilot.rs` | Copilot adapter: copies skills into the gitignored, project-local `.agents/skills/spm-managed-skills/`. |
| `tests/cli.rs` | End-to-end tests against the real binary with an isolated `SPM_HOME` per test. |
| `schema/ai.schema.json` | Source of truth for the `ai.json` shape (draft-07); embedded by `src/schema.rs`. |
| `docs/` | VitePress documentation site (published to GitHub Pages). User-facing docs live here — keep it in sync with `README.md` when behavior changes (`reference/` for CLI/`ai.json`/`ai.lock`/schema, `guide/` for concepts/targets). |
| `npm/` | npm distribution: `build.mjs` generates the launcher + 5 platform packages from the crate version. |
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
make fmt          # cargo fmt --all
make fmt-check    # fail if not formatted
make lint         # cargo clippy --all-targets -- -D warnings
make check        # fmt-check + lint + test  ← the full CI gate
make run ARGS=".." # cargo run -- $(ARGS)
make install      # release build → $(PREFIX)/bin/spm  (default /usr/local)
```

CI (`.github/workflows/ci.yml`) runs the same gate on Linux, macOS, and Windows
(`cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test --all`), plus an **MSRV** job that runs `cargo check --locked
--all-targets` on Rust **1.97** to guard the published minimum toolchain. When
you touch dependencies or use newer std/APIs, keep the MSRV job green (or bump
`rust-version` in `Cargo.toml` and `.tool-versions` deliberately).

### Always-green policy

`src/` must be warning- and clippy-clean. Do not silence lints (`#[allow(...)]`,
`--cap-lints`, etc.) in `src/` just to make a build pass — fix the underlying
issue. Formatting, clippy, and tests must all be green before merging.


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