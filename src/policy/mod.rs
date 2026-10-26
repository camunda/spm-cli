//! Source allowlist policy: restricts which git sources spm may fetch skills and
//! plugins from.
//!
//! A policy is made of up to two **layers**, and a source must be allowed by
//! *every* layer that exists (the effective policy is their intersection):
//!
//! 1. the **machine** layer — `$SPM_HOME/policy.json`, or the file `SPM_POLICY`
//!    points at — which CI and managed laptops set and a project cannot loosen;
//! 2. the **project** layer — `allowedSources` in the root `ai.json` (never in a
//!    dependency's nested `ai.json`, which is rejected at load).
//!
//! With no layer configured nothing is restricted. Patterns are matched against
//! a protocol-independent identity of the source (see [`Source::classify`]), so
//! the HTTPS, SSH-URL and scp-style spellings of one repository — and anything
//! `--protocol` rewrites between them — are judged identically.
//!
//! Enforcement happens before any network access: callers check each source up
//! front for a precise error, and two choke points back that up categorically —
//! every URL handed to git for a remote operation
//! (`git::transport::with_transport`) and
//! every store checkout, even a cached one (`store::ensure`), passes [`gate`].

mod active;
mod layer;
mod load;
mod pattern;
mod source;
mod testutil;

pub use active::{activate, gate, restricts};
pub use pattern::validate_patterns;

use anyhow::{bail, Result};
use layer::Layer;
use source::Source;

/// The effective source policy: the intersection of every configured layer.
#[derive(Debug, Clone, Default)]
pub struct Policy {
    layers: Vec<Layer>,
}

impl Policy {
    /// One line per layer in force (`the machine policy … (allows …)`); empty
    /// when no policy applies.
    pub fn describe(&self) -> Vec<String> {
        self.layers.iter().map(Layer::summary).collect()
    }

    /// Fail unless every layer allows `git`. The error names the source, the
    /// identity it was matched as, and each layer that denied it.
    pub fn check(&self, git: &str) -> Result<()> {
        self.judge(git, Source::classify(git))
    }

    /// Fail unless every layer allows a local directory (`dir`, as spelled for
    /// messages): a `spm link` override is the local source it really is, so it
    /// needs `file://*` whatever the dependency it overrides declares.
    pub fn check_local(&self, dir: &str) -> Result<()> {
        self.judge(dir, Source::Local)
    }

    fn judge(&self, git: &str, source: Source) -> Result<()> {
        if self.layers.is_empty() {
            return Ok(());
        }
        let denied: Vec<String> = self
            .layers
            .iter()
            .filter(|l| !l.allows(&source))
            .map(Layer::summary)
            .collect();
        if denied.is_empty() {
            return Ok(());
        }
        bail!(
            "source `{git}` ({}) is not allowed: denied by {}",
            source.describe(),
            denied.join(" and by ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::testutil::policy;

    #[test]
    fn no_layers_means_no_restriction_and_empty_list_denies_all() {
        assert!(Policy::default().check("ext::anything").is_ok());
        let err = format!(
            "{:#}",
            policy(&[]).check("https://github.com/o/r").unwrap_err()
        );
        assert!(err.contains("allows nothing"), "{err}");
    }

    /// The effective policy is the intersection: a source must pass every
    /// layer, and the error names each layer that denied it.
    #[test]
    fn layers_intersect_and_errors_name_every_denying_layer() {
        let machine = Layer::new(
            "machine policy M".to_string(),
            &["github.com/camunda/*".to_string()],
        )
        .unwrap();
        let project = Layer::new(
            "project policy P".to_string(),
            &["github.com/*".to_string()],
        )
        .unwrap();
        let p = Policy {
            layers: vec![machine, project],
        };
        assert!(p.check("https://github.com/camunda/x").is_ok());
        let err = format!("{:#}", p.check("https://github.com/other/x").unwrap_err());
        assert!(err.contains("machine policy M"), "{err}");
        assert!(!err.contains("project policy P"), "{err}");
        assert!(err.contains("github.com/other/x"), "{err}");
        let err = format!("{:#}", p.check("https://gitlab.com/o/x").unwrap_err());
        assert!(
            err.contains("machine policy M") && err.contains("project policy P"),
            "{err}"
        );
    }
}
