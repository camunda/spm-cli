use crate::lockfile::Lockfile;
use crate::manifest::{Manifest, SkillSpec};
use crate::vendor::{self, MaterializedSkill};
use crate::{resolver, store};
use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Parser)]
#[command(name = "spm", version, about = "Manage AI skills declared in ai.json")]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new ai.json in the current directory.
    Init {
        /// Target vendor(s): claude and/or copilot. Repeatable or comma-separated.
        #[arg(long = "target", value_delimiter = ',', default_value = "claude")]
        targets: Vec<String>,
    },
    /// Add a skill dependency and install it.
    Add {
        /// Git repository URL.
        git: String,
        #[command(flatten)]
        version: VersionArg,
        /// Subdirectory within the repo (monorepo of skills).
        #[arg(long)]
        path: Option<String>,
        /// Local name for the skill (defaults to the repo name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Remove a skill dependency.
    Remove { name: String },
    /// Re-resolve branches/tags to latest commits.
    Update {
        /// Skill to update; omit to update all.
        name: Option<String>,
    },
    /// Fetch + materialize everything from ai.lock (use after cloning).
    Install,
    /// List declared skills and their locked commits.
    List,
    /// Remove all generated vendor config for this project.
    Clean,
}

#[derive(Args)]
#[group(multiple = false)]
struct VersionArg {
    #[arg(long)]
    tag: Option<String>,
    #[arg(long)]
    branch: Option<String>,
    #[arg(long)]
    commit: Option<String>,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let root = std::env::current_dir()?;
    match cli.command {
        Command::Init { targets } => init(&root, targets),
        Command::Add {
            git,
            version,
            path,
            name,
        } => add(&root, git, version, path, name),
        Command::Remove { name } => remove(&root, &name),
        Command::Update { name } => update(&root, name),
        Command::Install => {
            let n = sync(&root, false, None)?;
            println!("installed {n} skill(s)");
            Ok(())
        }
        Command::List => list(&root),
        Command::Clean => clean(&root),
    }
}

fn clean(root: &Path) -> Result<()> {
    let manifest = Manifest::load(root)?;
    let lock = Lockfile::load_or_default(root)?;
    for target in &manifest.targets {
        vendor::for_target(target)?.clean(root, &lock.id)?;
    }
    println!(
        "cleaned generated config for target(s): {}",
        manifest.targets.join(", ")
    );
    Ok(())
}

fn init(root: &Path, targets: Vec<String>) -> Result<()> {
    if Manifest::exists(root) {
        bail!("{} already exists", Manifest::path_in(root).display());
    }
    if targets.is_empty() {
        bail!("at least one --target is required");
    }
    for target in &targets {
        vendor::for_target(target)?; // validate targets early
    }
    let manifest = Manifest {
        targets,
        skills: BTreeMap::new(),
    };
    manifest.save(root)?;
    println!("created {}", Manifest::path_in(root).display());
    Ok(())
}

fn add(
    root: &Path,
    git: String,
    version: VersionArg,
    path: Option<String>,
    name: Option<String>,
) -> Result<()> {
    let mut manifest = Manifest::load(root)?;
    let name = name.unwrap_or_else(|| default_name(&git));
    crate::manifest::validate_skill_name(&name)?;
    if let Some(sub) = &path {
        crate::manifest::validate_subpath(sub)?;
    }
    let spec = SkillSpec {
        git,
        tag: version.tag,
        branch: version.branch,
        commit: version.commit,
        path,
    };
    spec.version()?; // validate exactly one selector
    manifest.skills.insert(name.clone(), spec);
    manifest.save(root)?;
    sync(root, false, None)?;
    println!("added {name}");
    Ok(())
}

fn remove(root: &Path, name: &str) -> Result<()> {
    let mut manifest = Manifest::load(root)?;
    if manifest.skills.remove(name).is_none() {
        bail!(
            "no skill named `{name}` in {}",
            Manifest::path_in(root).display()
        );
    }
    manifest.save(root)?;
    sync(root, false, None)?;
    println!("removed {name}");
    Ok(())
}

fn update(root: &Path, name: Option<String>) -> Result<()> {
    sync(root, true, name.as_deref())?;
    println!("updated");
    Ok(())
}

