//! Post-resolution sanity checks that warn (never fail) when a materialized
//! skill is unlikely to be picked up by the target agents.
//!
//! This lives here — run once per skill in the sync pipeline — rather than in
//! each vendor adapter, so the `SKILL.md` presence check is a single source of
//! truth and is emitted once regardless of how many vendors are configured.

use std::path::Path;

/// Inspect a materialized skill and print an actionable warning to stderr when
/// it has no `SKILL.md` at its root. If the directory is actually a *container*
/// of skills (its immediate subdirectories carry `SKILL.md`), suggest the
/// concrete `spm add … --path <sub>` invocations instead of a bare warning.
///
/// `subpath` is the original `--path` from the manifest (if any); it is used to
/// build the suggested `--path` values so they are copy-pasteable.
pub fn warn_if_not_loadable(name: &str, git: &str, subpath: Option<&str>, content: &Path) {
    if content.join("SKILL.md").exists() {
        return;
    }

    let subskills = child_skills(content);
    if subskills.is_empty() {
        eprintln!("warning: skill `{name}` has no SKILL.md at its root — agents may ignore it");
        return;
    }

    let plural = if subskills.len() == 1 { "" } else { "s" };
    eprintln!(
        "warning: skill `{name}` has no SKILL.md at its root, but its directory contains {} skill{plural}.",
        subskills.len()
    );
    eprintln!(
        "  `--path {}` points at a collection of skills, not a single skill. Did you mean one of:",
        subpath.unwrap_or(".")
    );
    for sub in &subskills {
        let path = join_subpath(subpath, sub);
        eprintln!("    spm add {git} --path {path} --name {sub}");
    }
}

/// Names of immediate subdirectories of `dir` that contain a `SKILL.md`,
/// sorted for deterministic output.
fn child_skills(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| e.path().join("SKILL.md").is_file())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// Join an optional parent subpath with a child directory name using forward
/// slashes (matching the manifest `--path` convention on every platform).
fn join_subpath(parent: Option<&str>, child: &str) -> String {
    match parent {
        Some(p) if !p.is_empty() => format!("{}/{child}", p.trim_end_matches('/')),
        _ => child.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::join_subpath;

    #[test]
    fn join_subpath_combines_with_forward_slash() {
        assert_eq!(
            join_subpath(Some("skills"), "camunda-ds"),
            "skills/camunda-ds"
        );
        assert_eq!(join_subpath(Some("skills/"), "migrate"), "skills/migrate");
        assert_eq!(join_subpath(None, "greet"), "greet");
        assert_eq!(join_subpath(Some(""), "greet"), "greet");
    }
}
