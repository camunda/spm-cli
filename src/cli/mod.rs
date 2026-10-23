mod add;
mod clean;
mod common;
mod init;
mod link;
mod list;
mod outdated;
mod remove;
mod scan;
mod shorthand;
mod status;
mod sync;
mod target;
mod update;

use anyhow::Result;
use clap::{Parser, Subcommand};
use common::{JsonArg, ScopeArg, TransportArgs, VersionArg};
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
        /// Git repository URL, or `github.com/<owner>/<repo>[@<ref>]` shorthand
        /// (expands to `https://github.com/<owner>/<repo>.git`; `@<ref>` is looked
        /// up as a tag or branch, or taken as a commit if it is a full SHA, and
        /// cannot be combined with --tag/--branch/--commit).
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
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Manage the target vendors declared in ai.json.
    Target {
        #[command(subcommand)]
        command: TargetCommand,
    },
    /// Remove a skill (or, with --plugin, a plugin) dependency.
    #[command(visible_alias = "rm")]
    Remove {
        name: String,
        /// Remove a plugin dependency instead of a skill.
        #[arg(long)]
        plugin: bool,
        #[command(flatten)]
        scope: ScopeArg,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Develop a declared skill or plugin from a local directory: materialize
    /// its current content (uncommitted edits included) instead of the locked
    /// commit. The override is machine-local, kept under `$SPM_HOME` and never
    /// in the checkout (`links/<project>-<hash>.json`, or `links.json` with -g)
    /// — ai.json and ai.lock are not modified.
    /// Re-run `spm install` to pick up further edits.
    Link {
        /// Name of the skill or plugin, as declared in ai.json.
        name: String,
        /// Local directory to materialize from: the skill directory (holding
        /// SKILL.md) or the plugin root. Relative paths are kept relative to the
        /// project root (made absolute with -g).
        dir: String,
        #[command(flatten)]
        scope: ScopeArg,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Drop a `spm link` override and re-materialize the locked version.
    Unlink {
        /// Linked skill or plugin to restore.
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        name: Option<String>,
        /// Drop every link in this scope.
        #[arg(long)]
        all: bool,
        #[command(flatten)]
        scope: ScopeArg,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Re-resolve branches/tags to latest commits (skills and plugins).
    Update {
        /// Skill or plugin to update; omit to update all.
        name: Option<String>,
        #[command(flatten)]
        scope: ScopeArg,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Fetch + materialize everything from ai.lock (use after cloning).
    #[command(visible_alias = "i")]
    Install {
        /// Fail instead of re-resolving if ai.lock is out of sync with ai.json
        /// (a dependency missing, changed or stale in ai.lock, including
        /// transitive ones). Never writes ai.lock; still fetches into the store.
        /// Use in CI.
        #[arg(long)]
        locked: bool,
        #[command(flatten)]
        scope: ScopeArg,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// List declared skills and plugins with their locked commits.
    #[command(visible_alias = "ls")]
    List {
        #[command(flatten)]
        scope: ScopeArg,
        #[command(flatten)]
        output: JsonArg,
    },
    /// Show which declared skills and plugins are materialized in this checkout.
    Status {
        #[command(flatten)]
        scope: ScopeArg,
        #[command(flatten)]
        output: JsonArg,
    },
    /// Report locked skills and plugins with newer upstream versions (moved
    /// branches, newer semver tags, re-pointed tags). Read-only: never writes
    /// ai.json/ai.lock, fetches into the store or touches vendor dirs. Exits
    /// non-zero if a remote could not be checked.
    Outdated {
        /// Skill or plugin to check; omit to check all.
        name: Option<String>,
        /// Exit non-zero when anything is outdated (to gate a pipeline).
        #[arg(long)]
        exit_code: bool,
        #[command(flatten)]
        scope: ScopeArg,
        #[command(flatten)]
        output: JsonArg,
        #[command(flatten)]
        transport: TransportArgs,
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
    /// Scan skill content for suspicious patterns (prompt injection, secret
    /// exfiltration, encoded payloads, auto-run hooks, …). Exits non-zero if any
    /// blocking (high/critical) finding is present. The same scan runs
    /// automatically as a pre-materialize gate on add/install/update.
    Scan {
        /// Path to scan — a file or directory (defaults to the current directory).
        #[arg(default_value = ".")]
        path: String,
        /// Follow symlinks that resolve anywhere inside DIR (which must contain
        /// the path), as the install gate does with a dependency's checkout
        /// root. By default only symlinks resolving inside the path are followed.
        #[arg(long, value_name = "DIR")]
        within: Option<String>,
        #[command(flatten)]
        output: JsonArg,
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
        #[command(flatten)]
        scope: ScopeArg,
        #[command(flatten)]
        transport: TransportArgs,
    },
    /// Remove one or more target vendors from ai.json and clean what spm
    /// materialized for them (leaving your own skills untouched). Refuses to
    /// remove the last target — use `spm clean` for that.
    #[command(visible_alias = "rm")]
    Remove {
        /// Vendor(s) to remove: amp, claude, cline, codex, copilot, cursor, gemini
        /// and/or windsurf. Repeatable or comma-separated.
        #[arg(value_delimiter = ',', required = true)]
        vendors: Vec<String>,
        #[command(flatten)]
        scope: ScopeArg,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    let cwd = std::env::current_dir()?;
    match cli.command {
        Command::Init { targets, scope } => init::init(&scope.resolve(&cwd), targets),
        Command::Add {
            git,
            version,
            path,
            name,
            all,
            plugin,
            force,
            scope,
            transport,
        } => {
            transport.apply();
            add::add(
                &scope.resolve(&cwd),
                add::AddRequest {
                    git,
                    version,
                    path,
                    name,
                    all,
                    plugin,
                    force,
                },
            )
        }
        Command::Target { command } => match command {
            TargetCommand::Add {
                vendors,
                scope,
                transport,
            } => {
                transport.apply();
                target::target_add(&scope.resolve(&cwd), vendors)
            }
            TargetCommand::Remove { vendors, scope } => {
                target::target_remove(&scope.resolve(&cwd), vendors)
            }
        },
        Command::Remove {
            name,
            plugin,
            scope,
            transport,
        } => {
            transport.apply();
            remove::remove(&scope.resolve(&cwd), &name, plugin)
        }
        Command::Link {
            name,
            dir,
            scope,
            transport,
        } => {
            transport.apply();
            link::link(&scope.resolve(&cwd), &cwd, &name, &dir)
        }
        Command::Unlink {
            name,
            all,
            scope,
            transport,
        } => {
            transport.apply();
            link::unlink(&scope.resolve(&cwd), name.as_deref(), all)
        }
        Command::Update {
            name,
            scope,
            transport,
        } => {
            transport.apply();
            update::update(&scope.resolve(&cwd), name)
        }
        Command::Install {
            locked,
            scope,
            transport,
        } => {
            transport.apply();
            let scope = scope.resolve(&cwd);
            let n = sync::sync(&scope, false, None, locked)?;
            let noun = if n == 1 { "dependency" } else { "dependencies" };
            println!("installed {n} {noun}");
            Ok(())
        }
        Command::List { scope, output } => list::list(&scope.resolve(&cwd), output.json),
        Command::Status { scope, output } => status::status(&scope.resolve(&cwd), output.json),
        Command::Outdated {
            name,
            exit_code,
            scope,
            output,
            transport,
        } => {
            transport.apply();
            outdated::outdated(
                &scope.resolve(&cwd),
                name.as_deref(),
                output.json,
                exit_code,
            )
        }
        Command::Clean { scope } => clean::clean(&scope.resolve(&cwd)),
        Command::Prune { yes } => clean::prune(yes),
        Command::Scan {
            path,
            within,
            output,
        } => scan::scan_cmd(
            Path::new(&path),
            within.as_deref().map(Path::new),
            output.json,
        ),
    }
}
