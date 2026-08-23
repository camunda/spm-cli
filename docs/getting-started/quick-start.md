# Quick Start

This walkthrough takes you from an empty repo to skills materialized for your AI
tool. It assumes `spm` is already on your `PATH` — see
[Installation](/getting-started/installation).

## 1. Scaffold `ai.json`

Pick one or more target vendors (`claude`, `copilot`, `gemini`). Targets are
repeatable and comma-separated:

```bash
spm init --target claude --target copilot
```

This writes a minimal `ai.json` you commit to your repo.

## 2. Add a skill

`spm add` adds a skill to `ai.json`, resolves it to an immutable commit, records
that pin in `ai.lock`, and materializes it — all in one step. Choose exactly one
version selector (`--tag`, `--branch`, or `--commit`):

```bash
spm add https://github.com/org/skills --tag v1.2.0 --path skills/pdf --name pdf-tools
```

To pull in **every** skill under a directory at once (each immediate subdirectory
that has its own `SKILL.md`), use `--all`:

```bash
spm add https://github.com/org/repo --tag v1.0.0 --path skills --all
```

## 3. Commit `ai.json` and `ai.lock`

```bash
git add ai.json ai.lock
git commit -m "chore: add pdf-tools skill"
```

You commit the declaration (`ai.json`) and the pin file (`ai.lock`) — **never**
the materialized skills, which spm keeps gitignored.

## 4. On a fresh clone or new worktree

Teammates (and each new worktree) run:

```bash
spm install
```

This repopulates their own fetch cache and re-materializes the project-local
skills exactly as pinned in `ai.lock` — the same model as `node_modules`. Check
what is materialized in the current checkout with:

```bash
spm status
```

`spm status` exits non-zero when anything is missing, so it works in scripts and
git hooks too. See [Worktrees & Fresh Clones](/guide/worktrees).

## Where to go next

- [How It Works](/guide/how-it-works) — the resolve → fetch → project pipeline.
- [CLI Commands](/reference/cli-commands) — the full command reference.
- [ai.json Manifest](/reference/ai-json) — the file you author.
