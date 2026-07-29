#!/usr/bin/env node
"use strict";

// Generates the publishable npm packages for spm from prebuilt binaries.
//
// One root launcher package (`@camunda8/spm`) plus one package per platform
// (`@camunda8/spm-<os>-<cpu>`) carrying the native binary. Every manifest is
// derived here from a single config, so there is no drift between them or with
// the crate version.
//
// Usage:
//   node npm/build.mjs --bin-dir <dir> [--version X.Y.Z] [--out <dir>]
//
// --bin-dir  Directory containing the built binaries, one per target, laid out
//            as <bin-dir>/<rust-target>/spm[.exe] (the layout produced by the
//            release workflow's uploaded artifacts).
// --version  Package version. Defaults to the crate version in Cargo.toml.
// --out      Output directory for the generated packages. Defaults to npm/dist.

import { readFileSync, writeFileSync, mkdirSync, copyFileSync, chmodSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(__dirname, "..");

const SCOPE = "@camunda8";
const ROOT_PKG = `${SCOPE}/spm`;

// Shared metadata, kept in one place so the manifests can't drift.
const COMMON = {
  license: "Apache-2.0",
  homepage: "https://github.com/camunda/spm-cli#readme",
  repository: { type: "git", url: "git+https://github.com/camunda/spm-cli.git" },
  bugs: { url: "https://github.com/camunda/spm-cli/issues" },
  author: "Camunda",
  publishConfig: { access: "public" },
};

// The distribution matrix: rust target -> npm (os, cpu) + binary filename.
// `os`/`cpu` must match Node's process.platform / process.arch so npm installs
// exactly the right optional dependency and the launcher can resolve it.
const TARGETS = [
  { rustTarget: "x86_64-unknown-linux-gnu", os: "linux", cpu: "x64", exe: "spm" },
  { rustTarget: "aarch64-unknown-linux-gnu", os: "linux", cpu: "arm64", exe: "spm" },
  { rustTarget: "x86_64-apple-darwin", os: "darwin", cpu: "x64", exe: "spm" },
  { rustTarget: "aarch64-apple-darwin", os: "darwin", cpu: "arm64", exe: "spm" },
  { rustTarget: "x86_64-pc-windows-msvc", os: "win32", cpu: "x64", exe: "spm.exe" },
];

function parseArgs(argv) {
  const args = { out: join(repoRoot, "npm", "dist") };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === "--bin-dir") args.binDir = argv[++i];
    else if (a === "--version") args.version = argv[++i];
    else if (a === "--out") args.out = argv[++i];
    else throw new Error(`unknown argument: ${a}`);
  }
  return args;
}

function crateVersion() {
  const cargo = readFileSync(join(repoRoot, "Cargo.toml"), "utf8");
  const m = cargo.match(/\[package\][\s\S]*?\bversion\s*=\s*"([^"]+)"/);
  if (!m) throw new Error("could not read version from Cargo.toml");
  return m[1];
}

function platformPkgName(t) {
  return `${SCOPE}/spm-${t.os}-${t.cpu}`;
}

function writeJson(path, value) {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(value, null, 2) + "\n");
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  if (!args.binDir) throw new Error("--bin-dir is required");
  const version = args.version ?? crateVersion();
  const binDir = resolve(args.binDir);
  const out = resolve(args.out);

  rmSync(out, { recursive: true, force: true });

  const optionalDependencies = {};

  // Platform packages: each carries one native binary and is gated by os/cpu.
  for (const t of TARGETS) {
    const name = platformPkgName(t);
    optionalDependencies[name] = version;

    const pkgDir = join(out, name);
    const src = join(binDir, t.rustTarget, t.exe);
    const dst = join(pkgDir, "bin", t.exe);
    mkdirSync(dirname(dst), { recursive: true });
    copyFileSync(src, dst);
    if (t.os !== "win32") chmodSync(dst, 0o755);

    writeJson(join(pkgDir, "package.json"), {
      name,
      version,
      description: `Prebuilt spm binary for ${t.os}-${t.cpu}.`,
      ...COMMON,
      os: [t.os],
      cpu: [t.cpu],
      files: [`bin/${t.exe}`],
    });
    console.log(`  built ${name}@${version} (${t.rustTarget})`);
  }

  // Root launcher package: no binary, just the JS shim + optionalDependencies.
  const rootDir = join(out, ROOT_PKG);
  mkdirSync(join(rootDir, "bin"), { recursive: true });
  copyFileSync(join(__dirname, "launcher", "spm.js"), join(rootDir, "bin", "spm.js"));
  chmodSync(join(rootDir, "bin", "spm.js"), 0o755);
  for (const f of ["README.md", "LICENSE"]) {
    copyFileSync(join(repoRoot, f), join(rootDir, f));
  }

  writeJson(join(rootDir, "package.json"), {
    name: ROOT_PKG,
    version,
    description:
      "Skill package manager (spm): declare AI skills in ai.json and materialize them for Claude/Copilot.",
    ...COMMON,
    keywords: ["ai", "skills", "claude", "copilot", "package-manager", "cli"],
    bin: { spm: "bin/spm.js" },
    files: ["bin/spm.js"],
    engines: { node: ">=18" },
    // The launcher execs one of these; npm installs only the matching os/cpu.
    optionalDependencies,
  });
  console.log(`  built ${ROOT_PKG}@${version} (launcher)`);

  console.log(`\nGenerated ${TARGETS.length + 1} packages in ${out}`);
}

main();
