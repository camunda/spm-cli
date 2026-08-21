# spm — skill package manager

Declare AI skills as git dependencies in `ai.json`, and `spm` wires them into your
AI tool (Claude Code and GitHub Copilot CLI) **without ever committing skills to
your repo**. Anything spm materializes into the working tree is gitignored — no
symlinks, no skills under version control.

## How it works

```
ai.json ──resolve──▶ ai.lock ──fetch──▶ ~/.spm/store/<repo>@<sha>   (global cache, one clone per commit)
                                              │
                                              └─project──▶ materialized where the vendor expects it (see below)
```

- **`ai.json`** — you author it, commit it. Declares target vendors + skill deps.
- **`ai.lock`** — generated, commit it. Pins every version selector to an immutable commit SHA → reproducible installs.
- **Global store** (`~/.spm/store`) — a **fetch cache only**: each repo@commit is cloned once and shared across all projects. Nothing is *registered* or *materialized* here — it exists purely so repeated installs don't re-clone.
- **Vendor projection** — spm copies the store's skills into a **project-local** directory wherever each vendor loads them from. Nothing spm generates is committed to your repo, and nothing is written into a user-global vendor location.
- **Registration** differs per vendor:
  - **Claude** — spm assembles a self-contained plugin marketplace in the **project-local**, gitignored `.spm/claude/` dir and writes a pointer to it into `.claude/settings.local.json` (gitignored by convention). The dir sits outside `.agents/skills/` so Copilot's scanner never picks it up. Declarative, per-project, zero VCS footprint.
  - **Copilot CLI** — spm copies the resolved skills into a **project-local** directory, `.agents/skills/spm-managed-skills/<name>/`, where Copilot CLI auto-discovers them (`.agents/skills/**/SKILL.md`). That directory is added to the project's `.gitignore` (with an explanatory comment) so the materialized skills stay truly local and are never committed. No user-global state, no `copilot` CLI required.

On a fresh clone, teammates run `spm install` — it repopulates their own fetch cache and re-materializes the project-local skills from `ai.lock`. Same model as `node_modules`.

## ai.json

```json
{
  "targets": ["claude", "copilot"],
  "skills": {
    "pdf-tools": { "git": "https://github.com/org/skills", "tag": "v1.2.0", "path": "skills/pdf" },
    "reviewer":  { "git": "https://github.com/me/reviewer", "branch": "main" },
    "pinned":    { "git": "https://github.com/x/y",         "commit": "a1b2c3d" }
  }
}
```

`targets` lists one or more vendors (`claude`, `copilot`) — skills resolve once
and project into each independently.

### Schema & validation

`ai.json` is described by a JSON Schema at [`schema/ai.schema.json`](schema/ai.schema.json)
(draft-07). spm embeds it and validates every `ai.json` on load, reporting all
violations at once with their JSON path:

```
error: in ai.json: ai.json does not match schema:
  at /skills/x: {"git":"u"} is not valid under any of the schemas listed in the 'oneOf' keyword
```

Add a `"$schema"` reference for editor autocompletion/validation:

```json
{ "$schema": "./schema/ai.schema.json", "targets": ["claude"], "skills": {} }
```

Version selectors (exactly one per skill):

| field    | meaning                                    | locked to      |
|----------|--------------------------------------------|----------------|
| `tag`    | git tag (annotated tags deref to commit)   | resolved SHA   |
| `branch` | branch tip at install/update time          | resolved SHA   |
| `commit` | exact commit                               | itself         |

`path` (optional) selects a subdirectory — for monorepos holding many skills.

To pull in **every** skill under a directory at once (each immediate
subdirectory that has its own `SKILL.md`), add `--all` instead of naming them
one by one:

```bash
spm add https://github.com/org/repo --tag v1.0.0 --path skills --all
```

Each sub-skill becomes its own `ai.json` entry, keyed by its directory name
(`--all` cannot be combined with `--name`). This is the one-shot equivalent of
the per-skill `spm add … --path <sub> --name <sub>` commands spm suggests when
you point `--path` at a container of skills.

### Repo URLs (HTTPS & SSH)

`git` accepts any URL the system `git` understands:

```bash
spm add https://github.com/org/repo --tag v1.0.0            # HTTPS
spm add git@github.com:org/repo.git --branch main           # SSH (scp-style)
spm add ssh://git@github.com/org/repo.git --branch main     # SSH (url form)
```

**Any git host works** — spm shells out to `git` and never detects or
special-cases a provider, so GitHub, GitLab, Bitbucket, and self-hosted servers
are all supported with no extra config:

```bash
spm add git@bitbucket.org:org/repo.git --branch main        # Bitbucket
spm add https://gitlab.com/org/repo.git --tag v1.0.0        # GitLab
spm add ssh://git@git.internal.example.com:7999/p/repo.git --branch main  # self-hosted
```

SSH auth goes through your ssh-agent / keys — spm never handles credentials.
Private HTTPS repos use your git credential helper. spm runs git with
`GIT_TERMINAL_PROMPT=0`, so a missing credential fails with a clear error
instead of hanging on a prompt (helpers and ssh-agent still work).

## Installation

`spm` ships as a single self-contained binary (needs the system `git` on `PATH`
at runtime).

**From npm (recommended)** — the zero-setup path on every platform. It puts `spm`
on your `PATH` with no manual steps:

```bash
npm i -g @camunda8/spm
spm --help
```

`@camunda8/spm` is a thin launcher that pulls in the matching prebuilt binary for
your OS/CPU via an optional dependency (`@camunda8/spm-<os>-<cpu>`), so nothing is
compiled or downloaded outside npm. Supported: `darwin-x64`, `darwin-arm64`,
`linux-x64`, `linux-arm64`, `win32-x64`. Update with `npm i -g @camunda8/spm@latest`.

