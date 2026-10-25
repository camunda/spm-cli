//! `spm doctor`: a read-only checklist of everything that commonly breaks an
//! spm setup — each check reports ok, warn or error with a one-line fix. Every
//! verdict reuses the canonical rule (the `install --locked` drift check, the
//! source policy, `status`'s materialization report, the vendors' `.gitignore`
//! entries), so doctor can never disagree with the command it points at. It
//! never writes `ai.json`, `ai.lock`, links, the store or any target, and
//! contacts remotes only with `--online`.

use super::common::{render_report, scope_header};
use super::index::DependencyIndex;
use super::session::{Access, ScopeSession};
use super::verdict::{Finding, Severity};
use super::{status, sync};
use crate::git::{self, Reach};
use crate::ids::DepName;
use crate::links::Links;
use crate::lockfile::Lockfile;
use crate::manifest::Manifest;
use crate::policy::Policy;
use crate::scope::Scope;
use crate::{gitignore, paths, scan, skillcheck, vendor};
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Level {
    Ok,
    Warn,
    Error,
}

/// One diagnosis: what was checked, how it went, and how to fix it.
#[derive(Serialize)]
struct Check {
    /// Stable identifier for scripts (`git`, `manifest`, `lockfile`, …).
    id: &'static str,
    status: Level,
    message: String,
    /// One line per affected item.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    details: Vec<String>,
    /// The one-line remedy (absent when there is nothing to fix).
    #[serde(skip_serializing_if = "Option::is_none")]
    fix: Option<String>,
}

impl Check {
    fn new(id: &'static str, status: Level, message: impl Into<String>) -> Self {
        Check {
            id,
            status,
            message: message.into(),
            details: Vec::new(),
            fix: None,
        }
    }
    fn ok(id: &'static str, message: impl Into<String>) -> Self {
        Self::new(id, Level::Ok, message)
    }
    fn warn(id: &'static str, message: impl Into<String>) -> Self {
        Self::new(id, Level::Warn, message)
    }
    fn error(id: &'static str, message: impl Into<String>) -> Self {
        Self::new(id, Level::Error, message)
    }
    fn details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
    fn from_finding(id: &'static str, f: Finding) -> Self {
        let status = match f.severity {
            Severity::Ok => Level::Ok,
            Severity::Warn => Level::Warn,
            Severity::Error => Level::Error,
        };
        Check {
            id,
            status,
            message: f.message,
            details: f.details,
            fix: f.fix,
        }
    }
}

#[derive(Serialize)]
struct DoctorReport {
    scope: &'static str,
    /// Directory holding this scope's `ai.json`/`ai.lock`.
    #[serde(serialize_with = "crate::jsonutil::serialize_path")]
    root: PathBuf,
    /// Whether remotes were contacted (`--online`).
    online: bool,
    /// No check is an error (the exit status).
    ok: bool,
    errors: usize,
    warnings: usize,
    checks: Vec<Check>,
}

pub(super) fn doctor(scope: &Scope, online: bool, json: bool) -> Result<()> {
    let root = scope.manifest_dir()?;
    let checks = diagnose(scope, &root, online);
    let count = |l: Level| checks.iter().filter(|c| c.status == l).count();
    let report = DoctorReport {
        scope: scope.label(),
        root,
        online,
        ok: count(Level::Error) == 0,
        errors: count(Level::Error),
        warnings: count(Level::Warn),
        checks,
    };
    render_report(&report, json, render_text)?;
    match report.errors {
        0 => Ok(()),
        1 => bail!("spm doctor found 1 problem"),
        n => bail!("spm doctor found {n} problems"),
    }
}

fn render_text(report: &DoctorReport) {
    println!("{}", scope_header(report.scope, &report.root));
    for c in &report.checks {
        let mark = match c.status {
            Level::Ok => "✓",
            Level::Warn => "!",
            Level::Error => "✗",
        };
        println!("{mark} {}", c.message);
        for d in &c.details {
            println!("    {d}");
        }
        if let Some(fix) = &c.fix {
            println!("    fix: {fix}");
        }
    }
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    println!(
        "{}, {}",
        plural(report.errors, "error"),
        plural(report.warnings, "warning")
    );
}

