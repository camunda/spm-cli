# Why spm?

## The problem

AI coding tools — Claude Code, GitHub Copilot CLI, Gemini CLI — load "skills" from
specific directories in your project. If you want a team to share a set of skills,
you are usually left with bad options:

- **Commit the skills into every repo.** They drift, bloat your history, and
  couple your codebase to third-party skill content.
- **Symlink into a shared checkout.** Fragile across machines, breaks on Windows,
  and risks exfiltration through followed links.
- **Copy-paste by hand.** No versioning, no reproducibility, no single source of
  truth.

## The spm model

`spm` treats AI skills the way a package manager treats code dependencies:

- **You declare** skills as git dependencies in `ai.json` and commit that file.
- **spm resolves** each declaration to an immutable commit SHA and records it in
  `ai.lock`, which you also commit — giving you reproducible installs.
- **spm materializes** the skills into project-local, **gitignored** directories
  wherever each vendor expects them. Nothing spm writes into your working tree is
  committed — no symlinks, no skills under version control.

On a fresh clone, teammates run `spm install` and get exactly the pinned set —
the same mental model as `node_modules`.

## What you get

- **Skills as git dependencies.** Pin by tag, branch, or commit; lock to a SHA.
- **Zero VCS footprint.** Everything materialized is gitignored, per project.
- **Multi-vendor from one declaration.** Resolve once, project into Claude and
  Copilot independently.
- **A shared fetch cache.** Each repo@commit is cloned once into `~/.spm/store`
  and reused across all your projects.
- **Cross-platform.** A single binary that shells out to the system `git`; runs
  on Linux, macOS, and Windows with no symlinks.

Continue to [How It Works](/guide/how-it-works) for the pipeline, or jump to the
[Quick Start](/getting-started/quick-start).
