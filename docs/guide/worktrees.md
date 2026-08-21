# Worktrees & Fresh Clones

spm materializes skills into **gitignored** project-local directories
(`.spm/claude/`, `.agents/skills/spm-managed-skills/`). Git **worktrees** have
their own working tree and don't share those untracked files — so, exactly like
`node_modules`, **each checkout needs its own `spm install`**:

```bash
git worktree add ../feature -b feature
cd ../feature && spm install         # materialize this worktree's skills
```

Skipping this is the usual reason an agent doesn't see a declared skill in a new
worktree or a fresh clone.

## Check with `spm status`

`spm status` tells you at a glance what is materialized in the current checkout,
and **exits non-zero** when anything is missing — so it works in scripts too:

```bash
spm status
# [claude]  0/1 installed  .../.spm/claude/plugin/skills
#   reviewer  MISSING
# error: some declared skills are not materialized in this checkout — run `spm install` here
```

For Claude, `spm status` also reports a **stale marketplace pointer** explicitly:

```
  ! .claude/settings.local.json marketplace points at /repo/.spm/claude, not this checkout (/repo-feature/.spm/claude)
```

## Auto-install on checkout

To install automatically on every branch checkout and new worktree, add a
`post-checkout` git hook (worktrees share the repo's `.git/hooks`):

```sh
# .git/hooks/post-checkout   — then: chmod +x .git/hooks/post-checkout
#!/bin/sh
# Re-materialize spm skills so Claude/Copilot always see the declared set.
[ -f ai.lock ] && command -v spm >/dev/null 2>&1 && spm install >/dev/null 2>&1
exit 0
```

## Claude session gotcha

`spm install` writes the **absolute** path of the current checkout's
`.spm/claude/` into that checkout's `.claude/settings.local.json`. Since that
file is gitignored, a new worktree either has no registration at all or — if it
was copied over — one still pointing at the checkout it came from. Either way,
run `spm install` inside the worktree and start (or `/reload-plugins` in) the
Claude session from that same worktree; discovery is snapshotted at session
start.