/// Run every check. Each one is self-contained: a failure becomes its verdict,
/// never an early return, so one problem cannot hide the next.
fn diagnose(scope: &Scope, root: &Path, online: bool) -> Vec<Check> {
    let mut checks = vec![check_git(), check_store()];
    if scan::allow_suspicious() {
        checks.push(
            Check::warn(
                "scan",
                format!(
                    "{}=1: suspicious skill content is only warned about, not blocked",
                    scan::ALLOW_ENV
                ),
            )
            .fix(format!("unset {}", scan::ALLOW_ENV)),
        );
    }

    // Taken once, before reading ai.json, like every command that reads link
    // state (shared machine-wide: doctor activates no link). Without it a
    // concurrent command may be rewriting that state, so only the checks that
    // read none of it still run.
    let session = match ScopeSession::open(scope, Access::Write) {
        Ok(session) => session,
        Err(e) => {
            checks.push(Check::error(
                "state-lock",
                format!("can't lock spm state: {e:#}"),
            ));
            checks.push(check_machine_policy());
            return checks;
        }
    };

    if !Manifest::exists(root) {
        checks.push(
            Check::error("manifest", format!("no ai.json in {}", root.display()))
                .fix(scope.init_command()),
        );
        checks.push(check_machine_policy());
        return checks;
    }
    let manifest = match session.manifest() {
        Ok(m) => m,
        Err(e) => {
            checks.push(
                Check::error("manifest", format!("ai.json is invalid: {e:#}")).fix(
                    "correct ai.json (see https://camunda.github.io/spm-cli/reference/ai-json)",
                ),
            );
            checks.push(check_machine_policy());
            return checks;
        }
    };
    checks.push(Check::ok("manifest", "ai.json is valid"));
    // Every git access gates on the active policy, as in any other command; a
    // policy that won't load is reported below and leaves remotes unprobed.
    let policy_active = manifest.policy(root).map(crate::policy::activate).is_ok();
    checks.push(check_targets(scope, &manifest));
    checks.push(check_machine_policy());

    // The rest of the state, each part read once (in the canonical order)
    // and reported on by every check that can run with what did load.
    let lock = session.lockfile();
    let links = session.links();
    if let Ok(links) = &links {
        checks.push(check_policy(root, &manifest, links, lock.as_ref().ok()));
    }
    checks.extend(check_links(scope, &manifest, links.as_ref()));
    // Snapshot the remotes under the lock; they are probed once it's released.
    let remotes = (online && policy_active).then(|| remote_urls(&manifest, lock.as_ref().ok()));
    let links_error = links.as_ref().err().map(|e| anyhow!("{e:#}"));
    let state = session.snapshot_from(manifest.clone(), lock, links);
    checks.extend(
        sync::check_lock(&session, &manifest, links_error.as_ref(), state.as_ref())
            .findings(scope)
            .into_iter()
            .map(|f| Check::from_finding("lockfile", f)),
    );

    match state.and_then(|state| status::collect(&session, &state)) {
        Ok(report) => {
            checks.extend(materialization_checks(scope, &report));
            if !scope.is_global() {
                checks.extend(check_gitignore(scope, root, &manifest, &report));
            }
            checks.extend(check_loadable(scope, &report));
        }
        Err(e) => checks.push(Check::error(
            "materialized",
            format!("can't inspect the targets: {e:#}"),
        )),
    }
    // Release the lock before the slow network probes so a concurrent install
    // isn't blocked on them.
    drop(session);

    if let Some(c) = check_ssh_agent(&manifest) {
        checks.push(c);
    }
    if let Some(urls) = remotes {
        checks.extend(check_remotes(&urls));
    }
    checks
}

fn check_git() -> Check {
    match git::version() {
        Ok(v) => Check::ok("git", v),
        Err(e) => Check::error("git", format!("git can't be run: {e:#}"))
            .fix("install git and make sure it is on PATH"),
    }
}

