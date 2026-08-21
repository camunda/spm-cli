# Design Notes

The principles behind how spm is built. For the day-to-day model, see
[How It Works](/guide/how-it-works).

## Cross-OS by construction

- **Shells out to the system `git`** — no libgit2 build dependencies.
- **No symlinks** — everything materialized is a real copy, so it works on
  Windows and never risks exfiltration through followed links.
- **All paths via `std::path`** — runs on Linux, macOS, and Windows.

## `SPM_HOME`

`SPM_HOME` overrides the store root (default `~/.spm`, which holds only the fetch
cache). It is primarily used by tests. **Vendor output is always project-local**
and is *not* affected by `SPM_HOME`.

## Vendor adapters

Adding a target means implementing one `Vendor` trait in `src/vendor/`:

- **`claude`** assembles a plugin-marketplace layout
  (`marketplace.json` → `plugin.json` → `skills/<name>/SKILL.md`) into the
  gitignored project-local `.spm/claude/` directory and points to it.
- **`copilot`** copies skills into the gitignored project-local
  `.agents/skills/spm-managed-skills/`.

Both keep their materialized files out of VCS via a shared gitignore helper, so
the "nothing committed" guarantee is enforced in one place rather than
re-implemented per vendor.

## Safe copies

The recursive copy skips `.git` and does not follow symlinks — a skill repo can't
smuggle files out of your tree through a crafted link.

## Single source of truth

- The **crate version in `Cargo.toml`** is the single source of truth for
  releases across crates.io, npm, and the GitHub Release. npm package versions
  are derived from it, never hand-edited.
- `schema/ai.schema.json` is the single source of truth for the shape of
  `ai.json`; spm embeds it and validates on load. See
  [Schema & Validation](/reference/schema).

## Contributing

`make check` runs the full CI gate locally (`fmt-check` + `clippy` + `test`). A
pre-commit hook (fmt + clippy) installs itself automatically via `cargo-husky` —
run `cargo test` (or `cargo build`) once after cloning. Bypass a single commit
with `git commit --no-verify`.
