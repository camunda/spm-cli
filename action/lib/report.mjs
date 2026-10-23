// Pure helpers of the update action: what to do for an outdated row, how to
// name its branch, and how to describe it in a pull request.
import { createHash } from "node:crypto";

export const BRANCH_PREFIX = "spm/update-";
const MARKER = "spm-update";
const BODY_LIMIT = 60000; // GitHub rejects PR bodies over 65536 characters.
const FINDINGS_LIMIT = 50;

/// What the update action does for one `spm outdated --json` row:
/// - `retag`: a tag selector with a newer tag — re-pin to it (`spm add --force`);
/// - `update`: a moved branch/tag or a newer tag inside a `version` range —
///   re-resolve the selector (`spm update <name>`);
/// - `skip`: nothing this project's ai.json can do, with the reason.
export function planFor(row) {
  if (!row.outdated) return { action: "skip", reason: "up to date" };
  if (row.transitive) {
    return {
      action: "skip",
      reason: `transitive (requested by ${row.requestedBy.join(", ")}): the upstream ai.json that requests it must bump it`,
    };
  }
  const sel = row.selector ?? {};
  if (sel.tag !== undefined && row.latestTag) return { action: "retag", tag: row.latestTag };
  if (sel.version !== undefined && row.status === "newer-tag") {
    return {
      action: "skip",
      reason: `newer tag ${row.latestTag} is outside the version range \`${sel.version}\`: widen it in ai.json`,
    };
  }
  return { action: "update" };
}

/// `spm/update-<name>`, or for a name git can't take verbatim as a ref
/// component, a sanitized slug plus a hash of the real name so two different
/// names can never share a branch.
export function branchFor(name) {
  const plain =
    /^[A-Za-z0-9][A-Za-z0-9._-]*$/.test(name) &&
    !name.includes("..") &&
    !name.endsWith(".lock") &&
    !name.endsWith(".");
  if (plain) return `${BRANCH_PREFIX}${name}`;
  const slug =
    name
      .replace(/[^A-Za-z0-9._-]+/g, "-")
      .replace(/\.{2,}/g, ".")
      .replace(/^[-.]+|[-.]+$/g, "")
      .slice(0, 50) || "dependency";
  const hash = createHash("sha256").update(name).digest("hex").slice(0, 8);
  return `${BRANCH_PREFIX}${slug}-${hash}`;
}

/// The upstream compare view for a GitHub-hosted source (github.com or this
/// workflow's own server), `null` for any other host.
export function compareUrl(git, from, to, serverUrl) {
  if (!from || !to || from === to) return null;
  const hosts = new Set(["github.com"]);
  if (serverUrl) {
    try {
      hosts.add(new URL(serverUrl).host.toLowerCase());
    } catch {
      // An unparsable server URL just isn't a known GitHub host.
    }
  }
  const m =
    /^(?:(?:https?|ssh|git):\/\/(?:[^@/]+@)?([^/:]+)(?::\d+)?\/|[^@/\s]+@([^/:]+):)([^/]+)\/([^/]+?)(?:\.git)?\/?$/.exec(
      git,
    );
  if (!m) return null;
  const host = (m[1] ?? m[2]).toLowerCase();
  if (!hosts.has(host)) return null;
  return `https://${host}/${m[3]}/${m[4]}/compare/${from}...${to}`;
}

/// `kind:value` from an ai.lock `reference` (`tag:v1`, `branch:main`, …).
function splitReference(reference) {
  const i = (reference ?? "").indexOf(":");
  return i < 0 ? { kind: "?", value: reference ?? "" } : { kind: reference.slice(0, i), value: reference.slice(i + 1) };
}

/// A human label for a locked entry's selector: `tag v1.2.0`,
/// `branch main`, `version ^1.2 → v1.4.0`, `commit 0123abcd`.
export function describeSelector(entry) {
  const { kind, value } = splitReference(entry.reference);
  if (kind === "commit") return `commit ${value.slice(0, 8)}`;
  if (kind === "version" && entry.resolved_tag) return `version ${value} → ${entry.resolved_tag}`;
  return `${kind} ${value}`;
}

/// The short "to" version in a PR title: the tag when there is one, else the
/// branch and commit.
export function shortVersion(entry) {
  const { kind, value } = splitReference(entry.reference);
  if (kind === "tag") return value;
  if (entry.resolved_tag) return entry.resolved_tag;
  if (kind === "branch") return `${value}@${entry.commit.slice(0, 8)}`;
  return entry.commit.slice(0, 8);
}

/// Lock entries that differ between two ai.lock documents (added, removed or
/// re-pinned), skills and plugins alike.
export function diffLocks(before, after) {
  const changes = [];
  for (const [map, kind] of [
    ["skills", "skill"],
    ["plugins", "plugin"],
  ]) {
    const a = before?.[map] ?? {};
    const b = after?.[map] ?? {};
    for (const name of [...new Set([...Object.keys(a), ...Object.keys(b)])].sort()) {
      const from = a[name] ?? null;
      const to = b[name] ?? null;
      const same =
        from &&
        to &&
        from.commit === to.commit &&
        from.reference === to.reference &&
        (from.resolved_tag ?? null) === (to.resolved_tag ?? null);
      if (!same) changes.push({ name, kind, from, to });
    }
  }
  return changes;
}

