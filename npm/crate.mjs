import { readFileSync } from "node:fs";
import { join } from "node:path";

/// The `[package]` version in `<root>/Cargo.toml` — the single source of truth
/// for every release channel (npm packages, the GitHub Action's default
/// version, the release tag).
export function crateVersion(root) {
  const cargo = readFileSync(join(root, "Cargo.toml"), "utf8");
  const m = cargo.match(/\[package\][\s\S]*?\bversion\s*=\s*"([^"]+)"/);
  if (!m) throw new Error("could not read version from Cargo.toml");
  return m[1];
}
