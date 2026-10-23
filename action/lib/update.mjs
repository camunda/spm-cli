// `uses: camunda/spm-cli/update@<ref>` — open (or refresh) one pull request
// per outdated skill/plugin, with the version change and the content scan of
// the new code in its body.
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join, relative, resolve } from "node:path";
import { appendSummary, getBooleanInput, getInput, getListInput, log, setOutput } from "./core.mjs";
import { GitHub } from "./github.mjs";
import {
  blockingCount,
  branchFor,
  diffLocks,
  entryOf,
  parseMarker,
  planFor,
  renderBody,
  sameIdentity,
  shortVersion,
} from "./report.mjs";

const COMMITTER = { name: "github-actions[bot]", email: "41898282+github-actions[bot]@users.noreply.github.com" };

/// Run `cmd args` and return the spawn result; throws on a non-zero exit
/// unless `allowFail`, with stderr in the message.
function exec(cmd, args, { cwd, env, allowFail = false }) {
  const r = spawnSync(cmd, args, { cwd, env, encoding: "utf8", maxBuffer: 256 * 1024 * 1024, windowsHide: true });
  if (r.error?.code === "ENOENT" && cmd === "spm") {
    throw new Error("`spm` is not on PATH: run `uses: camunda/spm-cli@v0` before this action");
  }
  if (r.error) throw new Error(`could not run \`${cmd}\`: ${r.error.message}`);
  if (r.status !== 0 && !allowFail) {
    const out = (r.stderr || r.stdout || "").trim();
    throw new Error(`\`${cmd} ${args.join(" ")}\` failed (exit ${r.status})${out ? `:\n${out}` : ""}`);
  }
  return r;
}

function readJson(file) {
  return existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : null;
}

/// The spm store root, resolved exactly like spm's `paths::store_dir`.
function storeDir(env) {
  return join(env.SPM_HOME !== undefined ? env.SPM_HOME : join(homedir(), ".spm"), "store");
}

/// `spm scan --json` over the store checkout of one lock entry, bounded by its
/// checkout root (`--within`) exactly like the install gate, so a subdirectory
/// entry's symlinks into the rest of its repository are scanned too. Finding
/// paths are made relative to the scanned directory (forward slashes).
function scanEntry(change, { cwd, env }) {
  const { name, to } = change;
  const root = join(storeDir(env), to.store);
  const dir = join(root, ...(to.path ? to.path.split("/") : []));
  const r = exec("spm", ["scan", "--json", "--within", root, dir], { cwd, env, allowFail: true });
  try {
    const report = JSON.parse(r.stdout);
    const findings = report.findings.map((f) => ({
      ...f,
      path: relative(dir, resolve(dir, f.path)).split("\\").join("/") || ".",
    }));
    return { name, findings, summary: report.summary };
  } catch {
    return { name, error: (r.stderr || r.stdout || `exit ${r.status}`).trim() };
  }
}

