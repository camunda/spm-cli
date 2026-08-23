# FAQ

## What exactly is a "skill"?

A skill is a directory containing a `SKILL.md` (and any supporting files) that an
AI tool loads to gain extra capabilities or instructions. spm fetches skills from
git repositories and materializes them where each vendor discovers them.

## Which AI tools does spm support?

Four targets today: **Claude Code** (`claude`), **OpenAI Codex CLI** (`codex`),
**GitHub Copilot CLI** (`copilot`), and **Gemini CLI** (`gemini`). One `ai.json`
declaration resolves once and projects into each. See
[Targets & Vendors](/guide/targets).

## Why aren't the skills committed to my repo?

By design. Everything spm materializes into the working tree is **gitignored** —
no symlinks, no third-party skill content under version control. You commit only
`ai.json` (your declaration) and `ai.lock` (the pins). Teammates run `spm
install` to reproduce the skills locally, exactly like `node_modules`.

## Do I commit `ai.lock`?

Yes. `ai.json` and `ai.lock` are both committed. `ai.lock` pins every selector to
a commit SHA for [reproducible installs](/reference/ai-lock). The materialized
skills are never committed.

## My AI tool doesn't see a declared skill. Why?

Almost always because the current checkout hasn't been materialized. Git
worktrees and fresh clones don't share gitignored files, so **each checkout needs
its own `spm install`**. Run `spm status` — it reports what's missing and exits
non-zero. See [Worktrees & Fresh Clones](/guide/worktrees).

## Does spm need the `git` binary?

Yes. spm shells out to the system `git` (no libgit2), so `git` must be on your
`PATH` at runtime. This keeps the binary small and dependency-free across
platforms.

## How does authentication work for private repos?

- **SSH** URLs go through your ssh-agent / keys — spm never handles credentials.
- **Private HTTPS** repos use your git credential helper.

spm runs git with `GIT_TERMINAL_PROMPT=0`, so a missing credential fails with a
clear error instead of hanging on a prompt (helpers and ssh-agent still work).

## Where is the global cache and how do I clear it?

The fetch cache lives at `~/.spm/store` (override the root with `SPM_HOME`). Each
`<repo>@<sha>` is cloned once and shared across all your projects. Wipe it with:

```bash
spm prune --yes
```

## How do I update a skill to the latest commit?

```bash
spm update          # all skills
spm update <name>   # one skill
```

`branch`/`tag` selectors only move when you run `spm update`; otherwise `spm
install` reproduces the exact pins in `ai.lock`.

## Which platforms are supported?

Linux, macOS, and Windows. Prebuilt binaries cover `darwin-x64`, `darwin-arm64`,
`linux-x64`, `linux-arm64`, and `win32-x64`. See
[Installation](/getting-started/installation).
