# CLI Commands

The full `spm` command surface. Run `spm --help` or `spm <command> --help` for
the authoritative, version-specific usage.

```bash
spm init [--target amp|claude|cline|codex|copilot|cursor|gemini|windsurf ...] [-g]  # scaffold ai.json (repeatable / comma-separated)
spm add <git> (--tag|--branch|--commit <v>) \      # add + install a skill
        [--path <subdir>] [--name <local-name>] [--all] [--force] [-g]  # --all: add every skill under --path
        [--plugin]                                 # --plugin: add a full plugin instead of a skill
spm target add [vendor ...]                        # add target vendor(s); no arg = pick interactively
spm remove <name> [--plugin] [-g]                  # drop a skill (or a plugin with --plugin)
spm update [name] [-g]                             # re-resolve branches/tags to latest
spm install [-g]                                   # rebuild from ai.lock (after clone)
spm list [-g]                                      # show skills + pinned commits
spm status [-g]                                    # check skills are materialized in this checkout
spm clean [-g]                                     # remove generated vendor config
spm prune [--yes]                                  # wipe the global fetch cache ($SPM_HOME/store, default ~/.spm/store)
```

## Scope: project (default) vs. global (`-g`)

Every command except `target add` and `prune` accepts `-g` / `--global`. Without
it, the command operates on the **project** in the current directory. With it,
the command manages a **user-global** set of skills available to your AI tools in
every project:

- The global **manifest + lock** live under `$SPM_HOME` (default `~/.spm/ai.json`
  / `~/.spm/ai.lock`) and reuse the same fetch cache as project installs.
- Global skills materialize into user-global vendor locations:
  `~/.copilot/skills/<name>/` for Copilot, `~/.gemini/skills/<name>/` for Gemini,
  `~/.agents/skills/<name>/` for Codex, `~/.cursor/skills/<name>/` for Cursor,
  `~/.cline/skills/<name>/` for Cline, `~/.codeium/windsurf/skills/<name>/` for
  Windsurf, `~/.config/agents/skills/<name>/` for Amp, and a marketplace under
  `$SPM_HOME/claude-global/` registered in `~/.claude/settings.json` as
  `spm-global` (skills invoked as `/spm-global:<name>`) for Claude.
- The shared-dir tools' global dirs (Copilot, Gemini, Codex, Cursor, Cline,
  Windsurf, Amp) are shared with your hand-authored skills, so spm only touches
  the entries it manages there — it never wipes the directory.
- `spm status` warns when a skill name is installed in **both** scopes, since the
  two collide by name at discovery time.

```bash
spm init -g --target copilot
spm add  -g https://github.com/org/repo --tag v1.0.0 --name reviewer
spm status -g
```

## Command details

### `spm init`

Scaffolds an `ai.json`. `--target` is repeatable and comma-separated to declare
one or more vendors up front.

### `spm add`

Adds a skill to `ai.json`, resolves it to an immutable commit, pins it in
`ai.lock`, and materializes it — in one step. Provide exactly one version
selector:

| flag       | meaning                                  |
|------------|------------------------------------------|
| `--tag`    | git tag (annotated tags deref to commit) |
| `--branch` | branch tip at install/update time        |
| `--commit` | exact commit                             |

- `--path <subdir>` selects a subdirectory (for monorepos holding many skills).
- `--name <local-name>` sets the `ai.json` key for the skill.
- `--all` adds **every** skill under `--path` (each immediate subdirectory with
  its own `SKILL.md`), keyed by directory name. `--all` cannot be combined with
  `--name`.
- `--plugin` adds a **full plugin** instead of a single skill (see below).
  Point `--path` at the plugin root (the directory holding
  `.claude-plugin/plugin.json`). `--plugin` cannot be combined with `--all`.

```bash
spm add https://github.com/org/repo --tag v1.0.0 --path skills --all
```

#### Adding a full plugin (`--plugin`)

Beyond individual skills, spm can install a **Claude Code plugin** — one that
bundles agents, MCP servers, hooks and scripts in addition to (or instead of)
skills. Pass `--plugin` and point `--path` at the plugin root:

```bash
spm add https://github.com/camunda/design-system --branch main \
        --path plugins/camunda-design-system --plugin --name design-system
```

The plugin is written to the `plugins` map in [`ai.json`](/reference/ai-json),
pinned in [`ai.lock`](/reference/ai-lock), and materialized:

- **Claude** gets the whole plugin registered under a dedicated, project-local
  `spm-plugins` marketplace (`.spm/claude-plugins/`), so its agents, MCP servers,
  hooks and scripts all load.
- **Every other target** (Copilot, Gemini, Codex, …) gets the plugin's **bundled
  skills**, flattened into that target's normal skills location.

A bundled skill whose name collides with a standalone `skills` entry (or another
plugin's skill) is a hard error, never a silent overwrite.

### `spm target add`

Adds one or more target vendors. With no argument, prompts you to pick
interactively.

### `spm remove <name>`

Drops a skill from `ai.json` (and its materialized output). Pass `--plugin` to
remove a **full plugin** (from the `plugins` map) instead of a skill:

```bash
spm remove reviewer              # drop a skill
spm remove design-system --plugin  # drop a plugin
```

### `spm update [name]`

Re-resolves `branch`/`tag` selectors to their latest commit and updates
`ai.lock`. With no name, updates all skills.

### `spm install`

Rebuilds the materialized skills from `ai.lock`. This is the command teammates
run on a fresh clone and in each new worktree.

### `spm list`

Shows declared skills and their pinned commits.

### `spm status`

Checks that declared skills are materialized in the current checkout. **Exits
non-zero** when anything is missing or a Claude marketplace pointer is stale — so
it works in scripts and git hooks. See [Worktrees & Fresh Clones](/guide/worktrees).

### `spm clean`

Removes generated vendor config from the project.

### `spm prune [--yes]`

Wipes the global fetch cache (`$SPM_HOME/store`, default `~/.spm/store`). `--yes`
skips the confirmation prompt.

## Repo URLs (HTTPS & SSH)

`git` accepts any URL the system `git` understands:

```bash
spm add https://github.com/org/repo --tag v1.0.0            # HTTPS
spm add git@github.com:org/repo.git --branch main           # SSH (scp-style)
spm add ssh://git@github.com/org/repo.git --branch main     # SSH (url form)
```

SSH auth goes through your ssh-agent / keys — spm never handles credentials.
Private HTTPS repos use your git credential helper. spm runs git with
`GIT_TERMINAL_PROMPT=0`, so a missing credential fails with a clear error instead
of hanging on a prompt (helpers and ssh-agent still work).
