//! `mm`: command-line front end over `mm-core`. Useful for scripting, for
//! testing detection without the UI, and on machines without a desktop.

use std::path::{Path, PathBuf};

use anyhow::Result;
use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use mm_core::env::{self, crossover};
use mm_core::library::{Library, ModMeta, Prepared};
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
    /// Go back to the previous kept version of a mod.
    Rollback {
        /// Mod id or name (see `mm mods`).
        #[arg(long = "mod")]
        mod_id: String,
        #[arg(long)]
        instance: Option<String>,
    },
    /// Install a version of a mod again from its kept archive, with the same
    /// installer choices.
    Reinstall {
        #[arg(long = "mod")]
        mod_id: String,
        /// Revision to install (default: the current one).
        #[arg(long)]
        revision: Option<u32>,
        #[arg(long)]
        instance: Option<String>,
    },
    /// Show what each mod requires and whether it's there.
    Deps {
        #[arg(long)]
        instance: Option<String>,
        /// Look up the requirements on the mods' Nexus pages first.
        #[arg(long)]
        fetch: bool,
    },
    /// List what would keep mods from working if the game started now.
    Preflight {
        #[arg(long)]
        instance: Option<String>,
    },
    /// Check that deployed files are still as they were placed.
    Verify {
        #[arg(long)]
        instance: Option<String>,
    },
    /// Enable a mod in the active profile (by id or name).
    Enable {
        #[arg(long = "mod")]
        mod_ref: String,
        #[arg(long)]
        instance: Option<String>,
    },
    /// Disable a mod in the active profile (by id or name).
    Disable {
        #[arg(long = "mod")]
        mod_ref: String,
        #[arg(long)]
        instance: Option<String>,
    },
    /// List, create or switch profiles.
    Profiles {
        #[command(subcommand)]
        action: Option<ProfileAction>,
        #[arg(long)]
        instance: Option<String>,
    },
    /// Ask Nexus Mods which mods have updates (needs NEXUS_API_KEY or a key
    /// saved by the app).
    CheckUpdates {
        #[arg(long)]
        instance: Option<String>,
    },
    /// Update a mod: from an archive you downloaded, or from Nexus Mods
    /// (Premium) when a check found an update.
    Update {
        #[arg(long = "mod")]
        mod_ref: String,
        #[arg(long)]
        archive: Option<PathBuf>,
        #[arg(long)]
        instance: Option<String>,
    },
    /// List the archive store, or clean archives no mod uses.
    Archives {
        #[command(subcommand)]
        action: Option<ArchivesAction>,
    },
    /// List mods installed by hand, or adopt one by name.
    Adopt {
        /// The group to adopt (see the list); without it, only lists.
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        instance: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProfileAction {
    List,
    /// Create a profile and make it active.
    Create {
        name: String,
        /// Start from the active profile's mod list.
        #[arg(long)]
        copy: bool,
    },
    /// Make a profile active (by id or name).
    Switch {
        profile: String,
    },
}

#[derive(Subcommand)]
enum ArchivesAction {
    List,
    /// Delete archives that no installed mod was installed from.
    Clean,
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
        Command::Rollback { mod_id, instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let mut library = open_library()?;
            let mod_id = find_mod(&library, &game, &mod_id)?.id;
            let prepared = library.rollback(&game, &mod_id, Some(&instance.id))?;
            report_prepared(&mut library, &game, prepared, cli.json)?;
        }
        Command::Reinstall {
            mod_id,
            revision,
            instance,
        } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let mut library = open_library()?;
            let mod_id = find_mod(&library, &game, &mod_id)?.id;
            let revision = match revision {
                Some(r) => r,
                None => library
                    .revisions(&mod_id)?
                    .first()
                    .map(|r| r.revision)
                    .context(
                        "this mod has no recorded versions; install it again from its archive",
                    )?,
            };
            let prepared =
                library.reinstall(&game, &mod_id, revision, Some(&instance.id), false)?;
            report_prepared(&mut library, &game, prepared, cli.json)?;
        }
        Command::Deps { instance, fetch } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let mut library = open_library()?;
            if fetch && let Some(domain) = game.stores.nexus_domain.as_deref() {
                let ids: Vec<u32> = library
                    .requirement_targets(&game.id, true)?
                    .into_iter()
                    .map(|(_, id)| id)
                    .collect();
                let found = mm_core::dependencies::fetch_requirements(
                    &mm_core::collections::graphql_url(),
                    None,
                    domain,
                    &ids,
                    env!("CARGO_PKG_VERSION"),
                )?;
                let n = library.apply_nexus_requirements(&game.id, &found)?;
                eprintln!("Looked up {n} mods on Nexus Mods.");
            }
            let loadout = library.loadout(&game, &instance)?;
            if cli.json {
                let mods: Vec<_> = loadout
                    .mods
                    .iter()
                    .map(|m| {
                        serde_json::json!({
                            "id": m.record.id,
                            "name": m.record.name,
                            "enabled": m.enabled,
                            "requires": m.requires,
                            "required_by": m.required_by,
                        })
                    })
                    .collect();
                let report =
                    serde_json::json!({ "mods": mods, "cycles": loadout.dependency_cycles });
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print_deps(&loadout);
            }
        }
        Command::Preflight { instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let preflight = open_library()?.preflight(&game, &instance)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&preflight)?);
            } else if preflight.items.is_empty() {
                println!("Ready to play: nothing found that would keep mods from working.");
            } else {
                for item in &preflight.items {
                    let mark = match item.severity {
                        mm_core::preflight::PreflightSeverity::Error => "ERROR",
                        mm_core::preflight::PreflightSeverity::Warning => "warning",
                    };
                    println!("[{mark}] {}\n        {}", item.title, item.detail);
                }
            }
            if preflight.has_errors() {
                std::process::exit(2);
            }
        }
        Command::Enable { mod_ref, instance } => {
            set_enabled(
                &mod_ref,
                instance,
                true,
                cli.json,
                cli.bottles_dir.as_deref(),
            )?;
        }
        Command::Disable { mod_ref, instance } => {
            set_enabled(
                &mod_ref,
                instance,
                false,
                cli.json,
                cli.bottles_dir.as_deref(),
            )?;
        }
        Command::Profiles { action, instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let mut library = open_library()?;
            match action {
                None | Some(ProfileAction::List) => {}
                Some(ProfileAction::Create { name, copy }) => {
                    let source = if copy {
                        Some(library.ensure_active_profile(&instance.id)?)
                    } else {
                        None
                    };
                    library.create_profile(&instance.id, &name, source.as_deref())?;
                }
                Some(ProfileAction::Switch { profile }) => {
                    let loadout = library.loadout(&game, &instance)?;
                    let found = loadout
                        .profiles
                        .iter()
                        .find(|p| p.id == profile || p.name.eq_ignore_ascii_case(&profile))
                        .with_context(|| format!("no profile \"{profile}\" (see `mm profiles`)"))?;
                    library.set_active_profile(&instance.id, &found.id)?;
                }
            }
            let loadout = library.loadout(&game, &instance)?;
            if cli.json {
                let report = serde_json::json!({
                    "profiles": loadout.profiles,
                    "active_profile_id": loadout.active_profile_id,
                });
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for p in &loadout.profiles {
                    let mark = if p.id == loadout.active_profile_id {
                        "*"
                    } else {
                        " "
                    };
                    println!("{mark} {}  ({})", p.name, p.id);
                }
            }
        }
        Command::CheckUpdates { instance } => {
            let (game, _) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let library = open_library()?;
            let key = nexus_key()?;
            let client = mm_core::nexus::NexusClient::new(&key, env!("CARGO_PKG_VERSION"))?;
            let check = library
                .update_check(&game)?
                .context("this game isn't on Nexus Mods")?;
            let now = mm_core::library::now();
            let period = mm_core::updates::period_since(check.last_checked_at, now);
            let outcome = mm_core::updates::run(&client, &check, now, &|| Ok(()))?;
            let summary = library.apply_update_check(&outcome, period)?;
            let _ = library.save_rate_limits();
            let mods = library.mods_for_game(&game.id)?;
            let updates: Vec<_> = mods.iter().filter(|m| m.update_file_id.is_some()).collect();
            if cli.json {
                let report = serde_json::json!({ "summary": summary, "updates": updates });
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "Looked at {} mods with {} requests.",
                    summary.checked, summary.requests
                );
                for m in &updates {
                    println!(
                        "    {} {} -> {}",
                        m.name,
                        m.version.as_deref().unwrap_or("?"),
                        m.update_version.as_deref().unwrap_or("newer")
                    );
                }
                if updates.is_empty() {
                    println!("All mods are up to date.");
                }
            }
        }
        Command::Update {
            mod_ref,
            archive,
            instance,
        } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let mut library = open_library()?;
            let record = find_mod(&library, &game, &mod_ref)?;
            // Only a downloaded update is known to be the update's file.
            let (archive, nexus_file_id) = match archive {
                Some(path) => (path, None),
                None => (
                    download_update(&library, &game, &record)?,
                    record
                        .update_file_id
                        .filter(|_| record.nexus_mod_id.is_some()),
                ),
            };
            let meta = ModMeta {
                replaces: Some(record.id.clone()),
                nexus_mod_id: record.nexus_mod_id,
                nexus_file_id,
                ..Default::default()
            };
            let job = library.begin_install(&game, &archive, meta, Some(&instance.id))?;
            let extracted = job.extract(&game)?;
            let prepared = library.complete_install(&game, extracted)?;
            report_prepared(&mut library, &game, prepared, cli.json)?;
        }
        Command::Archives { action } => {
            let library = open_library()?;
            if matches!(action, Some(ArchivesAction::Clean)) {
                let (count, bytes) = library.clean_unused_archives()?;
                if cli.json {
                    println!(
                        "{}",
                        serde_json::json!({ "deleted": count, "bytes": bytes })
                    );
                } else {
                    println!(
                        "Deleted {count} unused archives ({} MB).",
                        bytes / 1_000_000
                    );
                }
            } else {
                let usage = library.archive_usage()?;
                if cli.json {
                    println!("{}", serde_json::to_string_pretty(&usage)?);
                } else {
                    for u in &usage {
                        println!(
                            "{:>10} KB  {}  {}",
                            u.archive.size / 1000,
                            u.archive
                                .original_name
                                .as_deref()
                                .unwrap_or(&u.archive.path),
                            if u.unused {
                                "(unused)".to_string()
                            } else {
                                u.mods.join(", ")
                            }
                        );
                    }
                    println!("Keeping: {:?}", library.archive_retention());
                }
            }
        }
        Command::Adopt { name, instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let mut library = open_library()?;
            let groups = library.unowned_mods(&game, &instance)?;
            match name {
                None => {
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&groups)?);
                    } else if groups.is_empty() {
                        println!("No hand-installed mods found.");
                    } else {
                        for g in &groups {
                            println!("{}  ({}, {} files)", g.name, g.kind, g.files.len());
                        }
                        println!("\nAdopt one with `mm adopt --name <name>`.");
                    }
                }
                Some(name) => {
                    let group = groups
                        .iter()
                        .find(|g| g.name.eq_ignore_ascii_case(&name))
                        .with_context(|| {
                            format!("no hand-installed mod named \"{name}\" (see `mm adopt`)")
                        })?;
                    let outcome = library.adopt(&game, &instance, &group.name, &group.files)?;
                    if cli.json {
                        println!("{}", serde_json::to_string_pretty(&outcome)?);
                    } else {
                        println!(
                            "Adopted {} ({} files); the game folder wasn't changed.\n\
                             It's managed now: disabling it or purging removes its files from the game\n\
                             folder (the library keeps a copy), and uninstalling deletes both.",
                            outcome.record.name, outcome.record.file_count
                        );
                    }
                }
            }
        }
        Command::Verify { instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let check = open_library()?.verify_deployment(&game, &instance)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&check)?);
            } else if check.drifted.is_empty() {
                println!(
                    "All {} placed files are as they were deployed.",
                    check.checked
                );
            } else {
                for d in &check.drifted {
                    println!("{:?}: {}", d.kind, d.target);
                }
                if check.needs_apply() {
                    println!(
                        "\nRun `mm deploy` to fix missing files and adopt files the game replaced."
                    );
                }
            }
        }
        Command::Purge { instance } => {
            let (game, instance) = pick_instance(instance, cli.bottles_dir.as_deref())?;
            let report = open_library()?.purge(&game, &instance)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
    }
    Ok(())
}

