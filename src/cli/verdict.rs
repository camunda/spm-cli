//! The one judgement of `ai.lock` against `ai.json`, made by the sync
//! pipeline's locked check and only *rendered* by its consumers: as the error
//! `spm install --locked` (or `spm link`/`spm unlink`) fails with, or as the
//! diagnoses `spm doctor` reports. Every message and fix hint about lock drift
//! lives here, so the two can never disagree on what is wrong or how to fix it.

use super::sync::install_command;
use crate::scope::Scope;
use anyhow::{anyhow, Error};
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

/// Every way `ai.lock` disagrees with `ai.json`, by problem class. Its
/// [`problems`](Self::problems) are exactly what `spm install --locked`
/// rejects.
#[derive(Default, Debug, PartialEq, Eq)]
pub(super) struct LockFindings {
    /// The lock has no project `id`.
    pub(super) missing_id: bool,
    /// Declared dependencies whose pin is missing or no longer matches.
    pub(super) drift: Vec<String>,
    /// Pins nothing declares or requires any more.
    pub(super) stale: Vec<String>,
    /// Transitive dependencies the lock does not pin.
    pub(super) unresolved: Vec<String>,
    /// What the specific checks missed: the lock differs from what spm would
    /// write.
    pub(super) rewritten: Vec<String>,
    /// Dependencies whose store checkout is missing, so what they contribute
    /// to `ai.lock` (a plugin's bundled skills, nested dependencies) could not
    /// be verified offline — not a problem, but `spm install` would fetch them.
    pub(super) unverified: Vec<String>,
}

impl LockFindings {
    /// The problems, one line each, in report order.
    pub(super) fn problems(&self) -> Vec<String> {
        self.missing_id
            .then(|| "ai.lock has no project `id`".to_string())
            .into_iter()
            .chain(self.drift.iter().cloned())
            .chain(self.stale.iter().cloned())
            .chain(self.unresolved.iter().cloned())
            .chain(self.rewritten.iter().cloned())
            .collect()
    }

    /// Whether `ai.lock` agrees with `ai.json` as far as could be checked.
    pub(super) fn in_sync(&self) -> bool {
        !self.missing_id
            && self.drift.is_empty()
            && self.stale.is_empty()
            && self.unresolved.is_empty()
            && self.rewritten.is_empty()
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
                        .details(findings.unverified)
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

    fn every_class() -> LockFindings {
        LockFindings {
            missing_id: true,
            drift: vec!["drift".into()],
            stale: vec!["stale".into()],
            unresolved: vec!["unresolved".into()],
            rewritten: vec!["rewritten".into()],
            unverified: vec!["unverified".into()],
        }
    }

    fn project() -> Scope {
        Scope::Project {
            root: PathBuf::from("p"),
        }
    }

    #[test]
    fn problems_list_every_class_in_report_order_but_not_unverified() {
        assert_eq!(
            every_class().problems(),
            [
                "ai.lock has no project `id`",
                "drift",
                "stale",
                "unresolved",
                "rewritten"
            ]
        );
    }

    #[test]
    fn any_problem_class_puts_the_lock_out_of_sync_but_unverified_does_not() {
        let unverified = LockFindings {
            unverified: vec!["u".into()],
            ..Default::default()
        };
        assert!(unverified.in_sync());
        let one_each: [fn(&mut LockFindings); 5] = [
            |f| f.missing_id = true,
            |f| f.drift.push("x".into()),
            |f| f.stale.push("x".into()),
            |f| f.unresolved.push("x".into()),
            |f| f.rewritten.push("x".into()),
        ];
        for set in one_each {
            let mut f = LockFindings::default();
            set(&mut f);
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
            "--locked: ai.lock is out of sync with ai.json (5 problem(s)):\n  \
             - ai.lock has no project `id`\n  - drift\n  - stale\n  - unresolved\n  \
             - rewritten\nrun `spm install` (without --locked) and commit ai.lock"
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
            unverified: vec!["u".into()],
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
