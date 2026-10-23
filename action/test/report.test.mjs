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
  marker,
  parseMarker,
  planFor,
  renderBody,
  shortVersion,
} from "../lib/report.mjs";
import { readOutputs, tempDir } from "./helpers.mjs";

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

test("branchFor keeps plain names and hashes anything git can't take verbatim", () => {
  assert.equal(branchFor("my-skill_1.2"), "spm/update-my-skill_1.2");
  for (const name of ["a b", "x..y", "v.lock", "-lead", ".dot", "trail.", "a~b^c", "ü", "a@{b"]) {
    const b = branchFor(name);
    assert.match(b, /^spm\/update-[A-Za-z0-9][A-Za-z0-9._-]*-[0-9a-f]{8}$/, `${name} -> ${b}`);
    assert.ok(!b.includes(".."));
  }
  // Names that sanitize to the same slug still get distinct branches.
  assert.notEqual(branchFor("a b"), branchFor("a~b"));
  assert.equal(branchFor("a b"), branchFor("a b"));
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

test("the PR marker round-trips and ignores foreign or broken markers", () => {
  const body = `text\n${marker("my skill", SHA_A)}\n`;
  assert.deepEqual(parseMarker(body), { name: "my skill", commit: SHA_A });
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
  branch: "spm/update-s",
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
  assert.match(body, /git fetch origin spm\/update-s/);
  assert.deepEqual(parseMarker(body), { name: "s", commit: SHA_B });
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
  assert.deepEqual(parseMarker(body), { name: "s", commit: SHA_B });
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
