# spm — skill package manager

Declare AI skills as git dependencies in `ai.json`, and `spm` wires them into your
AI tool (Claude Code today, Copilot next) **without ever copying skills into your
repo**. No symlinks in the project, no fragile `.gitignore` rules.

## How it works

```
ai.json ──resolve──▶ ai.lock ──fetch──▶ ~/.spm/store/<repo>@<sha>   (global cache, one clone per commit)
                                              │
                                              └─project──▶ ~/.spm/vendors/<target>/<project>/   (assembled marketplace)
                                                                 │
                                                                 └─pointer──▶ .claude/settings.local.json   (gitignored)
```

- **`ai.json`** — you author it, commit it. Declares target vendor + skill deps.
- **`ai.lock`** — generated, commit it. Pins every version selector to an immutable commit SHA → reproducible installs.
- **Global store** (`~/.spm/store`) — each repo@commit fetched once, shared across all projects.
- **Vendor projection** (`~/.spm/vendors`) — a self-contained marketplace assembled outside your repo. Claude requires skills to physically live inside a plugin dir, so spm copies them **here**, never into your project tree.
- **Project pointer** — spm registers the marketplace via `.claude/settings.local.json`, which Claude Code gitignores by convention. Zero VCS footprint, zero new gitignore lines.

On a fresh clone, teammates run `spm install` — it rebuilds their own store + local pointer from `ai.lock`. Same model as `node_modules`.

## ai.json

```json
{
  "target": "claude",
  "skills": {
    "pdf-tools": { "git": "https://github.com/org/skills", "tag": "v1.2.0", "path": "skills/pdf" },
    "reviewer":  { "git": "https://github.com/me/reviewer", "branch": "main" },
    "pinned":    { "git": "https://github.com/x/y",         "commit": "a1b2c3d" }
  }
}
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

## Commands

```bash
spm init [--target claude|copilot]                 # scaffold ai.json
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
- **Vendor adapters**: adding a target means implementing one `Vendor` trait (`src/vendor/`). Copilot is stubbed pending confirmation of its instruction-pickup layout.
