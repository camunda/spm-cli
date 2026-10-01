# Global Skills

`spm`'s scope-aware commands (`init`, `add`, `remove`, `update`, `install`,
`list`, `status`, `clean`) default to **project scope**: they read/write the
`ai.json` and `ai.lock` in the current directory and materialize skills into a
project-local location. Pass `-g` / `--global` to instead manage a
**user-global** set of skills — available to your AI tools in *every* project on
your machine, with no `ai.json` to add to each repo. (`spm target add`,
`spm prune`, and `spm scan` aren't scope-aware: `prune` always wipes the shared
global fetch cache and `scan` always operates on a path.)

```bash
spm init -g --target copilot                        # create the global manifest ($SPM_HOME/ai.json)
spm add  -g https://github.com/org/skills --tag v1.0.0 --name reviewer
spm list -g                                          # list global skills
spm status -g                                        # check they're materialized
spm remove -g reviewer                               # drop a global skill
spm clean  -g                                        # remove global vendor config
```

Reach for global scope for skills you want everywhere regardless of project —
e.g. a personal code-review checklist or a house style guide — instead of
re-declaring the same skill in every repo's `ai.json`.

## Where the global manifest lives

The global **manifest + lock** live under `$SPM_HOME` (default `~/.spm/ai.json`
and `~/.spm/ai.lock`), separate from any project's files. Commit/sync them with
your dotfiles for a reproducible personal setup across machines. They reuse the
same [fetch cache](/guide/how-it-works) as project installs, so a skill already
pulled for a project isn't re-cloned for global use (and vice versa).

## Where global skills materialize

Global skills land in each vendor's own **user-scope** directory instead of a
project-local one:

| Target    | Global directory                        |
|-----------|------------------------------------------|
| Copilot   | `~/.copilot/skills/<name>/`              |
| Gemini    | `~/.gemini/skills/<name>/`               |
| Codex     | `~/.agents/skills/<name>/`               |
| Cursor    | `~/.cursor/skills/<name>/`               |
| Cline     | `~/.cline/skills/<name>/`                |
| Windsurf  | `~/.codeium/windsurf/skills/<name>/`     |
| Amp       | `~/.config/agents/skills/<name>/`        |
| Claude    | `$SPM_HOME/claude-global/` (marketplace) |

For every shared-dir target (Copilot, Gemini, Codex, Cursor, Cline, Windsurf,
Amp) that **global** directory is also where you might keep **hand-authored**
skills, so spm never wipes it at global scope — it only adds/removes the
entries it manages. (Note this differs from Copilot's *project* scope, where
`.agents/skills/spm-managed-skills/` is entirely spm-owned and gets wiped and
rebuilt on every materialize; the other targets' project directories are
likewise shared and non-destructively managed.) See
[Targets & Vendors](/guide/targets) for the full per-vendor detail.

Claude is the one exception: global skills materialize into a self-contained
marketplace under `$SPM_HOME/claude-global/`, registered in
`~/.claude/settings.json` under a distinct marketplace name, `spm-global`
(skills are invoked as `/spm-global:<name>`, vs. `/spm:<name>` for a project
install). The distinct name keeps it from colliding with any project's own `spm`
marketplace.

::: tip Name collisions across scopes
A shared-dir target (Copilot, Gemini, Codex, Cursor, Cline, Windsurf, Amp)
installed with the same skill name in **both** scopes collides at discovery
time — the project and global copies land in the same kind of directory, so one
shadows the other. Claude doesn't have this problem: `/spm:foo` (project) and
`/spm-global:foo` (global) are distinct, namespaced commands. Either way, run
`spm status` (or `spm status -g`) — it warns on a same-name skill across scopes
even when the two installs target different vendors, since it compares the
names recorded in each scope's `ai.lock`.
:::

## Full command reference

`-g` works the same way on every scope-aware command (`init`, `add`, `remove`,
`update`, `install`, `list`, `status`, `clean`); see
[Scope: project (default) vs. global](/reference/cli-commands#scope-project-default-vs-global-g)
for the complete flag-by-flag reference.
