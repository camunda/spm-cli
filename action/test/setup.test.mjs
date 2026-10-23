import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { test } from "node:test";
import { TARGETS, assetName } from "../../npm/targets.mjs";
import { crateVersion } from "../../npm/crate.mjs";
import {
  CHECKSUMS,
  fetchBytes,
  parseChecksums,
  resolveTarget,
  resolveVersion,
  run,
} from "../lib/setup.mjs";
import { REPO_ROOT, readOutputs, run as exec, serveDir, sha256, spmBin, stageRelease, tempDir } from "./helpers.mjs";

const VERSION = crateVersion(REPO_ROOT);
const HOST = resolveTarget(process.platform, process.arch);
const HOST_ASSET = assetName(HOST);

/// A runner-like environment: RUNNER_TEMP plus GITHUB_OUTPUT/GITHUB_PATH files.
function runnerEnv(dir, inputs = {}) {
  const env = {
    RUNNER_TEMP: join(dir, "runner-temp"),
    GITHUB_OUTPUT: join(dir, "output"),
    GITHUB_PATH: join(dir, "path"),
  };
  for (const [k, v] of Object.entries(inputs)) env[`INPUT_${k.toUpperCase()}`] = v;
  return env;
}

test("every release target maps from its Node platform/arch", () => {
  for (const t of TARGETS) assert.equal(resolveTarget(t.os, t.cpu), t);
  assert.equal(assetName(resolveTarget("win32", "x64")), "spm-x86_64-pc-windows-msvc.exe");
  assert.equal(assetName(resolveTarget("linux", "arm64")), "spm-aarch64-unknown-linux-gnu");
});

test("platforms without a release binary are rejected with the supported list", () => {
  for (const [os, cpu] of [
    ["win32", "arm64"],
    ["linux", "ia32"],
    ["linux", "arm"],
    ["freebsd", "x64"],
  ]) {
    assert.throws(() => resolveTarget(os, cpu), new RegExp(`no spm release binary for ${os}/${cpu}.*linux/x64`));
  }
});

test("the version defaults to the action's own Cargo.toml version", () => {
  assert.equal(resolveVersion(""), VERSION);
  assert.equal(resolveVersion("", REPO_ROOT), VERSION);
});

test("explicit versions accept an optional v prefix, pre-release and build metadata", () => {
  assert.equal(resolveVersion("1.2.3"), "1.2.3");
  assert.equal(resolveVersion("v1.2.3"), "1.2.3");
  assert.equal(resolveVersion("v1.2.3-rc.1"), "1.2.3-rc.1");
  assert.equal(resolveVersion("1.2.3+build.7"), "1.2.3+build.7");
});

test("anything but a semver version is rejected before any download", () => {
  for (const bad of ["latest", "1.2", "v1", "1.2.3/../../x", "../1.2.3", " 1.2.3 x", "vv1.2.3"]) {
    assert.throws(() => resolveVersion(bad), /invalid spm version/, bad);
  }
});

test("SHA256SUMS parsing accepts text and binary markers, CRLF and upper-case hex", () => {
  const a = "a".repeat(64);
  const b = "B".repeat(64);
  const sums = parseChecksums(`${a}  spm-x\r\n${b} *spm-y.exe\r\n\r\n`);
  assert.deepEqual([...sums], [
    ["spm-x", a],
    ["spm-y.exe", "b".repeat(64)],
  ]);
});

test("SHA256SUMS with malformed lines or conflicting duplicates is not trusted", () => {
  assert.throws(() => parseChecksums("abc  spm-x\n"), /malformed line 1/);
  assert.throws(() => parseChecksums(`${"a".repeat(64)}spm-x\n`), /malformed/);
  assert.throws(() => parseChecksums(`${"a".repeat(64)}  spm-x\n${"b".repeat(64)}  spm-x\n`), /conflicting/);
  // An exact duplicate is harmless.
  assert.equal(parseChecksums(`${"a".repeat(64)}  spm-x\n${"A".repeat(64)}  spm-x\n`).size, 1);
});

test("installs the verified binary from a file:// mirror and puts it on PATH", async (t) => {
  const dir = tempDir(t);
  stageRelease(join(dir, "rel"), VERSION, { [HOST_ASSET]: spmBin() });
  const env = runnerEnv(dir, { "release-url": pathToFileURL(join(dir, "rel")).href });

  const { version, path } = await run(env);

  assert.equal(version, VERSION);
  assert.equal(path, join(env.RUNNER_TEMP, "spm", VERSION, HOST.rustTarget, HOST.exe));
  assert.equal(sha256(readFileSync(path)), sha256(readFileSync(spmBin())));
  assert.equal(exec(path, ["--version"]).stdout.trim(), `spm ${VERSION}`);
  assert.deepEqual(readOutputs(env.GITHUB_OUTPUT), { version: VERSION, path });
  assert.equal(readFileSync(env.GITHUB_PATH, "utf8").trim(), join(env.RUNNER_TEMP, "spm", VERSION, HOST.rustTarget));
});

