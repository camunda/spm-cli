# Targets & Vendors

`targets` in [`ai.json`](/reference/ai-json) lists one or more vendors. A skill
resolves once and projects into each target independently. Eight vendors are
supported today: `amp`, `claude`, `cline`, `codex`, `copilot`, `cursor`,
`gemini`, and `windsurf`.

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

## Cline

spm copies the resolved skills **one directory deep** into Cline's tool-native
skills directory:

```
.cline/skills/<name>/SKILL.md           # workspace (project) skills
~/.cline/skills/<name>/SKILL.md         # user (global) skills
```

Cline auto-discovers skills there in both scopes (a global skill takes precedence
over a workspace skill of the same name). The workspace dir is committed with the
repo, so spm shares it surgically — never wiping it, removing only the entries it
manages, and gitignoring only its own managed subdirs (`.cline/skills/<name>/`).

## Windsurf

spm copies the resolved skills **one directory deep** into Windsurf's tool-native
skills directory, which its **Cascade** agent auto-discovers:

```
.windsurf/skills/<name>/SKILL.md                 # workspace (project) skills
~/.codeium/windsurf/skills/<name>/SKILL.md       # user (global) skills
```

::: warning Asymmetric global dir
Windsurf's **global** skills live under `~/.codeium/windsurf/skills/` — *not*
`~/.windsurf/skills/`. spm's shared-dir adapter tracks the project and global
directories separately, so this is handled without a special case.
:::

The workspace dir is committed with the repo, so spm shares it surgically: it
never wipes the directory, removes only the entries it manages, and gitignores
only its own managed subdirs (`.windsurf/skills/<name>/`).

## Amp

Amp installs skills into the **cross-tool `.agents/skills` alias** by default —
the same standard location Codex reads. spm copies the resolved skills one
directory deep into:

```
.agents/skills/<name>/SKILL.md          # workspace (project) skills
~/.config/agents/skills/<name>/SKILL.md # user (global) skills
```

Same shared-dir handling as the other tools (never wiped, surgical add/remove,
per-skill gitignore). Because Amp's workspace dir is the shared `.agents/skills`
alias, targeting both `amp` and `codex` writes to the same workspace directory.

## Adding a new target

Adding a target means implementing one `Vendor` trait in `src/vendor/`. Tools
that auto-discover skills one directory deep under a shared root (`gemini`,
`codex`, `cursor`, `cline`, `windsurf`, `amp`) are expressed as config on the
generic `src/vendor/shareddir.rs` adapter — each is just a row naming its project
and global directories (which may differ, as with Windsurf and Amp). Every
adapter keeps its materialized files out of VCS via the shared gitignore helper.
See [Design Notes](/guide/design-notes).
