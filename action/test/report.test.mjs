import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { addPath, getBooleanInput, getInput, getListInput, setOutput } from "../lib/core.mjs";
import {
  blockingCount,
  branchFor,
  compareUrl,
  describeSelector,
  diffLocks,
  entryOf,
  marker,
  parseMarker,
  sameIdentity,
  planFor,
  renderBody,
  shortVersion,
} from "../lib/report.mjs";
import { readOutputs, run, tempDir } from "./helpers.mjs";

const SHA_A = "a".repeat(40);
const SHA_B = "b".repeat(40);

const row = (fields) => ({
  name: "s",
  outdated: true,
  transitive: false,
  requestedBy: [],
  selector: { tag: "v1.0.0" },
  status: "newer-tag",
  latestTag: null,
  ...fields,
});

test("planFor: a newer tag re-pins the tag (also when the old tag moved)", () => {
  assert.deepEqual(planFor(row({ latestTag: "v1.1.0" })), { action: "retag", tag: "v1.1.0" });
  assert.deepEqual(planFor(row({ status: "tag-moved", latestTag: "v2.0.0" })), { action: "retag", tag: "v2.0.0" });
});

test("planFor: moved branches, re-pointed tags and in-range tags re-resolve", () => {
  assert.deepEqual(planFor(row({ selector: { branch: "main" }, status: "behind" })), { action: "update" });
  assert.deepEqual(planFor(row({ status: "tag-moved" })), { action: "update" });
  assert.deepEqual(planFor(row({ selector: { version: "^1" }, status: "behind", latestTag: "v2.0.0" })), {
    action: "update",
  });
  // Outdated only through a secondary selector: still worth re-resolving.
  assert.deepEqual(planFor(row({ selector: { commit: SHA_A }, status: "pinned" })), { action: "update" });
});

test("planFor: skips what this project's ai.json can't adopt", () => {
  assert.equal(planFor(row({ outdated: false })).action, "skip");
  const transitive = planFor(row({ transitive: true, requestedBy: ["top", "mid"], latestTag: "v2" }));
  assert.equal(transitive.action, "skip");
  assert.match(transitive.reason, /requested by top, mid/);
  const range = planFor(row({ selector: { version: "^1" }, status: "newer-tag", latestTag: "v2.0.0" }));
  assert.equal(range.action, "skip");
  assert.match(range.reason, /v2\.0\.0 is outside the version range `\^1`/);
});

test("planFor: rejects a row whose ai.json request changed since ai.lock was written", () => {
  for (const fields of [{ latestTag: "v2.0.0" }, { selector: { branch: "main" }, status: "behind" }]) {
    const plan = planFor(row({ ...fields, drift: "`tag:v1.0.0` -> `version:^1`" }));
    assert.equal(plan.action, "reject");
    assert.match(plan.reason, /ai\.json no longer matches ai\.lock \(`tag:v1\.0\.0` -> `version:\^1`\): run `spm install`/);
  }
  // Nothing to plan anyway.
  assert.equal(planFor(row({ outdated: false, drift: "x" })).action, "skip");
  assert.deepEqual(planFor(row({ latestTag: "v2.0.0", drift: null })), { action: "retag", tag: "v2.0.0" });
});

test("branchFor gives every update identity its own valid branch", () => {
  const id = (name, dir = ".", base = "main") => ({ name, dir, base });
  assert.match(branchFor(id("my-skill_1.2")), /^spm\/update\/my-skill_1\.2-[0-9a-f]{12}$/);
  const names = ["my-skill_1.2", "a b", "x..y", "v.lock", "-lead", ".dot", "trail.", "a~b^c", "ü", "a@{b", "x".repeat(80)];
  for (const name of names) {
    const b = branchFor(id(name));
    assert.match(b, /^spm\/update\/[A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9_])?-[0-9a-f]{12}$/, `${name} -> ${b}`);
    assert.equal(run("git", ["check-ref-format", `refs/heads/${b}`], { allowFail: true }).status, 0, `git rejects ${b}`);
    assert.equal(branchFor(id(name)), b, "stable across runs");
  }
  const distinct = [
    // Names that sanitize alike, and a plain name equal to another's encoding.
    id("a b"),
    id("a~b"),
    id("a-b"),
    id(branchFor(id("a b")).slice("spm/update/".length)),
    // The same dependency in another project of the repository…
    id("a b", "tools/ai"),
    // …or updating another base branch.
    id("a b", ".", "release/1.x"),
    id("a b", "tools/ai", "release/1.x"),
  ];
  const branches = distinct.map(branchFor);
  assert.equal(new Set(branches).size, branches.length, branches.join("\n"));
});