function cell(text) {
  return String(text ?? "").replace(/\r?\n/g, " ").replace(/\|/g, "\\|");
}

function code(text) {
  return `\`${String(text ?? "").replace(/`/g, "'").replace(/\r?\n/g, " ")}\``;
}

/// The hidden marker identifying the PR's dependency and target commit, so a
/// later run can tell its own PRs (and declined ones) apart.
export function marker(name, commit) {
  return `<!-- ${MARKER} ${JSON.stringify({ name, commit })} -->`;
}

export function parseMarker(body) {
  const m = new RegExp(`<!-- ${MARKER} (\\{.*?\\}) -->`).exec(body ?? "");
  if (!m) return null;
  try {
    return JSON.parse(m[1]);
  } catch {
    return null;
  }
}

/// Total blocking findings across scans; a scan that failed to run counts as
/// blocking, so an unscanned update is never presented as clean.
export function blockingCount(scans) {
  return scans.reduce((n, s) => n + (s.error ? 1 : (s.summary?.blocking ?? 0)), 0);
}

function renderScans(scans) {
  const lines = ["### Content scan", ""];
  if (scans.length === 0) {
    lines.push("Nothing new to scan (no lock entry gained new content).");
    return lines;
  }
  const blocking = blockingCount(scans);
  const total = scans.reduce((n, s) => n + (s.findings?.length ?? 0), 0);
  lines.push(
    blocking > 0
      ? `> [!CAUTION]\n> **${blocking} blocking** finding(s) (high/critical). Review the upstream change before merging: \`spm install\` refuses this content unless \`SPM_ALLOW_SUSPICIOUS=1\` is set.`
      : total > 0
        ? `No blocking findings; ${total} non-blocking finding(s) below.`
        : "No findings.",
    "",
  );
  for (const s of scans) {
    if (s.error) {
      lines.push(`- **${cell(s.name)}**: scan failed — ${cell(s.error)}`);
      continue;
    }
    const findings = s.findings ?? [];
    lines.push(`- **${cell(s.name)}**: ${findings.length} finding(s), ${s.summary?.blocking ?? 0} blocking`);
  }
  const rows = scans.flatMap((s) => (s.findings ?? []).map((f) => ({ ...f, entry: s.name })));
  if (rows.length > 0) {
    lines.push("", "| Severity | Rule | Category | Location | Detail |", "| --- | --- | --- | --- | --- |");
    for (const f of rows.slice(0, FINDINGS_LIMIT)) {
      const loc = `${f.entry}/${f.path}${f.line ? `:${f.line}` : ""}`;
      const sev = f.blocking ? `**${f.severity}**` : f.severity;
      lines.push(`| ${sev} | ${code(f.rule)} | ${cell(f.category)} | ${code(loc)} | ${cell(f.detail)} |`);
    }
    if (rows.length > FINDINGS_LIMIT) {
      lines.push("", `…and ${rows.length - FINDINGS_LIMIT} more; run \`spm scan\` on the branch for the full list.`);
    }
  }
  return lines;
}

/// The pull request body for one dependency update.
export function renderBody({ name, kind, git, path, from, to, changes, scans, serverUrl, branch }) {
  const link = compareUrl(git, from?.commit, to.commit, serverUrl);
  const lines = [
    `Updates the ${kind} **${cell(name)}** declared in \`ai.json\`.`,
    "",
    "| | Selector | Commit |",
    "| --- | --- | --- |",
    `| **From** | ${from ? code(describeSelector(from)) : "_(not locked)_"} | ${from ? code(from.commit) : ""} |`,
    `| **To** | ${code(describeSelector(to))} | ${code(to.commit)} |`,
    "",
    `Source: ${code(git)}${path ? ` (path ${code(path)})` : ""}`,
  ];
  if (link) lines.push("", `Upstream changes: ${link}`);
  if (from && splitReference(from.reference).kind === "tag" && from.reference === to.reference) {
    lines.push(
      "",
      `> [!WARNING]\n> The tag ${code(splitReference(to.reference).value)} was **re-pointed** upstream: it now names a different commit than the one locked. Make sure the move is legitimate before merging.`,
    );
  }
  const others = changes.filter((c) => !(c.name === name && c.kind === kind));
  if (others.length > 0) {
    lines.push("", "### Other lock changes", "", "| Dependency | From | To |", "| --- | --- | --- |");
    for (const c of others) {
      const side = (e) => (e ? `${code(describeSelector(e))} ${code(e.commit.slice(0, 8))}` : "_(none)_");
      lines.push(`| ${cell(c.kind)} ${code(c.name)} | ${side(c.from)} | ${side(c.to)} |`);
    }
  }
  lines.push("", ...renderScans(scans));
  lines.push(
    "",
    "---",
    `Opened by the [spm update action](https://camunda.github.io/spm-cli/guide/ci). ` +
      `To review locally: \`git fetch origin ${branch} && git checkout ${branch} && spm install --locked && spm scan\`. ` +
      "Closing this PR without merging skips this version; a newer one opens a new PR.",
    marker(name, to.commit),
  );
  let body = lines.join("\n");
  if (body.length > BODY_LIMIT) {
    const tail = `\n\n_(truncated)_\n${marker(name, to.commit)}`;
    body = body.slice(0, BODY_LIMIT - tail.length) + tail;
  }
  return body;
}
