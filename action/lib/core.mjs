// The few @actions/core primitives the spm actions need, dependency-free so
// the actions run straight from a checkout of this repo (no bundling, no
// node_modules). Every function takes the environment explicitly so tests can
// drive them without touching process.env.
import { appendFileSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { EOL } from "node:os";

/// An action input as the runner passes it: `INPUT_<NAME>` with the name
/// upper-cased and spaces (not hyphens) replaced by `_`. Trimmed; `""` when unset.
export function getInput(env, name) {
  return (env[`INPUT_${name.replace(/ /g, "_").toUpperCase()}`] ?? "").trim();
}

/// A boolean input (YAML 1.2 core schema spellings, like @actions/core).
export function getBooleanInput(env, name) {
  const v = getInput(env, name);
  if (["true", "True", "TRUE"].includes(v)) return true;
  if (["", "false", "False", "FALSE"].includes(v)) return false;
  throw new Error(`input \`${name}\` must be \`true\` or \`false\`, got \`${v}\``);
}

/// A list input: one entry per line or comma, blanks dropped.
export function getListInput(env, name) {
  return getInput(env, name)
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

/// Append a `name<<delimiter` record to a runner file command
/// (`GITHUB_OUTPUT`). A random delimiter makes multi-line and
/// attacker-influenced values unable to inject extra records.
function appendRecord(file, name, value) {
  const delimiter = `ghadelimiter_${randomUUID()}`;
  const text = String(value);
  if (text.includes(delimiter)) throw new Error(`value for ${name} contains the delimiter`);
  appendFileSync(file, `${name}<<${delimiter}${EOL}${text}${EOL}${delimiter}${EOL}`);
}

export function setOutput(env, name, value) {
  if (env.GITHUB_OUTPUT) appendRecord(env.GITHUB_OUTPUT, name, value);
}

/// Prepend `dir` to PATH for every later step of the job.
export function addPath(env, dir) {
  if (env.GITHUB_PATH) appendFileSync(env.GITHUB_PATH, `${dir}${EOL}`);
}

export function appendSummary(env, markdown) {
  if (env.GITHUB_STEP_SUMMARY) appendFileSync(env.GITHUB_STEP_SUMMARY, `${markdown}${EOL}`);
}

/// Workflow-command escaping for annotation messages (`%`, CR and LF).
function escapeData(s) {
  return String(s).replace(/%/g, "%25").replace(/\r/g, "%0D").replace(/\n/g, "%0A");
}

export const log = {
  info: (msg) => console.log(msg),
  notice: (msg) => console.log(`::notice::${escapeData(msg)}`),
  warning: (msg) => console.log(`::warning::${escapeData(msg)}`),
  error: (msg) => console.log(`::error::${escapeData(msg)}`),
  group: (title) => console.log(`::group::${escapeData(title)}`),
  endGroup: () => console.log("::endgroup::"),
};

/// Run an action's `run(env)` as the process entry point: any thrown error
/// becomes an `::error::` annotation and a non-zero exit.
export async function main(run) {
  try {
    await run(process.env);
  } catch (e) {
    log.error(e?.message ?? String(e));
    process.exitCode = 1;
  }
}
