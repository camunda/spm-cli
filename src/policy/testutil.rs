#![cfg(test)]
//! Helpers shared by the policy unit tests.

use super::layer::Layer;
use super::Policy;

/// A one-layer policy allowing `raw`.
pub(super) fn policy(raw: &[&str]) -> Policy {
    let raw: Vec<String> = raw.iter().map(|s| s.to_string()).collect();
    Policy {
        layers: vec![Layer::new("test policy".to_string(), &raw).unwrap()],
    }
}
