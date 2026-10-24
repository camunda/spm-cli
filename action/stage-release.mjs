// Lay out release assets the way the setup action downloads them: one
// `spm-<target>[.exe]` per target plus a `SHA256SUMS` over all of them.
//
//   node action/stage-release.mjs --bin-dir <dir> --out <dir> [--host]
//
// --bin-dir  Built binaries as <bin-dir>/<rust-target>/spm[.exe] (the layout of
//            release.yml's build artifacts, shared with npm/build.mjs).
// --out      Where to write the assets and SHA256SUMS.
// --host     Stage only this machine's target (CI's local release mirror)
//            instead of requiring every target.
//
// release.yml uploads the resulting SHA256SUMS to the GitHub Release; ci.yml
// uses --host to stage a file:// mirror for the action's end-to-end test.
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { TARGETS, assetName } from "../npm/targets.mjs";
import { CHECKSUMS, resolveTarget, sha256 } from "./lib/setup.mjs";

export function stageRelease({ binDir, outDir, targets = TARGETS }) {
  mkdirSync(outDir, { recursive: true });
  const lines = targets.map((t) => {
    const src = join(binDir, t.rustTarget, t.exe);
    if (!existsSync(src)) throw new Error(`missing binary for ${t.rustTarget}: ${src}`);
    const name = assetName(t);
    copyFileSync(src, join(outDir, name));
    return `${sha256(readFileSync(join(outDir, name)))}  ${name}`;
  });
  writeFileSync(join(outDir, CHECKSUMS), `${lines.join("\n")}\n`);
  return lines;
}

function parseArgs(argv) {
  const args = { host: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--bin-dir") args.binDir = argv[++i];
    else if (a === "--out") args.outDir = argv[++i];
    else if (a === "--host") args.host = true;
    else throw new Error(`unknown argument: ${a}`);
  }
  if (!args.binDir || !args.outDir) throw new Error("usage: stage-release.mjs --bin-dir <dir> --out <dir> [--host]");
  return args;
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  const args = parseArgs(process.argv.slice(2));
  const lines = stageRelease({
    binDir: resolve(args.binDir),
    outDir: resolve(args.outDir),
    targets: args.host ? [resolveTarget(process.platform, process.arch)] : TARGETS,
  });
  process.stdout.write(`${lines.join("\n")}\n`);
}
