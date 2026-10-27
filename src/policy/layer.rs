//! One policy layer (machine or project): its origin and parsed patterns.

use super::pattern::Pattern;
use super::source::Source;
use anyhow::{Context, Result};

/// One policy layer: where it came from, plus its patterns.
#[derive(Debug, Clone)]
pub(super) struct Layer {
    /// e.g. `machine policy /etc/spm/policy.json`.
    origin: String,
    raw: Vec<String>,
    patterns: Vec<Pattern>,
}

impl Layer {
    pub(super) fn new(origin: String, raw: &[String]) -> Result<Layer> {
        let patterns = raw
            .iter()
            .map(|p| Pattern::parse(p))
            .collect::<Result<_>>()
            .with_context(|| format!("in the {origin}"))?;
        Ok(Layer {
            origin,
            raw: raw.to_vec(),
            patterns,
        })
    }

    pub(super) fn allows(&self, source: &Source) -> bool {
        self.patterns.iter().any(|p| p.matches(source))
    }

    pub(super) fn summary(&self) -> String {
        let allows = if self.raw.is_empty() {
            "allows nothing".to_string()
        } else {
            let list: Vec<String> = self.raw.iter().map(|p| format!("`{p}`")).collect();
            format!("allows {}", list.join(", "))
        };
        format!("the {} ({allows})", self.origin)
    }
}