export async function run(env, { cwd = process.cwd() } = {}) {
  const dryRun = getBooleanInput(env, "dry-run");
  const token = getInput(env, "token");
  if (!token && !dryRun) throw new Error("input `token` is required (it defaults to `github.token`)");
  const labels = getListInput(env, "labels");
  const blockedLabel = getInput(env, "blocked-label");
  const wd = resolve(cwd, getInput(env, "working-directory") || ".");
  const git = (args, opts = {}) => exec("git", args, { cwd: wd, env, ...opts });
  const out = (args) => git(args).stdout.trim();

  // git runs in `wd`, where pathspecs are relative to it. Comparing paths with
  // `--show-toplevel` instead breaks on symlinked temp dirs and Windows 8.3 names.
  const files = ["ai.json", "ai.lock"];
  const startRef = git(["symbolic-ref", "--quiet", "--short", "HEAD"], { allowFail: true }).stdout.trim();
  const baseSha = out(["rev-parse", "HEAD"]);
  const base = getInput(env, "base") || startRef;
  if (!base) throw new Error("HEAD is detached: check out the branch to update, or set the `base` input");
  // Each update is checked out with `-f` and the start is restored with `-f`:
  // refuse to run over tracked changes anywhere in the repository rather than
  // silently discard them.
  const dirty = out(["status", "--porcelain", "--untracked-files=no"]);
  if (dirty) {
    throw new Error(
      `the repository has uncommitted changes to tracked files; commit or stash them before this action (it checks out a branch per update):\n${dirty}`,
    );
  }
  // The project's repository-relative directory (forward slashes, `.` for the
  // root) is part of each update's identity.
  const dir = out(["rev-parse", "--show-prefix"]).replace(/\/+$/, "") || ".";
  // Update branches are built on HEAD and proposed against `base` on origin:
  // HEAD must be a commit of that branch, or every PR would also carry the
  // commits that only HEAD has (a diverged or unpushed checkout). HEAD being
  // behind a base that moved since the checkout is fine.
  const fetched = git(["fetch", "-q", "--no-tags", "origin", `+refs/heads/${base}:refs/spm-update/base`], { allowFail: true });
  if (fetched.status !== 0) {
    throw new Error(`could not fetch the base branch \`${base}\` from origin:\n${fetched.stderr.trim()}`);
  }
  if (git(["merge-base", "--is-ancestor", baseSha, "refs/spm-update/base"], { allowFail: true }).status !== 0) {
    throw new Error(
      `HEAD (${baseSha.slice(0, 8)}) is not a commit of \`${base}\` on origin, so update PRs against it would carry unrelated commits: check out \`${base}\` (or set \`base\` to the checked-out branch) and push any local commits first`,
    );
  }
  const github = dryRun
    ? null
    : new GitHub({ apiUrl: env.GITHUB_API_URL, token, repository: env.GITHUB_REPOSITORY });

  // `outdated` exits non-zero when a remote can't be checked but still prints
  // its report; the per-row errors are surfaced below.
  const outdated = exec("spm", ["outdated", "--json"], { cwd: wd, env, allowFail: true });
  let report;
  try {
    report = JSON.parse(outdated.stdout);
  } catch {
    throw new Error(`\`spm outdated --json\` failed (exit ${outdated.status}):\n${outdated.stderr.trim()}`);
  }
  const rows = [
    ...report.skills.map((r) => ({ ...r, kind: "skill" })),
    ...report.plugins.map((r) => ({ ...r, kind: "plugin" })),
  ];
  const problems = [];
  for (const r of rows) {
    const errors = [r.error, ...(r.alsoSatisfies ?? []).map((s) => s.error)].filter(Boolean);
    for (const e of errors) {
      log.warning(`${r.kind} ${r.name}: could not check for updates: ${e}`);
      problems.push(`${r.name}: ${e}`);
    }
  }
  const candidates = rows.filter((r) => r.outdated);
  setOutput(env, "outdated", candidates.length);
  const results = [];

  if (candidates.length === 0) {
    log.info("nothing is outdated");
  } else {
    const baseLock = readJson(join(wd, "ai.lock"));
    const manifest = readJson(join(wd, "ai.json"));
    // The gate is the scan reported in the PR, not a refusal to propose: a
    // blocking finding labels the PR instead of hiding the update.
    const spmEnv = { ...env, SPM_ALLOW_SUSPICIOUS: "1" };
    try {
      for (const row of candidates) {
        results.push(
          await updateOne(row, { wd, dir, env, spmEnv, git, out, files, baseSha, base, baseLock, manifest, github, labels, blockedLabel, dryRun, problems }),
        );
      }
    } finally {
      git(startRef ? ["checkout", "-q", "-f", startRef] : ["checkout", "-q", "-f", "--detach", baseSha]);
    }
  }

  const pulls = results.filter((r) => r.number);
  setOutput(env, "pull-requests", JSON.stringify(pulls));
  appendSummary(env, summaryMarkdown(results, problems));
  if (problems.length > 0) {
    throw new Error(`${problems.length} problem(s):\n${problems.join("\n")}`);
  }
  return results;
}

