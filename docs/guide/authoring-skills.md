# Authoring Skills

When you write or change a skill, you usually want to try it inside a real
project that consumes it — without committing and pushing every edit, and
without touching the project's `ai.json`/`ai.lock`. `spm link` does exactly
that.

## The local-development loop

The skill (or plugin) must already be declared in the consuming project. Point
it at your working copy:

```bash
cd ~/work/my-app                           # a project that depends on `reviewer`
spm link reviewer ../my-skills/reviewer    # the dir holding SKILL.md
# ...edit ../my-skills/reviewer/SKILL.md (no commit needed)...
spm install                                # re-copy the latest local content
spm unlink reviewer                        # back to the version pinned in ai.lock
```

For a plugin, link its root (the directory with `.claude-plugin/plugin.json`
and `skills/`):

```bash
spm link design-system ../design-system/plugins/cds
```

`spm link` materializes immediately into **every** configured target. There is
no file watching: re-run `spm install` (or any other syncing command) after an
edit. `spm unlink --all` drops every link in the scope.

## What a link changes — and what it doesn't

- **Machine-local.** The override is stored in `.spm/links.json` in the
  project, which spm gitignores, or in `$SPM_HOME/links.json` with `-g`.
- **`ai.json` and `ai.lock` stay byte-identical.** The lock keeps pinning the
  declared git source, so teammates, CI and `spm install --locked` are
  unaffected and your diff stays clean. `link`/`unlink` never write `ai.lock`,
  so if it is missing or out of sync with `ai.json` they ask you to run
  `spm install` first.
- **Keep the working copy outside spm's output.** A directory that overlaps
  what a target writes or deletes — e.g. Codex's `.agents/skills/<name>`, or
  the project root (which contains the output) — is rejected, since syncing
  would wipe it or copy the output into itself. So is a path that reaches it
  through a symlink inside the output; link the clone's real location instead.
- **Same safety rules.** Linked content is copied (never symlinked), `.git` is
  skipped, symlinks inside the directory may not point outside it, and it must
  pass the same content scan as fetched content.
- **Transitive dependencies** (with `"resolveTransitive": true`) are still read
  from the locked commit's nested `ai.json`, so linking a root does not change
  the resolved graph or the lock.
- **Visible.** `spm list` and `spm status` (and their `--json` output) show
  each active link as `linked → <dir>`.

## Paths

In project scope a relative directory is stored exactly as you typed it and is
resolved against the project root (the directory holding `ai.json`, where
project-scoped commands must be run), so the link stays portable if you move
the projects together. With `-g` the path is stored absolute. Forward
and back slashes both work on Windows (`spm link reviewer ..\my-skills\reviewer`).

## When things go stale

- `spm remove` a linked dependency: the leftover link is ignored with a warning
  (and flagged by `spm status`) until you `spm unlink` it.
- The linked directory is moved or deleted: syncs fail with an error naming the
  link; re-link it with `spm link <name> <dir>` or drop it with
  `spm unlink <name>`.
