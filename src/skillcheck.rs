//! Post-resolution sanity checks that warn (never fail) when a materialized
//! skill is unlikely to be picked up by the target agents.
//!
//! This lives here — run once per skill in the sync pipeline — rather than in
//! each vendor adapter, so the `SKILL.md` presence check is a single source of
//! truth and is emitted once regardless of how many vendors are configured.

use crate::fsutil;
use std::path::Path;

/// Inspect a materialized skill and print an actionable warning to stderr when
/// it has no `SKILL.md` at its root. If the directory is actually a *container*
/// of skills (its immediate subdirectories carry `SKILL.md`), suggest the
/// concrete `spm add … --path <sub>` invocations instead of a bare warning.
///
/// `subpath` is the original `--path` from the manifest (if any) and `reference`
/// is the locked selector (`tag:…`/`branch:…`/`commit:…`); both are used to
/// build fully copy-pasteable suggestions.
pub fn warn_if_not_loadable(
    name: &str,
    git: &str,
    reference: &str,
    subpath: Option<&str>,
    content: &Path,
    boundary: &Path,
) {
    let boundary_canon = std::fs::canonicalize(boundary).unwrap_or_else(|_| boundary.to_path_buf());
    if is_materialized_file(&content.join("SKILL.md"), &boundary_canon) {
        return;
    }

    let subskills = child_skills(content, boundary);
    if subskills.is_empty() {
        eprintln!("warning: skill `{name}` has no SKILL.md at its root — agents may ignore it");
        return;
    }

    let selector = selector_flag(reference);
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
        eprintln!("    spm add {git} {selector} --path {path} --name {sub}");
    }
}

/// Render a locked `reference` (`tag:v1`, `branch:main`, `commit:<sha>`) back
/// into the CLI selector flag (`--tag v1`, …) so suggestions are runnable. Falls
/// back to the raw reference if it is not in the expected `kind:value` shape.
fn selector_flag(reference: &str) -> String {
    match reference.split_once(':') {
        Some((kind @ ("tag" | "branch" | "commit"), value)) => format!("--{kind} {value}"),
        _ => reference.to_string(),
    }
}

/// Names of immediate subdirectories of `dir` that contain a `SKILL.md`,
/// sorted for deterministic output.
///
/// This is the single source of truth for "what are the sub-skills of this
/// container?": both the no-SKILL.md suggestion above and `spm add --all` (which
/// materializes every sub-skill at once) enumerate them the same way, so the two
/// can never disagree about which directories count as skills.
pub(crate) fn child_skills(dir: &Path, boundary: &Path) -> Vec<String> {
    // Canonicalize the boundary once (fall back to the raw path if it cannot be
    // resolved — this is a warn-only path, never a gate) so the per-entry
    // `SKILL.md` checks below only canonicalize each entry, not the boundary.
    let boundary_canon = std::fs::canonicalize(boundary).unwrap_or_else(|_| boundary.to_path_buf());
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(entries) => entries
            .flatten()
            .filter(|e| is_materialized_file(&e.path().join("SKILL.md"), &boundary_canon))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// True when `path` will be materialized as a regular file by
/// [`fsutil::copy_tree`] — i.e. it resolves, through any intermediate symlinks,
/// to a regular file that stays **inside** `boundary_canon` (the *canonical*
/// repo checkout root) and out of `.git`. Routing the whole path through
/// [`fsutil::resolve_within_canonical`] — rather than a shallow
/// `symlink_metadata` on the final component — is what keeps this in lockstep
/// with the copy: an escaping *intermediate* directory symlink (e.g. `dir ->
/// /etc`, then `dir/SKILL.md`) canonicalizes outside the boundary, so it is
/// *not* counted here, exactly as copy_tree refuses to descend into it. An
/// in-checkout `SKILL.md` (real, or a symlink to another in-checkout file)
/// counts as present so the "agents may ignore it" warning does not fire even
/// though the file lands.
fn is_materialized_file(path: &Path, boundary_canon: &Path) -> bool {
    fsutil::resolve_within_canonical(boundary_canon, path)
        .and_then(|target| std::fs::metadata(&target).ok())
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// Join an optional parent subpath with a child directory name using forward
/// slashes (matching the manifest `--path` convention on every platform).
/// Windows-style `\` separators in the parent are normalized to `/` so the
/// suggested commands stay consistent and copy-pasteable everywhere.
pub(crate) fn join_subpath(parent: Option<&str>, child: &str) -> String {
    match parent {
        Some(p) if !p.is_empty() => {
            let p = p.replace('\\', "/");
            format!("{}/{child}", p.trim_end_matches('/'))
        }
        _ => child.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{join_subpath, selector_flag};

    #[test]
    fn join_subpath_combines_with_forward_slash() {
        assert_eq!(
            join_subpath(Some("skills"), "camunda-ds"),
            "skills/camunda-ds"
        );
        assert_eq!(join_subpath(Some("skills/"), "migrate"), "skills/migrate");
        assert_eq!(join_subpath(Some("a\\b\\"), "c"), "a/b/c");
        assert_eq!(join_subpath(None, "greet"), "greet");
        assert_eq!(join_subpath(Some(""), "greet"), "greet");
    }

    #[test]
    fn selector_flag_renders_runnable_flags() {
        assert_eq!(selector_flag("tag:v1.2.0"), "--tag v1.2.0");
        assert_eq!(selector_flag("branch:main"), "--branch main");
        assert_eq!(
            selector_flag("commit:0123456789abcdef0123456789abcdef01234567"),
            "--commit 0123456789abcdef0123456789abcdef01234567"
        );
        // Unexpected shapes fall back to the raw reference rather than panicking.
        assert_eq!(selector_flag("weird"), "weird");
    }
}