/// The store root must be writable once `spm install` needs it. A missing
/// store is fine if install can create it; doctor never creates or writes it.
fn check_store() -> Check {
    let store = match paths::store_dir() {
        Ok(s) => s,
        Err(e) => return Check::error("store", format!("can't locate the store: {e:#}")),
    };
    let from = if std::env::var_os("SPM_HOME").is_some_and(|v| !v.is_empty()) {
        " (SPM_HOME)"
    } else {
        ""
    };
    if !store.exists() {
        // `spm install` creates it, if it can create it in its nearest
        // occupied ancestor (the store path itself may be occupied too).
        // A relative store's last ancestor is "", i.e. the working directory.
        let occupied = store
            .ancestors()
            .map(|a| {
                if a.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    a
                }
            })
            .find(|a| std::fs::symlink_metadata(a).is_ok());
        if let Some(parent) = occupied {
            if !parent.exists() {
                // Only a dangling symlink occupies a path that doesn't exist.
                return Check::error(
                    "store",
                    format!(
                        "store {}{from} can't be created: {} is a symlink that doesn't resolve",
                        store.display(),
                        parent.display()
                    ),
                )
                .fix(format!(
                    "remove {} or point SPM_HOME elsewhere",
                    parent.display()
                ));
            }
            if !parent.is_dir() {
                return Check::error(
                    "store",
                    format!(
                        "store {}{from} can't be created: {} is not a directory",
                        store.display(),
                        parent.display()
                    ),
                )
                .fix(format!(
                    "remove {} or point SPM_HOME elsewhere",
                    parent.display()
                ));
            }
            if let Ok(Some(false)) = crate::fsutil::can_create_in(parent) {
                return Check::error(
                    "store",
                    format!(
                        "store {}{from} can't be created: {} is not writable by you",
                        store.display(),
                        parent.display()
                    ),
                )
                .fix(format!(
                    "make {} writable or point SPM_HOME elsewhere",
                    parent.display()
                ));
            }
        }
        return Check::ok(
            "store",
            format!(
                "store {}{from} not created yet (spm install creates it)",
                store.display()
            ),
        );
    }
    if !store.is_dir() {
        return Check::error(
            "store",
            format!("store {}{from} is not a directory", store.display()),
        )
        .fix(format!(
            "remove {} or point SPM_HOME elsewhere",
            store.display()
        ));
    }
    // Asked of the OS for the invoking user: doctor never writes into the store.
    match crate::fsutil::can_create_in(&store) {
        Ok(Some(true)) => Check::ok(
            "store",
            format!("store {}{from} is writable", store.display()),
        ),
        Ok(Some(false)) => Check::error(
            "store",
            format!("store {}{from} is not writable by you", store.display()),
        )
        .fix(format!(
            "make {} writable or point SPM_HOME elsewhere",
            store.display()
        )),
        Ok(None) => Check::ok(
            "store",
            format!(
                "store {}{from} exists (write access is not checked on this platform)",
                store.display()
            ),
        ),
        Err(e) => Check::error(
            "store",
            format!("store {}{from} can't be checked: {e}", store.display()),
        ),
    }
}

fn check_targets(scope: &Scope, manifest: &Manifest) -> Check {
    let g = if scope.is_global() { " -g" } else { "" };
    if manifest.targets.is_empty() {
        return Check::error("targets", "ai.json declares no targets")
            .fix(format!("spm target add <target>{g}"));
    }
    let unknown: Vec<String> = manifest
        .targets
        .iter()
        .filter_map(|t| vendor::for_target(t).err().map(|e| format!("{e:#}")))
        .collect();
    if unknown.is_empty() {
        Check::ok(
            "targets",
            format!("targets: {}", manifest.targets.join(", ")),
        )
    } else {
        Check::error("targets", "ai.json declares unknown targets")
            .details(unknown)
            .fix(format!("spm target remove <target>{g}"))
    }
}

fn check_machine_policy() -> Check {
    match Policy::machine() {
        Ok(policy) => {
            let layers = policy.describe();
            if layers.is_empty() {
                Check::ok("machine-policy", "no machine source policy")
            } else {
                Check::ok("machine-policy", "machine source policy loaded").details(layers)
            }
        }
        Err(e) => Check::error(
            "machine-policy",
            format!("machine policy is invalid: {e:#}"),
        )
        .fix("correct the policy file (or SPM_POLICY)"),
    }
}