async function updateOne(row, ctx) {
  const { wd, dir, env, spmEnv, git, out, files, baseSha, base, baseLock, manifest, github, labels, blockedLabel, dryRun, problems } = ctx;
  const { name, kind } = row;
  const plan = planFor(row);
  if (plan.action === "skip") {
    log.notice(`${kind} ${name}: skipped — ${plan.reason}`);
    return { name, kind, result: "skipped", reason: plan.reason };
  }
  if (plan.action === "reject") {
    log.error(`${kind} ${name}: ${plan.reason}`);
    problems.push(`${name}: ${plan.reason}`);
    return { name, kind, result: "failed", reason: plan.reason };
  }
  const id = { name, dir, base };
  const branch = branchFor(id);
  log.group(`${kind} ${name} → ${branch}`);
  try {
    git(["checkout", "-q", "-f", "-B", branch, baseSha]);
    let args;
    if (plan.action === "retag") {
      const spec = entryOf(manifest, kind === "plugin" ? "plugins" : "skills", name);
      if (!spec) throw new Error(`${kind} \`${name}\` is not declared in ai.json`);
      // Values are attached with `=` and positionals follow `--`, so a name
      // (or URL, tag, path) starting with `-` is never parsed as a flag.
      args = ["add", `--tag=${plan.tag}`, `--name=${name}`, "--force", "--lock-only"];
      if (spec.path) args.push(`--path=${spec.path}`);
      if (kind === "plugin") args.push("--plugin");
      args.push("--", spec.git);
    } else {
      args = ["update", "--lock-only", "--", name];
    }
    const sync = exec("spm", args, { cwd: wd, env: spmEnv });
    log.info(sync.stdout.trim());

    if (git(["diff", "--quiet", "--", ...files], { allowFail: true }).status === 0) {
      log.notice(`${kind} ${name}: \`spm ${args[0]}\` changed nothing`);
      return { name, kind, result: "unchanged", reason: "no change to ai.json/ai.lock" };
    }
    const lock = readJson(join(wd, "ai.lock"));
    const map = kind === "plugin" ? "plugins" : "skills";
    const from = entryOf(baseLock, map, name);
    const to = entryOf(lock, map, name);
    if (!to) throw new Error(`${kind} \`${name}\` is missing from ai.lock after \`spm ${args[0]}\``);
    const changes = diffLocks(baseLock, lock);
    const scans = changes.filter((c) => c.to).map((c) => scanEntry(c, { cwd: wd, env }));
    const blocking = blockingCount(scans);
    const title = `chore(deps): update ${kind} ${name} to ${shortVersion(to)}`;
    const body = renderBody({
      name,
      kind,
      dir,
      base,
      git: to.git,
      path: to.path,
      from,
      to,
      changes,
      scans,
      serverUrl: env.GITHUB_SERVER_URL,
      branch,
    });

    git(["add", "--", ...files]);
    git([
      "-c", `user.name=${COMMITTER.name}`,
      "-c", `user.email=${COMMITTER.email}`,
      "-c", "commit.gpgsign=false",
      "commit", "-q", "--no-verify", "-m", title,
    ]);
    if (dryRun) {
      log.info(`dry run — would push ${branch} and open:\n${title}\n\n${body}`);
      return { name, kind, branch, result: "dry-run", blocking };
    }

    const closed = await github.listPulls(branch, "closed", base);
    const declined = closed.find((p) => {
      const m = parseMarker(p.body);
      return !p.merged_at && sameIdentity(m, id) && m.commit === to.commit;
    });
    if (declined) {
      log.notice(`${kind} ${name}: ${to.commit.slice(0, 8)} was declined in #${declined.number}; skipping`);
      return { name, kind, branch, result: "declined", number: declined.number, url: declined.html_url };
    }

    const published = pushIfChanged(branch, { git, out });
    // Another run of the workflow may push this branch while this one is
    // publishing (the documented `concurrency` group prevents it). The PR body
    // and labels must describe the branch head, so they are only written while
    // the head is still the one this run pushed.
    const headMoved = () => remoteHead(branch, git) !== published;
    // The branch already encodes the identity; a PR from it that was
    // retargeted to another base is not this update's and is left alone.
    const open = (await github.listPulls(branch, "open", base)).find((p) => p.base?.ref === base);
    if (headMoved()) {
      log.notice(`${kind} ${name}: ${branch} was pushed by another run; leaving its pull request to that run`);
      return { name, kind, branch, result: "superseded", reason: "another run pushed a newer update" };
    }
    let pr;
    let result;
    if (open) {
      result = open.title === title && open.body === body ? "unchanged" : "updated";
      pr = result === "updated" ? await github.updatePull(open.number, { title, body }) : open;
    } else {
      pr = await github.createPull({ title, head: branch, base, body });
      result = "created";
    }
    const has = new Set((pr.labels ?? []).map((l) => l.name));
    const want = [...labels, ...(blocking > 0 && blockedLabel ? [blockedLabel] : [])];
    const missing = want.filter((l) => !has.has(l));
    if (missing.length > 0) await github.addLabels(pr.number, missing);
    if (blocking === 0 && blockedLabel && has.has(blockedLabel)) await github.removeLabel(pr.number, blockedLabel);
    if (headMoved()) {
      throw new Error(
        `another run pushed ${branch} while this one published #${pr.number}, so its body and labels may describe an older commit: run the workflow in a \`concurrency\` group (see https://camunda.github.io/spm-cli/guide/ci) and re-run it`,
      );
    }
    log.info(`${result} #${pr.number} ${pr.html_url}`);
    return { name, kind, branch, result, number: pr.number, url: pr.html_url, blocking };
  } catch (e) {
    log.error(`${kind} ${name}: ${e.message}`);
    problems.push(`${name}: ${e.message}`);
    return { name, kind, branch, result: "failed", reason: e.message };
  } finally {
    log.endGroup();
  }
}

