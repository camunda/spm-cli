# Targets & Vendors

`targets` in [`ai.json`](/reference/ai-json) lists one or more vendors. A skill
resolves once and projects into each target independently. Two vendors are
supported today: `claude` and `copilot`.

Add or remove targets at any time:

```bash
spm target add            # pick interactively
spm target add copilot    # or name one or more explicitly
```

## Claude Code

spm assembles a **self-contained plugin marketplace** in the project-local,
gitignored `.spm/claude/` directory:

```
.spm/claude/
  marketplace.json  →  plugin.json  →  skills/<name>/SKILL.md
```

It then writes a pointer to that marketplace into `.claude/settings.local.json`
(gitignored by convention). The `.spm/claude/` directory sits **outside**
`.agents/skills/`, so Copilot's scanner never picks it up. Registration is
declarative, per-project, and leaves zero VCS footprint.

::: warning Worktree note
`spm install` writes the **absolute** path of the current checkout's
`.spm/claude/` into that checkout's `.claude/settings.local.json`. Because that
file is gitignored, a new worktree either has no registration or one still
pointing at the checkout it was copied from. Run `spm install` inside the
worktree and start (or `/reload-plugins` in) the Claude session from that same
checkout — discovery is snapshotted at session start. See
[Worktrees & Fresh Clones](/guide/worktrees).
:::

To inspect what Claude actually loaded:

```bash
claude plugin list
claude plugin marketplace list
```

## GitHub Copilot CLI

spm copies the resolved skills into the project-local, gitignored directory:

```
.agents/skills/spm-managed-skills/<name>/SKILL.md
```

Copilot CLI auto-discovers skills matching `.agents/skills/**/SKILL.md`. spm adds
that directory to the project's `.gitignore` (with an explanatory comment) so the
materialized skills stay truly local and are never committed. There is no
user-global state and no `copilot` CLI required.

To inspect what Copilot actually loaded:

```bash
copilot skill list
```

## Adding a new target

Adding a target means implementing one `Vendor` trait in `src/vendor/`. Both
existing adapters keep their materialized files out of VCS via the shared
gitignore helper. See [Design Notes](/guide/design-notes).
