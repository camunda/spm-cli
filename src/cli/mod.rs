mod add;
mod clean;
mod common;
mod init;
mod list;
mod remove;
mod scan;
mod status;
mod sync;
mod target;
mod update;

use crate::scope::Scope;
use anyhow::Result;
use clap::{Parser, Subcommand};
use common::{ScopeArg, VersionArg};
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
    /// Re-resolve branches/tags to latest commits (skills and plugins).
    Update {
        /// Skill or plugin to update; omit to update all.
        name: Option<String>,
        #[command(flatten)]
        scope: ScopeArg,
    },
    /// Fetch + materialize everything from ai.lock (use after cloning).
    Install {
        #[command(flatten)]
        scope: ScopeArg,
    },
    /// List declared skills and plugins with their locked commits.
    List {
        #[command(flatten)]
        scope: ScopeArg,
    },
    /// Show which declared skills and plugins are materialized in this checkout.
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
    /// Scan skill content for suspicious patterns (prompt injection, secret
    /// exfiltration, encoded payloads, auto-run hooks, …). Exits non-zero if any
    /// blocking (high/critical) finding is present. The same scan runs
    /// automatically as a pre-materialize gate on add/install/update.
    Scan {
        /// Path to scan — a file or directory (defaults to the current directory).
        #[arg(default_value = ".")]
        path: String,
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
        } => add::add(
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
        ),
        Command::Target { command } => match command {
            TargetCommand::Add { vendors } => {
                target::target_add(&Scope::Project { root: cwd }, vendors)
            }
        },
        Command::Remove {
            name,
            plugin,
            scope,
        } => remove::remove(&scope.resolve(&cwd), &name, plugin),
        Command::Update { name, scope } => update::update(&scope.resolve(&cwd), name),
        Command::Install { scope } => {
            let scope = scope.resolve(&cwd);
            let n = sync::sync(&scope, false, None)?;
            let noun = if n == 1 { "dependency" } else { "dependencies" };
            println!("installed {n} {noun}");
            Ok(())
        }
        Command::List { scope } => list::list(&scope.resolve(&cwd)),
        Command::Status { scope } => status::status(&scope.resolve(&cwd)),
        Command::Clean { scope } => clean::clean(&scope.resolve(&cwd)),
        Command::Prune { yes } => clean::prune(yes),
        Command::Scan { path } => scan::scan_cmd(Path::new(&path)),
    }
}