/// Force-push the update branch unless the remote one already holds the same
/// tree (a re-run with nothing new must not churn the PR). The lease makes a
/// concurrent push to the branch fail rather than be overwritten.
/// Returns the commit the branch then has on origin.
function pushIfChanged(branch, { git, out }) {
  const ref = `refs/heads/${branch}`;
  const remote = remoteHead(branch, git);
  if (remote) {
    git(["fetch", "-q", "--no-tags", "origin", `+${ref}:refs/spm-update/remote`]);
    if (out(["rev-parse", "refs/spm-update/remote^{tree}"]) === out(["rev-parse", "HEAD^{tree}"])) {
      log.info(`${branch} is already up to date on origin`);
      return remote;
    }
  }
  git(["push", "-q", `--force-with-lease=${ref}:${remote}`, "origin", `HEAD:${ref}`]);
  return out(["rev-parse", "HEAD"]);
}

/// The commit `branch` has on origin now, or "" when it doesn't exist.
function remoteHead(branch, git) {
  return git(["ls-remote", "origin", `refs/heads/${branch}`]).stdout.split(/\s/)[0] ?? "";
}

function summaryMarkdown(results, problems) {
  const lines = ["### spm update", ""];
  if (results.length === 0) {
    lines.push(problems.length > 0 ? "Nothing to update among the dependencies that could be checked." : "Everything is up to date.");
  } else {
    lines.push("| Dependency | Result | Pull request |", "| --- | --- | --- |");
    for (const r of results) {
      const pr = r.url ? `[#${r.number}](${r.url})` : (r.reason ?? "");
      const flag = r.blocking > 0 ? " ⚠️ blocking findings" : "";
      lines.push(`| ${r.kind} \`${r.name}\` | ${r.result}${flag} | ${pr.replace(/\|/g, "\\|")} |`);
    }
  }
  if (problems.length > 0) lines.push("", "**Problems:**", ...problems.map((p) => `- ${p}`));
  return lines.join("\n");
}
