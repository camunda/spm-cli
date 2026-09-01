use super::sync::sync;
use crate::scope::Scope;
use anyhow::Result;

pub(super) fn update(scope: &Scope, name: Option<String>) -> Result<()> {
    sync(scope, true, name.as_deref())?;
    println!("updated");
    Ok(())
}