/// Every declared source, linked dir and locked transitive source against the
/// effective policy — the same rule `spm install` enforces before fetching.
fn check_policy(root: &Path, manifest: &Manifest, links: &Links, lock: Option<&Lockfile>) -> Check {
    let policy = match manifest.policy(root) {
        Ok(p) => p,
        Err(e) => {
            return Check::error("policy", format!("source policy is invalid: {e:#}"))
                .fix("correct `allowedSources` in ai.json or the machine policy")
        }
    };
    let declared = links
        .links
        .iter()
        .filter(|(n, _)| manifest.contains_dependency(n))
        .map(|(n, p)| (n.as_str(), p.as_str()));
    let mut denied = sync::denied_sources(manifest, &policy, declared);
    // `install --locked` fetches locked transitive pins once their root is,
    // so they are gated too, even when doctor can't walk that root. An
    // unreadable lock is reported by the lockfile check.
    if let Some(lock) = lock {
        for l in DependencyIndex::new(manifest, lock).undeclared() {
            if let Err(e) = policy.check(&l.locked.git) {
                denied.push(format!("locked {} `{}`: {e:#}", l.kind, l.name));
            }
        }
    }
    if denied.is_empty() {
        Check::ok("policy", "every source is allowed by the source policy")
    } else {
        Check::error("policy", "sources denied by the source policy")
            .details(denied)
            .fix("remove those dependencies or ask for them to be allowed")
    }
}

fn check_links(
    scope: &Scope,
    manifest: &Manifest,
    links: Result<&Links, &anyhow::Error>,
) -> Vec<Check> {
    let links = match links {
        Ok(l) => l,
        Err(e) => {
            return vec![Check::error(
                "links",
                format!("links file is unreadable: {e:#}"),
            )]
        }
    };
    links
        .links
        .keys()
        .map(|name| {
            let unlink = sync::unlink_command(scope, name);
            let declared = manifest.contains_dependency(name);
            // Like every sync, before resolving: an ignored link's directory
            // doesn't matter.
            if !declared {
                return Check::warn(
                    "links",
                    format!("`{name}` is linked but not declared in ai.json (ignored)"),
                )
                .fix(unlink);
            }
            match links.resolve(scope, name) {
                Err(e) => Check::error("links", format!("{e:#}")).fix(unlink),
                Ok(linked) => {
                    let display = linked.as_ref().map(|l| l.display.as_str()).unwrap_or("");
                    // Resolving only canonicalizes it: install must also read
                    // everything it snapshots, by the very same traversal.
                    match linked
                        .as_ref()
                        .map(|l| crate::fsutil::check_readable(&l.dir, &l.dir))
                    {
                        Some(Err(e)) => Check::error(
                            "links",
                            format!("`{name}` is linked to {display}, which can't be read: {e:#}"),
                        )
                        .fix(format!("make {display} readable, or {unlink}")),
                        _ => Check::warn(
                            "links",
                            format!(
                                "`{name}` is linked to {display} — the working copy replaces the \
                                 locked version"
                            ),
                        )
                        .fix(format!("{unlink} when done")),
                    }
                }
            }
        })
        .collect()
}

fn materialization_checks(scope: &Scope, report: &status::StatusReport) -> Vec<Check> {
    let install = sync::install_command(scope);
    let mut problems = Vec::new();
    let mut stale = Vec::new();
    for p in &report.unlocked_plugins {
        problems.push(format!("plugin `{p}` is declared but not locked"));
    }
    for t in &report.targets {
        for (what, st) in [
            ("skills", Some(&t.skills.status)),
            ("plugins", t.plugins.as_ref().map(|p| &p.status)),
        ] {
            let Some(st) = st else { continue };
            if !st.missing.is_empty() {
                problems.push(format!(
                    "{}: missing {what}: {}",
                    t.target,
                    st.missing.join(", ")
                ));
            }
            problems.extend(st.notes.iter().map(|n| format!("{}: {n}", t.target)));
            if !st.stale.is_empty() {
                let kind = if what == "plugins" { " plugins" } else { "" };
                stale.push(format!(
                    "{}: no longer declared{kind}: {}",
                    t.target,
                    st.stale.join(", ")
                ));
            }
        }
    }
    if !report.complete && problems.is_empty() {
        problems.push("declared dependencies are not installed".to_string());
    }
    let mut checks = vec![if problems.is_empty() {
        Check::ok(
            "materialized",
            "every locked skill and plugin is installed in every target",
        )
    } else {
        Check::error("materialized", "targets are not up to date")
            .details(problems)
            .fix(install)
    }];
    if !stale.is_empty() {
        checks.push(
            Check::warn(
                "materialized",
                "targets hold skills or plugins spm no longer manages",
            )
            .details(stale)
            .fix(install),
        );
    }
    if !report.collisions.is_empty() {
        let names = report
            .collisions
            .iter()
            .map(|c| {
                format!(
                    "`{}` is also installed in the {} scope",
                    c.name, c.other_scope
                )
            })
            .collect();
        checks.push(
            Check::warn(
                "shadowing",
                "skills installed in both scopes collide by name",
            )
            .details(names)
            .fix("remove one of them (spm remove <name> or spm remove <name> -g)"),
        );
    }
    checks
}

