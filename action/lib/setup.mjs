// `uses: camunda/spm-cli@<ref>` — download the spm release binary for the
// runner, verify it against the release's SHA256SUMS, and put it on PATH.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { chmodSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { TARGETS, assetName } from "../../npm/targets.mjs";
import { crateVersion } from "../../npm/crate.mjs";
import { addPath, getInput, log, setOutput } from "./core.mjs";

/// The checkout of this repository the runner downloaded for the action.
export const ACTION_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
export const DEFAULT_RELEASE_URL = "https://github.com/camunda/spm-cli/releases/download";
export const CHECKSUMS = "SHA256SUMS";

/// The release target for a Node `process.platform`/`process.arch` pair.
export function resolveTarget(platform, arch) {
  const t = TARGETS.find((t) => t.os === platform && t.cpu === arch);
  if (!t) {
    const supported = TARGETS.map((t) => `${t.os}/${t.cpu}`).join(", ");
    throw new Error(`no spm release binary for ${platform}/${arch} (supported: ${supported})`);
  }
  return t;
}

const SEMVER = /^v?(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?)$/;

/// The version to install, without a leading `v`. Empty input means "the
/// version of this action", read from the action's own Cargo.toml — the file
/// the release tag is verified against, so `@vX.Y.Z` installs X.Y.Z and the
/// moving `@v0` tag installs the release it currently points at.
export function resolveVersion(input, actionRoot = ACTION_ROOT) {
  const raw = input || crateVersion(actionRoot);
  const m = SEMVER.exec(raw);
  if (!m) throw new Error(`invalid spm version \`${raw}\`: expected X.Y.Z (optionally prefixed with v)`);
  return m[1];
}

/// Parse a `sha256sum`-format file (`<hex>  <name>` or `<hex> *<name>`) into
/// name -> lower-case hex. Malformed lines and conflicting duplicates are
/// errors: a checksum file we can't fully understand is not trusted.
export function parseChecksums(text) {
  const sums = new Map();
  for (const [i, raw] of text.split("\n").entries()) {
    const line = raw.replace(/\r$/, "");
    if (!line.trim()) continue;
    const m = /^([0-9a-fA-F]{64}) [ *](.+)$/.exec(line);
    if (!m) throw new Error(`${CHECKSUMS}: malformed line ${i + 1}: ${JSON.stringify(line)}`);
    const [, hex, name] = m;
    const prev = sums.get(name);
    if (prev && prev !== hex.toLowerCase()) {
      throw new Error(`${CHECKSUMS}: conflicting checksums for ${name}`);
    }
    sums.set(name, hex.toLowerCase());
  }
  return sums;
}

export function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

class NotFound extends Error {}

/// GET `url` as bytes. `file:` URLs read a local mirror directory (e.g. a
/// release copied onto an air-gapped runner); anything else goes over HTTP(S),
/// following GitHub's redirect to its asset CDN.
export async function fetchBytes(url) {
  if (url.startsWith("file:")) {
    try {
      return readFileSync(fileURLToPath(url));
    } catch (e) {
      if (e.code === "ENOENT") throw new NotFound(`not found: ${url}`);
      throw e;
    }
  }
  const res = await fetch(url, { redirect: "follow", headers: { "User-Agent": "camunda/spm-cli action" } });
  if (res.status === 404) throw new NotFound(`not found: ${url}`);
  if (!res.ok) throw new Error(`GET ${url}: HTTP ${res.status}`);
  return Buffer.from(await res.arrayBuffer());
}

export async function run(env, { platform = process.platform, arch = process.arch } = {}) {
  const version = resolveVersion(getInput(env, "version"));
  const base = (getInput(env, "release-url") || DEFAULT_RELEASE_URL).replace(/\/+$/, "");
  const target = resolveTarget(platform, arch);
  const asset = assetName(target);
  const releaseUrl = `${base}/v${version}`;
  log.info(`installing spm ${version} (${target.rustTarget}) from ${releaseUrl}`);

  let sums;
  try {
    sums = parseChecksums((await fetchBytes(`${releaseUrl}/${CHECKSUMS}`)).toString("utf8"));
  } catch (e) {
    if (e instanceof NotFound) {
      throw new Error(
        `spm v${version} has no ${CHECKSUMS} asset (${e.message}). Either the version does not ` +
          `exist or it predates checksum publishing; the action refuses to install an unverified binary.`,
      );
    }
    throw e;
  }
  const expected = sums.get(asset);
  if (!expected) throw new Error(`${CHECKSUMS} of spm v${version} has no entry for ${asset}`);

  const bytes = await fetchBytes(`${releaseUrl}/${asset}`);
  const actual = sha256(bytes);
  if (actual !== expected) {
    throw new Error(`checksum mismatch for ${asset}: expected ${expected}, got ${actual}`);
  }
  log.info(`verified ${asset} sha256 ${actual}`);

  const dir = join(env.RUNNER_TEMP || tmpdir(), "spm", version, target.rustTarget);
  mkdirSync(dir, { recursive: true });
  const bin = join(dir, target.exe);
  writeFileSync(bin, bytes);
  chmodSync(bin, 0o755);

  // The checksum proves the bytes match the release; this proves the release
  // asset is the version that was asked for (a mislabeled mirror would not be).
  const reported = execFileSync(bin, ["--version"], { encoding: "utf8" }).trim();
  if (reported !== `spm ${version}`) {
    rmSync(bin, { force: true });
    throw new Error(`${asset} reports \`${reported}\`, expected \`spm ${version}\``);
  }

  addPath(env, dir);
  setOutput(env, "version", version);
  setOutput(env, "path", bin);
  log.info(`${reported} installed at ${bin}`);
  return { version, path: bin };
}