**From crates.io** — build and install from source via Cargo (needs a Rust
toolchain). The crate is `spm-cli`; the installed binary is `spm`:

```bash
cargo install spm-cli
```

**Prebuilt binary** — the repo is **internal**, so release assets require
authentication. Download with the [GitHub CLI](https://cli.github.com/) (you must
be signed in via `gh auth login` and be a Camunda org member), then put the binary
on your `PATH`:

```bash
# pick the asset for your platform (see list below); example: Apple Silicon macOS
gh release download --repo camunda/spm-cli \
  --pattern 'spm-aarch64-apple-darwin' --output spm
chmod +x spm && sudo mv spm /usr/local/bin/
```

`--repo camunda/spm-cli` with no tag grabs the latest release; add
`v0.1.0` as the first positional arg to pin a specific version.

Assets: `spm-x86_64-unknown-linux-gnu`, `spm-aarch64-unknown-linux-gnu`,
`spm-x86_64-apple-darwin`, `spm-aarch64-apple-darwin`,
`spm-x86_64-pc-windows-msvc.exe`.

**From source:**

```bash
git clone https://github.com/camunda/spm-cli && cd spm-cli
make install                  # release build → /usr/local/bin/spm
make install PREFIX=~/.local  # or a custom prefix
# or: cargo install --path .
```

## Commands

```bash
spm init [--target claude|copilot ...]             # scaffold ai.json (repeatable / comma-separated)
spm add <git> (--tag|--branch|--commit <v>) \      # add + install a skill
        [--path <subdir>] [--name <local-name>] [--all]  # --all: add every skill under --path
spm target add [vendor ...]                        # add target vendor(s); no arg = pick interactively
spm remove <name>                                  # drop a skill
spm update [name]                                  # re-resolve branches/tags to latest
spm install                                        # rebuild from ai.lock (after clone)
spm list                                           # show skills + pinned commits
spm status                                         # check skills are materialized in this checkout
spm clean                                          # remove generated vendor config
spm prune [--yes]                                  # wipe the global fetch cache ($SPM_HOME/store, default ~/.spm/store)
```

## Worktrees & fresh clones

spm materializes skills into **gitignored** project-local dirs (`.spm/claude/`,
`.agents/skills/spm-managed-skills/`). Git **worktrees** have their own working
tree and don't share those untracked files, so — exactly like `node_modules` —
**each checkout needs its own `spm install`**:

```bash
git worktree add ../feature -b feature
cd ../feature && spm install         # materialize this worktree's skills
```

Skipping this is the usual reason an agent doesn't see a declared skill in a new
worktree or a fresh clone. `spm status` tells you at a glance and **exits
non-zero** when anything is missing, so it works in scripts too:

```bash
spm status
# [claude]  0/1 installed  .../.spm/claude/plugin/skills
#   reviewer  MISSING
# error: some declared skills are not materialized in this checkout — run `spm install` here
```

To install automatically on every branch checkout and new worktree, add a
`post-checkout` git hook (worktrees share the repo's `.git/hooks`):

```sh
# .git/hooks/post-checkout   — then: chmod +x .git/hooks/post-checkout
#!/bin/sh
# Re-materialize spm skills so Claude/Copilot always see the declared set.
[ -f ai.lock ] && command -v spm >/dev/null 2>&1 && spm install >/dev/null 2>&1
exit 0
```

> **Claude note:** `spm install` writes the *absolute* path of the current
> checkout's `.spm/claude/` into that checkout's `.claude/settings.local.json`.
> Since that file is gitignored, a new worktree either has no registration at all
> or — if it was copied over — one still pointing at the checkout it came from.
> Either way, run `spm install` inside the worktree and start (or
> `/reload-plugins` in) the Claude session from that same worktree; discovery is
> snapshotted at session start. `spm status` reports a stale pointer explicitly:
>
> ```
>   ! .claude/settings.local.json marketplace points at /repo/.spm/claude, not this checkout (/repo-feature/.spm/claude)
> ```

To see what each harness actually loaded: `claude plugin list` /
`claude plugin marketplace list` for Claude; `copilot skill list` for Copilot.

## Design notes

- **Cross-OS**: shells out to the system `git` (no libgit2 build deps); no symlinks; all paths via `std::path`. Runs on Linux, macOS, Windows.
- **`SPM_HOME`** overrides the store root (default `~/.spm`, holding only the fetch cache) — used by tests. Vendor output is always project-local and is not affected by `SPM_HOME`.
- **Vendor adapters**: adding a target means implementing one `Vendor` trait (`src/vendor/`). `claude` assembles a plugin-marketplace layout (`marketplace.json` → `plugin.json` → `skills/<name>/SKILL.md`) into the gitignored project-local `.spm/claude/` and points to it; `copilot` copies skills into the gitignored project-local `.agents/skills/spm-managed-skills/`. Both keep their materialized files out of VCS via the shared `src/gitignore.rs` helper.

## Development

`make check` runs the full CI gate locally (`fmt-check` + `clippy` + `test`).

To cut a release, bump the crate version (the single source of truth for
crates.io, npm, and the GitHub Release) with `make bump` — `PART=patch|minor|major`
(default `patch`) or `VERSION=X.Y.Z`. Since `main` is protected, `make bump-pr`
does the bump on a branch and opens the PR for you. See [`RELEASE.md`](RELEASE.md)
for the full procedure.

A **pre-commit hook** (fmt + clippy) installs itself automatically via
[`cargo-husky`](https://github.com/rhysd/cargo-husky) — just run `cargo test`
(or `cargo build`) once after cloning and the hook lands in `.git/hooks`. The
hook source lives in [`.cargo-husky/hooks/`](.cargo-husky/hooks). Bypass a
single commit with `git commit --no-verify`.

