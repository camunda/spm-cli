---
name: spm
description: Operate the spm skill package manager. Use when the user wants to add, remove, update, install, list or check AI skills or plugins declared in ai.json, set up a fresh clone or git worktree, add a target tool such as Claude Code or Copilot, or fix an spm error such as a missing ref, a name clash or a blocked content scan.
---

# spm: skill package manager

`spm` manages AI skills the way a package manager manages dependencies. Skills
are declared as git dependencies in `ai.json`, pinned to commits in `ai.lock`,
and copied into the directories the user's AI tools read from. You commit
`ai.json` and `ai.lock`; the skills spm copies into the working tree are
gitignored and never committed. Run `spm --help` or
`spm <command> --help` for the authoritative, version-specific usage.

## Mental model

| File or directory | Who writes it | Commit it? |
| --- | --- | --- |
| `ai.json` | You (or `spm init`, `spm add`, `spm remove`, `spm target add`) | Yes |
| `ai.lock` | spm, on every successful sync | Yes |
| Materialized skills (for example `.spm/claude/`, `.agents/skills/spm-managed-skills/`) | spm | No, gitignored |
| Global store (`$SPM_HOME/store`, default `~/.spm/store`) | spm | No, a fetch cache shared by all projects |

- Do not hand-edit `ai.lock`. Edit `ai.json` only for things the CLI cannot do,
  then run `spm install`.
