# CLI Commands

The full `spm` command surface. Run `spm --help` or `spm <command> --help` for
the authoritative, version-specific usage.

```bash
spm init [--target amp|claude|cline|codex|copilot|cursor|gemini|windsurf ...] [-g]  # scaffold ai.json (repeatable / comma-separated)
spm add <git> [--tag|--branch|--commit <v>] \      # add + install a skill (no selector = default branch; github.com/owner/repo@ref also works)
        [--path <subdir>] [--name <local-name>] [--all] [--force] [-g]  # --all: add every skill under --path
        [--plugin]                                 # --plugin: add a full plugin instead of a skill
spm target add [vendor ...]                        # add target vendor(s); no arg = pick interactively
spm remove <name> [--plugin] [-g]                  # drop a skill (or a plugin with --plugin); alias: rm
spm update [name] [-g]                             # re-resolve branches/tags to latest
spm install [-g]                                   # rebuild from ai.lock (after clone); alias: i
spm list [-g]                                      # show skills + pinned commits; alias: ls
spm status [-g]                                    # check skills are materialized in this checkout
spm clean [-g]                                     # remove generated vendor config
spm prune [--yes]                                  # wipe the global fetch cache ($SPM_HOME/store, default ~/.spm/store)
spm scan [path]                                    # scan skill content for suspicious patterns (default: .)
```

Three commands have short aliases that behave identically, including `-g`:
`spm i` for `spm install`, `spm rm <name>` for `spm remove <name>`, and `spm ls`
for `spm list`.

`add`, `install`, `update`, `remove` and `target add` also accept `--protocol ssh|https` and
`--protocol-fallback` (see [Repo URLs](#repo-urls-https-ssh)).

## Scope: project (default) vs. global (`-g`)

Scope-aware commands (`init`, `add`, `remove`, `update`, `install`, `list`,
`status`, `clean`) accept `-g` / `--global`; `spm target add`, `spm prune`, and
`spm scan` don't (`prune` always wipes the shared global fetch cache and `scan`
always operates on a path). Without `-g`, a scope-aware command operates on the
**project** in the current directory. With it, it manages a **user-global** set
of skills available to your AI tools in *every* project, with its own
manifest/lock under `$SPM_HOME` and its own per-vendor materialization
locations. See [Global Skills](/guide/global-skills) for the full concept, where
each vendor materializes global skills, and how cross-scope name collisions are
handled.

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
`ai.lock`, and materializes it — in one step. Provide at most one version
selector:

| flag       | meaning                                  |
|------------|------------------------------------------|
| `--tag`    | git tag (annotated tags deref to commit) |
| `--branch` | branch tip at install/update time        |
| `--commit` | exact commit                             |

The repository can also be given as `github.com/<owner>/<repo>[@<ref>]`, which
expands to `https://github.com/<owner>/<repo>.git`. `@<ref>` stands in for the
version flag: spm looks it up on the remote and treats it as a tag or a branch
(a full 40-character SHA is taken as a commit). It is an error if the ref is
neither, or both a tag and a branch (use `--tag`/`--branch` instead), or if it is
combined with `--tag`, `--branch` or `--commit`. `ai.json` records the expanded
URL, never the shorthand; any other input is passed through untouched.

```bash
spm add github.com/org/repo@v1.0.0   # = spm add https://github.com/org/repo.git --tag v1.0.0
```

With **no** selector, `spm add` resolves the remote's default branch (what its
`HEAD` points at), prints the branch it chose, and records it in `ai.json` as an
explicit `branch` entry, so `spm update` keeps working as for any branch
dependency. `ai.lock` still pins the commit the branch pointed at. If the
default branch can't be determined (an empty repository or a detached `HEAD`),
`spm add` fails and asks you to pass a selector. Only `spm add` fills in a
default: a hand-edited `ai.json` entry must still set exactly one selector.

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
`ai.lock`. With no name, updates **all** skills — including the whole transitive
frontier.

