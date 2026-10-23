// The distribution matrix: rust target -> npm (os, cpu) + binary filename.
// `os`/`cpu` must match Node's process.platform / process.arch so npm installs
// exactly the right optional dependency and the launcher can resolve it.
//
// Single source of truth for every consumer of the release binaries: the npm
// packages (npm/build.mjs) and the GitHub Action (action/lib/setup.mjs, action/stage-release.mjs).
// action/test/release.test.mjs fails if release.yml's build matrix drifts from it.
export const TARGETS = [
  { rustTarget: "x86_64-unknown-linux-gnu", os: "linux", cpu: "x64", exe: "spm" },
  { rustTarget: "aarch64-unknown-linux-gnu", os: "linux", cpu: "arm64", exe: "spm" },
  { rustTarget: "x86_64-apple-darwin", os: "darwin", cpu: "x64", exe: "spm" },
  { rustTarget: "aarch64-apple-darwin", os: "darwin", cpu: "arm64", exe: "spm" },
  { rustTarget: "x86_64-pc-windows-msvc", os: "win32", cpu: "x64", exe: "spm.exe" },
];

/// The GitHub Release asset name of a target's binary (`spm-<target>[.exe]`).
export function assetName(t) {
  return `spm-${t.rustTarget}${t.exe.endsWith(".exe") ? ".exe" : ""}`;
}
