use super::sync::{sync, Apply};
use crate::scope::Scope;
use anyhow::Result;

pub(super) fn update(scope: &Scope, name: Option<String>, apply: Apply) -> Result<()> {
    sync(scope, true, name.as_deref(), false, apply)?;
    println!("updated");
    Ok(())
}