test("compareUrl links GitHub-hosted sources in every URL form", () => {
  const want = `https://github.com/o/r/compare/${SHA_A}...${SHA_B}`;
  for (const git of [
    "https://github.com/o/r",
    "https://github.com/o/r.git",
    "https://github.com/o/r/",
    "https://user:tok@github.com/o/r.git",
    "git@github.com:o/r.git",
    "ssh://git@github.com/o/r.git",
    "ssh://git@github.com:22/o/r",
    "https://GitHub.com/o/r",
  ]) {
    assert.equal(compareUrl(git, SHA_A, SHA_B), want, git);
  }
});

test("compareUrl recognizes the workflow's own GitHub server, and nothing else", () => {
  assert.equal(
    compareUrl("https://ghe.acme.test/o/r.git", SHA_A, SHA_B, "https://ghe.acme.test"),
    `https://ghe.acme.test/o/r/compare/${SHA_A}...${SHA_B}`,
  );
  assert.equal(compareUrl("https://ghe.acme.test/o/r.git", SHA_A, SHA_B), null);
  assert.equal(compareUrl("https://gitlab.com/o/r.git", SHA_A, SHA_B, "https://github.com"), null);
  assert.equal(compareUrl("file:///tmp/r", SHA_A, SHA_B), null);
  assert.equal(compareUrl("https://github.com/o/r/sub/dir", SHA_A, SHA_B), null);
  assert.equal(compareUrl("https://github.com/o/r", SHA_A, SHA_B, "not a url"), `https://github.com/o/r/compare/${SHA_A}...${SHA_B}`);
  assert.equal(compareUrl("https://github.com/o/r", null, SHA_B), null);
  assert.equal(compareUrl("https://github.com/o/r", SHA_A, SHA_A), null);
});

test("selectors and versions are described from ai.lock entries", () => {
  assert.equal(describeSelector({ reference: "tag:v1.2.0" }), "tag v1.2.0");
  assert.equal(describeSelector({ reference: "branch:main" }), "branch main");
  assert.equal(describeSelector({ reference: `commit:${SHA_A}` }), "commit aaaaaaaa");
  assert.equal(describeSelector({ reference: "version:^1.2", resolved_tag: "v1.4.0" }), "version ^1.2 → v1.4.0");
  assert.equal(describeSelector({ reference: "weird" }), "? weird");
  assert.equal(shortVersion({ reference: "tag:v1.2.0", commit: SHA_A }), "v1.2.0");
  assert.equal(shortVersion({ reference: "version:^1", resolved_tag: "v1.4.0", commit: SHA_A }), "v1.4.0");
  assert.equal(shortVersion({ reference: "branch:main", commit: SHA_A }), "main@aaaaaaaa");
  assert.equal(shortVersion({ reference: `commit:${SHA_A}`, commit: SHA_A }), "aaaaaaaa");
});

test("diffLocks reports added, removed and re-pinned entries of both kinds", () => {
  const e = (commit, reference = "branch:main", resolved_tag) => ({ commit, reference, resolved_tag });
  const before = { skills: { same: e(SHA_A), moved: e(SHA_A), gone: e(SHA_A), retag: e(SHA_A, "version:^1", "v1.0.0") }, plugins: { p: e(SHA_A) } };
  const after = { skills: { same: e(SHA_A), moved: e(SHA_B), added: e(SHA_B), retag: e(SHA_A, "version:^1", "v1.0.1") }, plugins: { p: e(SHA_B) } };
  assert.deepEqual(
    diffLocks(before, after).map((c) => `${c.kind}:${c.name}:${c.from ? "f" : "-"}${c.to ? "t" : "-"}`),
    ["skill:added:-t", "skill:gone:f-", "skill:moved:ft", "skill:retag:ft", "plugin:p:ft"],
  );
  assert.deepEqual(diffLocks(null, { skills: {} }), []);
});

