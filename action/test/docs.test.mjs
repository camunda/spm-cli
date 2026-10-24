// Drift guards for the CI docs: the snippets users copy must keep working.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { git, readRepoText, makeRepo, run, skillMd, spmBin, tempDir } from "./helpers.mjs";

const read = readRepoText;
const DOCS = [["docs", "guide", "ci.md"], ["README.md"], ["docs", "getting-started", "installation.md"]];

test("the documented Renovate regex finds exactly the tag-pinned entries spm writes", (t) => {
  const block = /```json\n(\{\n\s*"customManagers"[\s\S]*?)\n```/.exec(read("docs", "guide", "ci.md"));
  assert.ok(block, "docs/guide/ci.md has the Renovate config");
  const [manager] = JSON.parse(block[1]).customManagers;
  assert.ok(manager.fileMatch.some((f) => new RegExp(f).test("tools/ai.json")));
  assert.ok(manager.fileMatch.every((f) => !new RegExp(f).test("ai.lock")));

  const dir = tempDir(t);
  const url = makeRepo(join(dir, "up"), {
    "SKILL.md": skillMd("up"),
    "pkg/.claude-plugin/plugin.json": JSON.stringify({ name: "kit", version: "1.0.0", skills: "./skills/" }),
    "pkg/skills/helper/SKILL.md": skillMd("helper"),
  });
  git(join(dir, "up"), "tag", "v1.0.0");
  const env = { ...process.env, SPM_HOME: join(dir, "home") };
  const spm = (...args) => run(spmBin(), args, { cwd: join(dir, "project"), env });
  run("git", ["init", "-q", join(dir, "project")]);
  spm("init", "--target", "copilot");
  spm("add", url, "--tag", "v1.0.0", "--name", "tagged");
  spm("add", url, "--branch", "main", "--name", "tracked");
  spm("add", url, "--version", "^1", "--name", "ranged");
  spm("add", url, "--tag", "v1.0.0", "--path", "pkg", "--plugin", "--name", "kit");

  const manifest = readFileSync(join(dir, "project", "ai.json"), "utf8");
  const found = [...manifest.matchAll(new RegExp(manager.matchStrings[0], "g"))].map((m) => [m.groups.packageName, m.groups.currentValue]);
  assert.deepEqual(found, [
    [url, "v1.0.0"],
    [url, "v1.0.0"],
  ]);
});

test("documented workflow snippets only pass inputs the actions declare", () => {
  const inputs = (yml) => [.../^inputs:\n((?:(?: {2}.*)?\n)*)/m.exec(read(...yml))[1].matchAll(/^ {2}([\w-]+):/gm)].map((m) => m[1]);
  const known = { "": inputs(["action.yml"]), "/update": inputs(["update", "action.yml"]) };
  let checked = 0;
  for (const doc of DOCS) {
    const lines = read(...doc).split("\n");
    lines.forEach((line, i) => {
      const m = /^(\s*)(?:- )?uses: camunda\/spm-cli(\/update)?@(\S+)/.exec(line);
      if (!m) return;
      assert.equal(m[3], "v0", `${doc.join("/")}:${i + 1} uses the moving major tag`);
      checked++;
      const indent = m[1].length + (line.trimStart().startsWith("- ") ? 2 : 0);
      if (lines[i + 1]?.trim() !== "with:") return;
      for (let j = i + 2; j < lines.length; j++) {
        const w = /^(\s*)([\w-]+):/.exec(lines[j]);
        if (!w || w[1].length <= indent + 2 - 1) break;
        assert.ok(known[m[2] ?? ""].includes(w[2]), `${doc.join("/")}:${j + 1}: unknown input \`${w[2]}\``);
      }
    });
  }
  assert.ok(checked >= 6, `found ${checked} action usages in the docs`);
});

test("every documented update workflow serializes its runs without cancelling one mid-publication", () => {
  let checked = 0;
  for (const doc of DOCS) {
    for (const [, block] of read(...doc).matchAll(/```ya?ml\n([\s\S]*?)```/g)) {
      // Complete workflows only; step fragments inherit the workflow's settings.
      if (!/^jobs:/m.test(block) || !/uses: camunda\/spm-cli\/update@/.test(block)) continue;
      checked++;
      const where = `${doc.join("/")}: workflow using the update action`;
      const group = /^concurrency:(.*(?:\n {2}.*)*)/m.exec(block);
      assert.ok(group, `${where} has no \`concurrency\` group`);
      assert.match(group[1], /group:/, where);
      assert.match(group[1], /cancel-in-progress: false/, where);
    }
  }
  assert.ok(checked >= 2, `found ${checked} update workflows in the docs`);
});