fn print_deps(loadout: &mm_core::library::Loadout) {
    use mm_core::dependencies::DependencyState;
    let mut unmet = 0;
    for m in loadout
        .mods
        .iter()
        .filter(|m| m.enabled && !m.requires.is_empty())
    {
        println!("{}", m.record.name);
        for d in &m.requires {
            let state = match &d.state {
                DependencyState::Satisfied { by } => format!("ok ({by})"),
                DependencyState::Missing if d.optional => "optional, not installed".into(),
                DependencyState::Missing => "MISSING".into(),
                DependencyState::Disabled { .. } => "DISABLED: enable it".into(),
                DependencyState::TooOld { have, need } => format!("TOO OLD: {have}, needs {need}"),
                DependencyState::Unverifiable => "check yourself".into(),
            };
            if d.is_unmet() {
                unmet += 1;
            }
            let link = d
                .nexus_mod_id
                .filter(|_| d.is_unmet())
                .map(|id| format!(" (Nexus mod {id})"))
                .unwrap_or_default();
            println!("    {:<28} {state}{link}", d.name);
        }
        if !m.required_by.is_empty() {
            println!("    required by {}", m.required_by.join(", "));
        }
    }
    for cycle in &loadout.dependency_cycles {
        println!(
            "\nThese mods need each other in a circle: {}",
            cycle.join(" -> ")
        );
    }
    if unmet == 0 {
        println!("\nEvery enabled mod has what it needs, as far as can be told.");
    } else {
        println!("\n{unmet} requirements unmet.");
    }
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
            FrameworkState::Installed { mod_name, .. } => format!("installed ({mod_name})"),
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

/// Enables or disables a mod in the active profile and says what it means.
fn set_enabled(
    mod_ref: &str,
    instance: Option<String>,
    enable: bool,
    json: bool,
    bottles_dir: Option<&Path>,
) -> Result<()> {
    let (game, instance) = pick_instance(instance, bottles_dir)?;
    let mut library = open_library()?;
    let record = find_mod(&library, &game, mod_ref)?;
    // Others that need it lose it: say so (the app asks first).
    let dependents = library.dependents(&game, &instance, &record.id)?;
    library.set_mod_enabled(&game, &instance.id, &record.id, enable)?;
    let loadout = library.loadout(&game, &instance)?;
    let entry = loadout.mods.iter().find(|m| m.record.id == record.id);
    if json {
        println!("{}", serde_json::to_string_pretty(&entry)?);
    } else {
        println!(
            "{} {}.",
            if enable { "Enabled" } else { "Disabled" },
            record.name
        );
        if !enable && !dependents.is_empty() {
            println!("    warning: {} need it", dependents.join(", "));
        }
        println!("Run `mm deploy` to apply.");
    }
    Ok(())
}

/// A mod of the game by id, or by name when only one has it.
fn find_mod(
    library: &Library,
    game: &GameDefinition,
    query: &str,
) -> Result<mm_core::store::ModRecord> {
    let mods = library.mods_for_game(&game.id)?;
    if let Some(m) = mods.iter().find(|m| m.id == query) {
        return Ok(m.clone());
    }
    let named: Vec<_> = mods
        .iter()
        .filter(|m| m.name.eq_ignore_ascii_case(query))
        .collect();
    match named.as_slice() {
        [only] => Ok((*only).clone()),
        [] => bail!("no mod \"{query}\" (see `mm mods`)"),
        _ => bail!("several mods are called \"{query}\"; use its id (see `mm mods --json`)"),
    }
}

/// The Nexus API key: `NEXUS_API_KEY`, else the one the app saved.
fn nexus_key() -> Result<String> {
    if let Ok(key) = std::env::var("NEXUS_API_KEY")
        && !key.trim().is_empty()
    {
        return Ok(key);
    }
    mm_core::nexus::api_key::load()?
        .context("no Nexus Mods API key: set NEXUS_API_KEY or connect your account in the app")
}

/// Downloads the update a check found (Nexus Premium), into the library's
/// downloads folder.
fn download_update(
    library: &Library,
    game: &GameDefinition,
    record: &mm_core::store::ModRecord,
) -> Result<PathBuf> {
    let (Some(mod_id), Some(file_id), Some(domain)) = (
        record.nexus_mod_id,
        record.update_file_id,
        game.stores.nexus_domain.as_deref(),
    ) else {
        bail!(
            "no update is known for {}; run `mm check-updates`, or pass --archive",
            record.name
        );
    };
    let client = mm_core::nexus::NexusClient::new(&nexus_key()?, env!("CARGO_PKG_VERSION"))?;
    let url = client.download_url(domain, mod_id, file_id, None, None).context(
        "downloading directly needs Nexus Mods Premium; download the file on its page and pass --archive",
    )?;
    let file = client.file_info(domain, mod_id, file_id)?;
    let name = file
        .file_name
        .unwrap_or_else(|| format!("{mod_id}-{file_id}.zip"));
    let dest = library
        .downloads_dir()
        .join(mm_core::installer::sanitize_folder_name(&name));
    client.download(&url, &dest, |_, _| true)?;
    Ok(dest)
}

/// Prints an install result. Without someone to ask, a changed installer
/// is finished with the preselected (earlier, else default) choices.
fn report_prepared(
    library: &mut Library,
    game: &GameDefinition,
    prepared: Prepared,
    json: bool,
) -> Result<()> {
    let outcome = match prepared {
        Prepared::Installed(outcome) => outcome,
        Prepared::NeedsChoices(request) => {
            let mut outcome = library.finish_fomod(game, &request.token, &request.defaults)?;
            outcome.warnings.push(
                "The installer changed; earlier choices were used where they still fit.".into(),
            );
            outcome
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&outcome)?);
    } else {
        println!(
            "{} is now revision {} ({} files){}",
            outcome.record.name,
            outcome.record.revision,
            outcome.record.file_count,
            if outcome.replayed_choices {
                ", installer choices as before"
            } else {
                ""
            }
        );
        for warning in &outcome.warnings {
            println!("    warning: {warning}");
        }
        println!("Run `mm deploy` to put it in the game.");
    }
    Ok(())
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