test("diffLocks reports an entry that changed in any field, not only its commit", () => {
  const entry = { git: "file:///r", reference: "tag:v1", commit: SHA_A, path: "a", store: "r@a", bundled_skills: ["x"] };
  for (const [field, value] of [
    ["path", "b"],
    ["git", "file:///other"],
    ["store", "other@a"],
    ["bundled_skills", ["x", "y"]],
  ]) {
    const changes = diffLocks({ plugins: { p: entry } }, { plugins: { p: { ...entry, [field]: value } } });
    assert.deepEqual(changes.map((c) => [c.kind, c.name, c.to[field]]), [["plugin", "p", value]], field);
  }
  const { path, ...noPath } = entry;
  assert.equal(diffLocks({ skills: { s: entry } }, { skills: { s: noPath } }).length, 1, "a dropped field");
  // Field order is not a change.
  const reordered = Object.fromEntries(Object.entries(entry).reverse());
  assert.deepEqual(diffLocks({ skills: { s: entry } }, { skills: { s: reordered } }), []);
});

test("lock entries named like Object members are looked up as own properties only", () => {
  const entry = { git: "file:///r", reference: "tag:v1", commit: SHA_A, store: "r@a" };
  const lock = (names) => JSON.parse(JSON.stringify({ skills: Object.fromEntries(names.map((n) => [n, entry])) }));
  for (const name of ["constructor", "toString", "hasOwnProperty", "__proto__"]) {
    // JSON.parse keeps `__proto__` as an own key, as in a real ai.lock.
    const withIt = JSON.parse(`{"skills":{${JSON.stringify(name)}:${JSON.stringify(entry)}}}`);
    assert.equal(entryOf(lock([]), "skills", name), null, name);
    assert.deepEqual(entryOf(withIt, "skills", name), entry, name);
    assert.deepEqual(
      diffLocks(lock([]), withIt).map((c) => [c.name, c.from, c.to?.commit]),
      [[name, null, SHA_A]],
      `${name} added`,
    );
    assert.deepEqual(
      diffLocks(withIt, lock([])).map((c) => [c.name, c.from?.commit, c.to]),
      [[name, SHA_A, null]],
      `${name} removed`,
    );
    // Rendering an added one in the other-changes table must not crash.
    const body = renderBody({ ...base, changes: diffLocks(lock([]), withIt) });
    assert.ok(body.includes(`\`${name}\``), name);
  }
  assert.equal(entryOf(null, "skills", "x"), null);
  assert.equal(entryOf({}, "plugins", "x"), null);
});

test("the PR marker round-trips and ignores foreign or broken markers", () => {
  const id = { name: "my skill", dir: "tools/ai", base: "release/1.x" };
  const parsed = parseMarker(`text\n${marker(id, SHA_A)}\n`);
  assert.deepEqual(parsed, { ...id, commit: SHA_A });
  assert.ok(sameIdentity(parsed, id));
  assert.ok(!sameIdentity(parsed, { ...id, dir: "." }));
  assert.ok(!sameIdentity(parsed, { ...id, base: "main" }));
  assert.ok(!sameIdentity(parsed, { ...id, name: "other" }));
  assert.ok(!sameIdentity(null, id));
  assert.equal(parseMarker("no marker"), null);
  assert.equal(parseMarker("<!-- spm-update {broken} -->"), null);
  assert.equal(parseMarker(null), null);
});

test("blockingCount treats a scan that failed to run as blocking", () => {
  assert.equal(blockingCount([{ summary: { blocking: 2 } }, { summary: { blocking: 0 } }]), 2);
  assert.equal(blockingCount([{ error: "boom" }, { summary: { blocking: 1 } }]), 2);
  assert.equal(blockingCount([]), 0);
});

const base = {
  name: "s",
  kind: "skill",
  git: "https://github.com/o/r.git",
  path: "skills/s",
  dir: ".",
  base: "main",
  branch: "spm/update/s-0123456789ab",
  from: { reference: "tag:v1.0.0", commit: SHA_A },
  to: { reference: "tag:v1.1.0", commit: SHA_B },
  changes: [],
  scans: [{ name: "s", findings: [], summary: { total: 0, blocking: 0 } }],
};

