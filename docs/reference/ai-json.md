# `ai.json` Manifest

`ai.json` is the file **you author and commit**. It declares the target vendors
and the skill dependencies for your project. spm validates it against an embedded
[JSON Schema](/reference/schema) on every load.

## Example

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

## `targets`

A list of one or more vendors: `amp`, `claude`, `cline`, `codex`, `copilot`, `cursor`, `gemini`, `windsurf`. Skills
resolve once and project into each target independently. See
[Targets & Vendors](/guide/targets).

## `skills`

A map of local skill name → skill spec. Each spec has:

| field    | required | meaning                                             |
|----------|----------|-----------------------------------------------------|
| `git`    | yes      | Repo URL — any URL the system `git` understands.     |
| `tag`    | one-of   | git tag (annotated tags deref to commit).            |
| `branch` | one-of   | branch tip at install/update time.                   |
| `commit` | one-of   | exact commit.                                        |
| `path`   | optional | subdirectory to select (for monorepos of skills).    |

Exactly **one** version selector (`tag`, `branch`, or `commit`) is required per
skill. Each selector is locked to a resolved commit SHA in
[`ai.lock`](/reference/ai-lock):

| field    | locked to    |
|----------|--------------|
| `tag`    | resolved SHA |
| `branch` | resolved SHA |
| `commit` | itself       |

## `plugins`

A map of local plugin name → plugin spec. A **plugin** is a Claude Code plugin
that bundles agents, MCP servers, hooks and scripts in addition to (or instead
of) skills. The spec uses the **same fields as `skills`** (`git`, one version
selector, optional `path`), but `path` should point at the **plugin root** — the
directory holding `.claude-plugin/plugin.json`:

```json
{
  "targets": ["claude", "copilot"],
  "skills": {},
  "plugins": {
    "design-system": {
      "git": "https://github.com/camunda/design-system",
      "branch": "main",
      "path": "plugins/camunda-design-system"
    }
  }
}
```

How each target consumes a plugin:

- **Claude** registers the whole plugin under a dedicated, project-local
  `spm-plugins` marketplace (`.spm/claude-plugins/`) — its agents, MCP servers,
  hooks and scripts all load.
- **Every other target** gets only the plugin's **bundled skills**, flattened
  into that target's normal skills location.

`ai.lock` pins the plugin's commit and records its bundled skill set. A bundled
skill whose name collides with a standalone `skills` entry (or another plugin's
skill) is a hard error, never a silent overwrite. Add or remove plugins from the
CLI with [`spm add --plugin` / `spm remove --plugin`](/reference/cli-commands#adding-a-full-plugin-plugin).

## Editor autocompletion

Add a `$schema` reference so your editor validates and autocompletes `ai.json`:

```json
{ "$schema": "./schema/ai.schema.json", "targets": ["claude"], "skills": {} }
```

## Pulling in many skills at once

Instead of naming skills one by one, point `--path` at a container and pass
`--all` to add every immediate subdirectory that has its own `SKILL.md`:

```bash
spm add https://github.com/org/repo --tag v1.0.0 --path skills --all
```

Each sub-skill becomes its own `ai.json` entry, keyed by its directory name.