- Do not commit materialized skills. spm adds them to `.gitignore` for you.
- By default commands work on the project in the current directory. `init`,
  `add`, `remove`, `update`, `install`, `list`, `status` and `clean` accept `-g`
  or `--global` to switch to the user-global scope (see
  [Global scope](#global-scope)). `target add` is always project-only, `scan`
  works on the path it is given, and `prune` always empties the global cache;
  none of those three accepts `-g`.
- Requires the system `git` on `PATH`. spm never handles credentials itself.

## Which command to run

| Situation | Command |
| --- | --- |
| New project with no `ai.json` | `spm init --target <vendor>` |
| Add a skill the user named | `spm add <git-url> --tag <t> --path <dir>` |
| Fresh clone, new git worktree, or skills missing | `spm install` |
| Skill declared with a branch or tag should move to the latest commit | `spm update [name]` |
| Changed a version selector by editing `ai.json` | `spm install` |
| Check what is declared and pinned | `spm list` |
| Check what is materialized in this checkout | `spm status` |
| Drop a skill | `spm remove <name>` |
| Also use another AI tool | `spm target add <vendor>` |
| Review a skill before installing it | `spm scan <path>` |
| Undo all generated tool config for this project | `spm clean` |
| Free disk space in the global cache | `spm prune` |

## Commands

### `spm init`

```bash
spm init [--target <vendor>[,<vendor>...]] [-g]
```

Creates `ai.json` with an empty `skills` map. `--target` is repeatable or
comma-separated and defaults to `claude`. Valid vendors: `amp`, `claude`,
`cline`, `codex`, `copilot`, `cursor`, `gemini`, `windsurf`. Running it again
when `ai.json` exists leaves the file untouched and exits 0.

### `spm add`

```bash
spm add <git-url> (--tag <t> | --branch <b> | --commit <sha>) \
        [--path <subdir>] [--name <local-name>] [--all] [--plugin] [--force] [-g]
```

Adds the dependency to `ai.json`, resolves it to a commit, writes `ai.lock`,
scans the content, and materializes it, all in one step. It needs an existing
`ai.json`: run `spm init` first.

- Give exactly one of `--tag`, `--branch`, `--commit`. Combining them is a
  usage error. Giving none fails with `set one of tag/branch/commit`. Prefer
  `--tag` for reproducibility; `--commit` needs the full 40-character SHA.
- `--path <subdir>` selects a subdirectory of the repo (monorepos). It must be
  relative to the repo root and must not contain a `..` path component (a
  segment such as `v1..2` is fine).
- `--name` sets the local name (the `ai.json` key). It defaults to the last
  segment of `--path`, or the repo name when there is no usable path. A name
  must be non-empty, must not contain `/`, `\` or NUL, and must not be exactly
  `.` or `..` (so `foo.bar` is fine).
- `--all` treats `--path` as a container and adds every immediate
  subdirectory that has a `SKILL.md`, each keyed by its directory name. It
  cannot be combined with `--name` or `--plugin`.
- `--plugin` adds a full Claude Code plugin (agents, MCP servers, hooks,
  scripts and bundled skills). Point `--path` at the directory that holds
  `.claude-plugin/plugin.json`. Claude gets the whole plugin; every other
  target gets only the plugin's bundled skills.
- `--force` overwrites an existing entry of the same name and kind, for example
  to re-pin a version.

The URL can be any form `git` understands: `https://...`, `git@host:org/repo.git`,
`ssh://...`, `file://...`. Any git host works.

```bash
spm add https://github.com/org/skills --tag v1.2.0 --path skills/pdf --name pdf-tools
spm add https://github.com/org/skills --tag v1.2.0 --path skills --all
spm add https://github.com/org/design-system --branch main \
        --path plugins/camunda-design-system --plugin --name design-system
```

### `spm install`

```bash
spm install [-g]
```

Fetches and materializes everything declared in `ai.json`, reusing the commits
pinned in `ai.lock`. It only re-resolves an entry whose git URL, selector or
path changed, or that has no pin yet. Prints `installed N dependencies`. Run it
after every clone and in every new worktree.

### `spm update`

```bash
spm update [name] [-g]
```

Re-resolves `--tag` and `--branch` entries to their current commit and rewrites
`ai.lock`. With a name, updates only that skill or plugin. A `--commit` entry
never moves. An unchanged pin is only ever moved by `spm update`;
`spm install` keeps it, and re-resolves just the entries whose URL, selector or
path you changed, or that have no pin yet.

### `spm remove`

```bash
spm remove <name> [--plugin] [-g]
```

Drops the entry from `ai.json` and removes its materialized files. Use
`--plugin` when the name is a plugin. A name that does not exist is an error.

### `spm target add`

```bash
spm target add [vendor[,vendor...]]
```

Adds target tools to `ai.json` and materializes existing skills for them. With
no vendor it asks interactively, reading a numbered choice from stdin, so
always pass vendors explicitly when running non-interactively. Adding a target
that is already configured is a skip, not an error. This command has no `-g`
flag and always works on the current project.

### `spm list`

```bash
spm list [-g]
```

Prints each declared skill and plugin with its git URL and its pin as
`<selector> @ <first 8 chars of the commit>`, or `not installed` when
`ai.lock` has no pin for it.

### `spm status`

```bash
spm status [-g]
```

Reports, per target, whether each locked skill is present (`ok`) or `MISSING`.
Separately, it lists any *stale* directories — materialized but no longer in
`ai.lock`. Stale detection is best-effort: it is only performed for targets
whose skill directory is spm-owned, so it is disabled for the shared-directory
targets (Amp, Codex, Cursor, Cline, Gemini, Windsurf) in both scopes and for
Copilot in global scope, because their directories are shared with your own
skills and undeclared entries there are not spm's to flag. It exits non-zero
when anything is missing or when the Claude marketplace pointer is stale, so it
can gate scripts. It compares only names, not versions: after a hand edit of
`ai.json`, run `spm install` before trusting it. It also warns when a skill
name is installed in both project and global scope, because the two collide at
discovery time.

### `spm clean`

```bash
spm clean [-g]
```

Removes the generated vendor config for this scope. `ai.json` and `ai.lock`
stay, so `spm install` restores everything.

### `spm prune`

```bash
spm prune [--yes]
```

Deletes the whole global fetch cache. It is global, not per project, and asks
for confirmation on stdin unless `--yes` is given. Anything removed is
re-fetched by the next `spm install`. It has no `-g` flag.

### `spm scan`

```bash
spm scan [path]
```

Runs spm's deterministic content scanner over a file or directory (default
`.`) and prints every finding. It exits non-zero when any finding is high or
critical severity. See [Content scan](#content-scan).

## Targets

Each target gets the same resolved skills, projected where that tool looks for
them. The default project-scope locations:

| Target | Where spm materializes project skills |
| --- | --- |
| `claude` | Plugin marketplace in `.spm/claude/`, registered in `.claude/settings.local.json` (plugins go to `.spm/claude-plugins/`) |
| `copilot` | `.agents/skills/spm-managed-skills/<name>/` |
| `codex`, `amp` | `.agents/skills/<name>/` |
| `cursor` | `.cursor/skills/<name>/` |
| `cline` | `.cline/skills/<name>/` |
| `gemini` | `.gemini/skills/<name>/` |
| `windsurf` | `.windsurf/skills/<name>/` |

For the shared directories spm touches only the entries it manages and
gitignores just those, so the user's own hand-written skills there are safe.

After `spm install` for Claude, the Claude session has to be restarted, or
`/reload-plugins` run inside it, before the new skills are visible.

## Global scope

`-g` or `--global` makes a command operate on the user's global manifest and
lock (`$SPM_HOME/ai.json`, `$SPM_HOME/ai.lock`, default under `~/.spm/`) and on
user-global tool directories, so the skills are available in every project.
Use it only when the user asks for a skill "everywhere" or "globally".
On a first-time setup run `spm init -g` (safe to repeat) before `spm add -g`,
which fails with `no ai.json found` otherwise. Pass the user's agent to the global
init with `--target <vendor>` (it defaults to `claude`): no CLI command retargets a
global manifest afterwards, because `target add` operates only on a project manifest
and a repeated `init` is a no-op, so a non-Claude user who takes the default would
materialize the skill for the wrong agent. It is still recoverable by hand — edit the
`targets` array in `$SPM_HOME/ai.json` and run `spm install -g` — but passing
`--target` up front avoids it. Global Claude skills are invoked as
`/spm-global:<name>`, project ones as `/spm:<name>`.

## Content scan

Before anything is copied into an agent-visible directory, every command that
runs a sync (`spm add`, `spm install`, `spm update`, `spm remove` and
`spm target add`) scans the fetched content of every dependency in `ai.json`
for prompt injection, secret exfiltration, obfuscated payloads, command
execution and auto-run triggers. So a blocking dependency can also fail
`remove` or `target add`, even though neither adds anything. A high or critical
finding aborts the command: nothing is materialized and `ai.lock` is not
written. Lower severities print as warnings.

- Run `spm scan <path>` to review content by hand or to gate a skill in CI.
- Never bypass a block on the user's behalf. Show the user the findings and
  the source. Only if they confirm they trust it, re-run with
  `SPM_ALLOW_SUSPICIOUS=1` in the environment for that one command.

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `no ai.json found at ...` | The project is not initialized. Run `spm init --target <vendor>`, then retry. |
| `ref ... not found in <url>` | The tag or branch does not exist at that URL. Check the spelling and that it is pushed (`git ls-remote <url>`). Tags and branches are looked up separately, so use `--tag` for tags and `--branch` for branches. |
| `git ... failed:` with `Permission denied`, `Authentication failed` or `could not read Username` | Auth failure. spm runs git non-interactively (`GIT_TERMINAL_PROMPT=0`), so it fails instead of prompting. For a private repo use the SSH form (`git@host:org/repo.git`) with a key loaded in ssh-agent, or the HTTPS form with a git credential helper configured. Verify with `git ls-remote <url>` in the same shell, then retry. Do not ask the user for tokens. |
| `a skill named ... already exists` | The name is taken. Pick another with `--name`, pass `--force` to re-pin it, or `spm remove <name>` first. |
| `a plugin named ... already exists` or `... is declared as both a skill and a plugin` | Skills and plugins share one namespace. Use a different `--name`, or remove the other entry. `--force` does not convert a skill into a plugin. |
| `skill name collision: ... is provided by more than one skill/plugin` | A plugin bundles a skill with the same name as another entry. Rename one of them. |
| `failed the content scan: ... blocking finding(s)` | See [Content scan](#content-scan). Review the findings before deciding anything. |
| `warning: skill ... has no SKILL.md at its root` | `--path` is wrong, or points at a container of skills. When the warning lists suggested `spm add ... --path <sub>` commands, use one of them or re-run with `--all`. Fix the entry by running `spm remove <name>` and adding it again. |
| `ai.json does not match schema: ...` | `ai.json` is malformed. Each entry needs `git` and exactly one of `tag`, `branch`, `commit`. The message lists each violation with its JSON path. |
| `unknown target ... (supported: ...)` | Use one of the vendors listed under `spm init`. |
| `spm status` shows `MISSING`, or an agent cannot see a declared skill | The checkout was never installed (fresh clone or new worktree). Run `spm install`, then restart the agent session. |
| `ai.json declares ... skill(s) but ai.lock has none` | Same cause. Run `spm install`. |

Failure states to know about:

- `spm add` writes the new entry into `ai.json` before it resolves and fetches.
  If the add then fails (bad ref, auth error, blocked scan), the entry stays in
  `ai.json` with no pin (for a new name). Either fix the cause and run `spm install`, or drop it
  with `spm remove <name>`. This ordering is specific to a single `spm add`:
  `spm add --all` resolves and fetches the container (and checks each sub-skill
  against the existing *skills*) *before* it writes `ai.json`, so a bad ref, auth
  failure, invalid container or a collision with an existing skill leaves no new
  entries. Other collisions are only detected *after* the write, when `sync`
  reloads the manifest: a sub-skill whose name matches an existing *plugin* (or a
  skill bundled by one of your plugins) is saved into `ai.json` first and only
  then rejected, and — like a scan block — leaves the batch entries behind.
  Inspect the manifest before running `spm remove` after an `--all` failure
  rather than assuming nothing was added.
- `ai.lock` is only written when a sync fully succeeds, so a failure *before*
  materialization (resolution, fetch, scan, collision check) leaves `ai.lock` and
  every materialized skill on the old pin. Materialization itself is not atomic,
  though: `sync` rewrites each configured target in turn before saving `ai.lock`,
  so a copy/configuration failure partway through can leave some targets updated
  (or partially rewritten) while `ai.lock` still holds the old pin. If `spm add
  --force` replaces an existing entry and then fails, `ai.json` holds the new
  (failing) selector; `spm list` and `spm status` keep reporting the old pin as
  installed — and `status` only compares names, so it will not flag a
  half-updated target — until an `spm install` succeeds. Re-run `spm install`
  after fixing the cause to restore every target to a consistent state.
- To change the version selector of an existing entry from the CLI, re-run
  `spm add` for it with `--force` and the new selector (and the same
  `--name` and `--path`).

## Working rules for agents

- Prefer the CLI over editing `ai.json` by hand: the CLI validates, pins and
  materializes in one step.
- After editing `ai.json` by hand, run `spm install` first, then `spm status`.
  `status` only compares the names in `ai.lock` with what is on disk; it does
  not compare an entry's URL, selector or path with its pin, so before
  `install` it can report success for the old materialization. Commit
  `ai.json` and `ai.lock` together.
- Never stage or commit what spm materialized (see [Targets](#targets)). If
  `git status` lists spm-managed skill directories as untracked, their
  `.gitignore` entries were removed; run `spm install` to restore them.
- Do not run `spm prune` or `spm clean` unless the user asks. `prune` affects
  every project on the machine.
- When a command fails, read the error text in full before retrying. Most spm
  errors state the fix.
