# Referencing a Skill

Every skill (and plugin) dependency is a **git repo** + **exactly one version
selector** + an **optional path**. Selectors can differ from one entry to the
next — each resolves independently, then locks to a commit SHA in
[`ai.lock`](/reference/ai-lock). This page shows every way to point at a skill
and how to combine them.

## The four knobs

| Field    | Role                                                              | Locks to     |
|----------|------------------------------------------------------------------|--------------|
| `git`    | repo URL (https / ssh / scp-style / `file://`) — **required**     | —            |
| `tag`    | git tag; annotated tags deref to their commit                    | resolved SHA |
| `branch` | branch tip at install/update time                               | resolved SHA |
| `commit` | exact 40-character SHA                                           | itself       |
| `path`   | subdirectory (monorepo of skills), or **plugin root** for plugins | —            |

Pick **exactly one** of `tag` / `branch` / `commit` per entry. See the
[ai.json reference](/reference/ai-json) for the full schema.

## By tag — immutable release

```json
"pdf-tools": { "git": "https://github.com/org/skills", "tag": "v1.2.0" }
```

The tag is resolved once and pinned in `ai.lock`, so the install is
reproducible even if the tag is later moved.

## By branch — track the tip

```json
"reviewer": { "git": "https://github.com/me/reviewer", "branch": "main" }
```

The branch tip is captured at install/update time and pinned. `spm update`
re-resolves it to the current tip.

## By commit — hard pin

```json
"pinned": { "git": "https://github.com/x/y", "commit": "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2" }
```

Requires the full 40-character SHA. Locks to itself — nothing to re-resolve.

## Selector + folder — pick from a monorepo

`path` selects one skill directory inside a repo, and combines with any
selector. The same repo can appear several times with different selectors and
paths:

```json
"skills": {
  "pdf":      { "git": "https://github.com/org/skills", "tag": "v1.2.0",  "path": "skills/pdf" },
  "excel":    { "git": "https://github.com/org/skills", "branch": "main", "path": "skills/excel" },
  "reviewer": { "git": "https://github.com/org/skills", "commit": "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2", "path": "skills/reviewer" }
}
```

## Folder + `--all` — bulk add

Point `--path` at a container directory and pass `--all` to add every immediate
subdirectory that has its own `SKILL.md`:

```bash
spm add https://github.com/org/repo --tag v1.0.0 --path skills --all
```

Each sub-skill becomes its own `ai.json` entry, keyed by its directory name.

## Plugins — a whole Claude Code plugin

Plugins use the **same fields** as skills, but `path` points at the **plugin
root** — the directory holding `.claude-plugin/plugin.json` — and the entry goes
in the `plugins` map instead of `skills`:

```json
"plugins": {
  "design-system": {
    "git": "https://github.com/camunda/design-system",
    "branch": "main",
    "path": "plugins/camunda-design-system"
  }
}
```

- **Claude** target — the whole plugin loads: agents, MCP servers, hooks,
  scripts and bundled skills, via a project-local `spm-plugins` marketplace.
- **Every other target** — only the plugin's **bundled skills**, flattened into
  that target's normal skills location.

A bundled skill whose name collides with a standalone `skills` entry (or another
plugin's skill) is a hard error, never a silent overwrite. Add and remove
plugins from the CLI with
[`spm add --plugin` / `spm remove --plugin`](/reference/cli-commands).

## All together

Selectors, folders and plugins mix freely in one manifest:

```json
{
  "$schema": "./schema/ai.schema.json",
  "targets": ["claude", "copilot"],
  "skills": {
    "pdf-tools": { "git": "https://github.com/org/skills",  "tag": "v1.2.0", "path": "skills/pdf" },
    "reviewer":  { "git": "https://github.com/me/reviewer", "branch": "main" },
    "pinned":    { "git": "https://github.com/x/y",         "commit": "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2" }
  },
  "plugins": {
    "design-system": {
      "git": "https://github.com/camunda/design-system",
      "branch": "main",
      "path": "plugins/camunda-design-system"
    }
  }
}
```
