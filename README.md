# spm — skill package manager

Declare AI skills as git dependencies in `ai.json`, and `spm` wires them into your
AI tool (Claude Code, OpenAI Codex CLI, GitHub Copilot CLI, Cursor and Gemini CLI) **without ever committing
skills to your repo**. Anything spm materializes into the working tree is
gitignored — no symlinks, no skills under version control.

📖 **Documentation:** <https://camunda.github.io/spm-cli/> (built from
[`docs/`](docs/) and deployed via GitHub Pages).

## How it works

```
ai.json ──resolve──▶ ai.lock ──fetch──▶ ~/.spm/store/<repo>@<sha>   (global cache, one clone per commit)
                                              │
                                              └─project──▶ materialized where the vendor expects it (see below)
```

- **`ai.json`** — you author it, commit it. Declares target vendors + skill deps.
- **`ai.lock`** — generated, commit it. Pins every version selector to an immutable commit SHA → reproducible installs.
- **Global store** (`~/.spm/store`) — a **fetch cache only**: each repo@commit is cloned once and shared across all projects. Nothing is *registered* or *materialized* here — it exists purely so repeated installs don't re-clone.
- **Vendor projection** — spm copies the store's skills into the directory where each vendor loads them from. In the default **project** scope that is a **project-local**, gitignored dir — nothing spm generates is committed. With `-g`/`--global` (see [Global skills](#global-skills--g----global)) it materializes into a **user-global** location shared across all your projects instead.
- **Registration** differs per vendor:
  - **Claude** — spm assembles a self-contained plugin marketplace in the **project-local**, gitignored `.spm/claude/` dir and writes a pointer to it into `.claude/settings.local.json` (gitignored by convention). The dir sits outside `.agents/skills/` so Copilot's scanner never picks it up. Declarative, per-project, zero VCS footprint.
  - **Copilot CLI** — spm copies the resolved skills into a **project-local** directory, `.agents/skills/spm-managed-skills/<name>/`, where Copilot CLI auto-discovers them (`.agents/skills/**/SKILL.md`). That directory is added to the project's `.gitignore` (with an explanatory comment) so the materialized skills stay truly local and are never committed. No user-global state, no `copilot` CLI required.
  - **Gemini CLI** — spm copies the resolved skills one directory deep into the tool-native `.gemini/skills/<name>/`, where Gemini CLI auto-discovers them. Because Gemini treats that dir as a team-shared, version-controlled location, spm shares it with your own hand-authored skills: it never wipes the dir, touches only the entries it manages, and gitignores just those spm-managed subdirs (`.gemini/skills/<name>/`) so they stay local while your own skills remain committable.
  - **Codex CLI** — spm copies the resolved skills one directory deep into the cross-tool `.agents/skills/<name>/` alias (the same standard location Copilot and Gemini can read), where Codex CLI auto-discovers them. Same shared-dir handling as Gemini: spm never wipes the dir, touches only its managed entries, and gitignores just those spm-managed subdirs (`.agents/skills/<name>/`).
  - **Cursor** — spm copies the resolved skills one directory deep into the tool-native `.cursor/skills/<name>/`, where Cursor auto-discovers them. Same shared-dir handling as Gemini: Cursor treats that dir as version-controlled, so spm never wipes it, touches only the entries it manages, and gitignores just those spm-managed subdirs (`.cursor/skills/<name>/`).

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

`targets` lists one or more vendors (`claude`, `codex`, `copilot`, `cursor`, `gemini`) — skills
resolve once and project into each independently.

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

