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

A list of one or more vendors: `claude`, `copilot`, `gemini`. Skills resolve once
and project into each target independently. See [Targets & Vendors](/guide/targets).

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
