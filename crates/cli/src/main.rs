//! `mm`: command-line front end over `mm-core`. Useful for scripting, for
//! testing detection without the UI, and on machines without a desktop.

use std::path::{Path, PathBuf};

use anyhow::Result;
use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use mm_core::env::{self, crossover};
use mm_core::library::{Library, ModMeta};
use mm_core::requirements::FrameworkState;
use mm_core::{Environment, EnvironmentKind, GameDefinition, GameInstance, ScanResult, game};

#[derive(Parser)]
#[command(name = "mm", version, about = "Cross-platform mod manager")]
struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,

    /// Only look in this CrossOver bottle folder instead of every environment.
    #[arg(long, global = true)]
    bottles_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the supported games.
    Games,
    /// List CrossOver bottles and Wine prefixes.
    Envs,
    /// Find game installs in all environments.
    Detect {
        /// Only look for this game id.
        #[arg(long)]
        game: Option<String>,
    },
    /// List the mods of a game install in load order (later entries win).
    Mods {
        /// Install id from `mm detect --json`; defaults to the only install found.
        #[arg(long)]
        instance: Option<String>,
    },
    /// Install mod archives into the library and enable them.
    Install {
        archives: Vec<PathBuf>,
        #[arg(long)]
        instance: Option<String>,
    },
    /// Show missing frameworks, game updates and errors from the game's logs.
    Health {
        #[arg(long)]
        instance: Option<String>,
    },
    /// Make the game folder match the active profile.
    Deploy {
        #[arg(long)]
        instance: Option<String>,
    },
    /// Remove all deployed mod files and restore replaced game files.
    Purge {
        #[arg(long)]
        instance: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Games => {
            let games = game::builtin_games()?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&games)?);
            } else {
                print_games(&games);
            }
        }
        Command::Envs => {
            let envs = environments(cli.bottles_dir.as_deref())?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&envs)?);
            } else {
                print_envs(&envs);
            }
        }
        Command::Detect { game: game_id } => {
            let games = match game_id {
                Some(id) => vec![game::find_builtin(&id)?],
                None => game::builtin_games()?,
            };
            let result =
                mm_core::scan_environments(&games, environments(cli.bottles_dir.as_deref())?);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print_scan(&result);
            }
        }
        Command::Mods { instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let loadout = open_library()?.loadout(&game, &instance)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&loadout)?);
            } else {
                for (i, m) in loadout.mods.iter().enumerate() {
                    let mark = if m.enabled { "x" } else { " " };
                    println!(
                        "{:>3}. [{mark}] {} {}",
                        i + 1,
                        m.record.name,
                        m.record.version.as_deref().unwrap_or("")
                    );
                }
                if loadout.pending_changes {
                    println!("\nChanges not applied yet; run `mm deploy`.");
                }
            }
        }
        Command::Install { archives, instance } => {
            if archives.is_empty() {
                bail!("give one or more archives to install");
            }
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let mut library = open_library()?;
            for archive in archives {
                let outcome = library
                    .install_archive(&game, &archive, ModMeta::default(), Some(&instance.id))
                    .with_context(|| format!("installing {}", archive.display()))?;
                println!(
                    "Installed {} ({} files)",
                    outcome.record.name, outcome.record.file_count
                );
                for warning in outcome.warnings {
                    println!("    warning: {warning}");
                }
            }
        }
        Command::Health { instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let mut library = open_library()?;
            let loadout = library.loadout(&game, &instance)?;
            let logs = library.logs(&game, &instance)?;
            if cli.json {
                let report = serde_json::json!({ "game": loadout.game, "frameworks": loadout.frameworks, "logs": logs });
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_health(&loadout, &logs);
            }
        }
        Command::Deploy { instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let report = open_library()?.deploy(&game, &instance)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::Purge { instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let report = open_library()?.purge(&game, &instance)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

fn print_health(loadout: &mm_core::library::Loadout, logs: &[mm_core::health::LogReport]) {
    let game = &loadout.game;
    println!(
        "Game version {} (build {})",
        game.version.as_deref().unwrap_or("unknown"),
        game.build_id.as_deref().unwrap_or("-")
    );
    if !game.outdated_frameworks.is_empty() {
        println!(
            "  The game was updated after these were installed: {}",
            game.outdated_frameworks.join(", ")
        );
    }
    println!("\nFrameworks:");
    for f in &loadout.frameworks {
        let state = match &f.state {
            FrameworkState::Installed { mod_name } => format!("installed ({mod_name})"),
            FrameworkState::InstalledManually => "installed manually".into(),
            FrameworkState::Disabled { mod_name, .. } => format!("disabled ({mod_name})"),
            FrameworkState::NotInstalled => "not installed".into(),
        };
        let needed = if f.needed_by.is_empty() {
            String::new()
        } else {
            format!(", needed by {}", f.needed_by.join(", "))
        };
        println!("  {:<20} {state}{needed}", f.name);
    }
    for m in loadout
        .mods
        .iter()
        .filter(|m| !m.missing_requirements.is_empty())
    {
        let names: Vec<&str> = m
            .missing_requirements
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        println!("  ! {} needs {}", m.record.name, names.join(", "));
    }
    println!("\nLogs:");
    if logs.is_empty() {
        println!("  none yet; start the game once with mods applied");
    }
    for log in logs {
        let stale = if log.stale {
            " (from before the last apply)"
        } else {
            ""
        };
        println!(
            "  {} [{}]: {} errors, {} warnings{stale}",
            log.name, log.relative_path, log.error_count, log.warning_count
        );
        for e in log
            .entries
            .iter()
            .filter(|e| e.severity == mm_core::health::Severity::Error)
            .rev()
            .take(3)
        {
            let who = e
                .mod_name
                .as_deref()
                .map(|m| format!(" [{m}]"))
                .unwrap_or_default();
            println!("      line {}{who}: {}", e.line, e.text);
        }
    }
}

fn open_library() -> Result<Library> {
    Ok(Library::open(&Library::default_root())?)
}

/// The requested game install, or the only one found.
fn pick_instance(
    id: Option<String>,
    bottles_dir: Option<&Path>,
) -> Result<(GameDefinition, GameInstance)> {
    let games = game::builtin_games()?;
    let scan = mm_core::scan_environments(&games, environments(bottles_dir)?);
    let instance = match id {
        Some(id) => scan
            .instances
            .into_iter()
            .find(|i| i.id == id)
            .context("no game install with that id")?,
        None => {
            let mut all = scan.instances.into_iter();
            match (all.next(), all.next()) {
                (Some(only), None) => only,
                (None, _) => bail!("no game installs found"),
                _ => bail!(
                    "several game installs found; pick one with --instance (see `mm detect --json`)"
                ),
            }
        }
    };
    let game = game::find_builtin(&instance.game_id)?;
    Ok((game, instance))
}

fn environments(bottles_dir: Option<&Path>) -> Result<Vec<Environment>> {
    Ok(match bottles_dir {
        Some(dir) => crossover::discover_bottles(dir)?,
        None => {
            let steam_ids: Vec<String> = game::builtin_games()?
                .iter()
                .filter_map(|g| g.stores.steam_app_id.clone())
                .collect();
            env::discover(&steam_ids)
        }
    })
}

fn print_games(games: &[GameDefinition]) {
    for game in games {
        println!("{}  {}", game.id, game.name);
    }
}

fn print_envs(envs: &[Environment]) {
    if envs.is_empty() {
        println!("No CrossOver bottles or Wine prefixes found.");
    }
    for env in envs {
        println!("{}", describe_env(env));
        println!("    {}", env.prefix.display());
    }
}

fn print_scan(result: &ScanResult) {
    print_envs(&result.environments);
    println!();
    if result.instances.is_empty() {
        println!("No games found.");
    }
    for instance in &result.instances {
        println!(
            "{} ({:?}) in {}",
            instance.game_name, instance.store, instance.environment_id
        );
        println!("    {}", instance.install_path.display());
        for dll in &instance.dll_overrides {
            let mark = if dll.satisfied { "ok" } else { "MISSING" };
            let current = dll.current.as_deref().unwrap_or("not set");
            println!(
                "    [{mark}] {} = {current} (needs {}): {}",
                dll.dll, dll.required, dll.reason
            );
        }
    }
}

fn describe_env(env: &Environment) -> String {
    match &env.kind {
        EnvironmentKind::CrossOverBottle {
            template,
            crossover_version,
            ..
        } => format!(
            "CrossOver bottle \"{}\" ({}, CrossOver {})",
            env.name,
            template.as_deref().unwrap_or("unknown template"),
            crossover_version.as_deref().unwrap_or("?")
        ),
        EnvironmentKind::WinePrefix => format!("Wine prefix {}", env.name),
        EnvironmentKind::Proton { app_id, .. } => format!("Proton prefix for Steam app {app_id}"),
        EnvironmentKind::Native => "Windows".into(),
    }
}