**Prebuilt binary** — download a release asset directly; no authentication
required (the repo is public). The easiest way is the
[GitHub CLI](https://cli.github.com/):

```bash
# pick the asset for your platform (see list below); example: Apple Silicon macOS
gh release download --repo camunda/spm-cli \
  --pattern 'spm-aarch64-apple-darwin' --output spm
chmod +x spm && sudo mv spm /usr/local/bin/
```

`--repo camunda/spm-cli` with no tag grabs the latest release; add
`v0.1.0` as the first positional arg to pin a specific version.

Without `gh`, download straight from the public release URL with `curl`:

```bash
# latest release; swap the asset name for your platform
curl -fsSL -o spm \
  https://github.com/camunda/spm-cli/releases/latest/download/spm-aarch64-apple-darwin
chmod +x spm && sudo mv spm /usr/local/bin/
```

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
spm init [--target claude|codex|copilot|cursor|gemini ...] [-g]  # scaffold ai.json (repeatable / comma-separated)
spm add <git> (--tag|--branch|--commit <v>) \      # add + install a skill
        [--path <subdir>] [--name <local-name>] [--all] [-g]  # --all: add every skill under --path
spm target add [vendor ...]                        # add target vendor(s); no arg = pick interactively
spm remove <name> [-g]                             # drop a skill
spm update [name] [-g]                              # re-resolve branches/tags to latest
spm install [-g]                                   # rebuild from ai.lock (after clone)
spm list [-g]                                      # show skills + pinned commits
spm status [-g]                                    # check skills are materialized in this checkout
spm clean [-g]                                     # remove generated vendor config
spm prune [--yes]                                  # wipe the global fetch cache ($SPM_HOME/store, default ~/.spm/store)
```

## Global skills (`-g` / `--global`)

By default every command operates on the **project** in the current directory.
Pass `-g` (`--global`) to instead manage a **user-global** set of skills that is
available to your AI tools in *every* project:

```bash
spm init -g --target copilot                       # create the global manifest ($SPM_HOME/ai.json)
spm add  -g <git> --tag v1.0.0 --name reviewer     # install a skill globally
spm list -g                                        # list global skills
spm remove -g reviewer                             # drop a global skill
spm clean  -g                                      # remove global vendor config
```

- The global **manifest + lock** live under `$SPM_HOME` (default `~/.spm/ai.json`
  and `~/.spm/ai.lock`) — commit/sync them with your dotfiles for a reproducible
  personal setup. They reuse the same fetch cache as project installs.
- **Where global skills materialize:**
  - **Copilot CLI** → `~/.copilot/skills/<name>/` (its personal-skills dir). This
    directory is *shared* with skills you author by hand, so spm only ever
    touches the entries it manages and never wipes the whole directory.
  - **Gemini CLI** → `~/.gemini/skills/<name>/` (its user-skills dir). Also
    *shared* with your own hand-authored skills, so spm touches only its managed
    entries and never wipes the directory.
  - **Codex CLI** → `~/.agents/skills/<name>/` (the cross-tool user alias). Also
    *shared*, so spm touches only its managed entries and never wipes the
    directory.
  - **Cursor** → `~/.cursor/skills/<name>/` (its user-skills dir). Also *shared*,
    so spm touches only its managed entries and never wipes the directory.
  - **Claude** → a self-contained marketplace under `$SPM_HOME/claude-global/`,
    registered in `~/.claude/settings.json` under the marketplace name
    `spm-global` (skills invoked as `/spm-global:<name>`). A distinct name keeps
    it from colliding with a project's `spm` marketplace.
- A skill installed in **both** scopes collides by name at discovery time
  (`/spm:foo` vs `/spm-global:foo` for Claude; a duplicate `foo` dir for
  Copilot). `spm status` warns when it detects such a global/project shadow.

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
- **Vendor adapters**: adding a target means implementing one `Vendor` trait (`src/vendor/`). `claude` assembles a plugin-marketplace layout (`marketplace.json` → `plugin.json` → `skills/<name>/SKILL.md`) into the gitignored project-local `.spm/claude/` and points to it; `copilot` copies skills into the gitignored project-local `.agents/skills/spm-managed-skills/`; `gemini`, `codex` and `cursor` copy skills one level deep into a shared, team-committable skills dir (`.gemini/skills/`, the cross-tool `.agents/skills/` alias, and `.cursor/skills/` respectively) via the generic `src/vendor/shareddir.rs` adapter. All share the `src/vendor/dirskills.rs` copy/remove helpers and keep their materialized files out of VCS via the shared `src/gitignore.rs` helper.

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

