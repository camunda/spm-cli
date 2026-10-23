# Source Policy (allowlist)

Every skill or plugin spm installs is executable agent context, and so is every
transitive skill it pulls in. A **source policy** restricts *where* that content
may come from: an allowlist of repository patterns that every source — direct,
transitive, or re-installed from `ai.lock` — must match before spm contacts it.

With no policy configured, behavior is unchanged: any URL `git` understands is
accepted.

## Where a policy comes from

There are two layers. Each is optional, and a source must be allowed by **every**
layer that is configured (the effective policy is their intersection):

| Layer | Where | Who controls it |
| --- | --- | --- |
| Machine | The file named by `SPM_POLICY`, else `$SPM_HOME/policy.json` (default `~/.spm/policy.json`) | The user, or an organization via managed machines / CI |
| Project | `allowedSources` in the root project's `ai.json` (or the global `ai.json` for `-g`) | The repository |

So a project can **narrow** a machine policy but can never widen it. A
dependency's own (nested) `ai.json` may not declare `allowedSources` at all —
that is a hard error, so a skill can't loosen or even shape the policy that
governs it.

### Project policy

```json
{
  "targets": ["claude"],
  "allowedSources": ["github.com/camunda/*", "github.com/my-org/approved-skill"],
  "skills": {
    "reviewer": { "git": "https://github.com/camunda/reviewer", "tag": "v1.0.0" }
  }
}
```

### Machine policy

```json
{ "allowedSources": ["github.com/camunda/*", "gitlab.example.com/platform/*"] }
```

The file accepts only `allowedSources` (and an optional `$schema`); any other key
is an error, so a typo can't silently switch the policy off. A policy that is
present but broken never fails open:

- `SPM_POLICY` naming a file that doesn't exist is an error (an empty
  `SPM_POLICY` counts as unset).
- Unreadable or malformed JSON, an unknown key, a missing `allowedSources` or an
  invalid pattern are errors.
- Only a **default** `$SPM_HOME/policy.json` that doesn't exist at all means "no
  machine policy" — a dangling symlink there is an error.

## Pattern syntax

A pattern is `host/path`, with no scheme:

| Pattern | Allows |
| --- | --- |
| `github.com/camunda/*` | every repository under `camunda`, including nested groups (`github.com/camunda/team/repo`) |
| `github.com/camunda/reviewer` | exactly that repository |
| `github.com/*` | every repository on the host |
| `git.example.com:8443/team/*` | a host on a non-default port (the port must be spelled out) |
| `file://*` | local sources: `file://` URLs, absolute paths, `./`/`../` paths (on Windows also drive and UNC paths like `C:\skills\repo`) |

- `*` is only allowed as the **whole last segment**, and matches one or more
  segments; `github.com/*` allows a whole host. A bare host (`github.com`) and
  partial wildcards (`camunda-*`, `*/repo`) are rejected.
- The host is matched case-insensitively; the path is matched **case-sensitively**.
- A trailing `.git` is ignored on both sides.
- Patterns with a scheme (`https://github.com/...`) are rejected — the policy
  judges *which repository* is used, not the transport.

An empty list (`"allowedSources": []`) allows nothing.

## How sources are matched

A source URL is reduced to a canonical `host/path` identity before matching, so
the same repository matches one pattern regardless of how it is spelled:

```
https://github.com/camunda/reviewer.git      →  github.com/camunda/reviewer
https://user@GitHub.com/camunda/reviewer     →  github.com/camunda/reviewer
ssh://git@github.com/camunda/reviewer.git    →  github.com/camunda/reviewer
git@github.com:camunda/reviewer.git          →  github.com/camunda/reviewer
github.com/camunda/reviewer@v1 (shorthand)   →  github.com/camunda/reviewer
```