/// What `v` writes for full plugins in `scope` (nothing when it loads only
/// skills).
fn plugin_clean_paths(v: &dyn vendor::Vendor, scope: &Scope) -> Result<Vec<PathBuf>> {
    v.plugins()
        .map_or(Ok(Vec::new()), |p| p.plugin_clean_paths(scope, &[]))
}

/// Project scope: each vendor's `.gitignore` entries, the same ones `spm
/// install` ensures — so spm's output can't be committed.
fn check_gitignore(
    scope: &Scope,
    root: &Path,
    manifest: &Manifest,
    report: &status::StatusReport,
) -> Vec<Check> {
    let mut checks = Vec::new();
    let mut missing = Vec::new();
    // Output git would still pick up despite the entries: re-included by a
    // later rule, or committed (ignore rules don't apply to tracked files).
    let (mut exposed, mut tracked) = (Vec::new(), Vec::new());
    let mut relevant = false;
    let names: Vec<DepName> = report.expected.iter().map(|e| e.name.clone()).collect();
    let declares_any = !names.is_empty() || !report.plugins.is_empty();
    for target in &manifest.targets {
        let Ok(v) = vendor::for_target(target) else {
            continue;
        };
        // Even with nothing declared, an install may have written output
        // (e.g. Claude's empty marketplace) that must stay ignored.
        let has_output = || {
            [
                v.clean_paths(scope, &[]),
                plugin_clean_paths(v.as_ref(), scope),
            ]
            .into_iter()
            .flatten()
            .flatten()
            .any(|p| p.exists())
        };
        // Leftover skills (e.g. from another branch) need their entries too.
        let stale = report
            .targets
            .iter()
            .find(|t| &t.target == target)
            .map(|t| t.skills.status.stale.clone())
            .unwrap_or_default();
        if !declares_any && stale.is_empty() && !has_output() {
            continue;
        }
        relevant = true;
        // A leftover that is not a valid dependency name was never written by
        // spm, so it has no spm-managed entry or output to check.
        let owned: Vec<DepName> = names
            .iter()
            .cloned()
            .chain(stale.into_iter().filter_map(|s| DepName::try_from(s).ok()))
            .collect();
        let entries_present = match gitignore::missing(root, &v.gitignore_entries(&owned)) {
            Ok(m) => {
                let present = m.is_empty();
                missing.extend(m.into_iter().map(|e| format!("{target}: {e}")));
                present
            }
            Err(e) => {
                missing.push(format!("{target}: {e:#}"));
                false
            }
        };
        let outputs = [
            v.clean_paths(scope, &owned),
            plugin_clean_paths(v.as_ref(), scope),
        ]
        .into_iter()
        .flatten()
        .flatten()
        .filter(|p| p.exists());
        for path in outputs {
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if git::is_tracked(root, &rel) == Some(true) {
                tracked.push((target.clone(), rel));
            } else if entries_present && git::is_ignored(root, &format!("{rel}/")) == Some(false) {
                exposed.push(format!("{target}: {rel}/"));
            }
        }
    }
    if relevant {
        if !missing.is_empty() {
            checks.push(
                Check::warn("gitignore", ".gitignore is missing spm's entries")
                    .details(missing)
                    .fix(sync::install_command(scope)),
            );
        }
        if !exposed.is_empty() {
            checks.push(
                Check::warn("gitignore", "a .gitignore rule re-includes spm's output")
                    .details(exposed)
                    .fix("remove the .gitignore rule that re-includes it (e.g. `!.spm/`)"),
            );
        }
        if !tracked.is_empty() {
            // Targets sharing a dir report the same path; untrack it once.
            let paths: BTreeSet<&str> = tracked.iter().map(|(_, p)| p.as_str()).collect();
            checks.push(
                Check::warn(
                    "gitignore",
                    "spm's output is committed, so its .gitignore rules don't apply",
                )
                .details(tracked.iter().map(|(t, p)| format!("{t}: {p}")).collect())
                // Literal: a name like `g[1]` must not match the user's `g1`.
                .fix(format!(
                    "git --literal-pathspecs rm -r --cached -- {}",
                    paths
                        .into_iter()
                        .map(crate::shell::arg)
                        .collect::<Vec<_>>()
                        .join(" ")
                )),
            );
        }
        if checks.is_empty() {
            checks.push(Check::ok("gitignore", "spm's output is gitignored"));
        }
    }
    // Claude's settings.local.json holds a machine-specific marketplace path.
    let settings = ".claude/settings.local.json";
    if manifest.targets.iter().any(|t| t == "claude") && root.join(settings).exists() {
        let untrack = format!("git rm --cached {settings}");
        match (
            git::is_ignored(root, settings),
            git::is_tracked(root, settings) == Some(true),
        ) {
            (Some(false), tracked) => checks.push(
                Check::warn("gitignore", format!("{settings} is not gitignored")).fix(if tracked {
                    format!("add {settings} to .gitignore, then {untrack}")
                } else {
                    format!("add {settings} to .gitignore")
                }),
            ),
            (Some(true), true) => checks.push(
                Check::warn(
                    "gitignore",
                    format!("{settings} is committed, so its .gitignore rule doesn't apply"),
                )
                .fix(untrack),
            ),
            _ => {}
        }
    }
    checks
}

