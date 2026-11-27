//! The one judgement of `ai.lock` against `ai.json`, made by the sync
//! pipeline's locked check and only *rendered* by its consumers: as the error
//! `spm install --locked` (or `spm link`/`spm unlink`) fails with, or as the
//! diagnoses `spm doctor` reports. Every message and fix hint about lock drift
//! lives here, so the two can never disagree on what is wrong or how to fix it.

use super::sync::install_command;
use crate::ids::DepName;
use crate::manifest::DependencyKind;
use crate::scope::Scope;
use crate::transitive::{LockMiss, MissedPin};
use anyhow::{anyhow, Error};
use std::fmt;
use std::path::PathBuf;

/// Who rejects a lock that is missing or out of sync, which decides how the
/// error is labelled and what it suggests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Enforcer {
    /// `spm install --locked`: install exactly `ai.lock`.
    Locked,
    /// `spm link`/`spm unlink`: promise to leave `ai.lock` byte-identical.
    Links,
}

impl Enforcer {
    fn label(self) -> &'static str {
        match self {
            Enforcer::Locked => "--locked",
            Enforcer::Links => "`spm link`/`spm unlink` never write ai.lock",
        }
    }

    fn fix_hint(self, scope: &Scope) -> String {
        let install = install_command(scope);
        match self {
            Enforcer::Locked => format!("run `{install}` (without --locked) and commit ai.lock"),
            Enforcer::Links => {
                format!("run `{install}` first to bring ai.lock in sync, then retry")
            }
        }
    }
}

/// Why a direct dependency's pin in `ai.lock` can't be reused as is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Mismatch {
    /// `ai.lock` has no entry for it.
    Missing,
    /// A refresh (`spm update`) re-resolves it regardless.
    Refreshing,
    /// Its request changed; the changes as `Pin::drift_from` describes them.
    Changed(String),
}

/// What the whole-lock backstop found `ai.lock` to differ in from what spm
/// would write, after every specific check passed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Rewrite {
    /// Somewhere: the walk was complete, so the documents are compared whole.
    Lock,
    /// A top-level field other than `skills` and `plugins`.
    TopLevel(String),
    /// The `plugins` map's shape (e.g. an empty map spm omits).
    Plugins,
    /// One plugin's entry.
    Plugin(String),
    /// One skill's entry.
    Skill(DepName),
}

/// One way `ai.lock` disagrees with `ai.json`: what `spm install --locked`
/// rejects. Only ever rendered here (see [`fmt::Display`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum LockProblem {
    /// The lock has no project `id`.
    MissingId,
    /// A declared dependency whose pin is missing or no longer matches.
    Drift {
        kind: DependencyKind,
        name: DepName,
        why: Mismatch,
    },
    /// A direct pin `ai.json` no longer declares.
    Stale { kind: DependencyKind, name: DepName },
    /// A transitive pin nothing requires any more.
    StaleTransitive(DepName),
    /// A transitive dependency the lock does not pin.
    Unresolved(LockMiss),
    /// What the specific checks missed: the lock differs from what spm would
    /// write.
    Rewritten(Rewrite),
}

impl LockProblem {
    /// The problem class's rank in report order.
    fn rank(&self) -> u8 {
        match self {
            LockProblem::MissingId => 0,
            LockProblem::Drift { .. } => 1,
            LockProblem::Stale { .. } | LockProblem::StaleTransitive(_) => 2,
            LockProblem::Unresolved(_) => 3,
            LockProblem::Rewritten(_) => 4,
        }
    }
}

impl fmt::Display for LockProblem {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LockProblem::MissingId => f.write_str("ai.lock has no project `id`"),
            LockProblem::Drift { kind, name, why } => {
                write!(f, "{kind} `{name}`: ")?;
                match why {
                    Mismatch::Missing => f.write_str("missing from ai.lock"),
                    Mismatch::Refreshing => f.write_str("refreshing"),
                    Mismatch::Changed(changes) => {
                        write!(f, "changed since ai.lock was written ({changes})")
                    }
                }
            }
            LockProblem::Stale { kind, name } => {
                write!(f, "{kind} `{name}`: in ai.lock but not declared in ai.json")
            }
            LockProblem::StaleTransitive(name) => {
                write!(
                    f,
                    "transitive skill `{name}`: in ai.lock but no longer required"
                )
            }
            LockProblem::Unresolved(LockMiss {
                name,
                declared,
                parent,
                why,
            }) => {
                write!(
                    f,
                    "transitive skill `{name}` (`{declared}` required by `{parent}`): "
                )?;
                match why {
                    MissedPin::Missing => f.write_str("missing from ai.lock"),
                    MissedPin::Changed(changes) => write!(f, "changed in ai.lock ({changes})"),
                }
            }
            LockProblem::Rewritten(Rewrite::Lock) => f.write_str(
                "ai.lock differs from what spm would write (e.g. a plugin's bundled skills \
                 or a transitive skill's requesters changed)",
            ),
            LockProblem::Rewritten(Rewrite::TopLevel(key)) => {
                write!(
                    f,
                    "ai.lock top-level `{key}` differs from what spm would write"
                )
            }
            LockProblem::Rewritten(Rewrite::Plugins) => {
                f.write_str("ai.lock `plugins` differs from what spm would write")
            }
            LockProblem::Rewritten(Rewrite::Plugin(name)) => write!(
                f,
                "ai.lock entry for plugin `{name}` differs from what spm would write (e.g. \
                 its bundled skills changed)"
            ),
            LockProblem::Rewritten(Rewrite::Skill(name)) => write!(
                f,
                "ai.lock entry for skill `{name}` differs from what spm would write (e.g. \
                 a transitive skill's requesters changed)"
            ),
        }
    }
}