test("downloads over HTTP following the release-asset redirect, with a v-prefixed version", async (t) => {
  const dir = tempDir(t);
  stageRelease(join(dir, "rel"), VERSION, { [HOST_ASSET]: spmBin() });
  const url = await serveDir(t, join(dir, "rel"));
  const env = runnerEnv(dir, { version: `v${VERSION}`, "release-url": `${url}/` });

  const { version, path } = await run(env);

  assert.equal(version, VERSION);
  assert.ok(existsSync(path));
});

test("a binary that does not match SHA256SUMS is rejected and not put on PATH", async (t) => {
  const dir = tempDir(t);
  stageRelease(join(dir, "rel"), VERSION, { [HOST_ASSET]: spmBin() }, { tamper: [HOST_ASSET] });
  const env = runnerEnv(dir, { "release-url": pathToFileURL(join(dir, "rel")).href });

  await assert.rejects(run(env), new RegExp(`checksum mismatch for ${HOST_ASSET}`));
  assert.ok(!existsSync(env.GITHUB_PATH));
  assert.ok(!existsSync(env.GITHUB_OUTPUT));
  assert.ok(!existsSync(join(env.RUNNER_TEMP, "spm")));
});

test("a release without SHA256SUMS (or an unknown version) is refused", async (t) => {
  const dir = tempDir(t);
  stageRelease(join(dir, "rel"), VERSION, { [HOST_ASSET]: spmBin() }, { sums: false });
  const releaseUrl = pathToFileURL(join(dir, "rel")).href;

  await assert.rejects(run(runnerEnv(dir, { "release-url": releaseUrl })), /has no SHA256SUMS asset.*refuses to install an unverified binary/s);
  await assert.rejects(run(runnerEnv(dir, { "release-url": releaseUrl, version: "9.9.9" })), /spm v9\.9\.9 has no SHA256SUMS/);
});

test("SHA256SUMS without an entry for the runner's asset is refused", async (t) => {
  const dir = tempDir(t);
  const other = TARGETS.find((x) => x !== HOST);
  stageRelease(join(dir, "rel"), VERSION, { [assetName(other)]: spmBin() });
  const env = runnerEnv(dir, { "release-url": pathToFileURL(join(dir, "rel")).href });

  await assert.rejects(run(env), new RegExp(`SHA256SUMS of spm v${VERSION} has no entry for ${HOST_ASSET}`));
});

test("a verified asset that reports another version is refused", async (t) => {
  const dir = tempDir(t);
  stageRelease(join(dir, "rel"), "9.9.9", { [HOST_ASSET]: spmBin() });
  const env = runnerEnv(dir, { version: "9.9.9", "release-url": pathToFileURL(join(dir, "rel")).href });

  await assert.rejects(run(env), new RegExp(`reports \`spm ${VERSION.replace(/\./g, "\\.")}\`, expected \`spm 9\\.9\\.9\``));
  assert.ok(!existsSync(env.GITHUB_PATH));
  assert.ok(!existsSync(join(env.RUNNER_TEMP, "spm", "9.9.9", HOST.rustTarget, HOST.exe)));
});

test("unsupported runners fail before downloading anything", async (t) => {
  const dir = tempDir(t);
  const env = runnerEnv(dir, { "release-url": "http://127.0.0.1:1/never" });
  await assert.rejects(run(env, { platform: "win32", arch: "arm64" }), /no spm release binary for win32\/arm64/);
});

test("HTTP errors other than 404 are reported as such", async (t) => {
  const url = await serveDir(t, tempDir(t), { status: 503 });
  await assert.rejects(fetchBytes(`${url}/v1.0.0/${CHECKSUMS}`), /HTTP 503/);
});

test("the action entry point reads runner inputs and exits non-zero with an annotation on failure", async (t) => {
  const dir = tempDir(t);
  stageRelease(join(dir, "rel"), VERSION, { [HOST_ASSET]: spmBin() });
  const entry = join(REPO_ROOT, "action", "setup.mjs");
  const env = { ...process.env, ...runnerEnv(dir, { "release-url": pathToFileURL(join(dir, "rel")).href }) };

  const ok = exec(process.execPath, [entry], { env });
  assert.match(ok.stdout, new RegExp(`spm ${VERSION.replace(/\./g, "\\.")} installed at`));
  assert.equal(readOutputs(env.GITHUB_OUTPUT).version, VERSION);

  const bad = exec(process.execPath, [entry], { env: { ...env, INPUT_VERSION: "nope" }, allowFail: true });
  assert.equal(bad.status, 1);
  assert.match(bad.stdout, /^::error::invalid spm version `nope`/m);
});
