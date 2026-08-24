# Targets & Vendors

`targets` in [`ai.json`](/reference/ai-json) lists one or more vendors. A skill
resolves once and projects into each target independently. Five vendors are
supported today: `claude`, `codex`, `copilot`, `cursor`, and `gemini`.

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
materialized skills stay truly local and are never committed. spm does not invoke
the `copilot` CLI during install; it only writes files where Copilot looks.

To inspect what Copilot actually loaded:

```bash
copilot skill list
```

## Gemini CLI

spm copies the resolved skills **one directory deep** into Gemini CLI's
tool-native skills directory:

```
.gemini/skills/<name>/SKILL.md
```

Gemini CLI auto-discovers skills there (and in the user-global
`~/.gemini/skills/` for [global installs](/reference/cli-commands#scope-project-default-vs-global-g)). Because Gemini treats
`.gemini/skills/` as a **team-shared, version-controlled** location, spm shares
it with skills you author by hand:

- it never wipes the directory and removes only the entries it previously
  managed;
- it writes skills at the documented one-level depth (no spm-owned subdir);
- it gitignores only its own managed subdirs (`.gemini/skills/<name>/`), so your
  own skills in the same directory stay committable.

To inspect what Gemini actually loaded:

```bash
gemini /skills list
```

## OpenAI Codex CLI

Codex CLI discovers skills from the **cross-tool `.agents/skills` alias** — the
same standard location read by Copilot CLI (and accepted by Gemini). spm copies
the resolved skills one directory deep into:

```
.agents/skills/<name>/SKILL.md          # repo (project) skills
~/.agents/skills/<name>/SKILL.md        # user (global) skills
```

Codex scans `.agents/skills` from the working directory up to the repo root.
Because it treats that dir as a **team-shared, version-controlled** location, spm
shares it with your own hand-authored skills: it never wipes the directory,
removes only the entries it previously managed, and gitignores just its own
managed subdirs (`.agents/skills/<name>/`) so your own skills stay committable.

::: tip Shared `.agents/skills` sink
`.agents/skills` is the emerging cross-tool standard: Copilot CLI, Codex CLI, and
Gemini CLI all read it. Skills spm materializes there for `codex` are therefore
also visible to any other tool that scans the alias. (spm's `copilot` adapter
currently nests its skills under `.agents/skills/spm-managed-skills/`; if you
target both `copilot` and `codex`, Copilot may list a skill from both paths.)
:::

To inspect what Codex actually loaded:

```bash
codex   # then: /skills
```

## Cursor

spm copies the resolved skills **one directory deep** into Cursor's tool-native
skills directory:

```
.cursor/skills/<name>/SKILL.md          # workspace (project) skills
~/.cursor/skills/<name>/SKILL.md        # user (global) skills
```

Cursor auto-discovers skills there in both scopes (it also reads the
`.agents/skills` alias). Because Cursor treats `.cursor/skills/` as a
**version-controlled** location, spm shares it with skills you author by hand:

- it never wipes the directory and removes only the entries it previously
  managed;
- it writes skills at the documented one-level depth (no spm-owned subdir);
- it gitignores only its own managed subdirs (`.cursor/skills/<name>/`), so your
  own skills in the same directory stay committable.

In Cursor, type `/` in Agent chat to see the discovered skills.

## Adding a new target

Adding a target means implementing one `Vendor` trait in `src/vendor/`. Tools
that auto-discover skills one directory deep under a shared root (`gemini`,
`codex`, `cursor`) are expressed as config on the generic `src/vendor/shareddir.rs`
adapter; every adapter keeps its materialized files out of VCS via the shared
gitignore helper. See [Design Notes](/guide/design-notes).
