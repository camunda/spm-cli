use crate::lockfile::Lockfile;
use crate::manifest::{Manifest, SkillSpec};
use crate::scope::Scope;
use crate::vendor::{self, MaterializedPlugin, MaterializedSkill};
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
    /// Create a new ai.json in the current directory (or, with --global, under
    /// $SPM_HOME to manage skills shared across every project).
    Init {
        /// Target vendor(s): amp, claude, cline, codex, copilot, cursor, gemini and/or windsurf. Repeatable or comma-separated.
        #[arg(long = "target", value_delimiter = ',', default_value = "claude")]
        targets: Vec<String>,
        #[command(flatten)]
        scope: ScopeArg,
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
        /// Add as a full **plugin** dependency (agents, MCP servers, hooks,
        /// scripts + bundled skills) rather than a single skill. `--path` should
        /// point at the plugin root (the dir holding `.claude-plugin/plugin.json`).
        /// Incompatible with --all.
        #[arg(long, conflicts_with = "all")]
        plugin: bool,
        /// Overwrite existing skill(s) of the same name instead of erroring.
        /// Applies to both a single add and --all.
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        scope: ScopeArg,
    },
    /// Manage the target vendors declared in ai.json.
    Target {
        #[command(subcommand)]
        command: TargetCommand,
    },
    /// Remove a skill (or, with --plugin, a plugin) dependency.
    Remove {
        name: String,
        /// Remove a plugin dependency instead of a skill.
        #[arg(long)]
        plugin: bool,
        #[command(flatten)]
        scope: ScopeArg,
    },
    /// Re-resolve branches/tags to latest commits.
    Update {
        /// Skill to update; omit to update all.
        name: Option<String>,
        #[command(flatten)]
        scope: ScopeArg,
    },
    /// Fetch + materialize everything from ai.lock (use after cloning).
    Install {
        #[command(flatten)]
        scope: ScopeArg,
    },
    /// List declared skills and their locked commits.
    List {
        #[command(flatten)]
        scope: ScopeArg,
    },
    /// Show which declared skills are materialized in this checkout.
    Status {
        #[command(flatten)]
        scope: ScopeArg,
    },
    /// Remove all generated vendor config for this project.
    Clean {
        #[command(flatten)]
        scope: ScopeArg,
    },
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
        /// Vendor(s) to add: amp, claude, cline, codex, copilot, cursor, gemini and/or
        /// windsurf. Repeatable or comma-separated. Omit to choose interactively.
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

/// The `-g/--global` selector shared by every scope-aware subcommand. Operates
/// on the global manifest under `$SPM_HOME` and user-global vendor locations
/// instead of the current project.
#[derive(Args)]
struct ScopeArg {
    /// Operate on the user-global scope ($SPM_HOME manifest, user-global vendor
    /// locations) instead of the current project.
    #[arg(short = 'g', long = "global")]
    global: bool,
}

impl ScopeArg {
    fn resolve(&self, cwd: &Path) -> Scope {
        Scope::new(self.global, cwd.to_path_buf())
    }
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    match cli.command {
        Command::Init { targets, scope } => init(&scope.resolve(&cwd), targets),
        Command::Add {
            git,
            version,
            path,
            name,
            all,
            plugin,
            force,
            scope,
        } => add(
            &scope.resolve(&cwd),
            AddRequest {
                git,
                version,
                path,
                name,
                all,
                plugin,
                force,
            },
        ),
        Command::Target { command } => match command {
            TargetCommand::Add { vendors } => target_add(&Scope::Project { root: cwd }, vendors),
        },
        Command::Remove {
            name,
            plugin,
            scope,
        } => remove(&scope.resolve(&cwd), &name, plugin),
        Command::Update { name, scope } => update(&scope.resolve(&cwd), name),
        Command::Install { scope } => {
            let scope = scope.resolve(&cwd);
            let n = sync(&scope, false, None)?;
            let noun = if n == 1 { "dependency" } else { "dependencies" };
            println!("installed {n} {noun}");
            Ok(())
        }
        Command::List { scope } => list(&scope.resolve(&cwd)),
        Command::Status { scope } => status(&scope.resolve(&cwd)),
        Command::Clean { scope } => clean(&scope.resolve(&cwd)),
        Command::Prune { yes } => prune(yes),
    }
}