Local sources match only `file://*`. "Local" follows git's own rule — no colon,
or a `/` before the first colon (a drive letter counts only on Windows) — so
`host:org/repo` is an SSH remote judged by its host, never by `file://*`. And
since git resolves a bare name like `origin` or `team/skills` as a configured
remote before trying it as a path, a local path must be unambiguous under a
policy: a `file://` URL, an absolute path, or one starting with `./`/`../`.
A URL spm can't reduce to an identity (an
unknown scheme, a `transport::address` helper, a `?`/`#` inside the URL
authority, a path with `..`, `%`-escapes or other ambiguous characters, leading
or trailing whitespace) is **denied** under any policy.

## What is enforced, and when

The policy is checked **before** any git call, and before `ai.json` or `ai.lock`
is written:

- `spm add` checks the URL (after expanding a `github.com/...` shorthand) before
  the default-branch lookup and before saving `ai.json`.
- `spm install`, `spm update`, `spm remove` and `spm target add` check every
  direct skill and plugin up front and report **all** denied ones at once.
  This includes `spm install --locked` and installs that would be served
  entirely from `ai.lock` and the local store cache.
- With [`resolveTransitive`](/guide/transitive-dependencies), every nested
  source is checked before it is resolved or fetched; the error names the chain
  of skills that required it.
- `spm outdated` reports a denied source as an error row instead of querying it.
- `--protocol` and `--protocol-fallback` can't bypass the policy: the URL you
  wrote **and** every rewritten URL actually contacted must be allowed.
- While a policy is in force git does not follow HTTP redirects
  (`http.followRedirects=false`), so an allowed URL can't redirect to a host or
  namespace the policy never judged. The same setting stops git from following
  a dumb-HTTP server's `objects/info/http-alternates`. spm pins it for the exact
  URL being contacted, so it overrides your git config — including URL-scoped
  `http.<url>.followRedirects` entries — and checks the value git will actually
  use; if it still can't turn redirects off for a URL, it refuses to contact it.
  Without a policy, redirects and alternates behave as before.
- While a policy is in force, checkout filters are disabled. A repository
  selects a filter driver through its own `.gitattributes`, and a smudge filter
  such as Git LFS downloads content from wherever the repository's `.lfsconfig`
  or the LFS server points it, which spm can't check. Every configured filter
  driver is therefore made to fail and marked required: installing a repository
  that uses one fails, instead of downloading unchecked content or silently
  installing LFS pointer files as skills. Repositories that select no filter
  are unaffected.

Git's own URL rewriting in your git config (`url.<base>.insteadOf`) is machine
configuration, trusted like the policy file itself; spm judges the URL as written
in `ai.json`.

Commands that never contact a remote (`list`, `status`, `clean`, `scan`, `prune`,
`target remove`) don't evaluate the policy, so you can always inspect and clean
up a project whose sources are no longer allowed.

## Errors

```text
error: cannot add `https://evil.example/x/skill.git`: source `https://evil.example/x/skill.git`
(evil.example/x/skill) is not allowed: denied by the project policy `allowedSources`
in /work/app/ai.json (allows `github.com/camunda/*`)
```

```text
error: 2 dependencies are not allowed by the source policy:
  - skill `a`: source `https://evil.example/x/a.git` (evil.example/x/a) is not allowed: denied by the machine policy /home/me/.spm/policy.json (allows `github.com/camunda/*`) and by the project policy `allowedSources` in /work/app/ai.json (allows `github.com/camunda/*`)
  - plugin `p`: source `git@evil.example:x/p.git` (evil.example/x/p) is not allowed: denied by the machine policy /home/me/.spm/policy.json (allows `github.com/camunda/*`) and by the project policy `allowedSources` in /work/app/ai.json (allows `github.com/camunda/*`)
```

```text
error: nested skill `helper` required by `toolkit` (via toolkit -> helper): source
`https://evil.example/x/helper.git` (evil.example/x/helper) is not allowed: denied by ...
```

To fix a denial, either change the dependency to an allowed source or — if you
trust it — add a pattern for it to the layer that denied it (the error names
it). A machine policy is the boundary for the whole machine; a project can't
override it.