`spm update <name>` is deliberately **surgical**: it advances the named root's
own ref to latest and re-reads its nested manifest (resolving any newly-declared
child, and re-resolving a child whose pin the new manifest changed), but a
transitive child that root still requests by the **same** moving ref stays
pinned. Refresh the whole graph — every transitive child — with a bare
`spm update`. See [Transitive Skill Dependencies](/guide/transitive-dependencies).

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

### `spm scan [path]`

Runs spm's deterministic **content scanner** over a path — a single file or a
directory (default: the current directory) — and prints every finding. **Exits
non-zero** when any blocking (high/critical) finding is present, so it works as a
CI gate on skill sources.

The same scan runs automatically as a **pre-materialize gate**: every skill and
plugin is scanned from its store checkout — fetched only when its pin is new or
the cached checkout is missing or stale, so a cached dependency is scanned with
no network access — and *before* it is copied into any agent-discovered
directory, by every command that syncs skills:
`spm add`, `spm install`, `spm update`, `spm remove`, and `spm target add`
(each scans every dependency in `ai.json`). High/critical findings abort the command (nothing is materialized
and `ai.lock` is not written); medium/low findings are surfaced as warnings.

Detected pattern categories:

| category | example signals | severity |
|----------|-----------------|----------|
| prompt injection | "ignore previous instructions", "disregard your system prompt" | high |
| secret exfiltration | `~/.ssh/id_rsa`, `.aws/credentials`, `GITHUB_TOKEN` (escalated when paired with `curl`/`post`/`send`) | low → critical |
| obfuscation | zero-width/bidi Unicode; base64/hex blobs decoding to shell; files exceeding the 8 MiB scan cap | high / critical |
| command execution | `curl \| bash`, `/dev/tcp/…`, `nc -e` reverse shells | critical |
| path traversal | `../../` requested in skill text | medium |
| auto-run | `postinstall` scripts, git hooks, bundled `Makefile` | low / medium |

```bash
spm scan               # scan the current directory
spm scan ./my-skill    # scan a specific path
```

To override the gate for content you trust (or a false positive), set
`SPM_ALLOW_SUSPICIOUS=1`: findings are printed as warnings but never block.

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

### Protocol override and opt-in fallback

By default spm uses the URL exactly as you gave it: an SSH URL is contacted over
SSH and an HTTPS URL over HTTPS, and a failure is reported as-is. Two opt-in
flags (accepted by the commands that can contact a remote: `add`, `install`,
`update`, `remove` and `target add`) help when your preferred
protocol isn't set up on the current machine, e.g. no SSH key in CI, or HTTPS
blocked but SSH allowed:

```bash
spm add git@github.com:org/repo.git --branch main --protocol https   # force HTTPS
spm install --protocol-fallback                                      # try as given, then the other one
```

- `--protocol ssh|https` rewrites the URL to that protocol before contacting the
  remote (`https://host/org/repo` <-> `git@host:org/repo`). URLs with no
  equivalent form (a port, `file://`, a local path, or a `#fragment`, `?query`,
  percent-encoding, backslash or `~` path whose meaning differs between the two
  forms) are used as given.
- `--protocol-fallback` retries **once** over the other protocol, and only after
  a connection or authentication failure — never after a missing ref or path.
  When it does, spm prints which protocol worked; if both fail, the error lists
  both attempts.

Neither flag changes what is recorded: `ai.json` and `ai.lock` keep the URL as
recorded — the expanded `https://github.com/<owner>/<repo>.git` form for a
`github.com/...` shorthand, otherwise exactly what you supplied; the protocol
rewrite affects only the URL handed to git. **Use the fallback deliberately:**
switching protocols can hide a real
credential problem (an expired SSH key quietly replaced by an anonymous HTTPS
clone that only works for public repos, or the reverse), which is why it is off
by default.