fn clean(scope: &Scope) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir)?;
    let lock = Lockfile::load_or_default(&dir)?;
    let managed: Vec<String> = lock.skills.keys().cloned().collect();
    let managed_plugins: Vec<String> = lock.plugins.keys().cloned().collect();
    for target in &manifest.targets {
        let vendor = vendor::for_target(target)?;
        vendor.clean(scope, &lock.id, &managed)?;
        vendor.clean_plugins(scope, &lock.id, &managed_plugins)?;
    }
    println!(
        "cleaned generated {} config for target(s): {}",
        scope.label(),
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

fn init(scope: &Scope, targets: Vec<String>) -> Result<()> {
    // Validate --target before anything else so typos are caught regardless of
    // whether the project is already initialized — otherwise the idempotent
    // early-return below would silently accept a bogus target in scripts.
    if targets.is_empty() {
        bail!("at least one --target is required");
    }
    for target in &targets {
        vendor::for_target(target)?; // validate targets early
    }
    let dir = scope.manifest_dir()?;
    // The global manifest dir ($SPM_HOME) may not exist yet on a first run.
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    // Re-running `spm init` in an already-initialized scope is a harmless
    // no-op, not an error: leave the existing manifest untouched and tell the
    // user rather than failing the command.
    if Manifest::exists(&dir) {
        println!(
            "{} already exists — leaving it untouched",
            Manifest::path_in(&dir).display()
        );
        return Ok(());
    }
    let manifest = Manifest {
        targets,
        skills: BTreeMap::new(),
        plugins: BTreeMap::new(),
    };
    manifest.save(&dir)?;
    println!("created {}", Manifest::path_in(&dir).display());
    Ok(())
}

/// The parsed arguments for `spm add`, grouped so the handler takes one request
/// rather than a long positional argument list.
struct AddRequest {
    git: String,
    version: VersionArg,
    path: Option<String>,
    name: Option<String>,
    all: bool,
    plugin: bool,
    force: bool,
}

fn add(scope: &Scope, req: AddRequest) -> Result<()> {
    let AddRequest {
        git,
        version,
        path,
        name,
        all,
        plugin,
        force,
    } = req;
    let dir = scope.manifest_dir()?;
    let mut manifest = Manifest::load(&dir)?;
    if let Some(sub) = &path {
        crate::manifest::validate_subpath(sub)?;
    }
    if all {
        add_all(scope, &mut manifest, git, version, path, force)?;
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
        // A dependency name lives in one flat namespace shared by `skills` and
        // `plugins` (see `Manifest::load`), so a name already taken by the
        // *other* map is always a hard error — even with --force, since --force
        // only authorizes re-pinning an entry of the *same* kind, never turning
        // a skill into a plugin (or vice versa) behind the user's back. Check
        // this before `manifest.save`, otherwise we'd persist an ai.json that
        // `sync`'s re-load immediately rejects, stranding the user with an
        // invalid manifest.
        let (this_map_has, other_map_has, other_kind) = if plugin {
            (
                manifest.plugins.contains_key(&name),
                manifest.skills.contains_key(&name),
                "skill",
            )
        } else {
            (
                manifest.skills.contains_key(&name),
                manifest.plugins.contains_key(&name),
                "plugin",
            )
        };
        if other_map_has {
            bail!(
                "a {other_kind} named `{name}` already exists in {}; dependency names \
                 must be unique across skills and plugins — pick a different name with \
                 `--name <other>` or `spm remove` the existing {other_kind} first",
                Manifest::path_in(&dir).display()
            );
        }
        // Never clobber an existing entry silently: adding a name that already
        // exists is an error unless the user opts in with --force (e.g. to
        // re-pin a version). A plugin and a skill share neither map nor the
        // vendor `<name>` namespace question here, so each `--plugin` add is
        // checked against the plugins map and each skill add against skills.
        let kind = if plugin { "plugin" } else { "skill" };
        let occupied = this_map_has;
        if !force && occupied {
            bail!(
                "a {kind} named `{name}` already exists in {}; either give this one \
                 a different name with `--name <other>`, pass --force to overwrite \
                 the existing entry, or `spm remove {}{name}` first",
                Manifest::path_in(&dir).display(),
                if plugin { "--plugin " } else { "" }
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
        if plugin {
            manifest.plugins.insert(name.clone(), spec);
        } else {
            manifest.skills.insert(name.clone(), spec);
        }
        manifest.save(&dir)?;
        sync(scope, false, None)?;
        println!("added {kind} {name}");
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
    scope: &Scope,
    manifest: &mut Manifest,
    git: String,
    version: VersionArg,
    path: Option<String>,
    force: bool,
) -> Result<()> {
    let dir = scope.manifest_dir()?;
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
                Manifest::path_in(&dir).display()
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
    manifest.save(&dir)?;
    sync(scope, false, None)?;
    println!("added {} skill(s): {}", subs.len(), subs.join(", "));
    Ok(())
}

/// `spm target add [vendors...]`: append target vendors to an existing ai.json
/// and materialize skills for them. Explicit `vendors` are validated and added
/// verbatim; with none given, the vendors *not yet configured* are offered as an
/// interactive numbered picker. Adding an already-configured target is a skip,
/// not an error, so the command is safe to re-run.
fn target_add(scope: &Scope, vendors: Vec<String>) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let mut manifest = Manifest::load(&dir)?;

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

    manifest.save(&dir)?;
    // Materialize existing skills into the newly-added vendor(s).
    sync(scope, false, None)?;
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

fn remove(scope: &Scope, name: &str, plugin: bool) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let mut manifest = Manifest::load(&dir)?;
    let (removed, kind) = if plugin {
        (manifest.plugins.remove(name).is_some(), "plugin")
    } else {
        (manifest.skills.remove(name).is_some(), "skill")
    };
    if !removed {
        bail!(
            "no {kind} named `{name}` in {}",
            Manifest::path_in(&dir).display()
        );
    }
    manifest.save(&dir)?;
    sync(scope, false, None)?;
    println!("removed {kind} {name}");
    Ok(())
}

fn update(scope: &Scope, name: Option<String>) -> Result<()> {
    sync(scope, true, name.as_deref())?;
    println!("updated");
    Ok(())
}

fn list(scope: &Scope) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir)?;
    let lock = Lockfile::load_or_default(&dir)?;
    if manifest.skills.is_empty() && manifest.plugins.is_empty() {
        println!("no skills or plugins declared");
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
    for (name, spec) in &manifest.plugins {
        let pinned = lock
            .plugins
            .get(name)
            .map(|l| {
                let short = &l.commit[..l.commit.len().min(8)];
                if l.bundled_skills.is_empty() {
                    format!("{} @ {short}", l.reference)
                } else {
                    format!(
                        "{} @ {short}, skills: {}",
                        l.reference,
                        l.bundled_skills.join(", ")
                    )
                }
            })
            .unwrap_or_else(|| "not installed".into());
        println!("{name:<24} {}  (plugin; {pinned})", spec.git);
    }
    Ok(())
}

/// Show, per target, which declared skills are materialized for this scope.
/// Because spm materializes into gitignored dirs, a fresh clone or a new git
/// worktree sees nothing until `spm install` runs inside it — this command
/// surfaces exactly that, and exits non-zero when anything is missing so it
/// doubles as an automated gate.
fn status(scope: &Scope) -> Result<()> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir)?;
    let lock = Lockfile::load_or_default(&dir)?;
    // Plugin-bundled skills are flattened into every vendor's skills dir just
    // like standalone skills, so they belong in `expected` — otherwise `status`
    // would flag them as stale (present on disk, not declared).
    let mut expected: Vec<String> = lock.skills.keys().cloned().collect();
    for l in lock.plugins.values() {
        expected.extend(l.bundled_skills.iter().cloned());
    }
    expected.sort();
    expected.dedup();

    match scope {
        Scope::Project { root } => println!("project: {}", root.display()),
        Scope::Global => println!("global: {}", dir.display()),
    }
    println!("targets: {}", manifest.targets.join(", "));

    // Warn when a skill name is materialized in *both* scopes: the vendor
    // discovery layer keys by name (`/spm:foo`, `.agents/skills/foo`), so a
    // project skill and a global skill of the same name collide/shadow at
    // runtime. Surface it here rather than letting the tool silently pick one.
    for shadow in shadowed_names(scope, &expected)? {
        let other = if scope.is_global() {
            "project"
        } else {
            "global"
        };
        println!("! `{shadow}` is also installed in the {other} scope — they will collide by name");
    }

    // Nothing locked and nothing declared: a clean, correct empty state.
    if expected.is_empty() && manifest.plugins.is_empty() {
        if manifest.skills.is_empty() {
            println!("\nno skills or plugins declared — add one with `spm add <git-url>`");
            return Ok(());
        }
        bail!(
            "ai.json declares {} skill(s) but ai.lock has none — run `spm install`",
            manifest.skills.len()
        );
    }

    // Declared plugins come from the *manifest* (the source of truth for what
    // should be installed). Comparing against `ai.lock` catches a plugin that
    // was declared but never resolved/installed; comparing against the on-disk
    // marketplace (via each vendor's `status_plugins`) catches a deleted
    // `.spm/claude-plugins` dir or a fresh worktree that was never installed.
    let declared_plugins: Vec<String> = manifest.plugins.keys().cloned().collect();
    let width = expected
        .iter()
        .chain(declared_plugins.iter())
        .map(String::len)
        .max()
        .unwrap_or(0);
    let mut incomplete = false;

    // A plugin declared in ai.json but absent from ai.lock was never resolved —
    // installs would be silently incomplete. Flag it explicitly.
    for name in &declared_plugins {
        if !lock.plugins.contains_key(name) {
            incomplete = true;
            println!(
                "! plugin `{name}` is declared in ai.json but not in ai.lock — run `spm install`"
            );
        }
    }

    for target in &manifest.targets {
        let vendor = vendor::for_target(target)?;
        if !expected.is_empty() {
            let st = vendor.status(scope, &expected)?;
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

        // Full-plugin materialization is only meaningful for vendors that
        // register plugins beyond their bundled skills (currently Claude);
        // `status_plugins` returns `None` for the rest.
        if !declared_plugins.is_empty() {
            if let Some(st) = vendor.status_plugins(scope, &declared_plugins)? {
                println!(
                    "\n[{target}] plugins  {}/{} installed  {}",
                    st.present.len(),
                    declared_plugins.len(),
                    st.location.display()
                );
                for name in &declared_plugins {
                    let missing = st.missing.iter().any(|m| m == name);
                    if missing {
                        incomplete = true;
                    }
                    println!(
                        "  {name:<width$}  {}",
                        if missing { "MISSING" } else { "ok" }
                    );
                }
                for note in &st.notes {
                    incomplete = true;
                    println!("  ! {note}");
                }
            }
        }
    }

    if incomplete {
        bail!("some declared skills or plugins are not materialized — run `spm install`");
    }
    println!("\nall declared skills and plugins are materialized");
    Ok(())
}

/// Skill names in `expected` that are *also* locked in the opposite scope. Used
/// by `status` to warn about global/project name collisions. Returns an empty
/// list when the opposite scope has no lockfile.
fn shadowed_names(scope: &Scope, expected: &[String]) -> Result<Vec<String>> {
    let other_dir = match scope {
        Scope::Project { .. } => paths::spm_home()?,
        Scope::Global => std::env::current_dir()?,
    };
    let other = Lockfile::load_or_default(&other_dir)?;
    Ok(expected
        .iter()
        .filter(|n| other.skills.contains_key(*n))
        .cloned()
        .collect())
}

/// Core pipeline shared by add/remove/update/install:
/// resolve manifest -> ai.lock -> populate store -> materialize vendor config.
///
/// `force_refresh` re-resolves refs to their latest commit; `only` limits the
/// refresh to a single skill (used by `update <name>`).
fn sync(scope: &Scope, force_refresh: bool, only: Option<&str>) -> Result<usize> {
    let dir = scope.manifest_dir()?;
    let manifest = Manifest::load(&dir)?;
    if manifest.targets.is_empty() {
        bail!("ai.json declares no targets");
    }
    // Validate all targets up front before doing any network work.
    let vendors = manifest
        .targets
        .iter()
        .map(|t| vendor::for_target(t))
        .collect::<Result<Vec<_>>>()?;
    let mut prev = Lockfile::load_or_default(&dir)?;

    // Skill names spm materialized on the previous sync. Captured before the
    // resolve loop mutates `prev`, so global/shared-dir vendors (which share a
    // directory with the user's own skills) can purge only the entries spm
    // previously owned but that were since dropped from the manifest.
    //
    // Plugin-bundled skills are flattened into the same shared skills dirs via
    // `materialize`, so the previous set must include them too — otherwise
    // removing a plugin (or shrinking its bundled skill set) would orphan those
    // skill dirs in shared locations.
    let previously_managed: Vec<String> = prev
        .skills
        .keys()
        .cloned()
        .chain(
            prev.plugins
                .values()
                .flat_map(|l| l.bundled_skills.iter().cloned()),
        )
        .collect();
    let previously_managed_plugins: Vec<String> = prev.plugins.keys().cloned().collect();

    // Stable, path-independent project id: reuse the locked one, else mint & persist.
    let id = if prev.id.is_empty() {
        crate::lockfile::generate_id(&dir)
    } else {
        prev.id.clone()
    };
    let mut lock = Lockfile {
        id,
        ..Default::default()
    };

    // Column width for aligned per-entry output (skills + plugins).
    let width = manifest
        .skills
        .keys()
        .chain(manifest.plugins.keys())
        .map(String::len)
        .max()
        .unwrap_or(0);

    if manifest.skills.is_empty() && manifest.plugins.is_empty() {
        println!("no skills or plugins declared");
    } else {
        println!(
            "resolving {} skill(s) + {} plugin(s) for: {}",
            manifest.skills.len(),
            manifest.plugins.len(),
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

    // Resolve full plugins the same way. A plugin's version reuse also depends
    // on the pinned commit being unchanged; its bundled-skill list is (re)filled
    // from the store checkout below, so we never carry a stale one forward.
    for (name, spec) in &manifest.plugins {
        let requested = spec.version()?;
        let reference = requested.label();
        let refresh = force_refresh && only.is_none_or(|o| o == name);
        let reuse = prev.plugins.remove(name).filter(|l| {
            !refresh && l.git == spec.git && l.reference == reference && l.path == spec.path
        });
        let (mut locked, how) = match reuse {
            Some(l) => (l, "locked"),
            None => (
                resolver::resolve(spec).with_context(|| format!("resolving plugin `{name}`"))?,
                "resolved",
            ),
        };
        locked.bundled_skills.clear();
        let short = &locked.commit[..locked.commit.len().min(8)];
        println!("  {name:<width$}  {reference} @ {short}  (plugin, {how})");
        lock.plugins.insert(name.clone(), locked);
    }

    // Populate the store for plugins first: we need each plugin's checkout to
    // enumerate its bundled skills before writing the lockfile (so `ai.lock`
    // records the full component set) and before flattening those skills into
    // the shared list every vendor consumes.
    let mut plugins: Vec<MaterializedPlugin> = Vec::new();
    let mut plugin_skills: Vec<MaterializedSkill> = Vec::new();
    if !manifest.plugins.is_empty() {
        println!("fetching plugins into store");
    }
    for (name, locked) in &mut lock.plugins {
        let ensured = store::ensure(locked).with_context(|| format!("fetching plugin `{name}`"))?;
        println!(
            "  {name:<width$}  {}",
            if ensured.fetched { "fetched" } else { "cached" }
        );
        if !crate::plugin::looks_like_plugin(&ensured.path) {
            eprintln!(
                "warning: plugin `{name}` ({} @ {}) has no `.claude-plugin/plugin.json`{} — \
                 is it really a plugin? (only its bundled skills, if any, will be materialized)",
                locked.git,
                locked.reference,
                locked
                    .path
                    .as_deref()
                    .map(|p| format!(" under `{p}`"))
                    .unwrap_or_default()
            );
        }
        let bundled = crate::plugin::plugin_skills(&ensured.path)
            .with_context(|| format!("enumerating skills bundled in plugin `{name}`"))?;
        locked.bundled_skills = bundled.iter().map(|s| s.name.clone()).collect();
        plugin_skills.extend(bundled);
        plugins.push(MaterializedPlugin {
            name: name.clone(),
            path: ensured.path,
        });
    }

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

    // Flatten plugin-bundled skills into the same list every vendor consumes, so
    // even skills-only targets get them. A bundled skill name that collides with
    // a standalone skill (or another plugin's skill) is a hard error: silently
    // overwriting a skill's content by manifest-key ordering is exactly the kind
    // of drift AGENTS.md's "no drift surfaces" rule forbids.
    for s in plugin_skills {
        if materialized.iter().any(|m| m.name == s.name) {
            bail!(
                "skill name collision: `{}` is provided by more than one skill/plugin — \
                 rename the conflicting skill or plugin-bundled skill",
                s.name
            );
        }
        materialized.push(s);
    }

    // Same resolved skills + plugins projected into every configured vendor.
    if !manifest.skills.is_empty() || !manifest.plugins.is_empty() {
        println!("materializing: {}", manifest.targets.join(", "));
    }
    for vendor in &vendors {
        vendor.materialize(scope, &lock.id, &materialized, &previously_managed)?;
        vendor.materialize_plugins(scope, &lock.id, &plugins, &previously_managed_plugins)?;
    }
    // Only persist `ai.lock` once every fallible step (resolution, fetching,
    // collision checks, and vendor materialization) has succeeded — otherwise
    // a failed `install` could leave behind a lockfile pointing at state that
    // was never actually materialized.
    lock.save(&dir)?;
    Ok(lock.skills.len() + lock.plugins.len())
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
    use super::{default_name, human_size, init, Scope};

    /// `clap`'s `--target` always yields at least one element (it defaults to
    /// `["claude"]` and a comma split never produces zero tokens), so the
    /// empty-`targets` guard in `init` can't be reached through the CLI parser
    /// — but the function itself must still reject it defensively when called
    /// directly (e.g. if a future caller stops going through clap).
    #[test]
    fn init_rejects_empty_targets_vec() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "spm-cli-test-init-empty-targets-{}-{nanos}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let err = init(&Scope::Project { root: dir.clone() }, Vec::new()).unwrap_err();
        assert!(
            format!("{err}").contains("at least one --target is required"),
            "{err}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
