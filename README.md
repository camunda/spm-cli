# spm — skill package manager

Declare AI skills as git dependencies in `ai.json`, and `spm` wires them into your
AI tool (Claude Code and GitHub Copilot CLI) **without ever copying skills into
your repo**. No symlinks in the project, no fragile `.gitignore` rules.

## How it works

```
ai.json ──resolve──▶ ai.lock ──fetch──▶ ~/.spm/store/<repo>@<sha>   (global cache, one clone per commit)
                                              │
                                              └─project──▶ ~/.spm/vendors/<target>/<project>/   (assembled marketplace)
                                                                 │
                                                                 └─register─▶ vendor picks it up (see below)
```

- **`ai.json`** — you author it, commit it. Declares target vendors + skill deps.
- **`ai.lock`** — generated, commit it. Pins every version selector to an immutable commit SHA → reproducible installs.
- **Global store** (`~/.spm/store`) — each repo@commit fetched once, shared across all projects.
- **Vendor projection** (`~/.spm/vendors`) — a self-contained plugin marketplace assembled outside your repo. Both vendors require skills to physically live inside a plugin dir, so spm copies them **here**, never into your project tree.
- **Registration** differs per vendor:
  - **Claude** — spm writes a pointer to the marketplace into `.claude/settings.local.json` (gitignored by convention). Declarative, per-project, zero VCS footprint.
  - **Copilot CLI** — spm shells out to `copilot plugin marketplace add` + `copilot plugin install`. Copilot marketplaces/plugins are **user-global** (no project-local config), so registration is global. spm names the registration by a **stable, path-independent project id** stored in `ai.lock` (`spm-xxxxxxxx`), so a moved or re-cloned checkout re-registers the *same* entry instead of leaving a duplicate. Orphaned registrations (whose local dir no longer exists) are pruned automatically on each `spm install`/`clean`. Requires the `copilot` CLI on PATH.

On a fresh clone, teammates run `spm install` — it rebuilds their own store and re-registers from `ai.lock`. Same model as `node_modules`.

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

### Repo URLs (HTTPS & SSH)

`git` accepts any URL the system `git` understands:

```bash
spm add https://github.com/org/repo --tag v1.0.0            # HTTPS
spm add git@github.com:org/repo.git --branch main           # SSH (scp-style)
spm add ssh://git@github.com/org/repo.git --branch main     # SSH (url form)
```

SSH auth goes through your ssh-agent / keys — spm never handles credentials.
Private HTTPS repos use your git credential helper. spm runs git with
`GIT_TERMINAL_PROMPT=0`, so a missing credential fails with a clear error
instead of hanging on a prompt (helpers and ssh-agent still work).

## Installation

`spm` ships as a single self-contained binary (needs the system `git` on `PATH`
at runtime, plus the `copilot` CLI if you target `copilot`).

**From npm (recommended)** — _pending the initial publish ([#TODO](https://github.com/camunda/spm-cli/issues))_.
Once published, this is the zero-setup path on every platform — it puts `spm` on
your `PATH` with no manual steps:

```bash
npm i -g @camunda8/spm
spm --help
```

`@camunda8/spm` is a thin launcher that pulls in the matching prebuilt binary for
your OS/CPU via an optional dependency (`@camunda8/spm-<os>-<cpu>`), so nothing is
compiled or downloaded outside npm. Supported: `darwin-x64`, `darwin-arm64`,
`linux-x64`, `linux-arm64`, `win32-x64`. Update with `npm i -g @camunda8/spm@latest`.

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

**From crates.io** — _not published yet_. The crate name `spm-cli` is reserved
(the binary is `spm`); once it's published you'll be able to run:

```bash
cargo install spm-cli
```

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
        [--path <subdir>] [--name <local-name>]
spm remove <name>                                  # drop a skill
spm update [name]                                  # re-resolve branches/tags to latest
spm install                                        # rebuild from ai.lock (after clone)
spm list                                           # show skills + pinned commits
spm clean                                          # remove generated vendor config
```

## Design notes

- **Cross-OS**: shells out to the system `git` (no libgit2 build deps); no symlinks; all paths via `std::path`. Runs on Linux, macOS, Windows.
- **`SPM_HOME`** overrides the store/vendor root (default `~/.spm`) — used by tests.
- **Vendor adapters**: adding a target means implementing one `Vendor` trait (`src/vendor/`). Both `claude` and `copilot` assemble the same plugin-marketplace layout (`marketplace.json` → `plugin.json` → `skills/<name>/SKILL.md`); they differ only in how the marketplace is registered.

## Development

`make check` runs the full CI gate locally (`fmt-check` + `clippy` + `test`).

A **pre-commit hook** (fmt + clippy) installs itself automatically via
[`cargo-husky`](https://github.com/rhysd/cargo-husky) — just run `cargo test`
(or `cargo build`) once after cloning and the hook lands in `.git/hooks`. The
hook source lives in [`.cargo-husky/hooks/`](.cargo-husky/hooks). Bypass a
single commit with `git commit --no-verify`.