/// Each installed skill must have a `SKILL.md` with the frontmatter agents
/// discover skills by. Dirs shared by several targets are checked once.
fn check_loadable(scope: &Scope, report: &status::StatusReport) -> Vec<Check> {
    let mut seen = BTreeSet::new();
    let mut problems = Vec::new();
    for t in &report.targets {
        let st = &t.skills.status;
        let Ok(dir) = vendor::for_target(&t.target).and_then(|v| v.skills_dir(scope)) else {
            continue;
        };
        for name in &st.present {
            let skill = dir.join(name);
            if !seen.insert(skill.clone()) {
                continue;
            }
            if let Some(p) = skillcheck::loadability_problem(&skill) {
                problems.push(format!("`{name}` ({}): {p}", t.target));
            }
        }
    }
    if seen.is_empty() {
        return Vec::new();
    }
    vec![if problems.is_empty() {
        Check::ok("skills", "every installed skill has a loadable SKILL.md")
    } else {
        Check::warn("skills", "agents may not load these skills")
            .details(problems)
            .fix("fix the SKILL.md upstream (frontmatter needs `name` and `description`)")
    }]
}

/// SSH remotes authenticate through an agent; without one git prompts or
/// fails in non-interactive runs.
fn check_ssh_agent(manifest: &Manifest) -> Option<Check> {
    if cfg!(windows) || std::env::var_os("SSH_AUTH_SOCK").is_some_and(|v| !v.is_empty()) {
        return None;
    }
    let ssh: Vec<String> = manifest
        .dependencies()
        .map(|(_, _, s)| s.git.clone())
        .filter(|u| git::contacts_over_ssh(u))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    (!ssh.is_empty()).then(|| {
        Check::warn("ssh", "SSH remotes declared but SSH_AUTH_SOCK is not set")
            .details(ssh)
            .fix("start ssh-agent and add your key (ssh-add), or use --protocol https")
    })
}

/// `--online`: every remote in ai.json and ai.lock answers `git ls-remote`.
/// Every remote declared in `ai.json` or pinned in `ai.lock`.
fn remote_urls(manifest: &Manifest, lock: Option<&Lockfile>) -> BTreeSet<String> {
    let mut urls: BTreeSet<String> = manifest
        .dependencies()
        .map(|(_, _, s)| s.git.clone())
        .collect();
    if let Some(lock) = lock {
        urls.extend(lock.dependencies().map(|(_, _, l)| l.git.clone()));
    }
    urls
}

fn check_remotes(urls: &BTreeSet<String>) -> Vec<Check> {
    if urls.is_empty() {
        return Vec::new();
    }
    let mut failed = Vec::new();
    let mut fixes = BTreeSet::new();
    for url in urls {
        if let Reach::Unreachable { error, other } = git::probe(url) {
            failed.push(format!("{url}: {error:#}"));
            fixes.insert(match other {
                Some(p) => {
                    format!("reachable over {p}: pass `--protocol {p}` (or `--protocol-fallback`)")
                }
                None => "check network access and credentials for these remotes".to_string(),
            });
        }
    }
    vec![if failed.is_empty() {
        Check::ok("remotes", format!("{} remote(s) reachable", urls.len()))
    } else {
        Check::error("remotes", "remotes can't be reached")
            .details(failed)
            .fix(fixes.into_iter().collect::<Vec<_>>().join("; "))
    }]
}
