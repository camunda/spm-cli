use crate::lockfile::Lockfile;
use crate::manifest::{Manifest, SkillSpec};
use crate::vendor::{self, MaterializedSkill};
use crate::{paths, resolver, store};
use anyhow::{anyhow, bail, Context, Result};
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
        /// Treat the target directory as a container and add every skill inside
        /// it (each immediate subdirectory with a SKILL.md), rather than one
        /// skill. Incompatible with --name (names are derived per sub-skill).
        #[arg(long, conflicts_with = "name")]
        all: bool,
        /// Overwrite existing skill(s) of the same name instead of erroring.
        /// Applies to both a single add and --all.
        #[arg(long)]
        force: bool,
    },
    /// Manage the target vendors declared in ai.json.
    Target {
        #[command(subcommand)]
        command: TargetCommand,
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
    /// Show which declared skills are materialized in this checkout.
    Status,
    /// Remove all generated vendor config for this project.
    Clean,
    /// Remove everything from the global store (fetch cache). Frees disk by
    /// deleting every cached repo@commit; they re-fetch on demand on the next
    /// `install`. Prompts for confirmation unless `--yes` is given.
    Prune {
        /// Skip the confirmation prompt (for scripts/CI).
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum TargetCommand {
    /// Add one or more target vendors to ai.json (and materialize skills for
    /// them). With no vendor given, pick interactively from the vendors not yet
    /// configured.
    Add {
        /// Vendor(s) to add: claude and/or copilot. Repeatable or
        /// comma-separated. Omit to choose interactively.
        #[arg(value_delimiter = ',')]
        vendors: Vec<String>,
    },
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
            all,
            force,
        } => add(&root, git, version, path, name, all, force),
        Command::Target { command } => match command {
            TargetCommand::Add { vendors } => target_add(&root, vendors),
        },
        Command::Remove { name } => remove(&root, &name),
        Command::Update { name } => update(&root, name),
        Command::Install => {
            let n = sync(&root, false, None)?;
            println!("installed {n} skill(s)");
            Ok(())
        }
        Command::List => list(&root),
        Command::Status => status(&root),
        Command::Clean => clean(&root),
        Command::Prune { yes } => prune(yes),
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

/// `spm prune`: wipe the whole global store (`$SPM_HOME/store`, default
/// `~/.spm/store`). Unlike `clean` (project-local vendor config), this is global
/// and shared across every project, so it always confirms first unless `--yes`
/// is passed. The store is a pure cache — anything removed re-fetches on the
/// next `install`.
fn prune(yes: bool) -> Result<()> {
    let dir = paths::store_dir()?;
    // Gate on total emptiness, not the checkout count: a store holding only
    // stray files still has something to remove, and prune removes everything.
    if store::is_empty()? {
        println!("store is empty — nothing to prune ({})", dir.display());
        return Ok(());
    }
    if !yes {
        // Show the checkout count (a cheap shallow read) in the prompt, but
        // defer the recursive size walk until after confirmation — answering
        // `n` on a large store should be instant, not pay for a full du.
        println!(
            "store holds {} cached checkout(s) in {}",
            store::checkout_count()?,
            dir.display()
        );
        if !confirm("Remove everything from the store?")? {
            println!("aborted — nothing removed");
            return Ok(());
        }
    }
    // Size only now — once we know we're deleting — so `freed ~X` stays
    // accurate without slowing down an abort.
    let stats = store::stats()?;
    store::remove_all()?;
    println!(
        "pruned {} cached checkout(s), freed ~{}",
        stats.entries,
        human_size(stats.bytes)
    );
    Ok(())
}

/// Ask a yes/no question on stdin, defaulting to no. A plain line read (not a
/// raw-terminal TUI) so it stays portable and is drivable from tests with piped
/// input, matching `prompt_targets`.
fn confirm(question: &str) -> Result<bool> {
    use std::io::{self, BufRead, Write};
    print!("{question} [y/N]: ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let answer = line.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Human-readable byte size using binary (1024) units.
fn human_size(bytes: u64) -> String {
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

fn init(root: &Path, targets: Vec<String>) -> Result<()> {
    // Validate --target before anything else so typos are caught regardless of
    // whether the project is already initialized — otherwise the idempotent
    // early-return below would silently accept a bogus target in scripts.
    if targets.is_empty() {
        bail!("at least one --target is required");
    }
    for target in &targets {
        vendor::for_target(target)?; // validate targets early
    }
    // Re-running `spm init` in an already-initialized project is a harmless
    // no-op, not an error: leave the existing manifest untouched and tell the
    // user rather than failing the command.
    if Manifest::exists(root) {
        println!(
            "{} already exists — leaving it untouched",
            Manifest::path_in(root).display()
        );
        return Ok(());
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
    all: bool,
    force: bool,
) -> Result<()> {
    let mut manifest = Manifest::load(root)?;
    if let Some(sub) = &path {
        crate::manifest::validate_subpath(sub)?;
    }
    if all {
        add_all(root, &mut manifest, git, version, path, force)?;
    } else {
        let name = name.unwrap_or_else(|| {
            // Prefer the `--path` basename, but fall back to the git URL when the
            // path yields nothing usable as a skill name (e.g. `.`, `foo/.`, ``).
            path.as_deref()
                .map(default_name)
                .filter(|n| crate::manifest::validate_skill_name(n).is_ok())
                .unwrap_or_else(|| default_name(&git))
        });
        crate::manifest::validate_skill_name(&name)?;
        // Never clobber an existing entry silently: adding a name that already
        // exists is an error unless the user opts in with --force (e.g. to
        // re-pin a version).
        if !force && manifest.skills.contains_key(&name) {
            bail!(
                "a skill named `{name}` already exists in {}; either give this one \
                 a different name with `--name <other>`, pass --force to overwrite \
                 the existing entry, or `spm remove {name}` first",
                Manifest::path_in(root).display()
            );
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
    }
    Ok(())
}

/// `spm add --all`: treat `path` as a *container* and add every skill inside it
/// (each immediate subdirectory carrying a `SKILL.md`) as its own manifest entry,
/// keyed by the subdirectory name. This is the one-shot equivalent of the
/// per-sub-skill `spm add … --path <sub> --name <sub>` commands that the
/// container warning suggests — so it enumerates sub-skills through the same
/// `skillcheck::child_skills` source of truth, guaranteeing the two never drift.
fn add_all(
    root: &Path,
    manifest: &mut Manifest,
    git: String,
    version: VersionArg,
    path: Option<String>,
    force: bool,
) -> Result<()> {
    // Version selector is validated once for the container; every derived entry
    // reuses it verbatim.
    let container = SkillSpec {
        git: git.clone(),
        tag: version.tag,
        branch: version.branch,
        commit: version.commit,
        path: path.clone(),
    };
    container.version()?;

    // Fetch the container into the store so we can enumerate its sub-skills.
    let locked =
        resolver::resolve(&container).context("resolving container to enumerate skills")?;
    let ensured = store::ensure(&locked).context("fetching container to enumerate skills")?;
    let subs = crate::skillcheck::child_skills(&ensured.path);
    if subs.is_empty() {
        bail!(
            "--path `{}` is not a container of skills (no immediate subdirectory has a SKILL.md). \
             Drop --all to add it as a single skill.",
            path.as_deref().unwrap_or(".")
        );
    }

    // Reject the whole batch on the first collision so the manifest is never
    // left half-populated — unless --force opts into overwriting existing
    // entries. Name validation still runs regardless.
    for sub in &subs {
        crate::manifest::validate_skill_name(sub)?;
        if !force && manifest.skills.contains_key(sub) {
            bail!(
                "a skill named `{sub}` already exists in {}; either pass --force to \
                 overwrite the existing entries, or `spm remove {sub}` first",
                Manifest::path_in(root).display()
            );
        }
    }

    for sub in &subs {
        let subpath = crate::skillcheck::join_subpath(path.as_deref(), sub);
        manifest.skills.insert(
            sub.clone(),
            SkillSpec {
                git: git.clone(),
                tag: container.tag.clone(),
                branch: container.branch.clone(),
                commit: container.commit.clone(),
                path: Some(subpath),
            },
        );
    }
    manifest.save(root)?;
    sync(root, false, None)?;
    println!("added {} skill(s): {}", subs.len(), subs.join(", "));
    Ok(())
}

/// `spm target add [vendors...]`: append target vendors to an existing ai.json
/// and materialize skills for them. Explicit `vendors` are validated and added
/// verbatim; with none given, the vendors *not yet configured* are offered as an
/// interactive numbered picker. Adding an already-configured target is a skip,
/// not an error, so the command is safe to re-run.
fn target_add(root: &Path, vendors: Vec<String>) -> Result<()> {
    let mut manifest = Manifest::load(root)?;

    let requested = if vendors.is_empty() {
        let available: Vec<&str> = vendor::ALL_TARGETS
            .iter()
            .copied()
            .filter(|t| !manifest.targets.iter().any(|cur| cur == t))
            .collect();
        if available.is_empty() {
            println!(
                "all supported targets already configured: {}",
                manifest.targets.join(", ")
            );
            return Ok(());
        }
        prompt_targets(&available)?
    } else {
        // Validate every explicitly-requested vendor up front so a typo aborts
        // before the manifest is touched.
        for v in &vendors {
            vendor::for_target(v)?;
        }
        vendors
    };

    let mut added = Vec::new();
    for v in requested {
        if manifest.targets.iter().any(|cur| cur == &v) {
            println!("target `{v}` already configured — skipping");
            continue;
        }
        manifest.targets.push(v.clone());
        added.push(v);
    }

    if added.is_empty() {
        println!("no new targets added");
        return Ok(());
    }

    manifest.save(root)?;
    // Materialize existing skills into the newly-added vendor(s).
    sync(root, false, None)?;
    println!("added target(s): {}", added.join(", "));
    Ok(())
}

/// Interactive numbered picker over the vendors not yet configured. Reads a
/// comma-separated list of 1-based indices (or `all`) from stdin — a plain line
/// read, not a raw-terminal TUI, so it stays portable across Windows and Unix
/// shells and is drivable from tests with piped input.
fn prompt_targets(available: &[&str]) -> Result<Vec<String>> {
    use std::io::{self, BufRead, Write};

    println!("Select target(s) to add:");
    for (i, t) in available.iter().enumerate() {
        println!("  {}) {t}", i + 1);
    }
    print!("Enter numbers (comma-separated), or `all`: ");
    io::stdout().flush()?;

    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let line = line.trim();
    if line.is_empty() {
        bail!("no target selected");
    }
    if line.eq_ignore_ascii_case("all") {
        return Ok(available.iter().map(|s| s.to_string()).collect());
    }

    // Preserve the user's typed order while dropping duplicate picks.
    let mut chosen = Vec::new();
    for tok in line.split(',') {
        let tok = tok.trim();
        let n: usize = tok
            .parse()
            .map_err(|_| anyhow!("invalid selection `{tok}`: enter numbers from the list"))?;
        let vendor = available
            .get(
                n.checked_sub(1)
                    .ok_or_else(|| anyhow!("invalid selection `{tok}`: numbers start at 1"))?,
            )
            .ok_or_else(|| anyhow!("invalid selection `{tok}`: no such option"))?
            .to_string();
        if !chosen.contains(&vendor) {
            chosen.push(vendor);
        }
    }
    Ok(chosen)
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

/// Show, per target, which declared skills are materialized in the current
/// checkout. Because spm materializes into gitignored dirs, a fresh clone or a
/// new git worktree sees nothing until `spm install` runs inside it — this
/// command surfaces exactly that, and exits non-zero when anything is missing so
/// it doubles as an automated gate.
fn status(root: &Path) -> Result<()> {
    let manifest = Manifest::load(root)?;
    let lock = Lockfile::load_or_default(root)?;
    let expected: Vec<String> = lock.skills.keys().cloned().collect();

    println!("project: {}", root.display());
    println!("targets: {}", manifest.targets.join(", "));

    // Nothing locked: either nothing is declared (a clean, correct state) or
    // ai.json declares skills that were never resolved into ai.lock (a real
    // problem). Both cases have nothing per-target to inspect, so report and
    // return instead of falling through to a misleading "all materialized".
    if expected.is_empty() {
        if manifest.skills.is_empty() {
            println!("\nno skills declared — add one with `spm add <git-url>`");
            return Ok(());
        }
        bail!(
            "ai.json declares {} skill(s) but ai.lock has none — run `spm install` here",
            manifest.skills.len()
        );
    }

    let width = expected.iter().map(String::len).max().unwrap_or(0);
    let mut incomplete = false;

    for target in &manifest.targets {
        let vendor = vendor::for_target(target)?;
        let st = vendor.status(root, &expected)?;
        println!(
            "\n[{target}]  {}/{} installed  {}",
            st.present.len(),
            expected.len(),
            st.location.display()
        );
        for name in &expected {
            let missing = st.missing.iter().any(|m| m == name);
            if missing {
                incomplete = true;
            }
            println!(
                "  {name:<width$}  {}",
                if missing { "MISSING" } else { "ok" }
            );
        }
        for s in &st.stale {
            println!("  {s:<width$}  stale (not in ai.lock)");
        }
        for note in &st.notes {
            incomplete = true;
            println!("  ! {note}");
        }
    }

    if incomplete {
        bail!(
            "some declared skills are not materialized in this checkout — run `spm install` here"
        );
    }
    println!("\nall declared skills are materialized in this checkout");
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
            &locked.reference,
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