fn list(root: &Path) -> Result<()> {
    let manifest = Manifest::load(root)?;
    let lock = Lockfile::load_or_default(root)?;
    if manifest.skills.is_empty() {
        println!("no skills declared");
        return Ok(());
    }
    for (name, spec) in &manifest.skills {
        let pinned = lock
            .skills
            .get(name)
            .map(|l| format!("{} @ {}", l.reference, &l.commit[..l.commit.len().min(8)]))
            .unwrap_or_else(|| "not installed".into());
        println!("{name:<24} {}  ({pinned})", spec.git);
    }
    Ok(())
}

/// Core pipeline shared by add/remove/update/install:
/// resolve manifest -> ai.lock -> populate store -> materialize vendor config.
///
/// `force_refresh` re-resolves refs to their latest commit; `only` limits the
/// refresh to a single skill (used by `update <name>`).
fn sync(root: &Path, force_refresh: bool, only: Option<&str>) -> Result<usize> {
    let manifest = Manifest::load(root)?;
    if manifest.targets.is_empty() {
        bail!("ai.json declares no targets");
    }
    // Validate all targets up front before doing any network work.
    let vendors = manifest
        .targets
        .iter()
        .map(|t| vendor::for_target(t))
        .collect::<Result<Vec<_>>>()?;
    let mut prev = Lockfile::load_or_default(root)?;

    // Stable, path-independent project id: reuse the locked one, else mint & persist.
    let id = if prev.id.is_empty() {
        crate::lockfile::generate_id(root)
    } else {
        prev.id.clone()
    };
    let mut lock = Lockfile {
        id,
        ..Default::default()
    };

    // Column width for aligned per-skill output.
    let width = manifest.skills.keys().map(String::len).max().unwrap_or(0);

    if manifest.skills.is_empty() {
        println!("no skills declared");
    } else {
        println!(
            "resolving {} skill(s) for: {}",
            manifest.skills.len(),
            manifest.targets.join(", ")
        );
    }

    for (name, spec) in &manifest.skills {
        let requested = spec.version()?; // validates selectors
        let reference = requested.label();
        let refresh = force_refresh && only.is_none_or(|o| o == name);

        // Reuse the pinned commit if the request is unchanged and we're not refreshing.
        let reuse = prev.skills.remove(name).filter(|l| {
            !refresh && l.git == spec.git && l.reference == reference && l.path == spec.path
        });

        let (locked, how) = match reuse {
            Some(l) => (l, "locked"),
            None => (
                resolver::resolve(spec).with_context(|| format!("resolving skill `{name}`"))?,
                "resolved",
            ),
        };
        let short = &locked.commit[..locked.commit.len().min(8)];
        println!("  {name:<width$}  {reference} @ {short}  ({how})");
        lock.skills.insert(name.clone(), locked);
    }

    lock.save(root)?;

    // Populate the store and collect absolute paths for the vendor.
    if !manifest.skills.is_empty() {
        println!("fetching skills into store");
    }
    let mut materialized: Vec<MaterializedSkill> = Vec::new();
    for (name, locked) in &lock.skills {
        let ensured = store::ensure(locked).with_context(|| format!("fetching skill `{name}`"))?;
        println!(
            "  {name:<width$}  {}",
            if ensured.fetched { "fetched" } else { "cached" }
        );
        // Single source of truth for the "is this actually a loadable skill?"
        // check — run once here rather than per-vendor.
        crate::skillcheck::warn_if_not_loadable(
            name,
            &locked.git,
            locked.path.as_deref(),
            &ensured.path,
        );
        materialized.push(MaterializedSkill {
            name: name.clone(),
            path: ensured.path,
        });
    }

    // Same resolved skills projected into every configured vendor.
    if !manifest.skills.is_empty() {
        println!("materializing: {}", manifest.targets.join(", "));
    }
    for vendor in &vendors {
        vendor.materialize(root, &lock.id, &materialized)?;
    }
    Ok(lock.skills.len())
}

/// Derive a skill name from a repo URL. Handles https, `ssh://`, and scp-style
/// (`git@host:org/repo.git`) forms by splitting on both `/` and the scp `:`.
fn default_name(git: &str) -> String {
    git.trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or(git)
        .trim_end_matches(".git")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::default_name;

    #[test]
    fn default_name_handles_url_forms() {
        assert_eq!(default_name("https://github.com/org/repo"), "repo");
        assert_eq!(default_name("https://github.com/org/repo.git"), "repo");
        assert_eq!(default_name("git@github.com:org/repo.git"), "repo");
        assert_eq!(default_name("ssh://git@host/org/repo.git"), "repo");
        assert_eq!(default_name("git@host:repo.git"), "repo");
    }
}
