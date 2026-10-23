// End-to-end tests of the update action: the real spm binary, real git repos
// (skills, a bare `origin` and its clone) and a fake GitHub REST API.
import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, rmSync, symlinkSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { branchFor, parseMarker } from "../lib/report.mjs";
import { run as update } from "../lib/update.mjs";
import {
  MALICIOUS,
  REPO_ROOT,
  commitAll,
  envWithPath,
  fakeGitHub,
  git,
  makeRepo,
  readOutputs,
  run,
  skillMd,
  spmBin,
  tempDir,
  writeFiles,
} from "./helpers.mjs";

/// A project repo (cloned from a bare `origin`) that declares skills from
/// local upstream repos, plus a fake GitHub for `acme/project`.
async function sandbox(t, { subdir = "" } = {}) {
  const dir = tempDir(t);
  const home = join(dir, "spm-home");
  const origin = join(dir, "origin.git");
  const work = join(dir, "work");
  const project = subdir ? join(work, subdir) : work;
  run("git", ["init", "-q", "--bare", "-b", "main", origin]);
  run("git", ["clone", "-q", origin, work]);
  git(work, "checkout", "-q", "-b", "main");
  mkdirSync(project, { recursive: true });
  const gh = await fakeGitHub(t, { origin });
  const baseEnv = envWithPath([dirname(spmBin())], { SPM_HOME: home });
  const sb = {
    dir,
    work,
    project,
    origin,
    gh,
    spm: (...args) => run("spm", args, { cwd: project, env: baseEnv }).stdout,
    spmIn: (cwd, ...args) => run("spm", args, { cwd, env: baseEnv }).stdout,
    /// An upstream skill repo with `SKILL.md` committed on main and tagged.
    upstream(name, { files = {}, tag = "v1.0.0" } = {}) {
      const repo = join(dir, "upstream", name);
      const url = makeRepo(repo, { "SKILL.md": skillMd(name), ...files });
      if (tag) git(repo, "tag", tag);
      return { repo, url };
    },
    /// Commit (and push) the project's spm files as the base branch state.
    publish(message = "add skills") {
      git(work, "add", "-A");
      git(work, "commit", "-qm", message);
      git(work, "push", "-q", "origin", "HEAD:refs/heads/main");
    },
    env(inputs = {}) {
      const env = {
        ...baseEnv,
        GITHUB_API_URL: gh.url,
        GITHUB_REPOSITORY: gh.repository,
        GITHUB_SERVER_URL: "https://github.com",
        GITHUB_OUTPUT: join(dir, "output"),
        GITHUB_STEP_SUMMARY: join(dir, "summary"),
        INPUT_TOKEN: gh.token,
        "INPUT_WORKING-DIRECTORY": subdir || ".",
        INPUT_BASE: "",
        INPUT_LABELS: "dependencies",
        "INPUT_BLOCKED-LABEL": "spm-scan-blocked",
        "INPUT_DRY-RUN": "false",
      };
      for (const [k, v] of Object.entries(inputs)) env[`INPUT_${k.toUpperCase()}`] = v;
      rmSync(env.GITHUB_OUTPUT, { force: true });
      rmSync(env.GITHUB_STEP_SUMMARY, { force: true });
      return env;
    },
    update(inputs) {
      return update(this.env(inputs), { cwd: work });
    },
    /// A file as committed on a branch of `origin`.
    remoteFile(branch, path) {
      return run("git", ["--git-dir", origin, "show", `refs/heads/${branch}:${path}`]).stdout;
    },
    remoteSha(branch) {
      const r = run("git", ["--git-dir", origin, "rev-parse", "--verify", "-q", `refs/heads/${branch}`], { allowFail: true });
      return r.status === 0 ? r.stdout.trim() : null;
    },
    /// The branch of `name`'s update for this project and `base`.
    branch(name, base = "main") {
      return branchFor({ name, dir: subdir ? subdir.split("\\").join("/") : ".", base });
    },
    pull(number) {
      return gh.state.pulls.find((p) => p.number === number);
    },
    writes() {
      return gh.state.requests.filter((r) => r.method !== "GET");
    },
  };
  sb.spm("init", "--target", "copilot");
  return sb;
}

