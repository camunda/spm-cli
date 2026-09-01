//! Shared clap arg groups and small helpers used across multiple subcommands.

use crate::scope::Scope;
use anyhow::Result;
use clap::Args;
use std::path::Path;

#[derive(Args)]
#[group(multiple = false)]
pub(crate) struct VersionArg {
    #[arg(long)]
    pub(crate) tag: Option<String>,
    #[arg(long)]
    pub(crate) branch: Option<String>,
    #[arg(long)]
    pub(crate) commit: Option<String>,
}

/// The `-g/--global` selector shared by every scope-aware subcommand. Operates
/// on the global manifest under `$SPM_HOME` and user-global vendor locations
/// instead of the current project.
#[derive(Args)]
pub(crate) struct ScopeArg {
    /// Operate on the user-global scope ($SPM_HOME manifest, user-global vendor
    /// locations) instead of the current project.
    #[arg(short = 'g', long = "global")]
    pub(crate) global: bool,
}

impl ScopeArg {
    pub(crate) fn resolve(&self, cwd: &Path) -> Scope {
        Scope::new(self.global, cwd.to_path_buf())
    }
}

/// Ask a yes/no question on stdin, defaulting to no. A plain line read (not a
/// raw-terminal TUI) so it stays portable and is drivable from tests with piped
/// input, matching `prompt_targets`.
pub(crate) fn confirm(question: &str) -> Result<bool> {
    use std::io::{self, BufRead, Write};
    print!("{question} [y/N]: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let answer = line.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Human-readable byte size using binary (1024) units.
pub(crate) fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// Derive a skill name from a repo URL. Handles https, `ssh://`, and scp-style
/// (`git@host:org/repo.git`) forms by splitting on both `/` and the scp `:`.
pub(crate) fn default_name(git: &str) -> String {
    // Split on `\` too: a `file://` URL to a Windows path (or a `--path` on
    // Windows) uses backslash separators, and the basename must not keep them
    // or it fails `validate_skill_name`.
    git.trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or(git)
        .trim_end_matches(".git")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::{default_name, human_size};

    #[test]
    fn human_size_scales_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KiB");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(1024 * 1024), "1.0 MiB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }

    #[test]
    fn default_name_handles_url_forms() {
        assert_eq!(default_name("https://github.com/org/repo"), "repo");
        assert_eq!(default_name("https://github.com/org/repo.git"), "repo");
        assert_eq!(default_name("git@github.com:org/repo.git"), "repo");
        assert_eq!(default_name("ssh://git@host/org/repo.git"), "repo");
        assert_eq!(default_name("git@host:repo.git"), "repo");
    }

    /// spm is host-agnostic: it never detects or special-cases a hosting
    /// provider, so any git remote — Bitbucket, GitLab, self-hosted — parses
    /// through the same URL forms as GitHub. This locks that guarantee in.
    #[test]
    fn default_name_is_host_agnostic() {
        // Bitbucket Cloud
        assert_eq!(default_name("https://bitbucket.org/org/repo.git"), "repo");
        assert_eq!(default_name("git@bitbucket.org:org/repo.git"), "repo");
        assert_eq!(default_name("ssh://git@bitbucket.org/org/repo.git"), "repo");
        // GitLab
        assert_eq!(default_name("https://gitlab.com/org/repo.git"), "repo");
        assert_eq!(default_name("git@gitlab.com:org/repo.git"), "repo");
        // Self-hosted (Bitbucket Server / GitLab CE / plain git over ssh)
        assert_eq!(
            default_name("ssh://git@git.internal.example.com:7999/proj/repo.git"),
            "repo"
        );
    }

    #[test]
    fn default_name_handles_subpaths() {
        assert_eq!(default_name("skills/camunda-feel"), "camunda-feel");
        assert_eq!(default_name("skills/camunda-feel/"), "camunda-feel");
        assert_eq!(default_name("camunda-feel"), "camunda-feel");
    }

    #[test]
    fn default_name_handles_windows_separators() {
        assert_eq!(default_name(r"C:\Users\me\skill"), "skill");
        assert_eq!(default_name(r"file://C:\tmp\repo\"), "repo");
        assert_eq!(default_name(r"pack\alpha"), "alpha");
    }

    #[test]
    fn default_name_dot_paths_are_not_valid_names() {
        // `--path .` / `foo/.` yield "." — `add` must reject this as a name and
        // fall back to the git URL basename instead of erroring.
        for p in [".", "foo/.", ""] {
            let derived = default_name(p);
            assert!(
                crate::manifest::validate_skill_name(&derived).is_err(),
                "expected `{derived}` (from `{p}`) to be an invalid skill name"
            );
        }
    }
}
