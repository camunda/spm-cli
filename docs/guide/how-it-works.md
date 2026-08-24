# How It Works

## The pipeline

```
ai.json ──resolve──▶ ai.lock ──fetch──▶ ~/.spm/store/<repo>@<sha>   (global cache, one clone per commit)
                                              │
                                              └─project──▶ materialized where the vendor expects it
```

- **`ai.json`** — you author it, commit it. Declares target vendors + skill
  dependencies. See the [ai.json reference](/reference/ai-json).
- **`ai.lock`** — generated, commit it. Pins every version selector to an
  immutable commit SHA → reproducible installs. See the
  [ai.lock reference](/reference/ai-lock).
- **Global store** (`~/.spm/store`) — a **fetch cache only**: each repo@commit is
  cloned once and shared across all projects. Nothing is *registered* or
  *materialized* here — it exists purely so repeated installs don't re-clone.
- **Vendor projection** — spm copies the store's skills into a **project-local**
  directory wherever each vendor loads them from. Nothing spm generates is
  committed to your repo, and nothing is written into a user-global vendor
  location.

## Resolve → fetch → project

1. **Resolve.** A skill spec (`tag` / `branch` / `commit`) is resolved to an
   immutable commit SHA plus a store key. Annotated tags are dereferenced to
   their commit; a branch resolves to its tip at install/update time.
2. **Fetch.** spm clones `<repo>@<sha>` into `~/.spm/store` once. Subsequent
   installs across any project reuse that clone.
3. **Project.** spm copies the resolved skill(s) into the project-local directory
   each target vendor discovers them from, and adds that directory to
   `.gitignore`.

## Registration per vendor

Registration differs by target — see [Targets & Vendors](/guide/targets) for the
full detail:

- **Claude** — spm assembles a self-contained plugin marketplace in the
  project-local, gitignored `.spm/claude/` directory and writes a pointer to it
  into `.claude/settings.local.json`.
- **Copilot CLI** — spm copies the resolved skills into the project-local,
  gitignored `.agents/skills/spm-managed-skills/<name>/`, where Copilot CLI
  auto-discovers them (`.agents/skills/**/SKILL.md`).
- **Gemini CLI** — spm copies the resolved skills one level deep into the
  tool-native `.gemini/skills/<name>/`, a team-shared dir it never wipes;
  spm-managed subdirs are individually gitignored so your own skills stay
  committable.
- **Codex CLI** — spm copies the resolved skills one level deep into the
  cross-tool `.agents/skills/<name>/` alias (the same dir Copilot and Gemini can
  read), also shared and never wiped, with per-skill gitignore.
- **Cursor** — spm copies the resolved skills one level deep into the tool-native
  `.cursor/skills/<name>/`, a version-controlled dir it never wipes; spm-managed
  subdirs are individually gitignored so your own skills stay committable.
- **Cline** — spm copies the resolved skills one level deep into the tool-native
  `.cline/skills/<name>/`, also shared and never wiped, with per-skill gitignore.
- **Windsurf** — spm copies the resolved skills one level deep into the
  tool-native `.windsurf/skills/<name>/` (its Cascade agent auto-discovers them),
  also shared and never wiped, with per-skill gitignore. Its user-global dir is
  `~/.codeium/windsurf/skills/`.
- **Amp** — spm copies the resolved skills one level deep into the cross-tool
  `.agents/skills/<name>/` alias (Amp's documented default), also shared and
  never wiped, with per-skill gitignore.

## Fresh clones

On a fresh clone, teammates run `spm install` — it repopulates their own fetch
cache and re-materializes the project-local skills from `ai.lock`. Same model as
`node_modules`. See [Worktrees & Fresh Clones](/guide/worktrees).
