#!/usr/bin/env node
"use strict";

// Launcher for the `spm` command distributed via npm.
//
// The npm package `@camunda8/spm` ships no binary itself. Instead it declares
// the per-platform packages (`@camunda8/spm-<os>-<cpu>`) as optionalDependencies
// with matching `os`/`cpu` fields, so npm installs only the one that fits the
// host. This script resolves that package's prebuilt binary and execs it,
// forwarding argv, stdio, and the exit code — giving `spm` on PATH with no
// postinstall download and no extra setup.

const { spawnSync } = require("node:child_process");
const { existsSync } = require("node:fs");

function resolveBinary() {
  const platform = process.platform; // 'darwin' | 'linux' | 'win32' | ...
  const arch = process.arch; // 'x64' | 'arm64' | ...
  const pkg = `@camunda8/spm-${platform}-${arch}`;
  const exe = platform === "win32" ? "spm.exe" : "spm";
  try {
    // Resolve through Node's normal algorithm so it works whether installed
    // globally (npm i -g) or as a local dependency.
    const path = require.resolve(`${pkg}/bin/${exe}`);
    return existsSync(path) ? path : null;
  } catch {
    return null;
  }
}

function fail(message) {
  process.stderr.write(`[spm] ${message}\n`);
  process.exit(1);
}

const binary = resolveBinary();
if (!binary) {
  fail(
    `no prebuilt binary found for ${process.platform}-${process.arch}.\n` +
      `The optional dependency @camunda8/spm-${process.platform}-${process.arch} ` +
      `is missing or your platform is unsupported.\n` +
      `Supported platforms: darwin-x64, darwin-arm64, linux-x64, linux-arm64, win32-x64.\n` +
      `Try reinstalling: npm i -g @camunda8/spm`,
  );
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  fail(`failed to launch binary: ${result.error.message}`);
}

// Re-raise a terminating signal so callers (and shells) observe it faithfully;
// otherwise propagate the child's exit code.
if (result.signal) {
  process.kill(process.pid, result.signal);
}
process.exit(result.status === null ? 1 : result.status);
