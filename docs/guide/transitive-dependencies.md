# Transitive Skill Dependencies

A skill can itself declare an `ai.json` at its own content root, listing the
skills *it* needs. When you **opt in**, spm reads those nested manifests and
recursively resolves, fetches, scans, and materializes the skills they declare —
alongside your own — the way Cargo or npm pull in transitive dependencies.

This is **off by default**. Read [Why it's opt-in](#why-it-s-opt-in) before you
enable it.

## Opting in

Set `resolveTransitive: true` in your project's own `ai.json`:

```json
{
  "targets": ["claude", "copilot"],
  "resolveTransitive": true,
  "skills": {
    "toolkit": { "git": "https://github.com/org/toolkit", "tag": "v2.0.0" }
  }
}
```

Now, when `toolkit`'s repo ships an `ai.json` like:

```json
{ "skills": { "formatter": { "git": "https://github.com/org/formatter", "branch": "main" } } }
```

spm resolves and materializes **both** `toolkit` and `formatter` for every
configured target — no duplication, even when several of your skills pull in the
same dependency.

The flag lives in the committed manifest (not a per-command CLI flag) so the
decision applies uniformly to `spm install`, `add`, `update`, and `sync`, and is
reviewable by your team — exactly like `targets`.

## What a dependency's `ai.json` may declare

A dependency's own `ai.json` is parsed with a deliberately **lenient** shape:

- Only its `skills` map is read.
- `targets` is **not required** (transitive skills always inherit *your* root
  targets — see below) and is ignored if present.
- `plugins` are **ignored** in v1 — a full plugin (agents, hooks, MCP servers,
  scripts) has a much larger blast radius, so it is out of scope for automatic
  transitive fetching.
- `resolveTransitive` is **rejected** in a nested manifest: recursion is governed
  solely by *your* root project's flag. A dependency cannot re-enable resolution
  you opted out of.

Only the `ai.json` **co-located with a skill's own content** is read (the repo
root for a whole-repo skill, or the skill's `path` subdirectory for a monorepo
skill) — never a monorepo's unrelated repo-root manifest.

## Targets are inherited

A transitively-resolved skill always inherits your root project's `targets`. It
cannot declare its own subset — every configured vendor gets the full,
flattened skill set.

## Naming: how a transitive skill's directory is chosen

Because a transitive skill isn't named in your manifest, spm synthesizes a
stable, collision-resistant materialized name:

```
{requester}__{declared}-{shorthash}
```

- `requester` — the local name of the direct dependency that pulled it in.
- `declared` — the skill's key in the dependency's `ai.json`.
- `shorthash` — a short hash of the dependency's normalized `(git, path)`.

For example, a skill `formatter` pulled in by your `toolkit` skill materializes
as a directory like `toolkit__formatter-1a2b3c4d/`. The name is deterministic,
so re-running `spm install` never churns it.

## Deduplication, cycles, and the depth cap

- **Diamonds** (two of your skills pull in the same dependency) resolve that
  shared skill **exactly once**. Its `requested_by` in `ai.lock` lists every
  requester.
- **Cycles** (A depends on B depends on A) are detected and reported with the
  offending chain, rather than looping forever.
- A hard **depth cap** (8 levels) is a backstop against pathological or hostile
  graphs, independent of the flag.

## Updating: `update` vs `update <name>`

Moving refs (`branch`/`tag`) resolve to a commit once and stay **pinned** in
`ai.lock` until you deliberately refresh them — the same for a transitive child
as for a directly-declared skill.

- **`spm update`** (no name) refreshes **everything**: every direct skill *and*
  the whole transitive frontier re-resolves its `branch`/`tag` to the latest
  commit.
- **`spm update <name>`** is deliberately **surgical**. It advances the named
  root's own ref to latest and re-reads that root's nested `ai.json` — so a
  child the updated manifest **newly declares** is resolved fresh, and a child
  whose **pin the manifest changed** (a different ref) is re-resolved. But a
  transitive child the root still requests by the **same** moving ref stays
  pinned at its locked commit; the single-name update does **not** chase it to
  the branch/tag tip.

This non-cascade is intentional. Transitive children are **deduplicated across
roots** — a diamond shares one lock entry — so advancing one named root's
subtree could silently move an *unrelated* root's dependency. If you want a
root's moving-ref children chased to their latest commits, run a bare
`spm update`.

## Version conflicts

If two skills — directly or transitively — require the **same repo** at two
**different commits**, spm refuses to guess and **fails with an error** naming
both requesters and refs:

```
version conflict: `https://github.com/org/shared` is required at two different commits:
  branch:main @ 1a2b3c4d (via left)
  branch:other @ 9f8e7d6c (via right)
resolve it by pinning both requesters to the same ref, or removing one dependency
```

"Same repo" means the **same normalized git URL and `path`** — so two skills
that legitimately point at *different* subdirectories of one monorepo never
conflict. The normalized form treats `https://host/o/r`, `https://host/o/r.git`,
and a differently-cased host as one identity, so a conflict can't be evaded by a
URL spelling difference.

To fix a conflict, pin both requesters to the same ref, or drop one dependency.

## Seeing the graph

Both `spm list` and `spm status` surface provenance:

```bash
spm list
# toolkit                  https://github.com/org/toolkit  (tag:v2.0.0 @ 1a2b3c4d)
#
# transitive skills:
# toolkit__formatter-1a2b… https://github.com/org/formatter  (branch:main @ 9f8e7d6c; via toolkit)

spm status
#   toolkit                 ok
#   toolkit__formatter-1a2b ok  (transitive; via toolkit)
```

## Why it's opt-in

spm's trust model is different from a typical package manager: **skills can carry
executable agent instructions**. Transitive resolution means automatically
cloning third-party repos you never named, discovered only by reading a nested
`ai.json`. That is a materially larger supply-chain surface than resolving the
skills you listed yourself.

Every fetched skill (direct or transitive) still passes the
[content scan gate](/guide/how-it-works) **before** its nested manifest is read
or any of its dependencies are fetched — a blocked skill aborts before its
subtree is ever cloned. But the scan mitigates *what's in* a repo, not *whether
spm should fetch it at all*. Keeping resolution opt-in, committed, and reviewable
keeps that decision in your hands.
