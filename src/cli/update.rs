use super::sync::{sync, Apply, Mode, Refresh};
use crate::scope::Scope;
use anyhow::Result;

pub(super) fn update(scope: &Scope, name: Option<String>, apply: Apply) -> Result<()> {
    let refresh = Refresh::update(name.as_deref());
    sync(scope, Mode::Write { refresh, apply })?;
    println!("updated");
    Ok(())
}