/// Every way `ai.lock` disagrees with `ai.json`. Its
/// [`problems`](Self::problems) are exactly what `spm install --locked`
/// rejects.
#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct LockFindings {
    /// What `--locked` rejects, in the order found.
    problems: Vec<LockProblem>,
    /// Dependencies whose store checkout is missing, so what they contribute
    /// to `ai.lock` (a plugin's bundled skills, nested dependencies) could not
    /// be verified offline — not a problem, but `spm install` would fetch them.
    pub(super) unverified: Vec<DepName>,
}

impl LockFindings {
    /// Record `problem`.
    pub(super) fn push(&mut self, problem: LockProblem) {
        self.problems.push(problem);
    }

    /// The problems recorded so far, in the order found.
    #[cfg(test)]
    pub(super) fn recorded(&self) -> &[LockProblem] {
        &self.problems
    }

    /// The problems, one line each, in report order: by class, then in the
    /// order found.
    pub(super) fn problems(&self) -> Vec<String> {
        let mut problems: Vec<&LockProblem> = self.problems.iter().collect();
        problems.sort_by_key(|p| p.rank());
        problems.iter().map(ToString::to_string).collect()
    }

    /// Whether `ai.lock` agrees with `ai.json` as far as could be checked.
    pub(super) fn in_sync(&self) -> bool {
        self.problems.is_empty()
    }
}

/// The verdict on a scope's `ai.lock`.
#[derive(Debug)]
pub(super) enum LockVerdict {
    /// There is no `ai.lock` at `path`; `declared` is whether `ai.json`
    /// declares anything it should pin.
    Missing { path: PathBuf, declared: bool },
    /// The lock was judged.
    Checked(LockFindings),
    /// The lock could not be judged.
    Unchecked(Error),
}