function assertCleanCheckout(sb, branch = "main") {
  assert.equal(git(sb.work, "symbolic-ref", "--short", "HEAD"), branch);
  assert.equal(git(sb.work, "status", "--porcelain"), "");
}

test("nothing outdated: no branches, no API calls, empty outputs", async (t) => {
  const sb = await sandbox(t);
  sb.spm("add", sb.upstream("alpha").url, "--tag", "v1.0.0");
  sb.publish();

  assert.deepEqual(await sb.update(), []);
  assert.deepEqual(sb.gh.state.requests, []);
  assert.deepEqual(readOutputs(join(sb.dir, "output")), { outdated: "0", "pull-requests": "[]" });
  assert.match(readFileSync(join(sb.dir, "summary"), "utf8"), /Everything is up to date\./);
  assertCleanCheckout(sb);
});

test("one PR per outdated dependency; re-runs are idempotent and upstream moves refresh the same PR", async (t) => {
  const sb = await sandbox(t);
  const tagged = sb.upstream("tagged");
  const tracked = sb.upstream("tracked", { tag: null });
  sb.spm("add", tagged.url, "--tag", "v1.0.0");
  sb.spm("add", tracked.url, "--branch", "main");
  sb.publish();
  const baseLock = JSON.parse(readFileSync(join(sb.project, "ai.lock"), "utf8"));

  writeFiles(tagged.repo, { "SKILL.md": skillMd("tagged", "v1.1 body\n") });
  const taggedNew = commitAll(tagged.repo, "v1.1");
  git(tagged.repo, "tag", "v1.1.0");
  writeFiles(tracked.repo, { "notes.md": "more\n" });
  const trackedNew = commitAll(tracked.repo, "move main");

  const first = await sb.update();
  assert.deepEqual(
    first.map((r) => [r.name, r.result, r.branch, r.number]),
    [
      ["tagged", "created", sb.branch("tagged"), 1],
      ["tracked", "created", sb.branch("tracked"), 2],
    ],
  );
  assertCleanCheckout(sb);

  // The tag dependency is re-pinned in ai.json; both locks move.
  const taggedPr = sb.pull(1);
  assert.equal(taggedPr.title, "chore(deps): update skill tagged to v1.1.0");
  assert.equal(taggedPr.base, "main");
  assert.deepEqual(taggedPr.labels, ["dependencies"]);
  assert.equal(JSON.parse(sb.remoteFile(sb.branch("tagged"), "ai.json")).skills.tagged.tag, "v1.1.0");
  const taggedLock = JSON.parse(sb.remoteFile(sb.branch("tagged"), "ai.lock")).skills;
  assert.equal(taggedLock.tagged.commit, taggedNew);
  assert.deepEqual(taggedLock.tracked, baseLock.skills.tracked, "one dependency per PR");
  assert.match(taggedPr.body, new RegExp(`\\| \\*\\*From\\*\\* \\| \`tag v1\\.0\\.0\` \\| \`${baseLock.skills.tagged.commit}\` \\|`));
  assert.match(taggedPr.body, new RegExp(`\\| \\*\\*To\\*\\* \\| \`tag v1\\.1\\.0\` \\| \`${taggedNew}\` \\|`));
  assert.match(taggedPr.body, /### Content scan\n\nNo findings\./);

  const trackedPr = sb.pull(2);
  assert.equal(trackedPr.title, `chore(deps): update skill tracked to main@${trackedNew.slice(0, 8)}`);
  assert.equal(JSON.parse(sb.remoteFile(sb.branch("tracked"), "ai.lock")).skills.tracked.commit, trackedNew);
  const commit = run("git", ["--git-dir", sb.origin, "log", "-1", "--format=%an <%ae>|%s", sb.branch("tracked")]).stdout.trim();
  assert.equal(commit, `github-actions[bot] <41898282+github-actions[bot]@users.noreply.github.com>|${trackedPr.title}`);

  const outputs = readOutputs(join(sb.dir, "output"));
  assert.equal(outputs.outdated, "2");
  assert.deepEqual(JSON.parse(outputs["pull-requests"]).map((p) => [p.name, p.number, p.url]), [
    ["tagged", 1, taggedPr.html_url],
    ["tracked", 2, trackedPr.html_url],
  ]);
  assert.match(readFileSync(join(sb.dir, "summary"), "utf8"), /\| skill `tagged` \| created \| \[#1\]\(https:\/\/github\.test\/acme\/project\/pull\/1\) \|/);

  // Nothing new upstream: no push, no PR edits.
  const shas = [sb.remoteSha(sb.branch("tagged")), sb.remoteSha(sb.branch("tracked"))];
  const writes = sb.writes().length;
  assert.deepEqual((await sb.update()).map((r) => r.result), ["unchanged", "unchanged"]);
  assert.deepEqual([sb.remoteSha(sb.branch("tagged")), sb.remoteSha(sb.branch("tracked"))], shas);
  assert.equal(sb.writes().length, writes);

  // The branch moves again: the same PR is refreshed.
  writeFiles(tracked.repo, { "notes.md": "even more\n" });
  const trackedNewer = commitAll(tracked.repo, "move main again");
  const third = await sb.update();
  assert.deepEqual(third.map((r) => [r.name, r.result, r.number]), [
    ["tagged", "unchanged", 1],
    ["tracked", "updated", 2],
  ]);
  assert.equal(sb.gh.state.pulls.length, 2);
  assert.equal(JSON.parse(sb.remoteFile(sb.branch("tracked"), "ai.lock")).skills.tracked.commit, trackedNewer);
  assert.match(sb.pull(2).body, new RegExp(trackedNewer));
  assert.equal(sb.pull(2).title, `chore(deps): update skill tracked to main@${trackedNewer.slice(0, 8)}`);
  assertCleanCheckout(sb);
});

test("blocking scan findings label the PR and the label goes once the content is clean", async (t) => {
  const sb = await sandbox(t);
  const up = sb.upstream("risky", { tag: null });
  sb.spm("add", up.url, "--branch", "main");
  sb.publish();

  writeFiles(up.repo, { "SKILL.md": skillMd("risky", MALICIOUS) });
  commitAll(up.repo, "inject");
  const [flagged] = await sb.update();
  assert.equal(flagged.result, "created");
  assert.ok(flagged.blocking > 0);
  const pr = sb.pull(flagged.number);
  assert.deepEqual(pr.labels, ["dependencies", "spm-scan-blocked"]);
  assert.match(pr.body, /> \[!CAUTION\]\n> \*\*\d+ blocking\*\* finding\(s\)/);
  assert.match(pr.body, /`risky\/SKILL\.md:\d+`/);
  assert.match(readFileSync(join(sb.dir, "summary"), "utf8"), /created ⚠️ blocking findings/);
  // The base branch never got the flagged content.
  assert.doesNotMatch(readFileSync(join(sb.project, "ai.lock"), "utf8"), new RegExp(git(up.repo, "rev-parse", "HEAD")));

  writeFiles(up.repo, { "SKILL.md": skillMd("risky", "Fixed.\n") });
  commitAll(up.repo, "fix");
  const [fixed] = await sb.update();
  assert.equal(fixed.result, "updated");
  assert.equal(fixed.number, flagged.number);
  assert.equal(fixed.blocking, 0);
  assert.deepEqual(sb.pull(fixed.number).labels, ["dependencies"]);
  assert.doesNotMatch(sb.pull(fixed.number).body, /CAUTION/);
});

test("a declined (closed, unmerged) update is not re-proposed until upstream moves again", async (t) => {
  const sb = await sandbox(t);
  const up = sb.upstream("alpha", { tag: null });
  sb.spm("add", up.url, "--branch", "main");
  sb.publish();
  writeFiles(up.repo, { "a.md": "1\n" });
  commitAll(up.repo, "one");

  const [first] = await sb.update();
  sb.pull(first.number).state = "closed";

  const [again] = await sb.update();
  assert.deepEqual([again.result, again.number], ["declined", first.number]);
  assert.equal(sb.gh.state.pulls.length, 1);
  assert.equal(readOutputs(join(sb.dir, "output"))["pull-requests"], JSON.stringify([again]));

  // Upstream moves on: the new commit was never declined.
  writeFiles(up.repo, { "a.md": "2\n" });
  const newer = commitAll(up.repo, "two");
  const [fresh] = await sb.update();
  assert.equal(fresh.result, "created");
  assert.equal(fresh.number, 2);
  assert.match(sb.pull(2).body, new RegExp(newer));
});

test("a merged PR with the same commit does not count as declined", async (t) => {
  const sb = await sandbox(t);
  const up = sb.upstream("alpha", { tag: null });
  sb.spm("add", up.url, "--branch", "main");
  sb.publish();
  writeFiles(up.repo, { "a.md": "1\n" });
  commitAll(up.repo, "one");
  const [first] = await sb.update();
  Object.assign(sb.pull(first.number), { state: "closed", merged_at: "2025-01-01T00:00:00Z" });

  const [again] = await sb.update();
  assert.equal(again.result, "created");
  assert.equal(again.number, 2);
});

test("version ranges move within the range; a newer major outside it is skipped", async (t) => {
  const sb = await sandbox(t);
  const up = sb.upstream("ranged");
  sb.spm("add", up.url, "--version", "^1");
  sb.publish();

  writeFiles(up.repo, { "SKILL.md": skillMd("ranged", "v2\n") });
  commitAll(up.repo, "v2");
  git(up.repo, "tag", "v2.0.0");
  const [skipped] = await sb.update();
  assert.equal(skipped.result, "skipped");
  assert.match(skipped.reason, /v2\.0\.0 is outside the version range `\^1`/);
  assert.equal(sb.gh.state.pulls.length, 0);
  assert.equal(readOutputs(join(sb.dir, "output")).outdated, "1");

  git(up.repo, "tag", "v1.1.0", "HEAD~1");
  git(up.repo, "checkout", "-q", "-b", "maint", "v1.1.0");
  writeFiles(up.repo, { "SKILL.md": skillMd("ranged", "v1.2\n") });
  const v12 = commitAll(up.repo, "v1.2");
  git(up.repo, "tag", "v1.2.0");
  const [moved] = await sb.update();
  assert.equal(moved.result, "created");
  const pr = sb.pull(moved.number);
  assert.equal(pr.title, "chore(deps): update skill ranged to v1.2.0");
  assert.match(pr.body, /\| \*\*To\*\* \| `version \^1 → v1\.2\.0` \|/);
  const manifest = JSON.parse(sb.remoteFile(sb.branch("ranged"), "ai.json"));
  assert.equal(manifest.skills.ranged.version, "^1", "the range itself is kept");
  assert.equal(JSON.parse(sb.remoteFile(sb.branch("ranged"), "ai.lock")).skills.ranged.commit, v12);
});

test("a re-pointed tag is proposed with a warning", async (t) => {
  const sb = await sandbox(t);
  const up = sb.upstream("moving");
  sb.spm("add", up.url, "--tag", "v1.0.0");
  sb.publish();
  writeFiles(up.repo, { "SKILL.md": skillMd("moving", "rewritten\n") });
  const moved = commitAll(up.repo, "rewrite");
  git(up.repo, "tag", "-f", "v1.0.0");

  const [r] = await sb.update();
  assert.equal(r.result, "created");
  const pr = sb.pull(r.number);
  assert.equal(pr.title, "chore(deps): update skill moving to v1.0.0");
  assert.match(pr.body, /> \[!WARNING\]\n> The tag `v1\.0\.0` was \*\*re-pointed\*\* upstream/);
  assert.equal(JSON.parse(sb.remoteFile(sb.branch("moving"), "ai.lock")).skills.moving.commit, moved);
});

test("plugins are re-tagged with their path kept", async (t) => {
  const sb = await sandbox(t);
  const repo = join(sb.dir, "upstream", "plugin");
  const plugin = (v) => ({
    "pkg/.claude-plugin/plugin.json": JSON.stringify({ name: "kit", version: v, skills: "./skills/" }),
    "pkg/skills/helper/SKILL.md": skillMd("helper", `${v}\n`),
  });
  const url = makeRepo(repo, plugin("1.0.0"));
  git(repo, "tag", "v1.0.0");
  sb.spm("add", url, "--tag", "v1.0.0", "--path", "pkg", "--plugin", "--name", "kit");
  sb.publish();
  writeFiles(repo, plugin("1.1.0"));
  commitAll(repo, "1.1");
  git(repo, "tag", "v1.1.0");

  const [r] = await sb.update();
  assert.deepEqual([r.kind, r.result, r.branch], ["plugin", "created", sb.branch("kit")]);
  assert.equal(sb.pull(r.number).title, "chore(deps): update plugin kit to v1.1.0");
  const spec = JSON.parse(sb.remoteFile(sb.branch("kit"), "ai.json")).plugins.kit;
  assert.deepEqual([spec.tag, spec.path], ["v1.1.0", "pkg"]);
  assert.match(sb.pull(r.number).body, /Source: `.*` \(path `pkg`\)/);
});

test("a project in a subdirectory commits only its own spm files", async (t) => {
  const sb = await sandbox(t, { subdir: "tools/ai" });
  const up = sb.upstream("alpha", { tag: null });
  sb.spm("add", up.url, "--branch", "main");
  sb.publish();
  writeFiles(up.repo, { "a.md": "1\n" });
  const head = commitAll(up.repo, "one");

  const [r] = await sb.update();
  assert.equal(r.result, "created");
  assert.equal(JSON.parse(sb.remoteFile(sb.branch("alpha"), "tools/ai/ai.lock")).skills.alpha.commit, head);
  const changed = run("git", ["--git-dir", sb.origin, "diff", "--name-only", "main", sb.branch("alpha")]).stdout.trim();
  assert.equal(changed, "tools/ai/ai.lock");
  assertCleanCheckout(sb);
});

test("a plugin's symlinks into the rest of its repository are scanned like the install gate does", { skip: process.platform === "win32" && "symlinks need privileges on Windows" }, async (t) => {
  const sb = await sandbox(t);
  const repo = join(sb.dir, "upstream", "plugin");
  const url = makeRepo(repo, {
    "agents/dev.md": "I am the dev agent.\n",
    "plugins/kit/.claude-plugin/plugin.json": JSON.stringify({ name: "kit", version: "1.0.0" }),
  });
  symlinkSync("../../agents", join(repo, "plugins", "kit", "agents"));
  commitAll(repo, "link agents");
  git(repo, "tag", "v1.0.0");
  sb.spm("add", url, "--tag", "v1.0.0", "--path", "plugins/kit", "--plugin", "--name", "kit");
  sb.publish();
  // The new version's blocking content sits outside the plugin directory,
  // reached only through the in-checkout symlink.
  writeFiles(repo, { "agents/dev.md": MALICIOUS });
  commitAll(repo, "inject");
  git(repo, "tag", "v1.1.0");

  const [r] = await sb.update();
  assert.equal(r.result, "created");
  assert.ok(r.blocking > 0, "linked blocking content is reported");
  const pr = sb.pull(r.number);
  assert.deepEqual(pr.labels, ["dependencies", "spm-scan-blocked"]);
  assert.match(pr.body, /`kit\/agents\/dev\.md:\d+`/);
});

test("uncommitted tracked changes anywhere in the repository stop the action before any checkout", async (t) => {
  const sb = await sandbox(t, { subdir: "tools/ai" });
  const up = sb.upstream("alpha", { tag: null });
  sb.spm("add", up.url, "--branch", "main");
  writeFiles(sb.work, { "README.md": "base\n", "notes.md": "base\n" });
  sb.publish();
  writeFiles(up.repo, { "a.md": "1\n" });
  commitAll(up.repo, "one");

  // An unstaged change outside `working-directory` and a staged one.
  writeFiles(sb.work, { "README.md": "edited\n", "notes.md": "staged\n" });
  git(sb.work, "add", "notes.md");
  for (const inputs of [{}, { "dry-run": "true" }]) {
    await assert.rejects(sb.update(inputs), /uncommitted changes to tracked files[\s\S]*README\.md[\s\S]*notes\.md/);
    assert.equal(readFileSync(join(sb.work, "README.md"), "utf8"), "edited\n");
    assert.equal(readFileSync(join(sb.work, "notes.md"), "utf8"), "staged\n");
    assert.equal(git(sb.work, "diff", "--cached", "--name-only"), "notes.md");
    assert.equal(git(sb.work, "symbolic-ref", "--short", "HEAD"), "main");
  }
  assert.deepEqual(sb.gh.state.requests, []);
  assert.equal(sb.remoteSha(sb.branch("alpha")), null);

  // Untracked files are no obstacle: forced checkouts of the base leave them be.
  git(sb.work, "reset", "-q", "--hard");
  writeFiles(sb.work, { "scratch.txt": "mine\n" });
  const [r] = await sb.update();
  assert.equal(r.result, "created");
  assert.equal(readFileSync(join(sb.work, "scratch.txt"), "utf8"), "mine\n");
});

test("the same dependency in two projects of one repository gets a PR per project", async (t) => {
  const sb = await sandbox(t, { subdir: "a" });
  const up = sb.upstream("alpha", { tag: null });
  const other = join(sb.work, "b");
  mkdirSync(other, { recursive: true });
  sb.spmIn(other, "init", "--target", "copilot");
  for (const cwd of [sb.project, other]) sb.spmIn(cwd, "add", up.url, "--branch", "main");
  sb.publish();
  writeFiles(up.repo, { "a.md": "1\n" });
  const head = commitAll(up.repo, "one");

  const [ra] = await sb.update();
  const [rb] = await sb.update({ "working-directory": "b" });
  assert.deepEqual([ra.result, rb.result], ["created", "created"]);
  assert.notEqual(ra.branch, rb.branch);
  assert.equal(rb.branch, branchFor({ name: "alpha", dir: "b", base: "main" }));
  assert.notEqual(ra.number, rb.number);
  for (const [r, dir] of [[ra, "a"], [rb, "b"]]) {
    const changed = run("git", ["--git-dir", sb.origin, "diff", "--name-only", "main", r.branch]).stdout.trim();
    assert.equal(changed, `${dir}/ai.lock`);
    assert.equal(JSON.parse(sb.remoteFile(r.branch, `${dir}/ai.lock`)).skills.alpha.commit, head);
    assert.deepEqual(parseMarker(sb.pull(r.number).body), { name: "alpha", dir, base: "main", commit: head });
  }
  // A re-run of the first project refreshes its own PR, not the other's.
  const [again] = await sb.update();
  assert.deepEqual([again.result, again.number], ["unchanged", ra.number]);
  assert.equal(sb.gh.state.pulls.length, 2);
});

test("updates of a maintenance branch get their own branch and PR against that base", async (t) => {
  const sb = await sandbox(t);
  const up = sb.upstream("alpha", { tag: null });
  sb.spm("add", up.url, "--branch", "main");
  sb.publish();
  git(sb.work, "push", "-q", "origin", "HEAD:refs/heads/release/1.x");
  writeFiles(up.repo, { "a.md": "1\n" });
  const head = commitAll(up.repo, "one");

  const [onMain] = await sb.update();
  git(sb.work, "checkout", "-q", "-b", "release/1.x", "origin/main");
  const [onRelease] = await sb.update();
  assert.deepEqual([onMain.result, onRelease.result], ["created", "created"]);
  assert.equal(onRelease.branch, sb.branch("alpha", "release/1.x"));
  assert.notEqual(onMain.branch, onRelease.branch);
  assert.deepEqual([sb.pull(onMain.number).base, sb.pull(onRelease.number).base], ["main", "release/1.x"]);
  assert.equal(parseMarker(sb.pull(onRelease.number).body).base, "release/1.x");
  assertCleanCheckout(sb, "release/1.x");

  // The `base` input selects the identity too, whatever is checked out.
  const [viaInput] = await sb.update({ base: "main" });
  assert.deepEqual([viaInput.result, viaInput.number], ["unchanged", onMain.number]);

  // A PR from the update branch that someone retargeted is not reused.
  sb.pull(onMain.number).base = "elsewhere";
  git(sb.work, "checkout", "-q", "main");
  const [fresh] = await sb.update();
  assert.equal(fresh.result, "created");
  assert.notEqual(fresh.number, onMain.number);
  assert.equal(sb.pull(fresh.number).base, "main");
  assert.equal(JSON.parse(sb.remoteFile(fresh.branch, "ai.lock")).skills.alpha.commit, head);
});

test("dry-run commits locally but pushes nothing and needs no token", async (t) => {
  const sb = await sandbox(t);
  const up = sb.upstream("alpha", { tag: null });
  sb.spm("add", up.url, "--branch", "main");
  sb.publish();
  writeFiles(up.repo, { "a.md": "1\n" });
  commitAll(up.repo, "one");

  const results = await sb.update({ "dry-run": "true", token: "" });
  assert.deepEqual(results.map((r) => [r.name, r.result, r.branch]), [["alpha", "dry-run", sb.branch("alpha")]]);
  assert.equal(sb.remoteSha(sb.branch("alpha")), null);
  assert.deepEqual(sb.gh.state.requests, []);
  assert.equal(readOutputs(join(sb.dir, "output"))["pull-requests"], "[]");
  assertCleanCheckout(sb);
});

test("a dependency whose remote can't be reached fails the run without blocking the others", async (t) => {
  const sb = await sandbox(t);
  const gone = sb.upstream("gone", { tag: null });
  const ok = sb.upstream("ok", { tag: null });
  sb.spm("add", gone.url, "--branch", "main");
  sb.spm("add", ok.url, "--branch", "main");
  sb.publish();
  rmSync(gone.repo, { recursive: true, force: true, maxRetries: 5 });
  writeFiles(ok.repo, { "a.md": "1\n" });
  commitAll(ok.repo, "one");

  await assert.rejects(sb.update(), /1 problem\(s\):\ngone: /);
  assert.deepEqual(sb.gh.state.pulls.map((p) => p.head), [sb.branch("ok")]);
  const summary = readFileSync(join(sb.dir, "summary"), "utf8");
  assert.match(summary, /\*\*Problems:\*\*\n- gone: /);
  assert.equal(JSON.parse(readOutputs(join(sb.dir, "output"))["pull-requests"]).length, 1);
  assertCleanCheckout(sb);
});

test("a failing update is reported and the remaining dependencies still get PRs", async (t) => {
  const sb = await sandbox(t);
  const first = sb.upstream("first", { tag: null });
  const second = sb.upstream("second", { tag: null });
  sb.spm("add", first.url, "--branch", "main");
  sb.spm("add", second.url, "--branch", "main");
  sb.publish();
  writeFiles(first.repo, { "a.md": "1\n" });
  commitAll(first.repo, "one");
  writeFiles(second.repo, { "a.md": "1\n" });
  commitAll(second.repo, "one");
  // GitHub refuses the first PR.
  sb.gh.state.fail = (r) => (r.method === "POST" && r.body?.head === sb.branch("first") ? 403 : 0);

  await assert.rejects(sb.update(), /1 problem\(s\):\nfirst: POST \/repos\/acme\/project\/pulls: HTTP 403/);
  assert.deepEqual(sb.gh.state.pulls.map((p) => p.head), [sb.branch("second")]);
  const [failed] = JSON.parse(readOutputs(join(sb.dir, "output"))["pull-requests"]);
  assert.equal(failed.name, "second");
  assertCleanCheckout(sb);
});

test("a detached checkout needs `base`, and is restored afterwards", async (t) => {
  const sb = await sandbox(t);
  const up = sb.upstream("alpha", { tag: null });
  sb.spm("add", up.url, "--branch", "main");
  sb.publish();
  writeFiles(up.repo, { "a.md": "1\n" });
  commitAll(up.repo, "one");
  const head = git(sb.work, "rev-parse", "HEAD");
  git(sb.work, "checkout", "-q", "--detach");

  await assert.rejects(sb.update(), /HEAD is detached/);
  const [r] = await sb.update({ base: "main" });
  assert.equal(sb.pull(r.number).base, "main");
  assert.equal(git(sb.work, "rev-parse", "HEAD"), head);
  assert.equal(run("git", ["symbolic-ref", "-q", "HEAD"], { cwd: sb.work, allowFail: true }).status, 1);
});

test("the action entry point maps runner inputs and fails with an annotation", async (t) => {
  const sb = await sandbox(t);
  const up = sb.upstream("alpha", { tag: null });
  sb.spm("add", up.url, "--branch", "main");
  sb.publish();
  writeFiles(up.repo, { "a.md": "1\n" });
  commitAll(up.repo, "one");
  const entry = join(REPO_ROOT, "update", "main.mjs");

  // The runner hands the fake API's URL to a child process: run it async so
  // this process's event loop keeps serving it.
  const spawn = async (env) => {
    const { spawn: nodeSpawn } = await import("node:child_process");
    return new Promise((resolve) => {
      const child = nodeSpawn(process.execPath, [entry], { cwd: sb.work, env, windowsHide: true });
      let stdout = "";
      child.stdout.on("data", (d) => (stdout += d));
      child.stderr.on("data", (d) => (stdout += d));
      child.on("close", (status) => resolve({ status, stdout }));
    });
  };

  const ok = await spawn(sb.env({ labels: "deps, bot" }));
  assert.equal(ok.status, 0, ok.stdout);
  assert.deepEqual(sb.pull(1).labels, ["deps", "bot"]);
  assert.equal(JSON.parse(readOutputs(join(sb.dir, "output"))["pull-requests"])[0].number, 1);

  const bad = await spawn(sb.env({ token: "" }));
  assert.equal(bad.status, 1);
  assert.match(bad.stdout, /^::error::input `token` is required/m);
  assert.ok(!existsSync(join(sb.dir, "output")), "no outputs on a failed start");
});

test("without spm on PATH the error says to run the setup action first", async (t) => {
  const sb = await sandbox(t);
  sb.spm("add", sb.upstream("alpha").url, "--tag", "v1.0.0");
  sb.publish();
  const gitDir = dirname(run(process.platform === "win32" ? "where" : "which", ["git"]).stdout.split(/\r?\n/)[0].trim());
  if (["spm", "spm.exe"].some((f) => existsSync(join(gitDir, f)))) {
    t.skip(`an spm is installed next to git in ${gitDir}`);
    return;
  }
  const env = sb.env();
  const pathKey = Object.keys(env).find((k) => k.toUpperCase() === "PATH");
  env[pathKey] = gitDir;
  await assert.rejects(update(env, { cwd: sb.work }), /`spm` is not on PATH: run `uses: camunda\/spm-cli@v0` before this action/);
});
