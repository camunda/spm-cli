// Drift guards between the actions, the release workflows and the shared
// target list: each of these is a contract a release depends on.
import assert from "node:assert/strict";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { TARGETS, assetName } from "../../npm/targets.mjs";
import { stageRelease } from "../stage-release.mjs";
import { CHECKSUMS, DEFAULT_RELEASE_URL, parseChecksums, resolveTarget, sha256 } from "../lib/setup.mjs";
import { REPO_ROOT, run, tempDir } from "./helpers.mjs";

const read = (...p) => readFileSync(join(REPO_ROOT, ...p), "utf8");

/// GitHub's `on.push.tags` glob: `*` matches anything but `/`.
function tagMatches(glob, tag) {
  const re = glob.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*\*/g, "\0").replace(/\*/g, "[^/]*").replace(/\0/g, ".*").replace(/\?/g, ".");
  return new RegExp(`^${re}$`).test(tag);
}

/// The patterns of a workflow's `on.push.tags`, inline (`["a"]`) or block list.
function pushTags(workflow) {
  const m = /^on:\n {2}push:\n {4}tags:(.*)\n((?: {6}- .*\n)*)/m.exec(workflow.replace(/^\s*#.*\n/gm, ""));
  assert.ok(m, "workflow has an on.push.tags trigger");
  const raw = m[1].trim() ? JSON.parse(m[1].trim()) : m[2].trim().split("\n").map((l) => l.replace(/^\s*- /, "").trim());
  return raw.map((p) => p.replace(/^['"]|['"]$/g, ""));
}

/// `inputs`/`outputs` names declared at the top level of an action.yml.
function declared(actionYml, section) {
  const block = new RegExp(`^${section}:\\n((?:(?: {2}.*)?\\n)*)`, "m").exec(actionYml);
  assert.ok(block, `action.yml has ${section}`);
  return [...block[1].matchAll(/^ {2}([\w-]+):/gm)].map((m) => m[1]).sort();
}

/// Input/output names a module reads or sets through core.mjs.
function used(source, fns) {
  const re = new RegExp(`\\b(?:${fns.join("|")})\\(env, "([^"]+)"`, "g");
  return [...new Set([...source.matchAll(re)].map((m) => m[1]))].sort();
}

test("release.yml builds exactly the shared target list", () => {
  const rows = [...read(".github", "workflows", "release.yml").matchAll(/- \{ os: [\w-]+, +target: ([\w-]+), +ext: "([^"]*)" \}/g)];
  assert.deepEqual(
    rows.map((m) => `${m[1]}${m[2]}`).sort(),
    TARGETS.map((t) => `${t.rustTarget}${t.exe.endsWith(".exe") ? ".exe" : ""}`).sort(),
  );
  // ...and its Package step names the assets the way the setup action fetches them.
  assert.match(read(".github", "workflows", "release.yml"), /out="spm-\$\{\{ matrix\.target \}\}\$\{\{ matrix\.ext \}\}"/);
  for (const t of TARGETS) assert.equal(assetName(t), `spm-${t.rustTarget}${t.os === "win32" ? ".exe" : ""}`);
});

test("release tags publish; the action's moving major tag does not", () => {
  for (const file of ["release.yml", "publish.yml"]) {
    const globs = pushTags(read(".github", "workflows", file));
    const fires = (tag) => globs.some((g) => tagMatches(g, tag));
    for (const tag of ["v0.10.0", "v1.2.3", "v1.0.0-rc.1"]) assert.ok(fires(tag), `${file} must release ${tag}`);
    for (const tag of ["v0", "v1", "v1.2", "latest"]) assert.ok(!fires(tag), `${file} must not release ${tag}`);
  }
});

test("release.yml publishes and verifies SHA256SUMS from the build artifacts", () => {
  const wf = read(".github", "workflows", "release.yml");
  const job = /^ {2}checksums:\n((?: {4}.*\n|\n)*)/m.exec(wf)?.[1];
  assert.ok(job, "release.yml has a checksums job");
  assert.match(job, /needs: build/);
  assert.match(job, /node action\/stage-release\.mjs --bin-dir artifacts --out release-assets/);
  assert.match(job, /files: release-assets\/SHA256SUMS/);
  assert.match(job, /sha256sum --check --strict SHA256SUMS/);
});

test("stage-release writes every asset with a SHA256SUMS the setup action accepts", (t) => {
  const dir = tempDir(t);
  for (const tg of TARGETS) {
    mkdirSync(join(dir, "bin", tg.rustTarget), { recursive: true });
    writeFileSync(join(dir, "bin", tg.rustTarget, tg.exe), `binary for ${tg.rustTarget}`);
  }
  stageRelease({ binDir: join(dir, "bin"), outDir: join(dir, "out") });
  const sums = parseChecksums(readFileSync(join(dir, "out", CHECKSUMS), "utf8"));
  assert.deepEqual([...sums.keys()].sort(), TARGETS.map(assetName).sort());
  for (const tg of TARGETS) {
    assert.equal(sums.get(assetName(tg)), sha256(readFileSync(join(dir, "out", assetName(tg)))));
    assert.equal(readFileSync(join(dir, "out", assetName(tg)), "utf8"), `binary for ${tg.rustTarget}`);
  }
});

test("stage-release refuses a release with a missing target, unless staging only the host", (t) => {
  const dir = tempDir(t);
  const host = resolveTarget(process.platform, process.arch);
  mkdirSync(join(dir, "bin", host.rustTarget), { recursive: true });
  writeFileSync(join(dir, "bin", host.rustTarget, host.exe), "host");
  assert.throws(() => stageRelease({ binDir: join(dir, "bin"), outDir: join(dir, "all") }), /missing binary for/);

  const script = join(REPO_ROOT, "action", "stage-release.mjs");
  run(process.execPath, [script, "--bin-dir", join(dir, "bin"), "--out", join(dir, "host"), "--host"]);
  assert.deepEqual([...parseChecksums(readFileSync(join(dir, "host", CHECKSUMS), "utf8")).keys()], [assetName(host)]);

  const bad = run(process.execPath, [script, "--bin-dir", join(dir, "bin")], { allowFail: true });
  assert.notEqual(bad.status, 0);
  assert.match(bad.stderr, /usage: stage-release\.mjs/);
});

test("each action declares exactly the inputs and outputs its code uses", () => {
  for (const [yml, src] of [
    [["action.yml"], ["action", "lib", "setup.mjs"]],
    [["update", "action.yml"], ["action", "lib", "update.mjs"]],
  ]) {
    const action = read(...yml);
    const code = read(...src);
    assert.deepEqual(declared(action, "inputs"), used(code, ["getInput", "getBooleanInput", "getListInput"]), yml.join("/"));
    assert.deepEqual(declared(action, "outputs"), used(code, ["setOutput"]), yml.join("/"));
    const main = /^ {2}main: "([^"]+)"$/m.exec(action)[1];
    assert.match(action, /^ {2}using: "node24"$/m);
    assert.ok(existsSync(join(REPO_ROOT, ...yml.slice(0, -1), main)), `${yml.join("/")} main ${main} exists`);
  }
  assert.match(read("action.yml"), new RegExp(`default: "${DEFAULT_RELEASE_URL}"`));
});