impl LockVerdict {
    /// Fail as `enforcer` would on a missing or out-of-sync lock.
    pub(super) fn enforce(self, enforcer: Enforcer, scope: &Scope) -> anyhow::Result<()> {
        let (label, fix) = (enforcer.label(), enforcer.fix_hint(scope));
        match self {
            LockVerdict::Missing { path, .. } => {
                Err(anyhow!("{label}: {} not found\n{fix}", path.display()))
            }
            LockVerdict::Checked(findings) if findings.in_sync() => Ok(()),
            LockVerdict::Checked(findings) => {
                let problems = findings.problems();
                Err(anyhow!(
                    "{label}: ai.lock is out of sync with ai.json ({} problem(s)):\n{}\n{fix}",
                    problems.len(),
                    problems
                        .iter()
                        .map(|m| format!("  - {m}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ))
            }
            LockVerdict::Unchecked(e) => Err(e),
        }
    }

    /// The verdict as diagnoses, in report order (`spm doctor`).
    pub(super) fn findings(self, scope: &Scope) -> Vec<Finding> {
        let install = install_command(scope);
        let commit = format!("{install} and commit ai.lock");
        match self {
            LockVerdict::Missing { declared: true, .. } => vec![Finding::new(
                Severity::Error,
                "no ai.lock: declared dependencies are not pinned",
            )
            .fix(commit)],
            LockVerdict::Missing { .. } => {
                vec![Finding::new(
                    Severity::Ok,
                    "no ai.lock (nothing declared yet)",
                )]
            }
            LockVerdict::Unchecked(e) => {
                vec![
                    Finding::new(Severity::Error, format!("ai.lock can't be checked: {e:#}"))
                        .fix(commit),
                ]
            }
            LockVerdict::Checked(findings) => {
                let mut out = vec![if findings.in_sync() {
                    Finding::new(Severity::Ok, "ai.lock matches ai.json")
                } else {
                    Finding::new(
                        Severity::Error,
                        format!(
                            "ai.lock is out of sync with ai.json (`{install} --locked` would fail)"
                        ),
                    )
                    .details(findings.problems())
                    .fix(commit)
                }];
                if !findings.unverified.is_empty() {
                    out.push(
                        Finding::new(
                            Severity::Warn,
                            "not in the store, so what they add to ai.lock was not verified offline",
                        )
                        .details(findings.unverified.iter().map(ToString::to_string).collect())
                        .fix(format!("{install} (fetches them)")),
                    );
                }
                out
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Severity {
    Ok,
    Warn,
    Error,
}

/// One diagnosis of the lock.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Finding {
    pub(super) severity: Severity,
    pub(super) message: String,
    /// One line per affected item.
    pub(super) details: Vec<String>,
    pub(super) fix: Option<String>,
}

impl Finding {
    fn new(severity: Severity, message: impl Into<String>) -> Self {
        Finding {
            severity,
            message: message.into(),
            details: Vec::new(),
            fix: None,
        }
    }
    fn details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
    fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::dep;

    fn drift() -> LockProblem {
        LockProblem::Drift {
            kind: DependencyKind::Skill,
            name: dep("d"),
            why: Mismatch::Missing,
        }
    }

    fn miss(why: MissedPin) -> LockProblem {
        LockProblem::Unresolved(LockMiss {
            name: dep("t"),
            declared: dep("u"),
            parent: dep("p"),
            why,
        })
    }

    /// One problem of each class, found out of report order.
    fn every_class() -> LockFindings {
        let mut f = LockFindings {
            unverified: vec![dep("unverified")],
            ..Default::default()
        };
        for p in [
            LockProblem::Rewritten(Rewrite::Plugins),
            miss(MissedPin::Missing),
            LockProblem::StaleTransitive(dep("s")),
            drift(),
            LockProblem::MissingId,
        ] {
            f.push(p);
        }
        f
    }

    const EVERY_CLASS: [&str; 5] = [
        "ai.lock has no project `id`",
        "skill `d`: missing from ai.lock",
        "transitive skill `s`: in ai.lock but no longer required",
        "transitive skill `t` (`u` required by `p`): missing from ai.lock",
        "ai.lock `plugins` differs from what spm would write",
    ];

    /// Every problem's wording, the one place it is written.
    #[test]
    fn each_problem_renders_its_message() {
        let drift = |kind, why| LockProblem::Drift {
            kind,
            name: dep("x"),
            why,
        };
        let cases = [
            (LockProblem::MissingId, "ai.lock has no project `id`"),
            (
                drift(DependencyKind::Skill, Mismatch::Missing),
                "skill `x`: missing from ai.lock",
            ),
            (
                drift(DependencyKind::Plugin, Mismatch::Refreshing),
                "plugin `x`: refreshing",
            ),
            (
                drift(
                    DependencyKind::Skill,
                    Mismatch::Changed("`a` -> `b`".into()),
                ),
                "skill `x`: changed since ai.lock was written (`a` -> `b`)",
            ),
            (
                LockProblem::Stale {
                    kind: DependencyKind::Plugin,
                    name: dep("x"),
                },
                "plugin `x`: in ai.lock but not declared in ai.json",
            ),
            (
                LockProblem::StaleTransitive(dep("x")),
                "transitive skill `x`: in ai.lock but no longer required",
            ),
            (
                miss(MissedPin::Missing),
                "transitive skill `t` (`u` required by `p`): missing from ai.lock",
            ),
            (
                miss(MissedPin::Changed("`a` -> `b`".into())),
                "transitive skill `t` (`u` required by `p`): changed in ai.lock (`a` -> `b`)",
            ),
            (
                LockProblem::Rewritten(Rewrite::Lock),
                "ai.lock differs from what spm would write (e.g. a plugin's bundled skills \
                 or a transitive skill's requesters changed)",
            ),
            (
                LockProblem::Rewritten(Rewrite::TopLevel("id".into())),
                "ai.lock top-level `id` differs from what spm would write",
            ),
            (
                LockProblem::Rewritten(Rewrite::Plugins),
                "ai.lock `plugins` differs from what spm would write",
            ),
            (
                LockProblem::Rewritten(Rewrite::Plugin("x".into())),
                "ai.lock entry for plugin `x` differs from what spm would write (e.g. its \
                 bundled skills changed)",
            ),
            (
                LockProblem::Rewritten(Rewrite::Skill(dep("x"))),
                "ai.lock entry for skill `x` differs from what spm would write (e.g. a \
                 transitive skill's requesters changed)",
            ),
        ];
        for (problem, message) in cases {
            assert_eq!(problem.to_string(), message);
        }
    }

    fn project() -> Scope {
        Scope::Project {
            root: PathBuf::from("p"),
        }
    }

    #[test]
    fn problems_list_every_class_in_report_order_but_not_unverified() {
        assert_eq!(every_class().problems(), EVERY_CLASS);
    }

    /// Within a class, problems keep the order they were found in; direct
    /// stale pins are found before transitive ones.
    #[test]
    fn problems_keep_the_order_found_within_a_class() {
        let mut f = LockFindings::default();
        let stale = LockProblem::Stale {
            kind: DependencyKind::Skill,
            name: dep("z"),
        };
        for p in [
            stale,
            LockProblem::StaleTransitive(dep("a")),
            drift(),
            LockProblem::StaleTransitive(dep("b")),
        ] {
            f.push(p);
        }
        assert_eq!(
            f.problems(),
            [
                "skill `d`: missing from ai.lock",
                "skill `z`: in ai.lock but not declared in ai.json",
                "transitive skill `a`: in ai.lock but no longer required",
                "transitive skill `b`: in ai.lock but no longer required",
            ]
        );
    }

    #[test]
    fn any_problem_class_puts_the_lock_out_of_sync_but_unverified_does_not() {
        let unverified = LockFindings {
            unverified: vec![dep("u")],
            ..Default::default()
        };
        assert!(unverified.in_sync());
        for problem in every_class().problems {
            let mut f = LockFindings::default();
            f.push(problem);
            assert!(!f.in_sync(), "{f:?}");
            assert_eq!(f.problems().len(), 1, "{f:?}");
        }
    }

    #[test]
    fn enforcing_renders_the_problems_with_the_enforcers_label_and_fix() {
        let err = LockVerdict::Checked(every_class())
            .enforce(Enforcer::Locked, &project())
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            format!(
                "--locked: ai.lock is out of sync with ai.json (5 problem(s)):\n  - {}\n\
                 run `spm install` (without --locked) and commit ai.lock",
                EVERY_CLASS.join("\n  - ")
            )
        );
        let err = LockVerdict::Missing {
            path: PathBuf::from("x/ai.lock"),
            declared: false,
        }
        .enforce(Enforcer::Links, &Scope::Global)
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "`spm link`/`spm unlink` never write ai.lock: x/ai.lock not found\n\
             run `spm install -g` first to bring ai.lock in sync, then retry"
        );
    }

    #[test]
    fn enforcing_passes_a_lock_in_sync_and_raises_why_it_was_unchecked() {
        let unverified = LockFindings {
            unverified: vec![dep("u")],
            ..Default::default()
        };
        assert!(LockVerdict::Checked(unverified)
            .enforce(Enforcer::Locked, &project())
            .is_ok());
        let err = LockVerdict::Unchecked(anyhow!("boom"))
            .enforce(Enforcer::Locked, &project())
            .unwrap_err();
        assert_eq!(err.to_string(), "boom");
    }

    #[test]
    fn findings_report_the_same_problems_enforcing_raises() {
        let findings = LockVerdict::Checked(every_class()).findings(&Scope::Global);
        assert_eq!(findings.len(), 2);
        let (error, warn) = (&findings[0], &findings[1]);
        assert_eq!(error.severity, Severity::Error);
        assert_eq!(
            error.message,
            "ai.lock is out of sync with ai.json (`spm install -g --locked` would fail)"
        );
        assert_eq!(error.details, every_class().problems());
        assert_eq!(
            error.fix.as_deref(),
            Some("spm install -g and commit ai.lock")
        );
        let enforced = LockVerdict::Checked(every_class())
            .enforce(Enforcer::Locked, &Scope::Global)
            .unwrap_err()
            .to_string();
        for detail in &error.details {
            assert!(enforced.contains(detail.as_str()), "{detail}");
        }
        assert_eq!(warn.severity, Severity::Warn);
        assert_eq!(warn.details, ["unverified"]);
        assert_eq!(warn.fix.as_deref(), Some("spm install -g (fetches them)"));
    }

    #[test]
    fn findings_judge_a_missing_clean_or_unchecked_lock() {
        let missing = |declared| {
            LockVerdict::Missing {
                path: PathBuf::from("ai.lock"),
                declared,
            }
            .findings(&project())
        };
        assert_eq!(
            missing(true),
            [Finding::new(
                Severity::Error,
                "no ai.lock: declared dependencies are not pinned"
            )
            .fix("spm install and commit ai.lock")]
        );
        assert_eq!(
            missing(false),
            [Finding::new(
                Severity::Ok,
                "no ai.lock (nothing declared yet)"
            )]
        );
        assert_eq!(
            LockVerdict::Checked(LockFindings::default()).findings(&project()),
            [Finding::new(Severity::Ok, "ai.lock matches ai.json")]
        );
        assert_eq!(
            LockVerdict::Unchecked(anyhow!("bad").context("reading ai.lock")).findings(&project()),
            [Finding::new(
                Severity::Error,
                "ai.lock can't be checked: reading ai.lock: bad"
            )
            .fix("spm install and commit ai.lock")]
        );
    }
}