test("the PR body shows old and new versions, the compare link and a clean scan", () => {
  const body = renderBody(base);
  assert.match(body, /\| \*\*From\*\* \| `tag v1\.0\.0` \| `a{40}` \|/);
  assert.match(body, /\| \*\*To\*\* \| `tag v1\.1\.0` \| `b{40}` \|/);
  assert.match(body, /Source: `https:\/\/github\.com\/o\/r\.git` \(path `skills\/s`\)/);
  assert.match(body, new RegExp(`Upstream changes: https://github.com/o/r/compare/${SHA_A}\\.\\.\\.${SHA_B}`));
  assert.match(body, /### Content scan\n\nNo findings\./);
  assert.match(body, /git fetch origin spm\/update\/s-0123456789ab/);
  assert.deepEqual(parseMarker(body), { name: "s", dir: ".", base: "main", commit: SHA_B });
  assert.doesNotMatch(body, /re-pointed|Other lock changes|CAUTION/);
});

test("the PR body flags blocking findings, failed scans and other lock changes", () => {
  const body = renderBody({
    ...base,
    git: "file:///tmp/r",
    from: null,
    changes: [
      { name: "s", kind: "skill", from: null, to: base.to },
      { name: "child", kind: "skill", from: base.from, to: null },
      { name: "p", kind: "plugin", from: null, to: { reference: "branch:main", commit: SHA_B } },
    ],
    scans: [
      {
        name: "s",
        summary: { total: 2, blocking: 1 },
        findings: [
          { rule: "curl-pipe-shell", category: "command-execution", severity: "critical", blocking: true, path: "SKILL.md", line: 7, detail: "a | b `c`" },
          { rule: "wide", category: "x", severity: "low", blocking: false, path: "dir/file.md", line: null, detail: "line\nbreak" },
        ],
      },
      { name: "p", error: "could not scan" },
    ],
  });
  assert.match(body, /_\(not locked\)_/);
  assert.doesNotMatch(body, /Upstream changes/);
  assert.match(body, /> \[!CAUTION\]\n> \*\*2 blocking\*\* finding\(s\)/);
  assert.match(body, /\| \*\*critical\*\* \| `curl-pipe-shell` \| command-execution \| `s\/SKILL\.md:7` \| a \\\| b `c` \|/);
  assert.match(body, /\| low \| `wide` \| x \| `s\/dir\/file\.md` \| line break \|/);
  assert.match(body, /- \*\*p\*\*: scan failed — could not scan/);
  assert.match(body, /### Other lock changes/);
  assert.match(body, /\| skill `child` \| `tag v1\.0\.0` `aaaaaaaa` \| _\(none\)_ \|/);
  assert.match(body, /\| plugin `p` \| _\(none\)_ \| `branch main` `bbbbbbbb` \|/);
});

test("the PR body notes non-blocking findings and an empty scan set", () => {
  const nonBlocking = renderBody({
    ...base,
    scans: [{ name: "s", summary: { total: 1, blocking: 0 }, findings: [{ rule: "r", category: "c", severity: "low", blocking: false, path: "f", line: 1, detail: "d" }] }],
  });
  assert.match(nonBlocking, /No blocking findings; 1 non-blocking finding\(s\) below\./);
  assert.match(renderBody({ ...base, scans: [] }), /Nothing new to scan/);
});

test("the PR body warns when a pinned tag was re-pointed upstream", () => {
  const body = renderBody({ ...base, to: { reference: "tag:v1.0.0", commit: SHA_B } });
  assert.match(body, /> \[!WARNING\]\n> The tag `v1\.0\.0` was \*\*re-pointed\*\* upstream/);
});

test("the PR body warns when the tag a version range is locked to was re-pointed", () => {
  const range = (tag, commit) => ({ reference: "version:^1", resolved_tag: tag, commit });
  const warn = /> \[!WARNING\]\n> The tag `v1\.0\.0` was \*\*re-pointed\*\* upstream/;
  assert.match(renderBody({ ...base, from: range("v1.0.0", SHA_A), to: range("v1.0.0", SHA_B) }), warn);
  // A newer tag in the range, or the same commit, is no re-pointing.
  assert.doesNotMatch(renderBody({ ...base, from: range("v1.0.0", SHA_A), to: range("v1.1.0", SHA_B) }), /re-pointed/);
  assert.doesNotMatch(renderBody({ ...base, from: range("v1.0.0", SHA_A), to: range("v1.0.0", SHA_A) }), /re-pointed/);
  const tag = { reference: "tag:v1.0.0", commit: SHA_A };
  assert.doesNotMatch(renderBody({ ...base, from: tag, to: tag }), /re-pointed/);
  // Branches have no tag to re-point.
  const br = (commit) => ({ reference: "branch:main", commit });
  assert.doesNotMatch(renderBody({ ...base, from: br(SHA_A), to: br(SHA_B) }), /re-pointed/);
});

test("the size cap cuts lock-change details, never the blocking scan summary", () => {
  const changes = Array.from({ length: 400 }, (_, i) => ({
    name: `transitive-${"n".repeat(150)}-${i}`,
    kind: "skill",
    from: null,
    to: { reference: "tag:v1.0.0", commit: SHA_B, path: `skills/${"p".repeat(150)}` },
  }));
  const body = renderBody({
    ...base,
    changes,
    scans: [
      {
        name: "s",
        summary: { total: 1, blocking: 1 },
        findings: [{ rule: "r", category: "c", severity: "critical", blocking: true, path: "SKILL.md", line: 1, detail: "d" }],
      },
    ],
  });
  assert.ok(body.length <= 60000, `${body.length}`);
  assert.match(body, /_\(truncated\)_/, "the lock changes alone exceed the limit");
  assert.match(body, /> \[!CAUTION\]\n> \*\*1 blocking\*\* finding\(s\)/);
  assert.match(body, /\| \*\*critical\*\* \| `r` \|/);
  assert.ok(body.indexOf("### Content scan") < body.indexOf("### Other lock changes"));
  assert.deepEqual(parseMarker(body), { name: "s", dir: ".", base: "main", commit: SHA_B });
});

test("the PR body caps the findings table and its total size, keeping the marker", () => {
  const findings = Array.from({ length: 80 }, (_, i) => ({
    rule: "r",
    category: "c",
    severity: "low",
    blocking: false,
    path: `f${i}`,
    line: 1,
    detail: "x".repeat(2000),
  }));
  const body = renderBody({ ...base, scans: [{ name: "s", summary: { total: 80, blocking: 0 }, findings }] });
  assert.ok(body.length <= 60000, `${body.length}`);
  assert.match(body, /_\(truncated\)_/);
  assert.deepEqual(parseMarker(body), { name: "s", dir: ".", base: "main", commit: SHA_B });
  const few = renderBody({ ...base, scans: [{ name: "s", summary: { total: 80, blocking: 0 }, findings: findings.map((f) => ({ ...f, detail: "d" })) }] });
  assert.match(few, /…and 30 more/);
});

test("inputs are read the way the runner passes them", () => {
  const env = { "INPUT_RELEASE-URL": " https://x ", INPUT_MY_INPUT: "v", INPUT_LIST: "a, b\n\nc,", INPUT_T: "true", INPUT_F: "False" };
  assert.equal(getInput(env, "release-url"), "https://x");
  assert.equal(getInput(env, "my input"), "v");
  assert.equal(getInput(env, "missing"), "");
  assert.deepEqual(getListInput(env, "list"), ["a", "b", "c"]);
  assert.equal(getBooleanInput(env, "t"), true);
  assert.equal(getBooleanInput(env, "f"), false);
  assert.equal(getBooleanInput(env, "missing"), false);
  assert.throws(() => getBooleanInput({ INPUT_X: "yes" }, "x"), /must be `true` or `false`/);
});

test("outputs and PATH entries are written as runner file commands", (t) => {
  const dir = tempDir(t);
  const env = { GITHUB_OUTPUT: join(dir, "out"), GITHUB_PATH: join(dir, "path") };
  setOutput(env, "multi", "line1\nline2");
  setOutput(env, "n", 3);
  addPath(env, "/opt/spm");
  assert.deepEqual(readOutputs(env.GITHUB_OUTPUT), { multi: "line1\nline2", n: "3" });
  assert.equal(readFileSync(env.GITHUB_PATH, "utf8").trim(), "/opt/spm");
  // Without the runner files these are no-ops rather than errors.
  setOutput({}, "x", "y");
  addPath({}, "/x");
});
