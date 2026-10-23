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
| `ai.json` | You (or `spm init`, `spm add`, `spm remove`, `spm target add`, `spm target remove`) | Yes |
| `ai.lock` | spm, on every successful sync | Yes |
| Materialized skills (for example `.spm/claude/`, `.agents/skills/spm-managed-skills/`) | spm | No, gitignored |
| Global store (`$SPM_HOME/store`, default `~/.spm/store`) | spm | No, a fetch cache shared by all projects |

- Do not hand-edit `ai.lock`. Edit `ai.json` only for things the CLI cannot do,
  then run `spm install`.
- Do not commit materialized skills. spm adds them to `.gitignore` for you.
- By default commands work on the project in the current directory. `init`,
  `add`, `remove`, `update`, `install`, `list`, `status`, `outdated`, `clean`,
  `target add` and `target remove` accept `-g` or `--global` to switch to the
  user-global scope (see [Global scope](#global-scope)). `scan` works on the
  path it is given and `prune` always empties the global cache; neither accepts
  `-g`.
- Requires the system `git` on `PATH`. spm never handles credentials itself.

## Which command to run

| Situation | Command |
| --- | --- |
| New project with no `ai.json` | `spm init --target <vendor>` |
| Add a skill the user named | `spm add <git-url> --tag <t> --path <dir>` |
| Fresh clone, new git worktree, or skills missing | `spm install` |
| CI, or check `ai.lock` matches `ai.json` without changing it | `spm install --locked` |
| See which pins have newer upstream versions, without changing anything | `spm outdated --json` |
| Follow compatible releases of a skill (e.g. any 1.x from 1.2) | `spm add <git-url> --version "^1.2"` |
| Skill declared with a branch, tag or version range should move to the latest commit | `spm update [name]` |
| Also pull in skills a dependency itself declares | set `"resolveTransitive": true` in `ai.json`, then `spm install` |
| Changed a version selector by editing `ai.json` | `spm install` |
| Check what is declared and pinned | `spm list --json` |
| Check what is materialized in this checkout | `spm status --json` |
| Drop a skill | `spm remove <name>` |
| Also use another AI tool | `spm target add <vendor>` |
| Stop using an AI tool | `spm target remove <vendor>` |
| Review a skill before installing it | `spm scan <path> --json` |
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
spm add <git-url | github.com/<owner>/<repo>[@<ref>]> \
        [--tag <t> | --branch <b> | --commit <sha> | --version "<range>"] \
        [--path <subdir>] [--name <local-name>] [--all] [--plugin] [--force] \
        [--protocol ssh|https] [--protocol-fallback] [-g]
```

Adds the dependency to `ai.json`, resolves it to a commit, writes `ai.lock`,
scans the content, and materializes it, all in one step. It needs an existing
`ai.json`: run `spm init` first.

- The version selector is optional. Give at most one of `--tag`, `--branch`,
  `--commit`, `--version`; combining them is a usage error. With none, spm resolves the
  remote's default branch, prints the branch it chose, and records it as an
  explicit `branch` entry in `ai.json` — the common "just give me the skill"
  case. If the default branch cannot be determined (empty repo, detached HEAD)
  it fails and leaves `ai.json` untouched. Prefer `--tag` for reproducibility;
  `--commit` needs the full 40-character SHA.
- `--version "<range>"` takes a semver requirement (`^1.2`, `~1.4`,
  `>=1.2, <2`) and locks the highest tag satisfying it; `ai.json` records it as
  `"version"`, `ai.lock` as `reference: "version:^1.2"` plus `resolved_tag`.
  Tags may have one leading `v`; non-semver tags are ignored, and pre-releases
  only match a range that names one. `spm install` keeps the pin, `spm update`
  moves it to the newest tag still in the range (never past it). Always wrap the
  range in double quotes — `^` is an escape character in Windows `cmd.exe`. A
  `--tag` is always exact, never a range.
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

- `github.com/<owner>/<repo>[@<ref>]` is a shorthand that expands to
  `https://github.com/<owner>/<repo>.git` before anything else runs, so
  `ai.json`, `ai.lock` and the store only ever see the full URL. An optional
  `@<ref>` replaces the version flag: a full SHA is taken as a commit, otherwise
  it is looked up once and used as a tag or a branch. `@<ref>` cannot be
  combined with `--tag`/`--branch`/`--commit`/`--version`. Any input not starting with
  `github.com/` is passed through untouched.
- `--protocol ssh|https` rewrites a GitHub-style URL to that protocol before
  contacting the remote (`https://host/org/repo` <-> `git@host:org/repo`); a URL
  with no equivalent form (a port, `file://`, a local path) is used as given.
  `--protocol-fallback` retries once over the other protocol after a network or
  auth failure (never for a missing ref) and reports which one worked. Both are
  off by default. The protocol rewrite affects only the URL handed to git, never
  what is persisted: `ai.json` and `ai.lock` keep the URL as recorded — the
  expanded `https://github.com/<owner>/<repo>.git` form for a `github.com/...`
  shorthand, otherwise exactly what you supplied.
  Silent switching can mask a real credential problem, so reach for these only
  when a URL fails for protocol or auth reasons. The same two flags are accepted
  by `install`, `update`, `remove`, `target add` and `outdated`.

```bash
spm add github.com/org/skills --path skills/pdf --name pdf-tools
spm add github.com/org/skills@v1.2.0 --path skills/pdf --name pdf-tools
spm add https://github.com/org/skills --tag v1.2.0 --path skills/pdf --name pdf-tools
spm add https://github.com/org/skills --tag v1.2.0 --path skills --all
spm add https://github.com/org/design-system --branch main \
        --path plugins/camunda-design-system --plugin --name design-system
```

### `spm install`

```bash
spm install [--locked] [--protocol ssh|https] [--protocol-fallback] [-g]
```

Alias: `spm i`. Fetches and materializes everything declared in `ai.json`, reusing the commits
pinned in `ai.lock`. It only re-resolves an entry whose git URL, selector or
path changed, or that has no pin yet. Prints `installed N dependencies`. Run it
after every clone and in every new worktree. `--protocol`/`--protocol-fallback`
behave as under [`spm add`](#spm-add). When `"resolveTransitive": true` is set
in `ai.json`, it also resolves and materializes the skills each dependency
declares in its own nested `ai.json` (see
[Transitive dependencies](#transitive-dependencies)).

With `--locked` (use it in CI) it never re-resolves and never writes `ai.lock`.
It fails, listing every problem in one error, when a direct or transitive
dependency is missing from `ai.lock`, its `git`/selector/`path` changed, an
`ai.lock` entry is no longer declared, or `ai.lock` is missing. The error ends
with ``run `spm install` (without --locked) and commit ai.lock`` (`spm install -g`
for the global scope); do exactly that. It still fetches, so it is not offline.

### `spm update`

```bash
spm update [name] [--protocol ssh|https] [--protocol-fallback] [-g]
```

Re-resolves `--tag` and `--branch` entries to their current commit, and
`--version` ranges to the newest tag still in the range (never past it), and
rewrites `ai.lock`. With a name, updates only that skill or plugin. A `--commit` entry
never moves. An unchanged pin is only ever moved by `spm update`;
`spm install` keeps it, and re-resolves just the entries whose URL, selector or
path you changed, or that have no pin yet. `--protocol`/`--protocol-fallback`
behave as under [`spm add`](#spm-add).

With transitive resolution enabled, `spm update` (no name) refreshes the
**whole** frontier — every direct skill *and* every transitive child's
moving-ref pin. `spm update <name>` is deliberately **surgical**: it advances
the named root and re-reads its nested `ai.json` (resolving a newly declared or
re-pinned child fresh), but a transitive child the root still requests by the
**same** moving ref stays at its locked commit — the single-name update does
**not** chase it to the branch/tag tip. Run a bare `spm update` to chase those
too. See [Transitive dependencies](#transitive-dependencies).

### `spm remove`

```bash
spm remove <name> [--plugin] [--protocol ssh|https] [--protocol-fallback] [-g]
```

Alias: `spm rm`. Drops the entry from `ai.json` and removes its materialized files. Use
`--plugin` when the name is a plugin. A name that does not exist is an error.
Because `remove` re-runs the content scan over the remaining dependencies (see
[Content scan](#content-scan)), it re-materializes them from their cached
checkouts — so it works offline when every remaining pin is already in the
store, and only reaches the network for a dependency whose cached checkout is
missing or stale. For that case it accepts `--protocol`/`--protocol-fallback`,
which behave as under [`spm add`](#spm-add).

### `spm target add`

```bash
spm target add [vendor[,vendor...]] [-g] [--protocol ssh|https] [--protocol-fallback]
```

Adds target tools to `ai.json` and materializes existing skills for them. With
no vendor it asks interactively, reading a numbered choice from stdin, so
always pass vendors explicitly when running non-interactively. Adding a target
that is already configured is a skip, not an error. With `-g` it adds to the
global manifest. It re-materializes every declared skill, so it accepts
`--protocol`/`--protocol-fallback`, which behave as under [`spm add`](#spm-add).

### `spm target remove`

```bash
spm target remove <vendor>[,<vendor>...] [-g]
```

Alias: `spm target rm`. Removes target tools from `ai.json` and deletes what spm
materialized for them; the user's own skills in a shared dir are left alone. An
unknown vendor is an error and changes nothing; a vendor that is not configured
is a skip. A skills dir still used by a remaining target (`codex` and `amp`
share `.agents/skills`) is kept. It refuses to remove the last target: use
`spm clean` instead. With `-g` it works on the global manifest. It does not
fetch or sync, and leaves `.gitignore` unchanged.

### `spm list`

```bash
spm list [-g] [--json]
```

Alias: `spm ls`. Prints each declared skill and plugin with its git URL and its pin as
`<selector> @ <first 8 chars of the commit>` (a range shows the tag it chose:
`version:^1.2 → v1.4.1 @ …`, and `resolvedTag` in `--json`), or `not installed` when
`ai.lock` has no pin for it. Transitively-resolved skills are listed separately
under a `transitive skills:` heading, each annotated with its provenance
(`via <requester>`) — see [Transitive dependencies](#transitive-dependencies).

### `spm status`

```bash
spm status [-g] [--json]
```

Reports, per target, whether each locked skill is present (`ok`) or `MISSING`.
A transitively-resolved skill is reported with its provenance
(`transitive; via <requester>`). Separately, it lists any *stale* directories — materialized but no longer in
`ai.lock`. Stale detection is best-effort: it is only performed for targets
whose skill directory is spm-owned, so it is disabled for the shared-directory
targets (Amp, Codex, Cursor, Cline, Gemini, Windsurf) in both scopes and for
Copilot in global scope, because their directories are shared with your own
skills and undeclared entries there are not spm's to flag. It exits non-zero
when anything is missing or when the Claude marketplace pointer is stale, so it
can gate scripts. It compares only names, not versions: after a hand edit of
`ai.json`, run `spm install` before trusting it. It also warns, conservatively,
when a skill name is locked in both project and global scope. That check
compares only lockfile names, not targets, so it is not proof of a real clash:
for Claude the two scopes use distinct command namespaces (`/spm:<name>` vs
`/spm-global:<name>`) and do not actually collide; the real overlap is on the
name-keyed targets (Copilot and the shared-directory tools), where both scopes
flatten into a single `<name>` directory. Treat it as a prompt to check, not a
guarantee — do not rename a valid Claude-only (or different-target) install on
the strength of this warning alone.

### `spm outdated`

```bash
spm outdated [name] [--exit-code] [--json] [--protocol ssh|https] [--protocol-fallback] [-g]
```

Read-only: reports which `ai.lock` pins have newer upstream versions using only
`git ls-remote`. It never writes `ai.json`/`ai.lock`, fetches into the store or
touches materialized skills, so it is safe to run any time and in CI. Per entry
the status is `behind` (branch tip moved), `newer tag` (a newer semver tag
exists; non-semver and floating tags such as `v1` are ignored), `tag moved`
(the pinned tag now points at a different commit — also printed as a warning;
treat it as suspicious and tell the user before updating), `pinned` (a commit;
never queried), `up to date`, or `error` (remote unreachable or ref gone; the
other rows are still checked). For a `version` range, `behind` means a newer
tag exists **inside** the range (`spm update` moves to it; `latestInRange` in
`--json`), `newer tag` means one exists only **outside** it (widen `version` in
`ai.json`, then `spm install`), and `tag moved` means the locked
`resolved_tag` was re-pointed. Plugins are included; transitive skills are
marked `transitive; via <requester>`, and the fix belongs in that requester's
own `ai.json`, not this one. With a name, only that entry is checked.
A deduplicated entry shared through different selectors has every selector
checked the same way (including newer semver tags); drift in a secondary one
shows as `also <selector>: <status>`.

It exits 0 even when things are outdated; `--exit-code` makes it exit non-zero
when anything is outdated. Any `error` row always makes it exit non-zero.
`--protocol`/`--protocol-fallback` behave as under [`spm add`](#spm-add). To
apply what it reports: `behind`/`tag moved` are refreshed by re-resolving the
same selector — `spm update <name>` for a direct entry, but a bare `spm update`
(no name) for a transitive one, because a named update leaves unchanged child
pins alone. `newer tag` is **not** applied by
`spm update`, because a newer tag is a different selector:

- **Direct entry:** change the tag in this project's `ai.json`, then run
  `spm install`.
- **Transitive entry:** the tag lives in the requester's upstream `ai.json`.
  Publish the bump there, then walk up the requester chain until the change
  reaches something that will pick it up:
  - A requester that **tracks a branch** (direct or transitive) picks it up
    once the bump is on that branch: run a bare `spm update` (`spm install`
    would reuse every locked commit and its old manifest).
  - A requester that **tracks a `version` range** (direct or transitive) picks
    it up once it publishes a release inside that range — no selector edit
    above it: run a bare `spm update`. A release outside the range needs the
    range widened where it is declared.
  - A **transitive** requester pinned to a tag or commit must publish a new
    version, and *its* requester must bump that selector in its own upstream
    `ai.json`; keep walking up.
  - A **locally declared** dependency pinned to a tag or commit: change its
    selector in this `ai.json` to a version that includes the chain of
    bumps, then run `spm install`.

  The summary line spells out this chain for each transitive newer tag.

Keep `-g` on these commands in global scope.

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
spm scan [path] [--json]
```

Runs spm's deterministic content scanner over a file or directory (default
`.`) and prints every finding. It exits non-zero when any finding is high or
critical severity. See [Content scan](#content-scan).

### Machine-readable output (`--json`)

Prefer `--json` on `list`, `status`, `outdated` and `scan` when you need to read their
results; do not parse the human text, whose layout can change. With `--json`,
stdout is exactly one JSON document with `"schemaVersion": 1`, and errors stay
on stderr. Exit codes do not change: `status --json` still fails when
something is missing (check `"complete"` and each target's `missing`),
`outdated --json` fails when a remote errored (check each row's `status` and
`error`; `outdated` is true for `behind`, `newer-tag` and `tag-moved`, including
drift in a deduplicated entry's `alsoSatisfies` selectors), and
`scan --json` still fails on a blocking finding (check `summary.blocking`). The
document is printed before the failure, so read stdout even on a non-zero exit.
Commits are full SHAs. `list` marks transitive skills with `"transitive": true`
and `requestedBy`, and plugins carry `bundledSkills`; `status` reports
cross-scope name clashes under `collisions`.

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

## Transitive dependencies

A skill's own repo can ship an `ai.json` listing the skills *it* needs. When you
**opt in** by setting `"resolveTransitive": true` in your project's own
`ai.json`, spm reads those nested manifests and recursively resolves, fetches,
scans, and materializes the skills they declare alongside yours — the way Cargo
or npm pull in transitive dependencies.

- **Opt-in only.** The flag lives in your committed root `ai.json`, never a CLI
  flag, so it applies uniformly to `install`, `add`, `update` and `sync`. A
  dependency **cannot** re-enable it: `resolveTransitive` in a nested manifest is
  rejected. It is off by default because skills carry executable agent
  instructions, so auto-fetching repos the user never named is a larger
  supply-chain surface.
- **What is read.** Only the nested `ai.json` *co-located with a skill's own
  content* (the repo root, or the skill's `path` subdirectory for a monorepo),
  and only its `skills` map. Nested `targets` and `plugins` are ignored;
  transitive skills always inherit your root `targets`.
- **Scan still gates it.** Every fetched skill (direct or transitive) passes the
  [content scan](#content-scan) **before** its nested manifest is read or its
  own dependencies are fetched, so a blocked skill aborts before recursion.
- **Synthesized names.** A transitive skill isn't named in your manifest, so spm
  synthesizes a stable materialized directory name
  `{requester}__{declared}-{shorthash}` (for example `toolkit__formatter-1a2b3c4d`),
  deterministic so re-running `spm install` never churns it.
- **Dedup, cycles, depth.** The same `(git, path)` pulled in by several skills
  is materialized **once** (its `requested_by` in `ai.lock` records every
  requester); cycles among non-root skills and an excessive depth are hard
  errors.
- **Version conflicts.** If two skills require the **same** normalized git URL +
  `path` at two **different commits**, spm refuses to guess and fails, naming
  both requesters and refs. Fix it by pinning both to the same ref, or dropping
  one. Different subdirectories of one monorepo never conflict.

`spm list` and `spm status` surface provenance (`via <requester>`), and
`spm update` vs `spm update <name>` differ for transitive children (see those
commands above).

## Global scope

`-g` or `--global` makes a command operate on the user's global manifest and
lock (`$SPM_HOME/ai.json`, `$SPM_HOME/ai.lock`, default under `~/.spm/`) and on
user-global tool directories, so the skills are available in every project.
Use it only when the user asks for a skill "everywhere" or "globally".
On a first-time setup run `spm init -g` (safe to repeat) before `spm add -g`,
which fails with `no ai.json found` otherwise. Pass the user's agent to the global
init with `--target <vendor>`: it defaults to `claude` and a repeated `init` is a
no-op, so a non-Claude user who takes the default would materialize the skill for
the wrong agent. To fix that afterwards, run `spm target add -g <vendor>` and then
`spm target remove -g claude`. Global Claude skills are invoked as
`/spm-global:<name>`, project ones as `/spm:<name>`.

## Content scan

Before anything is copied into an agent-visible directory, every command that
runs a sync (`spm add`, `spm install`, `spm update`, `spm remove` and
`spm target add`) scans the content of every dependency in `ai.json` — read
from its cached store checkout, which is fetched only when the pin is new or the
cache is missing or stale — for prompt injection, secret exfiltration,
obfuscated payloads, command execution and auto-run triggers. So a blocking
dependency can also fail `remove` or `target add`, even though neither adds
anything. A high or critical finding aborts the command: nothing is
materialized and `ai.lock` is not written. Lower severities print as warnings.

- Run `spm scan <path>` to review content by hand or to gate a skill in CI.
- Never bypass a block on the user's behalf. Show the user the findings and
  the source. Only if they confirm they trust it, re-run with
  `SPM_ALLOW_SUSPICIOUS=1` in the environment for that one command.

## Source policy

A project can restrict where dependencies may come from with a top-level
`allowedSources` array in `ai.json`, and a machine can do the same with a policy
file (`SPM_POLICY`, else `$SPM_HOME/policy.json`, shaped
`{ "allowedSources": [...] }`). A source must be allowed by **every** configured
layer, so a project can narrow a machine policy but never widen it. With neither
configured, any source is accepted.

- Patterns are `host/path` with no scheme: `github.com/camunda/*` (anything
  below, `*` only as the whole last segment), `github.com/me/one-skill` (exact),
  `file://*` (local sources: `file://` URLs, absolute or `./`/`../` paths — a
  bare name like `origin` could be a git remote, so it is denied). An empty
  list allows nothing.
- HTTPS, `ssh://` and `git@host:org/repo` spellings match the same pattern.
- It applies to `spm add` (before `ai.json` is written), to every sync
  (`install`, including `--locked` and cache-only installs, `update`, `remove`,
  `target add`), to transitive sources, and to `spm outdated`. `--protocol` and
  `--protocol-fallback` cannot bypass it.
- Under a policy git follows no HTTP redirects, and checkout filters (e.g. Git
  LFS) are disabled, so a repository that needs one fails to install.
- A dependency's own `ai.json` may not declare `allowedSources`.
- Never edit the policy (either layer) to get a dependency in on the user's
  behalf. Tell the user which layer denied which source and let them decide.

## Troubleshooting

| Symptom | Cause and fix |
| --- | --- |
| `no ai.json found at ...` | The project is not initialized. The error now also names the fix (`spm init`, or `spm init --global` in global scope). Run it, then retry. |
| `ref ... not found in <url>` | The tag or branch does not exist at that URL. Check the spelling and that it is pushed (`GIT_TERMINAL_PROMPT=0 git ls-remote <url>`). Tags and branches are looked up separately, so use `--tag` for tags and `--branch` for branches. |
| `git ... failed:` with `Permission denied`, `Authentication failed` or `could not read Username` | Auth failure. spm runs git non-interactively (`GIT_TERMINAL_PROMPT=0`), so it fails instead of prompting. For a private repo use the SSH form (`git@host:org/repo.git`) with a key loaded in ssh-agent, or the HTTPS form with a git credential helper configured. Verify with `GIT_TERMINAL_PROMPT=0 git ls-remote <url>` in the same shell (the env var keeps the probe from blocking on a username/password prompt, matching spm), then retry. If only one protocol works on this machine, re-run with `--protocol ssh`/`--protocol https` or `--protocol-fallback` (see [`spm add`](#spm-add)). Do not ask the user for tokens. |
| `a skill named ... already exists` | The name is taken. Pick another with `--name`, pass `--force` to re-pin it, or `spm remove <name>` first. |
| `a plugin named ... already exists` or `... is declared as both a skill and a plugin` | Skills and plugins share one namespace. Use a different `--name`, or remove the other entry. `--force` does not convert a skill into a plugin. |
| `skill name collision: ... is provided by more than one skill/plugin` | A plugin bundles a skill with the same name as another entry. Rename one of them. |
| `... is not allowed: denied by the ... policy ...` | The source is outside the [source policy](#source-policy); the message names each denying layer and what it allows. Use an allowed source, or ask the user whether to extend that layer. A machine policy can't be overridden from `ai.json`. |
| `failed the content scan: ... blocking finding(s)` | See [Content scan](#content-scan). Review the findings before deciding anything. |
| `warning: skill ... has no SKILL.md at its root` | `--path` is wrong, or points at a container of skills. When the warning lists suggested `spm add ... --path <sub>` commands, use one of them or re-run with `--all`. Fix the entry by running `spm remove <name>` and adding it again. |
| `ai.json does not match schema: ...` | `ai.json` is malformed. Each entry needs `git` and exactly one of `tag`, `branch`, `commit`. The message lists each violation with its JSON path. |
| `unknown target ... (supported: ...)` | Use one of the vendors listed under `spm init`. |
| `spm status` shows `MISSING`, or an agent cannot see a declared skill | The checkout was never installed (fresh clone or new worktree). Run `spm install`, then restart the agent session. |
| `ai.json declares ... skill(s) but ai.lock has none` | Same cause. Run `spm install`. |

Failure states to know about:

- Commands that edit `ai.json` and then sync — `spm add` (with or without
  `--all`/`--force`), `spm remove` and `spm target add` — only write `ai.json`
  once resolution, fetching, the content scan and collision checks have all
  passed. So a failure at any of those steps (a bad ref or commit, an
  unreachable source or auth error, a blocked scan, a name collision, a
  malformed shorthand, a duplicate name) leaves `ai.json` and `ai.lock`
  untouched: fix the cause and re-run the *same* command — it will not be
  rejected as `already exists`.
- If the command then fails while *writing the targets* (e.g. an unreadable
  `.gitignore`, a permission error), some targets may already hold the new
  output, so `ai.json` deliberately keeps the change (it is what lets spm clean
  that output later) and the error ends with a `hint:` saying `ai.json` keeps
  this change and to fix the cause and run `spm install` to finish applying it.
  Do exactly that; to abandon the change instead, run `spm install` first and
  then `spm remove <name>` / `spm target remove <target>`. `ai.lock` is only
  written when a sync fully succeeds, and materialization is not atomic across
  targets, so until that `spm install` succeeds some targets may be updated (or
  partially rewritten) while others are not; `spm status` only compares names,
  so it will not flag a half-updated target.
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
