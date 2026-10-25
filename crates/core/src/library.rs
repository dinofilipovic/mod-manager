//! The mod library: installed mods, per-install profiles (load orders) and
//! deployment. This is the main entry point the CLI and the app use.
//!
//! On disk (in the app's data folder):
//! - `library.db`              SQLite database
//! - `mods/<mod id>/...`       each mod unpacked in game-folder layout
//! - `downloads/`              archives being downloaded from Nexus
//! - `archives/<sha256>.<ext>` archives mods were installed from (the archive store)
//! - `backups/<instance>/...`  original game files replaced by mods
//! - `preserved/<instance>/..` hand-edited deployed files, copied before being replaced
//! - `db-backups/`             copies of `library.db` from before migrations and deploys
//! - `tmp/`                    extraction scratch space

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use ts_rs::TS;
use walkdir::WalkDir;

use crate::archive;
use crate::archives::{
    self, ArchiveRecord, ArchiveRetention, ArchiveSource, ArchiveUsage, Hashes, Ingest,
    ModArchiveSize,
};
use crate::deploy::{self, DeployReport, DeploymentCheck, DesiredFile};
use crate::detect::GameInstance;
use crate::env::Environment;
use crate::error::{Error, Result};
use crate::fomod;
use crate::fs_util::{resolve_case_insensitive, sha256_file};
use crate::game::GameDefinition;
use crate::health;
use crate::installer::{self, InstallPlan};
use crate::nexus::{NexusMatch, parse_archive_name};
use crate::requirements::{self, FrameworkState, FrameworkStatus, MissingRequirement, ModFiles};
use crate::saves;
use crate::store::{ModFile, ModRecord, ModRevision, Profile, ProfileEntry, Store};
use crate::updates::{CheckOutcome, UpdateCheck, UpdateSummary};

/// What is known about a mod before installing it.
#[derive(Debug, Clone, Default)]
pub struct ModMeta {
    pub name: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub picture_url: Option<String>,
    pub nexus_mod_id: Option<u32>,
    pub nexus_file_id: Option<u32>,
    /// Replace this installed mod (an update) instead of adding a new one.
    pub replaces: Option<String>,
    /// The archive, when it is already in the archive store.
    pub archive_id: Option<i64>,
    /// Installer choices to apply again (a reinstall); for an update they
    /// come from the replaced mod's latest revision.
    pub fomod_choices: Option<fomod::FomodChoices>,
    /// Show the installer even when the earlier choices still fit.
    pub ask_choices: bool,
}

impl From<NexusMatch> for ModMeta {
    fn from(m: NexusMatch) -> Self {
        Self {
            name: Some(m.name),
            version: m.version,
            author: m.author,
            summary: m.summary,
            picture_url: m.picture_url,
            nexus_mod_id: Some(m.mod_id),
            nexus_file_id: m.file_id,
            replaces: None,
            archive_id: None,
            fomod_choices: None,
            ask_choices: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct InstallOutcome {
    #[serde(rename = "mod")]
    #[ts(rename = "mod")]
    pub record: ModRecord,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
    /// True when an installed mod was updated in place.
    pub replaced_existing: bool,
    /// The installer's options were chosen like last time, without asking.
    pub replayed_choices: bool,
}

/// One install's mod list in its active profile, ready to display.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct Loadout {
    pub instance_id: String,
    pub profiles: Vec<Profile>,
    pub active_profile_id: String,
    /// In load order: later entries win file conflicts.
    pub mods: Vec<LoadoutMod>,
    /// True when the game folder doesn't match the active profile yet.
    pub pending_changes: bool,
    #[ts(type = "number | null")]
    pub deployed_at: Option<i64>,
    /// The game's frameworks (script extenders) and whether each is usable.
    pub frameworks: Vec<FrameworkStatus>,
    pub game: GameState,
    /// How `.archive` mods are ordered; `None` if the game has no choice.
    pub archive_order: Option<ArchiveOrder>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct GameState {
    pub version: Option<String>,
    pub build_id: Option<String>,
    /// When the game's executable last changed (an update), if seen.
    #[ts(type = "number | null")]
    pub updated_at: Option<i64>,
    /// Frameworks installed before that update whose needed version isn't
    /// known. Script extenders usually need a new version after a game patch.
    pub outdated_frameworks: Vec<String>,
    /// Frameworks older than the version this game version needs.
    pub too_old_frameworks: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize, TS)]
#[ts(export)]
pub enum ArchiveOrder {
    /// The game's default: alphabetical by file name, first loaded wins.
    ByFileName,
    /// This app writes the game's load-order file from the mod list.
    ByList,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct LoadoutMod {
    #[serde(rename = "mod")]
    #[ts(rename = "mod")]
    pub record: ModRecord,
    pub enabled: bool,
    /// Enabled mods earlier in the order whose files this one replaces.
    pub overrides: Vec<String>,
    /// Enabled mods later in the order that replace some of this one's files.
    pub overridden_by: Vec<String>,
    /// Files shared with other enabled mods.
    pub conflict_files: u32,
    /// Frameworks this mod needs that aren't available.
    pub missing_requirements: Vec<MissingRequirement>,
}

const PROFILE_FORMAT: &str = "mod-manager-profile";

/// Copies of `library.db` kept per reason.
const DB_BACKUPS_KEPT: usize = 5;

/// A profile written to a file for sharing or moving to another machine.
#[derive(Debug, Clone, Serialize, serde::Deserialize, TS)]
#[ts(export)]
pub struct ProfileExport {
    pub format: String,
    pub version: u32,
    pub game_id: String,
    pub name: String,
    #[ts(type = "number")]
    pub exported_at: i64,
    /// In load order.
    pub mods: Vec<ExportedMod>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize, TS)]
#[ts(export)]
pub struct ExportedMod {
    pub name: String,
    pub version: Option<String>,
    pub enabled: bool,
    pub nexus_mod_id: Option<u32>,
    pub nexus_file_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ImportReport {
    pub profile_id: String,
    pub profile_name: String,
    pub matched: u32,
    /// Enabled mods in the file that aren't in this library.
    pub missing: Vec<ExportedMod>,
}

/// Owner recorded for the generated load-order file in the deployment record.
pub const LOAD_ORDER_FILE_OWNER: &str = "generated:load-order";

pub struct Library {
    store: Store,
    root: PathBuf,
    /// Extracted FOMOD archives waiting for the user's choices, by token.
    pending: HashMap<String, PendingFomod>,
    /// Problems found while opening (an interrupted deploy that couldn't be
    /// finished yet), to show the user.
    warnings: Vec<String>,
}

struct PendingFomod {
    job: InstallJob,
    content_root: PathBuf,
    installer: fomod::FomodInstaller,
}

/// An install between its first and last step (see `Library::begin_install`).
pub struct InstallJob {
    root: PathBuf,
    game_id: String,
    token: String,
    /// Where the archive is extracted.
    scratch: PathBuf,
    archive: JobArchive,
    archive_name: Option<String>,
    meta: ModMeta,
    instance_id: Option<String>,
}

enum JobArchive {
    /// Already in the archive store.
    Stored(ArchiveRecord),
    /// The user's file, to clone or copy into the store.
    New {
        path: PathBuf,
        original_name: Option<String>,
    },
}

/// An archive in the store, recorded in the library or not yet.
enum StoredArchive {
    Recorded(i64),
    Put {
        hashes: Hashes,
        relative: String,
        original_name: Option<String>,
        nexus: (Option<u32>, Option<u32>),
    },
}

/// An extracted archive: files ready to record, or an installer waiting
/// for the user's choices.
pub enum Extracted {
    Files(Staged),
    Choices(ChoicesJob),
}

/// Files moved into a staging folder (and hashed), ready to swap in.
pub struct Staged {
    job: InstallJob,
    archive: StoredArchive,
    staging: PathBuf,
    files: Vec<ModFile>,
    size_bytes: u64,
    plan: InstallPlan,
    fomod_choices: Option<fomod::FomodChoices>,
    replayed_choices: bool,
}

/// An extracted FOMOD archive whose installer needs the user's choices.
pub struct ChoicesJob {
    job: InstallJob,
    archive: StoredArchive,
    installer: fomod::FomodInstaller,
    content_root: PathBuf,
}

/// A waiting installer taken out of the library with the user's choices.
pub struct FomodJob {
    pending: PendingFomod,
    selections: fomod::Selections,
}

impl InstallJob {
    /// The slow step: puts the archive in the store (copying and hashing
    /// it), extracts it and stages the files. Needs no library.
    pub fn extract(self, game: &GameDefinition) -> Result<Extracted> {
        let (archive, path) = match &self.archive {
            JobArchive::Stored(record) => (StoredArchive::Recorded(record.id), record.path.clone()),
            JobArchive::New {
                path,
                original_name,
            } => {
                let (hashes, relative) = archives::put(
                    &self.root,
                    path,
                    original_name.as_deref(),
                    Ingest::Copy,
                    None,
                )?;
                let archive = StoredArchive::Put {
                    hashes,
                    relative: relative.clone(),
                    original_name: original_name.clone(),
                    nexus: (self.meta.nexus_mod_id, self.meta.nexus_file_id),
                };
                (archive, relative)
            }
        };
        let found = archive::extract(&self.root.join(path), &self.scratch)
            .and_then(|()| fomod::find_installer(&self.scratch));
        let installer = match found {
            Ok(installer) => installer,
            Err(e) => {
                let _ = fs::remove_dir_all(&self.scratch);
                return Err(e);
            }
        };
        let Some((installer, content_root)) = installer else {
            let name = self
                .meta
                .name
                .clone()
                .unwrap_or_else(|| "Unnamed mod".into());
            return match installer::plan_install(game, &self.scratch, &name) {
                Ok(plan) => stage(self, archive, plan, None, false).map(Extracted::Files),
                Err(e) => {
                    let _ = fs::remove_dir_all(&self.scratch);
                    Err(e)
                }
            };
        };
        // Same steps, groups and options as last time: no need to ask.
        let replay = self
            .meta
            .fomod_choices
            .as_ref()
            .map(|choices| fomod::replay(&installer, choices))
            .filter(|r| r.complete && !self.meta.ask_choices);
        if let Some(replay) = replay {
            let plan = match fomod_plan(
                game,
                &installer,
                &content_root,
                &self.scratch,
                &replay.selections,
            ) {
                Ok(plan) => plan,
                Err(e) => {
                    let _ = fs::remove_dir_all(&self.scratch);
                    return Err(e);
                }
            };
            let choices = fomod::name_choices(&installer, &replay.selections);
            return stage(self, archive, plan, Some(choices), true).map(Extracted::Files);
        }
        Ok(Extracted::Choices(ChoicesJob {
            job: self,
            archive,
            installer,
            content_root,
        }))
    }
}

impl FomodJob {
    /// Stages the files the chosen options install. Needs no library.
    pub fn stage(self, game: &GameDefinition) -> Result<Staged> {
        let FomodJob {
            pending,
            selections,
        } = self;
        let plan = fomod_plan(
            game,
            &pending.installer,
            &pending.content_root,
            &pending.job.scratch,
            &selections,
        );
        let plan = match plan {
            Ok(plan) => plan,
            Err(e) => {
                let _ = fs::remove_dir_all(&pending.job.scratch);
                return Err(e);
            }
        };
        let choices = fomod::name_choices(&pending.installer, &selections);
        // Recorded when the installer started waiting.
        let archive = StoredArchive::Recorded(pending.job.meta.archive_id.unwrap_or_default());
        stage(pending.job, archive, plan, Some(choices), false)
    }
}

/// The files a FOMOD installer's choices install, as an install plan whose
/// sources are relative to `scratch`.
fn fomod_plan(
    game: &GameDefinition,
    installer: &fomod::FomodInstaller,
    content_root: &Path,
    scratch: &Path,
    selections: &fomod::Selections,
) -> Result<InstallPlan> {
    let mappings = fomod::resolve(installer, selections);
    let pairs = fomod::expand(content_root, &mappings);
    if pairs.is_empty() {
        return Err(Error::Install(
            "The chosen options don't install any files.".into(),
        ));
    }
    let prefix = content_root
        .strip_prefix(scratch)
        .unwrap_or(Path::new(""))
        .components()
        .map(|c| format!("{}/", c.as_os_str().to_string_lossy()))
        .collect::<String>();
    let files: Vec<installer::PlannedFile> = pairs
        .into_iter()
        .map(|(source, target)| installer::PlannedFile {
            source: format!("{prefix}{source}"),
            target: installer::normalize_target(game, &target),
        })
        .collect();
    let kinds: std::collections::BTreeSet<String> = files
        .iter()
        .filter_map(|f| game.layout_for(&f.target).map(|t| t.kind.clone()))
        .collect();
    Ok(InstallPlan {
        files,
        skipped: Vec::new(),
        kinds: kinds.into_iter().collect(),
        warnings: Vec::new(),
    })
}

/// Moves the planned files from the extracted archive into a staging folder,
/// hashing each so deploying and checking drift don't have to.
fn stage(
    job: InstallJob,
    archive: StoredArchive,
    plan: InstallPlan,
    fomod_choices: Option<fomod::FomodChoices>,
    replayed_choices: bool,
) -> Result<Staged> {
    let staging = job.root.join("tmp").join(format!("{}.staging", job.token));
    let result = (|| {
        let mut size_bytes = 0u64;
        let mut files = Vec::with_capacity(plan.files.len());
        for file in &plan.files {
            let from = join_relative(&job.scratch, &file.source);
            let to = join_relative(&staging, &file.target);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            fs::rename(&from, &to)
                .or_else(|_| fs::copy(&from, &to).map(|_| ()))
                .map_err(|e| Error::io(&to, e))?;
            let size = fs::metadata(&to).map(|m| m.len()).unwrap_or(0);
            size_bytes += size;
            files.push(ModFile {
                target: file.target.clone(),
                size: Some(size),
                sha256: Some(sha256_file(&to).map_err(|e| Error::io(&to, e))?),
            });
        }
        Ok((size_bytes, files))
    })();
    match result {
        Ok((size_bytes, files)) => Ok(Staged {
            job,
            archive,
            staging,
            files,
            size_bytes,
            plan,
            fomod_choices,
            replayed_choices,
        }),
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            let _ = fs::remove_dir_all(&job.scratch);
            Err(e)
        }
    }
}

#[derive(Debug)]
pub enum Prepared {
    Installed(InstallOutcome),
    NeedsChoices(FomodRequest),
}

/// A FOMOD installer waiting for the user's choices.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct FomodRequest {
    pub token: String,
    pub game_id: String,
    pub mod_name: String,
    pub installer: fomod::FomodInstaller,
    pub defaults: fomod::Selections,
    /// `defaults` hold the choices made last time (where they still fit).
    pub preselected: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct FomodState {
    pub visible_steps: Vec<u32>,
    /// `[step][group][option]`: an option's kind can change with earlier choices.
    pub option_kinds: Vec<Vec<Vec<fomod::OptionKind>>>,
    /// Why the choices can't be installed yet, if they can't.
    pub error: Option<String>,
}

impl Library {
    /// The default data folder, e.g. `~/Library/Application Support/ModManager`.
    /// `MM_DATA_DIR` overrides it, so tests and scripts never touch the real library.
    pub fn default_root() -> PathBuf {
        if let Some(dir) = std::env::var_os("MM_DATA_DIR").filter(|d| !d.is_empty()) {
            return PathBuf::from(dir);
        }
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ModManager")
    }

    pub fn open(root: &Path) -> Result<Self> {
        for dir in ["mods", "downloads", "backups", "tmp"] {
            let path = root.join(dir);
            fs::create_dir_all(&path).map_err(|e| Error::io(&path, e))?;
        }
        let db = root.join("library.db");
        if Store::version_of(&db)?.is_some_and(|v| v < crate::store::SCHEMA_VERSION) {
            backup_database(root, &db, "pre-migration")?;
        }
        let store = Store::open(&db)?;
        let mut library = Self {
            store,
            root: root.to_path_buf(),
            pending: HashMap::new(),
            warnings: Vec::new(),
        };
        library.clean_tmp();
        // A recovery that fails now (say, the game's drive isn't mounted) is
        // retried before the next deploy; it mustn't keep the app from starting.
        library.warnings = library.recover_deployments(None)?;
        library.sweep_archives()?;
        Ok(library)
    }

    /// Problems found while opening the library.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Drops archive records whose file is gone and files no record knows.
    /// Recent files are left alone: another process (the CLI next to the
    /// app) may be adding them right now.
    fn sweep_archives(&self) -> Result<()> {
        let mut known = Vec::new();
        for a in self.store.archives()? {
            if self.root.join(&a.path).is_file() {
                known.push(a.path);
            } else {
                self.store.delete_archive(a.id)?;
            }
        }
        let cutoff = SystemTime::now() - std::time::Duration::from_secs(600);
        for orphan in archives::orphan_files(&self.root, &known) {
            let old = fs::metadata(&orphan)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t < cutoff);
            if old {
                let _ = fs::remove_file(orphan);
            }
        }
        Ok(())
    }

    /// Finishes or undoes deployment steps cut short when the app last
    /// stopped (of one install, or all), so the record matches the game
    /// folder again. Returns the steps that couldn't be recovered; they stay
    /// pending.
    fn recover_deployments(&mut self, instance_id: Option<&str>) -> Result<Vec<String>> {
        let mut failed = Vec::new();
        for entry in self.store.pending_journal()? {
            if instance_id.is_some_and(|id| id != entry.instance_id) {
                continue;
            }
            // Instance ids start with the game id, which says which folders
            // must stay even when empty.
            let protected = crate::game::builtin_games()
                .unwrap_or_default()
                .into_iter()
                .find(|g| entry.instance_id.starts_with(&format!("{}-", g.id)))
                .map(|g| protected_dirs(&g))
                .unwrap_or_default();
            let backups = self.backup_dir(&entry.instance_id);
            let preserved = self.preserved_dir(&entry.instance_id);
            if let Err(e) =
                deploy::recover(&entry, &backups, &preserved, &protected, &mut self.store)
            {
                failed.push(format!(
                    "An interrupted apply couldn't be finished for {} ({e}). Check that the game folder is reachable; it is retried before the next apply.",
                    entry.target
                ));
            }
        }
        self.store.clear_finished_journal()?;
        Ok(failed)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn downloads_dir(&self) -> PathBuf {
        self.root.join("downloads")
    }

    fn mod_dir(&self, mod_id: &str) -> PathBuf {
        self.root.join("mods").join(mod_id)
    }

    fn backup_dir(&self, instance_id: &str) -> PathBuf {
        self.root.join("backups").join(instance_id)
    }

    /// Where hand-edited deployed files are copied before being replaced.
    pub fn preserved_dir(&self, instance_id: &str) -> PathBuf {
        self.root.join("preserved").join(instance_id)
    }

    fn clean_tmp(&self) {
        if let Ok(entries) = fs::read_dir(self.root.join("tmp")) {
            for entry in entries.flatten() {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    // Installing
    //
    // An install has three steps so the slow part doesn't need the library:
    // `begin_install` decides what to do (database reads), `InstallJob::
    // extract` copies, hashes and extracts the archive and stages the files
    // (no library), `complete_install` records it and swaps the folder in.
    // The methods below that take `&mut self` do all three in a row.

    /// Extracts and installs an archive. When `instance_id` is given, the mod
    /// is enabled at the end of that install's active profile.
    pub fn install_archive(
        &mut self,
        game: &GameDefinition,
        archive_path: &Path,
        meta: ModMeta,
        instance_id: Option<&str>,
    ) -> Result<InstallOutcome> {
        // Without someone to ask, FOMOD installers get their default options.
        match self.prepare_archive(game, archive_path, meta, instance_id)? {
            Prepared::Installed(outcome) => Ok(outcome),
            Prepared::NeedsChoices(request) => {
                let mut outcome = self.finish_fomod(game, &request.token, &request.defaults)?;
                outcome.warnings.push(if request.preselected {
                    "The installer changed: your earlier choices were used where they still fit, defaults elsewhere.".into()
                } else {
                    "This mod has installer options; the default ones were used.".into()
                });
                Ok(outcome)
            }
        }
    }

    /// Extracts an archive and installs it, unless it has a FOMOD installer:
    /// then the extracted files are kept and the installer is returned so the
    /// user can choose options (`finish_fomod` or `cancel_fomod`).
    pub fn prepare_archive(
        &mut self,
        game: &GameDefinition,
        archive_path: &Path,
        meta: ModMeta,
        instance_id: Option<&str>,
    ) -> Result<Prepared> {
        let job = self.begin_install(game, archive_path, meta, instance_id)?;
        let extracted = job.extract(game)?;
        self.complete_install(game, extracted)
    }

    /// The first step of an install: what is known about the archive and the
    /// mod it installs or updates.
    pub fn begin_install(
        &self,
        game: &GameDefinition,
        archive_path: &Path,
        mut meta: ModMeta,
        instance_id: Option<&str>,
    ) -> Result<InstallJob> {
        let token = uuid::Uuid::new_v4().to_string();
        let stored = match meta.archive_id {
            Some(id) => self.store.archive(id)?,
            None => None,
        };
        let archive_name = stored
            .as_ref()
            .and_then(|a| a.original_name.clone())
            .or_else(|| {
                archive_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            });
        // An update installs with the options chosen for the version it replaces.
        if meta.fomod_choices.is_none()
            && let Some(replaced) = &meta.replaces
        {
            meta.fomod_choices = self
                .store
                .revisions(replaced)?
                .into_iter()
                .find_map(|r| r.fomod_choices);
        }
        if let Some(parsed) = archive_name.as_deref().map(parse_archive_name) {
            meta.name = meta.name.or(Some(parsed.name));
            meta.version = meta.version.or(parsed.version);
            meta.nexus_mod_id = meta.nexus_mod_id.or(parsed.nexus_mod_id);
        }
        let archive = match stored {
            Some(record) => JobArchive::Stored(record),
            // Keep a copy of the user's archive so the mod can be reinstalled.
            None => JobArchive::New {
                path: archive_path.to_path_buf(),
                original_name: archive_name.clone(),
            },
        };
        Ok(InstallJob {
            root: self.root.clone(),
            game_id: game.id.clone(),
            scratch: self.root.join("tmp").join(&token),
            token,
            archive,
            archive_name,
            meta,
            instance_id: instance_id.map(String::from),
        })
    }

    /// The last step of an install: records the mod (or the waiting
    /// installer) in the library.
    pub fn complete_install(
        &mut self,
        game: &GameDefinition,
        extracted: Extracted,
    ) -> Result<Prepared> {
        match extracted {
            Extracted::Files(staged) => self.commit_staged(game, staged).map(Prepared::Installed),
            Extracted::Choices(waiting) => {
                let ChoicesJob {
                    mut job,
                    archive,
                    installer,
                    content_root,
                } = waiting;
                job.meta.archive_id = Some(self.commit_archive(archive)?);
                let request = FomodRequest {
                    token: job.token.clone(),
                    game_id: job.game_id.clone(),
                    mod_name: job
                        .meta
                        .name
                        .clone()
                        .unwrap_or_else(|| installer.module_name.clone()),
                    defaults: job.meta.fomod_choices.as_ref().map_or_else(
                        || fomod::default_selections(&installer),
                        |c| fomod::replay(&installer, c).selections,
                    ),
                    preselected: job.meta.fomod_choices.is_some(),
                    installer: installer.clone(),
                };
                self.pending.insert(
                    job.token.clone(),
                    PendingFomod {
                        job,
                        content_root,
                        installer,
                    },
                );
                Ok(Prepared::NeedsChoices(request))
            }
        }
    }

    /// Where the installer stands with these choices: which steps show, what
    /// each option currently is, and whether the choices are complete.
    pub fn fomod_state(&self, token: &str, selections: &fomod::Selections) -> Result<FomodState> {
        let pending = self.pending_fomod(token)?;
        Ok(FomodState {
            visible_steps: fomod::visible_steps(&pending.installer, selections)
                .into_iter()
                .map(|s| s as u32)
                .collect(),
            option_kinds: fomod::option_kinds(&pending.installer, selections),
            error: fomod::validate(&pending.installer, selections).err(),
        })
    }

    /// An image shipped with a FOMOD installer, for showing an option.
    pub fn fomod_image(&self, token: &str, relative: &str) -> Result<Vec<u8>> {
        let pending = self.pending_fomod(token)?;
        if relative.split(['/', '\\']).any(|p| p == "..") {
            return Err(Error::NotFound(relative.into()));
        }
        let path = resolve_case_insensitive(&pending.content_root, relative)
            .ok_or_else(|| Error::NotFound(relative.into()))?;
        fs::read(&path).map_err(|e| Error::io(&path, e))
    }

    /// Installs a FOMOD mod with the chosen options.
    pub fn finish_fomod(
        &mut self,
        game: &GameDefinition,
        token: &str,
        selections: &fomod::Selections,
    ) -> Result<InstallOutcome> {
        let job = self.take_fomod(token, selections)?;
        let staged = job.stage(game)?;
        self.commit_staged(game, staged)
    }

    /// Takes a waiting installer with valid choices out of the library, to
    /// stage its files without it ([`FomodJob::stage`]), then
    /// [`Library::complete_install`] with `Extracted::Files`.
    pub fn take_fomod(&mut self, token: &str, selections: &fomod::Selections) -> Result<FomodJob> {
        let pending = self.pending_fomod(token)?;
        fomod::validate(&pending.installer, selections).map_err(Error::Install)?;
        let pending = self.pending.remove(token).expect("checked above");
        Ok(FomodJob {
            pending,
            selections: selections.clone(),
        })
    }

    /// A waiting installer again, e.g. when the user comes back to it.
    pub fn fomod_request(&self, token: &str) -> Result<FomodRequest> {
        let pending = self.pending_fomod(token)?;
        Ok(FomodRequest {
            token: token.to_string(),
            game_id: pending.job.game_id.clone(),
            mod_name: pending
                .job
                .meta
                .name
                .clone()
                .unwrap_or_else(|| pending.installer.module_name.clone()),
            defaults: pending.job.meta.fomod_choices.as_ref().map_or_else(
                || fomod::default_selections(&pending.installer),
                |c| fomod::replay(&pending.installer, c).selections,
            ),
            preselected: pending.job.meta.fomod_choices.is_some(),
            installer: pending.installer.clone(),
        })
    }

    pub fn cancel_fomod(&mut self, token: &str) {
        if let Some(pending) = self.pending.remove(token) {
            let _ = fs::remove_dir_all(pending.job.scratch);
        }
    }

    fn pending_fomod(&self, token: &str) -> Result<&PendingFomod> {
        self.pending
            .get(token)
            .ok_or_else(|| Error::NotFound("this installer (it may have been cancelled)".into()))
    }

    /// Records an archive put in the store during extraction (or finds the
    /// stored copy with the same contents) and returns its id.
    fn commit_archive(&self, archive: StoredArchive) -> Result<i64> {
        match archive {
            StoredArchive::Recorded(id) => Ok(id),
            StoredArchive::Put {
                hashes,
                relative,
                original_name,
                nexus,
            } => self
                .record_archive(
                    hashes,
                    relative,
                    original_name.as_deref(),
                    ArchiveSource::Manual,
                    nexus,
                )
                .map(|a| a.id),
        }
    }

    /// Records the staged files as the mod (new, or replacing the one it
    /// updates) and swaps its folder in.
    fn commit_staged(&mut self, game: &GameDefinition, staged: Staged) -> Result<InstallOutcome> {
        let Staged {
            job,
            archive,
            staging,
            files,
            size_bytes,
            plan,
            fomod_choices,
            replayed_choices,
        } = staged;
        let result = (|| {
            let archive_id = self.commit_archive(archive)?;
            let meta = job.meta;
            let name = meta.name.clone().unwrap_or_else(|| "Unnamed mod".into());

            // Same Nexus file installed again, or an explicit update: replace it.
            let existing = match &meta.replaces {
                Some(id) => self.store.get_mod(id)?,
                None => match (meta.nexus_mod_id, meta.nexus_file_id) {
                    (Some(mod_id), Some(file_id)) => self
                        .store
                        .find_mod_by_nexus_file(&game.id, mod_id, file_id)?,
                    _ => None,
                },
            };
            let id = existing
                .as_ref()
                .map_or_else(|| uuid::Uuid::new_v4().to_string(), |m| m.id.clone());

            let final_dir = self.mod_dir(&id);
            if final_dir.exists() {
                fs::remove_dir_all(&final_dir).map_err(|e| Error::io(&final_dir, e))?;
            }
            fs::rename(&staging, &final_dir).map_err(|e| Error::io(&final_dir, e))?;

            let record = ModRecord {
                id: id.clone(),
                game_id: game.id.clone(),
                name: existing
                    .as_ref()
                    .map_or(name, |m| meta.name.clone().unwrap_or(m.name.clone())),
                // The version is the archive's; other details not known for
                // this archive stay as they were.
                version: meta.version,
                author: meta
                    .author
                    .or_else(|| existing.as_ref().and_then(|m| m.author.clone())),
                summary: meta
                    .summary
                    .or_else(|| existing.as_ref().and_then(|m| m.summary.clone())),
                picture_url: meta
                    .picture_url
                    .or_else(|| existing.as_ref().and_then(|m| m.picture_url.clone())),
                nexus_mod_id: meta
                    .nexus_mod_id
                    .or_else(|| existing.as_ref().and_then(|m| m.nexus_mod_id)),
                nexus_file_id: meta.nexus_file_id,
                archive_name: job.archive_name,
                kinds: plan.kinds.clone(),
                file_count: files.len() as u32,
                size_bytes,
                installed_at: now(),
                revision: existing.as_ref().map_or(1, |m| m.revision + 1),
                update_file_id: None,
                update_version: None,
                // An update keeps the mod's pin and skipped file; the next
                // check looks at its files again.
                nexus_updated_at: None,
                pinned: existing.as_ref().is_some_and(|m| m.pinned),
                ignored_update_file_id: existing.as_ref().and_then(|m| m.ignored_update_file_id),
            };
            self.store.insert_mod(&record, &files)?;
            self.store.insert_revision(&ModRevision {
                mod_id: id.clone(),
                revision: record.revision,
                archive_id: Some(archive_id),
                fomod_choices,
                version: record.version.clone(),
                nexus_file_id: record.nexus_file_id,
                installed_at: record.installed_at,
            })?;
            // The install is done; a failed cleanup mustn't make it look failed.
            let mut warnings = plan.warnings;
            if let Err(e) = self.apply_retention() {
                warnings.push(format!(
                    "Couldn't delete old archives ({e}); use Clean unused in Settings."
                ));
            }

            if existing.is_none()
                && let Some(instance_id) = job.instance_id.as_deref()
            {
                let profile_id = self.ensure_active_profile(instance_id)?;
                let mut entries = self.effective_entries(&game.id, &profile_id)?;
                entries.retain(|e| e.mod_id != id);
                entries.push(ProfileEntry {
                    mod_id: id.clone(),
                    enabled: true,
                    priority: 0,
                });
                self.save_entries(&profile_id, entries)?;
            }

            Ok(InstallOutcome {
                record,
                skipped: plan.skipped,
                warnings,
                replaced_existing: existing.is_some(),
                replayed_choices,
            })
        })();
        let _ = fs::remove_dir_all(&staging);
        let _ = fs::remove_dir_all(&job.scratch);
        result
    }

    // Updates

    /// What an update check of this game needs, copied so the check can run
    /// without holding the library.
    pub fn update_check(&self, game: &GameDefinition) -> Result<Option<UpdateCheck>> {
        let Some(domain) = game.stores.nexus_domain.clone() else {
            return Ok(None);
        };
        Ok(Some(UpdateCheck {
            game_id: game.id.clone(),
            game_domain: domain,
            mods: self.store.mods_for_game(&game.id)?,
            last_checked_at: self.last_update_check(&game.id),
        }))
    }

    /// When the game's mods were last checked for updates.
    pub fn last_update_check(&self, game_id: &str) -> Option<i64> {
        self.store
            .setting(&format!("update_checked:{game_id}"))
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
    }

    /// Records a finished check.
    pub fn apply_update_check(
        &self,
        outcome: &CheckOutcome,
        period: Option<&str>,
    ) -> Result<UpdateSummary> {
        for m in &outcome.mods {
            // Updated or removed while the check ran: its result is stale.
            let current = self.store.get_mod(&m.mod_id)?;
            if current.is_none_or(|c| c.revision != m.revision) {
                continue;
            }
            self.store.set_mod_checked(
                &m.mod_id,
                m.nexus_updated_at,
                m.update.as_ref().map(|(id, v)| (*id, v.as_deref())),
            )?;
        }
        self.store.set_setting(
            &format!("update_checked:{}", outcome.game_id),
            Some(&outcome.checked_at.to_string()),
        )?;
        Ok(UpdateSummary {
            checked: outcome.mods.len() as u32,
            available: self
                .store
                .mods_for_game(&outcome.game_id)?
                .iter()
                .filter(|m| m.update_file_id.is_some())
                .count() as u32,
            requests: outcome.requests,
            period: period.map(String::from),
        })
    }

    /// Mods without a Nexus link that were installed from a stored archive:
    /// `(mod id, archive MD5, archive file name)`, for finding them on Nexus.
    /// Mods looked up in vain during the last week are left out.
    pub fn unlinked_archives(
        &self,
        game_id: &str,
    ) -> Result<Vec<(String, String, Option<String>)>> {
        let mut found = Vec::new();
        for m in self.store.mods_for_game(game_id)? {
            let tried = self
                .store
                .setting(&format!("nexus_lookup_failed:{}", m.id))?
                .and_then(|v| v.parse::<i64>().ok())
                .is_some_and(|at| now() - at < 7 * 86_400);
            if m.nexus_file_id.is_some() || tried {
                continue;
            }
            let archive = self
                .store
                .revisions(&m.id)?
                .into_iter()
                .find_map(|r| r.archive_id)
                .map(|id| self.store.archive(id))
                .transpose()?
                .flatten();
            if let Some(a) = archive {
                found.push((m.id, a.md5, m.archive_name));
            }
        }
        Ok(found)
    }

    /// Whether mods are checked for updates on their own (on unless turned off).
    pub fn auto_update_check(&self) -> bool {
        !matches!(self.store.setting("auto_update_check"), Ok(Some(v)) if v == "off")
    }

    pub fn set_auto_update_check(&self, on: bool) -> Result<()> {
        self.store
            .set_setting("auto_update_check", (!on).then_some("off"))
    }

    /// Whether an automatic check of this game is due: on, and not checked
    /// in the last day.
    pub fn update_check_due(&self, game_id: &str, now: i64) -> bool {
        self.auto_update_check()
            && self
                .last_update_check(game_id)
                .is_none_or(|last| now - last >= 86_400)
    }

    /// Mods with an update waiting (not pinned), per game.
    pub fn update_counts(&self) -> Result<BTreeMap<String, u32>> {
        let mut counts = BTreeMap::new();
        for game in crate::game::builtin_games().unwrap_or_default() {
            let n = self
                .store
                .mods_for_game(&game.id)?
                .iter()
                .filter(|m| m.update_file_id.is_some() && !m.pinned)
                .count() as u32;
            if n > 0 {
                counts.insert(game.id, n);
            }
        }
        Ok(counts)
    }

    /// Remembers that Nexus didn't know a mod, so it isn't asked again for a week.
    pub fn mark_lookup_failed(&self, mod_id: &str) -> Result<()> {
        self.store.set_setting(
            &format!("nexus_lookup_failed:{mod_id}"),
            Some(&now().to_string()),
        )
    }

    /// Saves the latest Nexus rate limits, so Settings can show them after a restart.
    pub fn save_rate_limits(&self) -> Result<()> {
        if let Some(limits) = crate::nexus::rate_limits() {
            let json = serde_json::to_string(&limits).unwrap_or_default();
            self.store.set_setting("nexus_rate_limits", Some(&json))?;
        }
        Ok(())
    }

    /// The rate limits seen last, also from an earlier run.
    pub fn load_rate_limits(&self) -> Option<crate::nexus::RateLimits> {
        if let Some(limits) = self
            .store
            .setting("nexus_rate_limits")
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
        {
            crate::nexus::remember_rate_limits(limits);
        }
        crate::nexus::rate_limits()
    }

    /// Keeps a mod at its version: update checks skip it.
    pub fn set_mod_pinned(&self, mod_id: &str, pinned: bool) -> Result<()> {
        self.store.set_mod_pinned(mod_id, pinned)
    }

    /// Skips the mod's pending update file; `false` forgets the skip.
    pub fn ignore_update(&self, mod_id: &str, ignore: bool) -> Result<()> {
        let record = self
            .store
            .get_mod(mod_id)?
            .ok_or_else(|| Error::NotFound(format!("mod {mod_id}")))?;
        let file = if ignore {
            Some(
                record
                    .update_file_id
                    .ok_or_else(|| Error::Install("This mod has no update to skip.".into()))?,
            )
        } else {
            None
        };
        self.store.ignore_mod_update(mod_id, file)
    }

    /// A cached changelog, with when it was fetched.
    pub fn cached_changelog(
        &self,
        game_domain: &str,
        nexus_mod_id: u32,
    ) -> Result<Option<(i64, crate::nexus::Changelog)>> {
        self.store.changelog(game_domain, nexus_mod_id)
    }

    pub fn cache_changelog(
        &self,
        game_domain: &str,
        nexus_mod_id: u32,
        entries: &crate::nexus::Changelog,
    ) -> Result<()> {
        self.store
            .set_changelog(game_domain, nexus_mod_id, now(), entries)
    }

    // Reinstall, roll back, change options

    /// Installs a revision of a mod again from its stored archive, with the
    /// installer choices it had. With `change_options` the installer is
    /// shown with those choices preselected. The result is a new revision.
    pub fn reinstall(
        &mut self,
        game: &GameDefinition,
        mod_id: &str,
        revision: u32,
        instance_id: Option<&str>,
        change_options: bool,
    ) -> Result<Prepared> {
        let job = self.reinstall_job(game, mod_id, revision, instance_id, change_options)?;
        let extracted = job.extract(game)?;
        self.complete_install(game, extracted)
    }

    /// The first step of [`Library::reinstall`].
    pub fn reinstall_job(
        &self,
        game: &GameDefinition,
        mod_id: &str,
        revision: u32,
        instance_id: Option<&str>,
        change_options: bool,
    ) -> Result<InstallJob> {
        let record = self
            .store
            .get_mod(mod_id)?
            .ok_or_else(|| Error::NotFound(format!("mod {mod_id}")))?;
        let rev = self
            .store
            .revisions(mod_id)?
            .into_iter()
            .find(|r| r.revision == revision)
            .ok_or_else(|| Error::NotFound(format!("revision {revision} of {}", record.name)))?;
        let archive = rev
            .archive_id
            .map(|id| self.store.archive(id))
            .transpose()?
            .flatten()
            .filter(|a| self.root.join(&a.path).is_file())
            .ok_or_else(|| {
                Error::Install(format!(
                    "The archive of {} {} is no longer kept (see Mod archives in Settings). Download that version from Nexus Mods again.",
                    record.name,
                    rev.version.as_deref().unwrap_or("")
                ))
            })?;
        if change_options && rev.fomod_choices.is_none() {
            return Err(Error::Install(format!(
                "{} has no installer options to change.",
                record.name
            )));
        }
        let meta = ModMeta {
            name: Some(record.name.clone()),
            version: rev.version.clone(),
            nexus_mod_id: record.nexus_mod_id,
            nexus_file_id: rev.nexus_file_id.or(archive.nexus_file_id),
            replaces: Some(record.id.clone()),
            archive_id: Some(archive.id),
            fomod_choices: rev.fomod_choices.clone(),
            ask_choices: change_options,
            ..Default::default()
        };
        let path = self.root.join(&archive.path);
        self.begin_install(game, &path, meta, instance_id)
    }

    /// Goes back to the newest earlier revision whose archive is kept.
    pub fn rollback(
        &mut self,
        game: &GameDefinition,
        mod_id: &str,
        instance_id: Option<&str>,
    ) -> Result<Prepared> {
        let job = self.rollback_job(game, mod_id, instance_id)?;
        let extracted = job.extract(game)?;
        self.complete_install(game, extracted)
    }

    /// The first step of [`Library::rollback`].
    pub fn rollback_job(
        &self,
        game: &GameDefinition,
        mod_id: &str,
        instance_id: Option<&str>,
    ) -> Result<InstallJob> {
        let record = self
            .store
            .get_mod(mod_id)?
            .ok_or_else(|| Error::NotFound(format!("mod {mod_id}")))?;
        // The revision installed now may itself be a reinstall of an older
        // one; go back from the version it holds.
        let revisions = self.store.revisions(mod_id)?;
        let current_archive = revisions.first().and_then(|r| r.archive_id);
        let target = revisions
            .iter()
            .skip(1)
            .filter(|r| r.archive_id.is_some() && r.archive_id != current_archive)
            .find(|r| {
                r.archive_id
                    .and_then(|id| self.store.archive(id).ok().flatten())
                    .is_some_and(|a| self.root.join(&a.path).is_file())
            })
            .map(|r| r.revision)
            .ok_or_else(|| {
                Error::Install(format!(
                    "No earlier version of {} is kept to roll back to.",
                    record.name
                ))
            })?;
        self.reinstall_job(game, mod_id, target, instance_id, false)
    }

    /// How many revisions per mod keep their archive under the
    /// "last revisions" retention (default 2).
    pub fn revisions_kept(&self) -> u32 {
        self.store
            .setting("revisions_kept")
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2)
    }

    pub fn set_revisions_kept(&self, count: u32) -> Result<()> {
        let count = count.clamp(1, 20);
        self.store
            .set_setting("revisions_kept", Some(&count.to_string()))?;
        self.apply_retention()
    }

    // Archive store

    /// Adds an archive to the archive store (or finds the stored copy with
    /// the same contents) and returns its record.
    pub fn store_archive(
        &self,
        path: &Path,
        original_name: Option<&str>,
        source: ArchiveSource,
        mode: Ingest,
        known: Option<Hashes>,
        nexus: (Option<u32>, Option<u32>),
    ) -> Result<ArchiveRecord> {
        let (hashes, relative) = archives::put(&self.root, path, original_name, mode, known)?;
        self.record_archive(hashes, relative, original_name, source, nexus)
    }

    /// Records a file already put in the store by [`archives::put`].
    pub fn record_archive(
        &self,
        hashes: Hashes,
        relative: String,
        original_name: Option<&str>,
        source: ArchiveSource,
        (nexus_mod_id, nexus_file_id): (Option<u32>, Option<u32>),
    ) -> Result<ArchiveRecord> {
        if let Some(existing) = self.store.archive_by_sha256(&hashes.sha256)? {
            self.store
                .link_archive(existing.id, nexus_mod_id, nexus_file_id, original_name)?;
            return Ok(self.store.archive(existing.id)?.unwrap_or(existing));
        }
        let mut record = ArchiveRecord {
            id: 0,
            sha256: hashes.sha256,
            md5: hashes.md5,
            size: hashes.size,
            path: relative,
            original_name: original_name.map(String::from),
            nexus_mod_id,
            nexus_file_id,
            source,
            added_at: now(),
        };
        record.id = self.store.insert_archive(&record)?;
        Ok(record)
    }

    pub fn archive_retention(&self) -> ArchiveRetention {
        ArchiveRetention::parse(
            self.store
                .setting("archive_retention")
                .ok()
                .flatten()
                .as_deref(),
        )
    }

    pub fn set_archive_retention(&self, retention: ArchiveRetention) -> Result<()> {
        self.store
            .set_setting("archive_retention", Some(retention.as_str()))?;
        self.apply_retention()
    }

    /// Deletes archives that only older revisions use, per the retention
    /// setting. Archives no revision uses are left for "clean unused".
    fn apply_retention(&self) -> Result<()> {
        let keep = match self.archive_retention() {
            ArchiveRetention::KeepAll => return Ok(()),
            ArchiveRetention::LastRevisions => self.revisions_kept() as usize,
            ArchiveRetention::DeleteAfterInstall => 0,
        };
        let revisions = self.store.all_revisions()?;
        let mut kept = std::collections::HashSet::new();
        let mut dropped = std::collections::HashSet::new();
        // Distinct archives per mod, newest first: a reinstall of the same
        // archive doesn't push the one before it out.
        let mut seen: HashMap<&str, Vec<i64>> = HashMap::new();
        for r in &revisions {
            let Some(id) = r.archive_id else { continue };
            let distinct = seen.entry(r.mod_id.as_str()).or_default();
            if !distinct.contains(&id) {
                distinct.push(id);
            }
            if distinct
                .iter()
                .position(|a| *a == id)
                .is_some_and(|i| i < keep)
            {
                kept.insert(id);
            } else {
                dropped.insert(id);
            }
        }
        for id in dropped.difference(&kept) {
            self.delete_archive(*id)?;
        }
        Ok(())
    }

    fn delete_archive(&self, id: i64) -> Result<()> {
        if let Some(a) = self.store.archive(id)? {
            let path = self.root.join(&a.path);
            crate::fs_util::remove_file_if_exists(&path).map_err(|e| Error::io(&path, e))?;
        }
        self.store.delete_archive(id)
    }

    /// Every stored archive with the mods installed from it.
    pub fn archive_usage(&self) -> Result<Vec<ArchiveUsage>> {
        let names: HashMap<String, String> = self.mod_names()?;
        let revisions = self.store.all_revisions()?;
        Ok(self
            .store
            .archives()?
            .into_iter()
            .map(|archive| {
                let mut mods: Vec<String> = revisions
                    .iter()
                    .filter(|r| r.archive_id == Some(archive.id))
                    .filter_map(|r| names.get(&r.mod_id).cloned())
                    .collect();
                mods.dedup();
                // An installer waiting for choices still needs its archive.
                let waiting = self
                    .pending
                    .values()
                    .any(|p| p.job.meta.archive_id == Some(archive.id));
                ArchiveUsage {
                    unused: mods.is_empty() && !waiting,
                    mods,
                    archive,
                }
            })
            .collect())
    }

    /// Archive disk use per mod, largest first.
    pub fn mod_archive_sizes(&self) -> Result<Vec<ModArchiveSize>> {
        let archives: HashMap<i64, u64> = self
            .store
            .archives()?
            .into_iter()
            .map(|a| (a.id, a.size))
            .collect();
        let names = self.mod_names()?;
        let mut sizes: BTreeMap<String, ModArchiveSize> = BTreeMap::new();
        let mut counted = std::collections::HashSet::new();
        for r in self.store.all_revisions()? {
            let Some(id) = r.archive_id else { continue };
            let Some(bytes) = archives.get(&id) else {
                continue;
            };
            if !counted.insert((r.mod_id.clone(), id)) {
                continue;
            }
            let entry = sizes
                .entry(r.mod_id.clone())
                .or_insert_with(|| ModArchiveSize {
                    mod_name: names.get(&r.mod_id).cloned().unwrap_or_default(),
                    mod_id: r.mod_id.clone(),
                    archives: 0,
                    bytes: 0,
                });
            entry.archives += 1;
            entry.bytes += bytes;
        }
        let mut sizes: Vec<ModArchiveSize> = sizes.into_values().collect();
        sizes.sort_by_key(|s| std::cmp::Reverse(s.bytes));
        Ok(sizes)
    }

    /// Deletes archives no mod revision uses. Returns how many and the bytes freed.
    pub fn clean_unused_archives(&self) -> Result<(u32, u64)> {
        let (mut count, mut bytes) = (0, 0);
        for usage in self.archive_usage()? {
            if usage.unused {
                self.delete_archive(usage.archive.id)?;
                count += 1;
                bytes += usage.archive.size;
            }
        }
        Ok((count, bytes))
    }

    /// Total size of the archive store, in bytes.
    pub fn archives_bytes(&self) -> u64 {
        self.store
            .archives()
            .map(|a| a.iter().map(|a| a.size).sum())
            .unwrap_or(0)
    }

    pub fn revisions(&self, mod_id: &str) -> Result<Vec<ModRevision>> {
        self.store.revisions(mod_id)
    }

    fn mod_names(&self) -> Result<HashMap<String, String>> {
        let mut names = HashMap::new();
        for game in crate::game::builtin_games().unwrap_or_default() {
            for m in self.store.mods_for_game(&game.id)? {
                names.insert(m.id, m.name);
            }
        }
        Ok(names)
    }

    /// Removes a mod from the library and all profiles. Its files leave the
    /// game folder on the next deployment.
    pub fn remove_mod(&mut self, mod_id: &str) -> Result<()> {
        self.store.delete_mod(mod_id)?;
        let dir = self.mod_dir(mod_id);
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
        }
        Ok(())
    }

    /// Fills in an installed mod's details from Nexus.
    pub fn apply_nexus_match(&self, mod_id: &str, m: &NexusMatch) -> Result<()> {
        self.store.set_nexus_metadata(
            mod_id,
            &m.name,
            m.version.as_deref(),
            m.author.as_deref(),
            m.summary.as_deref(),
            m.picture_url.as_deref(),
            m.mod_id,
            m.file_id,
        )
    }

    pub fn rename_mod(&self, mod_id: &str, name: &str) -> Result<()> {
        self.store.rename_mod(mod_id, &non_empty(name, "mod name")?)
    }

    pub fn mod_files(&self, mod_id: &str) -> Result<Vec<String>> {
        self.store.mod_files(mod_id)
    }

    pub fn mods_for_game(&self, game_id: &str) -> Result<Vec<ModRecord>> {
        self.store.mods_for_game(game_id)
    }

    // Profiles

    /// The install's active profile, creating "Default" on first use.
    pub fn ensure_active_profile(&mut self, instance_id: &str) -> Result<String> {
        let profiles = self.store.profiles(instance_id)?;
        if let Some(active) = self.store.active_profile_id(instance_id)?
            && profiles.iter().any(|p| p.id == active)
        {
            return Ok(active);
        }
        let profile = match profiles.into_iter().next() {
            Some(p) => p,
            None => {
                let p = Profile {
                    id: uuid::Uuid::new_v4().to_string(),
                    instance_id: instance_id.to_string(),
                    name: "Default".into(),
                };
                self.store.insert_profile(&p, now())?;
                p
            }
        };
        self.store.set_active_profile(instance_id, &profile.id)?;
        Ok(profile.id)
    }

    pub fn create_profile(
        &mut self,
        instance_id: &str,
        name: &str,
        copy_from: Option<&str>,
    ) -> Result<Profile> {
        let profile = Profile {
            id: uuid::Uuid::new_v4().to_string(),
            instance_id: instance_id.to_string(),
            name: non_empty(name, "profile name")?,
        };
        self.store.insert_profile(&profile, now())?;
        if let Some(source) = copy_from {
            let entries = self.store.profile_entries(source)?;
            self.store.set_profile_entries(&profile.id, &entries)?;
        }
        self.store.set_active_profile(instance_id, &profile.id)?;
        Ok(profile)
    }

    pub fn rename_profile(&self, profile_id: &str, name: &str) -> Result<()> {
        self.store
            .rename_profile(profile_id, &non_empty(name, "profile name")?)
    }

    pub fn delete_profile(&mut self, instance_id: &str, profile_id: &str) -> Result<()> {
        if self.store.profiles(instance_id)?.len() <= 1 {
            return Err(Error::Install("can't delete the only profile".into()));
        }
        self.store.delete_profile(profile_id)?;
        self.ensure_active_profile(instance_id)?;
        Ok(())
    }

    pub fn set_active_profile(&self, instance_id: &str, profile_id: &str) -> Result<()> {
        if !self
            .store
            .profiles(instance_id)?
            .iter()
            .any(|p| p.id == profile_id)
        {
            return Err(Error::NotFound(format!("profile {profile_id}")));
        }
        self.store.set_active_profile(instance_id, profile_id)
    }

    pub fn set_mod_enabled(
        &mut self,
        game: &GameDefinition,
        instance_id: &str,
        mod_id: &str,
        enabled: bool,
    ) -> Result<()> {
        let profile_id = self.ensure_active_profile(instance_id)?;
        let mut entries = self.effective_entries(&game.id, &profile_id)?;
        let entry = entries
            .iter_mut()
            .find(|e| e.mod_id == mod_id)
            .ok_or_else(|| Error::NotFound(format!("mod {mod_id}")))?;
        entry.enabled = enabled;
        self.save_entries(&profile_id, entries)
    }

    /// Sets the load order of the active profile. Mods not listed keep their
    /// relative order after the listed ones.
    pub fn reorder(
        &mut self,
        game: &GameDefinition,
        instance_id: &str,
        order: &[String],
    ) -> Result<()> {
        let profile_id = self.ensure_active_profile(instance_id)?;
        let entries = self.effective_entries(&game.id, &profile_id)?;
        let position: HashMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        let mut sorted = entries;
        sorted.sort_by_key(|e| {
            position
                .get(e.mod_id.as_str())
                .copied()
                .unwrap_or(usize::MAX)
        });
        self.save_entries(&profile_id, sorted)
    }

    /// The profile's entries for every mod of the game, in load order. Mods
    /// installed after the profile was last saved are appended, disabled.
    fn effective_entries(&self, game_id: &str, profile_id: &str) -> Result<Vec<ProfileEntry>> {
        let mods = self.store.mods_for_game(game_id)?;
        let mut entries: Vec<ProfileEntry> = self
            .store
            .profile_entries(profile_id)?
            .into_iter()
            .filter(|e| mods.iter().any(|m| m.id == e.mod_id))
            .collect();
        for m in &mods {
            if !entries.iter().any(|e| e.mod_id == m.id) {
                entries.push(ProfileEntry {
                    mod_id: m.id.clone(),
                    enabled: false,
                    priority: i64::MAX,
                });
            }
        }
        Ok(entries)
    }

    fn save_entries(&mut self, profile_id: &str, mut entries: Vec<ProfileEntry>) -> Result<()> {
        for (i, e) in entries.iter_mut().enumerate() {
            e.priority = i as i64;
        }
        self.store.set_profile_entries(profile_id, &entries)
    }

    // Loadout and deployment

    pub fn loadout(&mut self, game: &GameDefinition, instance: &GameInstance) -> Result<Loadout> {
        let profile_id = self.ensure_active_profile(&instance.id)?;
        let entries = self.effective_entries(&game.id, &profile_id)?;
        let mods: HashMap<String, ModRecord> = self
            .store
            .mods_for_game(&game.id)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();

        // Which enabled mods provide each file, in load order.
        let mut providers: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, e) in entries.iter().enumerate().filter(|(_, e)| e.enabled) {
            for target in self.store.mod_files(&e.mod_id)? {
                providers
                    .entry(target.to_ascii_lowercase())
                    .or_default()
                    .push(i);
            }
        }
        let mut overrides: Vec<BTreeMap<usize, ()>> = vec![BTreeMap::new(); entries.len()];
        let mut overridden_by: Vec<BTreeMap<usize, ()>> = vec![BTreeMap::new(); entries.len()];
        let mut conflict_files = vec![0u32; entries.len()];
        for owners in providers.values().filter(|o| o.len() > 1) {
            for (pos, &i) in owners.iter().enumerate() {
                conflict_files[i] += 1;
                for &earlier in &owners[..pos] {
                    overrides[i].insert(earlier, ());
                }
                for &later in &owners[pos + 1..] {
                    overridden_by[i].insert(later, ());
                }
            }
        }
        let name_of = |i: &usize| {
            mods.get(&entries[*i].mod_id)
                .map(|m| m.name.clone())
                .unwrap_or_default()
        };

        let deployed = self.store.deployed_files(&instance.id)?;
        let files: Vec<Vec<String>> = entries
            .iter()
            .map(|e| self.store.mod_files(&e.mod_id))
            .collect::<Result<_>>()?;
        let mut analysis = {
            let mod_files: Vec<ModFiles> = entries
                .iter()
                .zip(&files)
                .filter_map(|(e, f)| {
                    Some(ModFiles {
                        id: &e.mod_id,
                        name: &mods.get(&e.mod_id)?.name,
                        enabled: e.enabled,
                        files: f,
                    })
                })
                .collect();
            let placed: std::collections::HashSet<String> = deployed
                .iter()
                .map(|d| d.target.to_ascii_lowercase())
                .collect();
            requirements::analyze(game, &mod_files, |f| {
                f.provides.iter().any(|p| {
                    !placed.contains(&p.to_ascii_lowercase())
                        && resolve_case_insensitive(&instance.install_path, p)
                            .is_some_and(|f| f.is_file())
                })
            })
        };
        self.set_framework_versions(game, instance, &mut analysis.frameworks, &mods);

        let loadout_mods = entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let mut record = mods.get(&e.mod_id)?.clone();
                // From the current game definition, so older installs stay accurate.
                record.kinds = self.mod_kinds(game, &record.id);
                Some(LoadoutMod {
                    record,
                    enabled: e.enabled,
                    overrides: overrides[i].keys().map(name_of).collect(),
                    overridden_by: overridden_by[i].keys().map(name_of).collect(),
                    conflict_files: conflict_files[i],
                    missing_requirements: analysis
                        .missing
                        .get(&e.mod_id)
                        .cloned()
                        .unwrap_or_default(),
                })
            })
            .collect();

        let desired = self.desired_files(game, instance, &profile_id)?;
        let pending_changes = deployed.len() != desired.len()
            || deployed.iter().any(|d| {
                desired
                    .get(&d.target.to_ascii_lowercase())
                    .is_none_or(|w| w.mod_id != d.mod_id || w.revision != d.revision)
            })
            || deploy::verify(
                &instance.install_path,
                &deployed,
                self.store.game_updated_at(&instance.id)?,
            )
            .needs_apply();

        Ok(Loadout {
            instance_id: instance.id.clone(),
            profiles: self.store.profiles(&instance.id)?,
            active_profile_id: profile_id,
            mods: loadout_mods,
            pending_changes,
            deployed_at: self.store.deployed_at(&instance.id)?,
            game: self.game_state(instance, &analysis.frameworks, &mods)?,
            frameworks: analysis.frameworks,
            archive_order: game
                .load_order
                .as_ref()
                .map(|_| self.archive_order(&instance.id)),
        })
    }

    /// Fills in each installed framework's version (from its DLL's version
    /// resource, else the version of the mod that installed it) and whether
    /// it is too old for this game version.
    fn set_framework_versions(
        &self,
        game: &GameDefinition,
        instance: &GameInstance,
        frameworks: &mut [FrameworkStatus],
        mods: &HashMap<String, ModRecord>,
    ) {
        for status in frameworks.iter_mut().filter(|f| f.is_available()) {
            let Some(framework) = game.framework(&status.id) else {
                continue;
            };
            let provider = match &status.state {
                FrameworkState::Installed { mod_name } => {
                    mods.values().find(|m| &m.name == mod_name)
                }
                _ => None,
            };
            let binary = framework.provides.iter().find_map(|p| {
                let has_version_resource = Path::new(p).extension().is_some_and(|e| {
                    ["dll", "asi", "exe"]
                        .iter()
                        .any(|x| e.eq_ignore_ascii_case(x))
                });
                if !has_version_resource {
                    return None;
                }
                // The library's copy works before the mod is applied too.
                let path = provider
                    .and_then(|m| resolve_case_insensitive(&self.mod_dir(&m.id), p))
                    .or_else(|| resolve_case_insensitive(&instance.install_path, p))?;
                crate::detect::file_version(&path)
            });
            status.version = binary.or_else(|| provider.and_then(|m| m.version.clone()));
            status.min_version = instance
                .version
                .as_deref()
                .and_then(|v| framework.min_version_for(v))
                .map(String::from);
            status.too_old = match (&status.version, &status.min_version) {
                (Some(have), Some(need)) => {
                    crate::updates::compare_versions(have, need) == std::cmp::Ordering::Less
                }
                _ => false,
            };
        }
    }

    /// Notes game updates and which installed frameworks are too old for
    /// the game: by their version where the game definition says which
    /// version is needed, else by being installed before the last update.
    fn game_state(
        &self,
        instance: &GameInstance,
        frameworks: &[FrameworkStatus],
        mods: &HashMap<String, ModRecord>,
    ) -> Result<GameState> {
        let updated_at = if instance.fingerprint.is_empty() {
            None
        } else {
            self.store
                .observe_game(&instance.id, &instance.fingerprint, now())?
        };
        let too_old_frameworks = frameworks
            .iter()
            .filter(|f| f.too_old)
            .map(|f| f.name.clone())
            .collect();
        let outdated_frameworks = match updated_at {
            Some(updated) => frameworks
                .iter()
                // Known versions settle it; the heuristic is for the rest.
                .filter(|f| f.version.is_none() || f.min_version.is_none())
                .filter_map(|f| match &f.state {
                    FrameworkState::Installed { mod_name } => mods
                        .values()
                        .find(|m| &m.name == mod_name && m.installed_at < updated)
                        .map(|_| f.name.clone()),
                    _ => None,
                })
                .collect(),
            None => Vec::new(),
        };
        Ok(GameState {
            version: instance.version.clone(),
            build_id: instance.build_id.clone(),
            updated_at,
            outdated_frameworks,
            too_old_frameworks,
        })
    }

    pub fn archive_order(&self, instance_id: &str) -> ArchiveOrder {
        match self.store.setting(&format!("archive_order:{instance_id}")) {
            Ok(Some(v)) if v == "list" => ArchiveOrder::ByList,
            _ => ArchiveOrder::ByFileName,
        }
    }

    pub fn set_archive_order(&self, instance_id: &str, order: ArchiveOrder) -> Result<()> {
        let value = (order == ArchiveOrder::ByList).then_some("list");
        self.store
            .set_setting(&format!("archive_order:{instance_id}"), value)
    }

    fn mod_kinds(&self, game: &GameDefinition, mod_id: &str) -> Vec<String> {
        let kinds: std::collections::BTreeSet<String> = self
            .store
            .mod_files(mod_id)
            .unwrap_or_default()
            .iter()
            .filter_map(|f| game.layout_for(f).map(|t| t.kind.clone()))
            .collect();
        kinds.into_iter().collect()
    }

    fn desired_files(
        &self,
        game: &GameDefinition,
        instance: &GameInstance,
        profile_id: &str,
    ) -> Result<BTreeMap<String, DesiredFile>> {
        let mods: HashMap<String, ModRecord> = self
            .store
            .mods_for_game(&game.id)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        let mut desired = BTreeMap::new();
        // Archive file names per enabled mod, in load order (for modlist.txt).
        let mut archives_by_mod: Vec<Vec<String>> = Vec::new();
        let order_folder = game.load_order.as_ref().and_then(|o| {
            o.file
                .rsplit_once('/')
                .map(|(dir, _)| dir.to_ascii_lowercase())
        });
        for entry in self
            .effective_entries(&game.id, profile_id)?
            .iter()
            .filter(|e| e.enabled)
        {
            let Some(record) = mods.get(&entry.mod_id) else {
                continue;
            };
            let dir = self.mod_dir(&record.id);
            let mut archives = Vec::new();
            for ModFile { target, sha256, .. } in self.store.mod_file_details(&record.id)? {
                if let (Some(folder), Some(order)) = (&order_folder, &game.load_order)
                    && let Some((parent, name)) = target.rsplit_once('/')
                    && parent.eq_ignore_ascii_case(folder)
                    && name.rsplit_once('.').is_some_and(|(_, ext)| {
                        order.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
                    })
                {
                    archives.push(name.to_string());
                }
                desired.insert(
                    target.to_ascii_lowercase(),
                    DesiredFile {
                        source: join_relative(&dir, &target),
                        target,
                        mod_id: record.id.clone(),
                        revision: record.revision,
                        sha256,
                    },
                );
            }
            archives_by_mod.push(archives);
        }

        if let Some(order) = &game.load_order
            && self.archive_order(&instance.id) == ArchiveOrder::ByList
            && archives_by_mod.iter().any(|a| !a.is_empty())
        {
            let generated = self.write_load_order(instance, order, &archives_by_mod, &desired)?;
            desired.insert(order.file.to_ascii_lowercase(), generated);
        }
        Ok(desired)
    }

    /// Writes the game's load-order file. The game loads the first-listed
    /// archive first and the first loaded wins, while in the mod list the
    /// lowest mod wins, so mods are listed bottom-up. Archives this app
    /// doesn't manage keep the game's alphabetical order, after ours.
    fn write_load_order(
        &self,
        instance: &GameInstance,
        order: &crate::game::LoadOrderFile,
        archives_by_mod: &[Vec<String>],
        desired: &BTreeMap<String, DesiredFile>,
    ) -> Result<DesiredFile> {
        let mut lines: Vec<String> = Vec::new();
        for archives in archives_by_mod.iter().rev() {
            for name in archives {
                if !lines.iter().any(|l| l.eq_ignore_ascii_case(name)) {
                    lines.push(name.clone());
                }
            }
        }
        let folder = order.file.rsplit_once('/').map_or("", |(dir, _)| dir);
        let deployed: std::collections::HashSet<String> = self
            .store
            .deployed_files(&instance.id)?
            .into_iter()
            .map(|d| d.target.to_ascii_lowercase())
            .collect();
        let mut unmanaged: Vec<String> = resolve_case_insensitive(&instance.install_path, folder)
            .and_then(|dir| fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| {
                let key = format!("{folder}/{name}").to_ascii_lowercase();
                name.rsplit_once('.').is_some_and(|(_, ext)| {
                    order.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
                }) && !desired.contains_key(&key)
                    && !deployed.contains(&key)
            })
            .collect();
        unmanaged.sort();
        lines.extend(unmanaged);

        let content = format!("{}\n", lines.join("\n"));
        let path = join_relative(&self.root.join("generated").join(&instance.id), &order.file);
        if fs::read_to_string(&path).ok().as_deref() != Some(content.as_str()) {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            fs::write(&path, &content).map_err(|e| Error::io(&path, e))?;
        }
        let revision = content.bytes().fold(0x811c9dc5u32, |h, b| {
            (h ^ u32::from(b)).wrapping_mul(0x01000193)
        });
        Ok(DesiredFile {
            target: order.file.clone(),
            mod_id: LOAD_ORDER_FILE_OWNER.into(),
            revision,
            source: path,
            sha256: None,
        })
    }

    /// Makes the game folder match the active profile.
    pub fn deploy(
        &mut self,
        game: &GameDefinition,
        instance: &GameInstance,
    ) -> Result<DeployReport> {
        let profile_id = self.ensure_active_profile(&instance.id)?;
        let desired = self.desired_files(game, instance, &profile_id)?;
        self.apply(game, instance, &desired)
    }

    /// Removes every deployed mod file and restores replaced game files.
    pub fn purge(
        &mut self,
        game: &GameDefinition,
        instance: &GameInstance,
    ) -> Result<DeployReport> {
        self.apply(game, instance, &BTreeMap::new())
    }

    fn apply(
        &mut self,
        game: &GameDefinition,
        instance: &GameInstance,
        desired: &BTreeMap<String, DesiredFile>,
    ) -> Result<DeployReport> {
        // A game update noticed now still counts for the files it replaced.
        let game_updated_at = if instance.fingerprint.is_empty() {
            None
        } else {
            self.store
                .observe_game(&instance.id, &instance.fingerprint, now())?
        };
        // Steps an earlier apply couldn't finish come first, or this one
        // would plan from a game folder the record doesn't describe.
        let unfinished = self.recover_deployments(Some(&instance.id))?;
        if let Some(problem) = unfinished.into_iter().next() {
            return Err(Error::Install(problem));
        }
        backup_database(&self.root, &self.root.join("library.db"), "pre-deploy")?;
        let protected = protected_dirs(game);
        let backups = self.backup_dir(&instance.id);
        let preserved = self.preserved_dir(&instance.id);
        let deployment = deploy::Deployment {
            instance_id: &instance.id,
            game_root: &instance.install_path,
            backup_root: &backups,
            preserved_root: &preserved,
            protected_dirs: &protected,
            game_updated_at,
            mods_root: &self.root.join("mods"),
        };
        deploy::deploy(&deployment, desired, &mut self.store)
    }

    /// Deployed files that no longer match what was placed: missing, edited
    /// by hand, or replaced by the game.
    pub fn verify_deployment(
        &self,
        game: &GameDefinition,
        instance: &GameInstance,
    ) -> Result<DeploymentCheck> {
        let deployed = self.store.deployed_files(&instance.id)?;
        let mut check = deploy::verify(
            &instance.install_path,
            &deployed,
            self.store.game_updated_at(&instance.id)?,
        );
        let names: HashMap<String, String> = self
            .store
            .mods_for_game(&game.id)?
            .into_iter()
            .map(|m| (m.id, m.name))
            .collect();
        for d in &mut check.drifted {
            d.mod_name = names.get(&d.mod_id).cloned();
        }
        Ok(check)
    }

    /// The game's logs with their errors and warnings, attributed to the
    /// active profile's enabled mods where possible.
    pub fn logs(
        &mut self,
        game: &GameDefinition,
        instance: &GameInstance,
    ) -> Result<Vec<health::LogReport>> {
        let profile_id = self.ensure_active_profile(&instance.id)?;
        let mods: HashMap<String, ModRecord> = self
            .store
            .mods_for_game(&game.id)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        let mut hints = Vec::new();
        for entry in self
            .effective_entries(&game.id, &profile_id)?
            .iter()
            .filter(|e| e.enabled)
        {
            if let Some(record) = mods.get(&entry.mod_id) {
                hints.push(health::hints_for(
                    game,
                    &record.name,
                    &self.store.mod_files(&record.id)?,
                ));
            }
        }
        let applied_at = self.store.deployed_at(&instance.id)?;
        Ok(health::read_logs(
            game,
            &instance.install_path,
            &hints,
            applied_at,
        ))
    }

    /// The end of one of the game's known logs. Only paths the game
    /// definition lists can be read.
    pub fn log_tail(
        &self,
        game: &GameDefinition,
        instance: &GameInstance,
        relative_path: &str,
        max_lines: usize,
    ) -> Result<Vec<String>> {
        let wanted = join_relative(&instance.install_path, relative_path);
        let path = health::log_paths(game, &instance.install_path)
            .into_iter()
            .find(|p| *p == wanted)
            .ok_or_else(|| Error::NotFound(format!("log {relative_path}")))?;
        health::tail(&path, max_lines).map_err(|e| Error::io(&path, e))
    }

    // Save backups

    fn save_backup_dir(&self, instance_id: &str) -> PathBuf {
        self.root.join("save-backups").join(instance_id)
    }

    /// The game's save folder in this environment, if it exists.
    pub fn save_dir(&self, game: &GameDefinition, env: &Environment) -> Option<PathBuf> {
        saves::find_save_dir(env, game.saves.as_deref()?)
    }

    pub fn save_backups(&self, instance_id: &str) -> Vec<saves::SaveBackup> {
        saves::list(&self.save_backup_dir(instance_id))
    }

    pub fn backup_saves(
        &self,
        game: &GameDefinition,
        env: &Environment,
        instance_id: &str,
        reason: saves::BackupReason,
    ) -> Result<Option<saves::SaveBackup>> {
        let Some(dir) = self.save_dir(game, env) else {
            return Ok(None);
        };
        let backups = self.save_backup_dir(instance_id);
        match reason {
            saves::BackupReason::BeforeLaunch => saves::auto_backup(&dir, &backups, now()),
            _ => saves::create(&dir, &backups, reason, now()),
        }
    }

    pub fn restore_saves(
        &self,
        game: &GameDefinition,
        env: &Environment,
        instance_id: &str,
        backup_id: &str,
    ) -> Result<()> {
        let dir = self
            .save_dir(game, env)
            .ok_or_else(|| Error::NotFound("the game's save folder".into()))?;
        saves::restore(&dir, &self.save_backup_dir(instance_id), backup_id, now())
    }

    pub fn delete_save_backup(&self, instance_id: &str, backup_id: &str) -> Result<()> {
        saves::delete(&self.save_backup_dir(instance_id), backup_id)
    }

    /// Whether saves are backed up before each launch (on unless turned off).
    pub fn auto_save_backup(&self, instance_id: &str) -> bool {
        !matches!(self.store.setting(&format!("auto_save_backup:{instance_id}")), Ok(Some(v)) if v == "off")
    }

    pub fn set_auto_save_backup(&self, instance_id: &str, on: bool) -> Result<()> {
        self.store.set_setting(
            &format!("auto_save_backup:{instance_id}"),
            (!on).then_some("off"),
        )
    }

    // Profile sharing

    /// A profile as a portable list: mods in load order with enough Nexus
    /// details to find them again on another machine.
    pub fn export_profile(
        &mut self,
        game: &GameDefinition,
        instance_id: &str,
        profile_id: &str,
    ) -> Result<ProfileExport> {
        let profile = self
            .store
            .profiles(instance_id)?
            .into_iter()
            .find(|p| p.id == profile_id)
            .ok_or_else(|| Error::NotFound(format!("profile {profile_id}")))?;
        let mods: HashMap<String, ModRecord> = self
            .store
            .mods_for_game(&game.id)?
            .into_iter()
            .map(|m| (m.id.clone(), m))
            .collect();
        let entries = self
            .effective_entries(&game.id, profile_id)?
            .into_iter()
            .filter_map(|e| {
                let m = mods.get(&e.mod_id)?;
                Some(ExportedMod {
                    name: m.name.clone(),
                    version: m.version.clone(),
                    enabled: e.enabled,
                    nexus_mod_id: m.nexus_mod_id,
                    nexus_file_id: m.nexus_file_id,
                })
            })
            .collect();
        Ok(ProfileExport {
            format: PROFILE_FORMAT.into(),
            version: 1,
            game_id: game.id.clone(),
            name: profile.name,
            exported_at: now(),
            mods: entries,
        })
    }

    /// Creates a profile from an export, matching mods in this library by
    /// Nexus file, then Nexus mod, then name. Mods that aren't installed are
    /// reported so they can be downloaded.
    pub fn import_profile(
        &mut self,
        game: &GameDefinition,
        instance_id: &str,
        export: &ProfileExport,
    ) -> Result<ImportReport> {
        if export.format != PROFILE_FORMAT {
            return Err(Error::Install(
                "This file isn't a Mod Manager profile.".into(),
            ));
        }
        if export.game_id != game.id {
            return Err(Error::Install(format!(
                "This profile is for another game ({}).",
                export.game_id
            )));
        }
        let installed = self.store.mods_for_game(&game.id)?;
        let mut used = std::collections::HashSet::new();
        let mut entries = Vec::new();
        let mut missing = Vec::new();
        for wanted in &export.mods {
            let found = installed
                .iter()
                .filter(|m| !used.contains(&m.id))
                .find(|m| wanted.nexus_file_id.is_some() && m.nexus_file_id == wanted.nexus_file_id)
                .or_else(|| {
                    installed
                        .iter()
                        .filter(|m| !used.contains(&m.id))
                        .find(|m| {
                            wanted.nexus_mod_id.is_some() && m.nexus_mod_id == wanted.nexus_mod_id
                        })
                })
                .or_else(|| {
                    installed
                        .iter()
                        .filter(|m| !used.contains(&m.id))
                        .find(|m| m.name.eq_ignore_ascii_case(&wanted.name))
                });
            match found {
                Some(m) => {
                    used.insert(m.id.clone());
                    entries.push(ProfileEntry {
                        mod_id: m.id.clone(),
                        enabled: wanted.enabled,
                        priority: 0,
                    });
                }
                None if wanted.enabled => missing.push(wanted.clone()),
                None => {}
            }
        }
        let name = format!("{} (imported)", export.name);
        let profile = self.create_profile(instance_id, &name, None)?;
        self.save_entries(&profile.id, entries)?;
        Ok(ImportReport {
            profile_id: profile.id,
            profile_name: name,
            matched: used.len() as u32,
            missing,
        })
    }

    /// Total size of the library's mod folders, in bytes.
    pub fn disk_usage(&self) -> u64 {
        WalkDir::new(self.root.join("mods"))
            .into_iter()
            .flatten()
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum()
    }
}

/// Copies the database to `db-backups/<reason>-<time>.db`, keeping the
/// newest few per reason.
fn backup_database(root: &Path, db: &Path, reason: &str) -> Result<()> {
    let dir = root.join("db-backups");
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let dest = dir.join(format!("{reason}-{stamp:015}.db"));
    Store::backup_file(db, &dest)?;
    let mut existing: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| Error::io(&dir, e))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(&format!("{reason}-")))
        })
        .collect();
    existing.sort();
    let excess = existing.len().saturating_sub(DB_BACKUPS_KEPT);
    for old in &existing[..excess] {
        let _ = fs::remove_file(old);
    }
    Ok(())
}

/// Folders mods are dropped into stay even when empty; a framework's own
/// folder (no extensions or marker) is removed with it.
fn protected_dirs(game: &GameDefinition) -> Vec<String> {
    game.layout
        .iter()
        .filter(|t| !t.extensions.is_empty() || t.marker_file.is_some())
        .map(|t| t.path.clone())
        .chain(game.root_markers.iter().cloned())
        .collect()
}

fn join_relative(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .filter(|p| !p.is_empty())
        .fold(base.to_path_buf(), |path, part| path.join(part))
}

fn non_empty(value: &str, what: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        Err(Error::Install(format!("{what} can't be empty")))
    } else {
        Ok(value.to_string())
    }
}

/// The current time in Unix seconds.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::detect::Store as GameStore;
    use crate::game::find_builtin;

    struct Fixture {
        _tmp: tempfile::TempDir,
        lib: Library,
        game: GameDefinition,
        instance: GameInstance,
        archives: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let game_dir = tmp.path().join("Cyberpunk 2077");
        fs::create_dir_all(game_dir.join("bin/x64")).unwrap();
        fs::write(game_dir.join("bin/x64/Cyberpunk2077.exe"), b"").unwrap();
        let archives = tmp.path().join("archives");
        fs::create_dir_all(&archives).unwrap();
        Fixture {
            lib: Library::open(&tmp.path().join("data")).unwrap(),
            game: find_builtin("cyberpunk2077").unwrap(),
            instance: GameInstance {
                id: "cp-test".into(),
                game_id: "cyberpunk2077".into(),
                game_name: "Cyberpunk 2077".into(),
                environment_id: "test".into(),
                store: GameStore::Steam,
                install_path: game_dir,
                version: None,
                build_id: None,
                fingerprint: "test".into(),
                launch_id: None,
                launch_options_hint: None,
                dll_overrides: vec![],
            },
            archives,
            _tmp: tmp,
        }
    }

    fn zip(f: &Fixture, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let path = f.archives.join(name);
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        for (n, data) in files {
            zip.start_file(*n, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn install_deploy_conflict_and_purge() {
        let mut f = fixture();
        let a = zip(
            &f,
            "Mod A-100-1-0-1700000000.zip",
            &[
                ("archive/pc/mod/shared.archive", "A"),
                ("archive/pc/mod/a.archive", "A"),
            ],
        );
        let b = zip(&f, "Mod B.zip", &[("shared.archive", "B")]);
        let id = f.instance.id.clone();
        let out_a = f
            .lib
            .install_archive(&f.game, &a, ModMeta::default(), Some(&id))
            .unwrap();
        assert_eq!(out_a.record.name, "Mod A");
        let out_b = f
            .lib
            .install_archive(&f.game, &b, ModMeta::default(), Some(&id))
            .unwrap();

        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        assert!(loadout.pending_changes);
        assert_eq!(loadout.mods.len(), 2);
        assert_eq!(loadout.mods[1].overrides, ["Mod A"]);
        assert_eq!(loadout.mods[0].overridden_by, ["Mod B"]);

        f.lib.deploy(&f.game, &f.instance).unwrap();
        let shared = f
            .instance
            .install_path
            .join("archive/pc/mod/shared.archive");
        assert_eq!(fs::read_to_string(&shared).unwrap(), "B");
        assert!(!f.lib.loadout(&f.game, &f.instance).unwrap().pending_changes);

        // Move B before A: A now wins.
        f.lib
            .reorder(
                &f.game,
                &id,
                &[out_b.record.id.clone(), out_a.record.id.clone()],
            )
            .unwrap();
        f.lib.deploy(&f.game, &f.instance).unwrap();
        assert_eq!(fs::read_to_string(&shared).unwrap(), "A");

        // Disable A: B's file comes back, A's own file goes.
        f.lib
            .set_mod_enabled(&f.game, &id, &out_a.record.id, false)
            .unwrap();
        f.lib.deploy(&f.game, &f.instance).unwrap();
        assert_eq!(fs::read_to_string(&shared).unwrap(), "B");
        assert!(
            !f.instance
                .install_path
                .join("archive/pc/mod/a.archive")
                .exists()
        );

        f.lib.purge(&f.game, &f.instance).unwrap();
        assert!(!shared.exists());
    }

    #[test]
    fn writes_load_order_file_when_enabled() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let a = zip(&f, "a.zip", &[("a.archive", "A")]);
        let b = zip(&f, "b.zip", &[("b.archive", "B")]);
        f.lib
            .install_archive(&f.game, &a, ModMeta::default(), Some(&id))
            .unwrap();
        f.lib
            .install_archive(&f.game, &b, ModMeta::default(), Some(&id))
            .unwrap();
        let folder = f.instance.install_path.join("archive/pc/mod");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("manual.archive"), "M").unwrap();
        let modlist = folder.join("modlist.txt");

        f.lib.deploy(&f.game, &f.instance).unwrap();
        assert!(!modlist.exists(), "off by default");

        f.lib.set_archive_order(&id, ArchiveOrder::ByList).unwrap();
        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        assert!(loadout.pending_changes);
        assert_eq!(loadout.archive_order, Some(ArchiveOrder::ByList));
        f.lib.deploy(&f.game, &f.instance).unwrap();
        // B is lower in the list, so it must load (and win) first.
        assert_eq!(
            fs::read_to_string(&modlist).unwrap(),
            "b.archive\na.archive\nmanual.archive\n"
        );

        f.lib
            .set_archive_order(&id, ArchiveOrder::ByFileName)
            .unwrap();
        f.lib.deploy(&f.game, &f.instance).unwrap();
        assert!(!modlist.exists());
    }

    #[test]
    fn flags_missing_frameworks() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let script = zip(&f, "script.zip", &[("r6/scripts/Cool/main.reds", "x")]);
        f.lib
            .install_archive(&f.game, &script, ModMeta::default(), Some(&id))
            .unwrap();
        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        let missing: Vec<&str> = loadout.mods[0]
            .missing_requirements
            .iter()
            .map(|m| m.framework_id.as_str())
            .collect();
        assert_eq!(missing, ["redscript"]);

        // A manual redscript install in the game folder satisfies it.
        let scc = f.instance.install_path.join("engine/tools/scc.exe");
        fs::create_dir_all(scc.parent().unwrap()).unwrap();
        fs::write(&scc, "x").unwrap();
        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        assert!(loadout.mods[0].missing_requirements.is_empty());
    }

    #[test]
    fn frameworks_older_than_the_game_needs_are_flagged() {
        let mut f = fixture();
        f.instance.version = Some("2.5".into());
        let id = f.instance.id.clone();
        let scc = zip(
            &f,
            "redscript.zip",
            &[("engine/tools/scc.exe", "not a real exe")],
        );
        let meta = |v: &str| ModMeta {
            name: Some("redscript".into()),
            version: Some(v.into()),
            nexus_mod_id: Some(1511),
            nexus_file_id: Some(v.len() as u32),
            ..Default::default()
        };
        let installed = f
            .lib
            .install_archive(&f.game, &scc, meta("0.5.31"), Some(&id))
            .unwrap();
        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        let redscript = loadout
            .frameworks
            .iter()
            .find(|f| f.id == "redscript")
            .unwrap();
        assert_eq!(
            redscript.version.as_deref(),
            Some("0.5.31"),
            "from the mod, scc.exe has no version"
        );
        assert_eq!(redscript.min_version.as_deref(), Some("0.5.32"));
        assert!(redscript.too_old);
        assert_eq!(loadout.game.too_old_frameworks, ["redscript"]);

        let newer = zip(&f, "redscript2.zip", &[("engine/tools/scc.exe", "newer")]);
        let meta = ModMeta {
            replaces: Some(installed.record.id),
            ..meta("0.5.32")
        };
        f.lib
            .install_archive(&f.game, &newer, meta, Some(&id))
            .unwrap();
        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        assert!(loadout.game.too_old_frameworks.is_empty());
        assert!(!loadout.frameworks.iter().any(|f| f.too_old));
    }

    /// Reads a real framework DLL's version: `MM_PE_SAMPLE=<path> cargo test -- --ignored`.
    #[test]
    #[ignore = "needs a real Windows DLL"]
    fn reads_a_real_file_version() {
        let path = std::env::var("MM_PE_SAMPLE").expect("set MM_PE_SAMPLE");
        let version = crate::detect::file_version(Path::new(&path)).unwrap();
        println!("{path}: {version}");
        assert!(version.chars().next().unwrap().is_ascii_digit());
    }

    #[test]
    fn exports_and_imports_profiles() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let a = zip(&f, "a.zip", &[("a.archive", "A")]);
        let b = zip(&f, "b.zip", &[("b.archive", "B")]);
        let meta = ModMeta {
            nexus_mod_id: Some(5),
            nexus_file_id: Some(50),
            ..Default::default()
        };
        let a = f.lib.install_archive(&f.game, &a, meta, Some(&id)).unwrap();
        f.lib
            .install_archive(&f.game, &b, ModMeta::default(), Some(&id))
            .unwrap();
        let profile = f.lib.ensure_active_profile(&id).unwrap();

        let mut export = f.lib.export_profile(&f.game, &id, &profile).unwrap();
        assert_eq!(export.mods.len(), 2);
        // Round-trips through JSON, and reports what isn't installed here.
        export.mods.reverse();
        export.mods.push(ExportedMod {
            name: "Elsewhere".into(),
            version: None,
            enabled: true,
            nexus_mod_id: Some(9),
            nexus_file_id: Some(90),
        });
        let json = serde_json::to_string(&export).unwrap();
        let parsed: ProfileExport = serde_json::from_str(&json).unwrap();
        let report = f.lib.import_profile(&f.game, &id, &parsed).unwrap();
        assert_eq!(report.matched, 2);
        assert_eq!(report.missing[0].name, "Elsewhere");

        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        assert_eq!(loadout.active_profile_id, report.profile_id);
        assert_eq!(
            loadout.mods.last().unwrap().record.id,
            a.record.id,
            "order follows the file"
        );
    }

    #[test]
    fn fomod_archives_wait_for_choices() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let config = r#"<config><moduleName>Pick</moduleName><installSteps><installStep name="S">
            <optionalFileGroups><group name="G" type="SelectExactlyOne"><plugins>
              <plugin name="A"><description/><files><file source="a.archive" destination="archive/pc/mod/a.archive"/></files><typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
              <plugin name="B"><description/><files><file source="b.archive" destination="archive/pc/mod/b.archive"/></files><typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
            </plugins></group></optionalFileGroups></installStep></installSteps></config>"#;
        let archive = zip(
            &f,
            "Pick.zip",
            &[
                ("fomod/ModuleConfig.xml", config),
                ("a.archive", "A"),
                ("b.archive", "B"),
            ],
        );
        let Prepared::NeedsChoices(request) = f
            .lib
            .prepare_archive(&f.game, &archive, ModMeta::default(), Some(&id))
            .unwrap()
        else {
            panic!("expected the installer to ask");
        };
        let choose_b = fomod::Selections(vec![vec![vec![1]]]);
        assert!(
            f.lib
                .fomod_state(&request.token, &choose_b)
                .unwrap()
                .error
                .is_none()
        );
        let outcome = f
            .lib
            .finish_fomod(&f.game, &request.token, &choose_b)
            .unwrap();
        assert_eq!(
            f.lib.mod_files(&outcome.record.id).unwrap(),
            ["archive/pc/mod/b.archive"]
        );
        assert!(
            f.lib.fomod_state(&request.token, &choose_b).is_err(),
            "finished installers are gone"
        );

        // Without a UI, defaults are used.
        let outcome = f
            .lib
            .install_archive(&f.game, &archive, ModMeta::default(), Some(&id))
            .unwrap();
        assert_eq!(
            f.lib.mod_files(&outcome.record.id).unwrap(),
            ["archive/pc/mod/a.archive"]
        );
    }

    #[test]
    fn profiles_are_independent() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let a = zip(&f, "a.zip", &[("a.archive", "A")]);
        let installed = f
            .lib
            .install_archive(&f.game, &a, ModMeta::default(), Some(&id))
            .unwrap();
        let default = f.lib.ensure_active_profile(&id).unwrap();

        let empty = f.lib.create_profile(&id, "Vanilla", None).unwrap();
        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        assert_eq!(loadout.active_profile_id, empty.id);
        assert!(
            !loadout.mods[0].enabled,
            "new mods are off in other profiles"
        );

        f.lib.set_active_profile(&id, &default).unwrap();
        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        assert!(
            loadout
                .mods
                .iter()
                .any(|m| m.record.id == installed.record.id && m.enabled)
        );
    }

    #[test]
    fn archives_are_stored_deduplicated_and_retained() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let v1 = zip(&f, "Mod-5-1-0-1700000000.zip", &[("a.archive", "1")]);
        let first = f
            .lib
            .install_archive(&f.game, &v1, ModMeta::default(), Some(&id))
            .unwrap();
        assert!(v1.exists(), "the user's archive is left where it was");
        let archives = f.lib.archive_usage().unwrap();
        assert_eq!(archives.len(), 1);
        assert_eq!(archives[0].mods, ["Mod"]);
        assert_eq!(archives[0].archive.source, ArchiveSource::Manual);
        assert!(f.lib.root().join(&archives[0].archive.path).is_file());

        // The same bytes again are stored once.
        let copy = f.archives.join("copy.zip");
        fs::copy(&v1, &copy).unwrap();
        let duplicate = f
            .lib
            .install_archive(&f.game, &copy, ModMeta::default(), Some(&id))
            .unwrap();
        assert_eq!(f.lib.archive_usage().unwrap().len(), 1);
        assert_eq!(f.lib.archive_usage().unwrap()[0].mods.len(), 2);
        f.lib.remove_mod(&duplicate.record.id).unwrap();

        // Updates write revisions; only the last two keep their archives.
        for (n, data) in [(2, "2"), (3, "3")] {
            let archive = zip(&f, &format!("v{n}.zip"), &[("a.archive", data)]);
            let meta = ModMeta {
                replaces: Some(first.record.id.clone()),
                version: Some(format!("{n}.0")),
                ..Default::default()
            };
            f.lib
                .install_archive(&f.game, &archive, meta, Some(&id))
                .unwrap();
        }
        let revisions = f.lib.revisions(&first.record.id).unwrap();
        assert_eq!(
            revisions.iter().map(|r| r.revision).collect::<Vec<_>>(),
            [3, 2, 1]
        );
        assert_eq!(revisions[0].version.as_deref(), Some("3.0"));
        assert!(revisions[0].archive_id.is_some() && revisions[1].archive_id.is_some());
        assert_eq!(
            revisions[2].archive_id, None,
            "the oldest archive was deleted"
        );
        let sizes = f.lib.mod_archive_sizes().unwrap();
        let mine = sizes.iter().find(|s| s.mod_id == first.record.id).unwrap();
        assert_eq!(mine.archives, 2);

        f.lib.remove_mod(&first.record.id).unwrap();
        let (count, bytes) = f.lib.clean_unused_archives().unwrap();
        assert!(count >= 2 && bytes > 0);
        assert!(f.lib.archive_usage().unwrap().iter().all(|a| !a.unused));

        f.lib
            .set_archive_retention(ArchiveRetention::DeleteAfterInstall)
            .unwrap();
        assert!(f.lib.archive_usage().unwrap().is_empty());
    }

    #[test]
    fn fomod_choices_are_recorded_per_revision() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let config = r#"<config><moduleName>Pick</moduleName><installSteps><installStep name="S">
            <optionalFileGroups><group name="G" type="SelectExactlyOne"><plugins>
              <plugin name="A"><description/><files><file source="a.archive" destination="archive/pc/mod/a.archive"/></files><typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
              <plugin name="B"><description/><files><file source="b.archive" destination="archive/pc/mod/b.archive"/></files><typeDescriptor><type name="Optional"/></typeDescriptor></plugin>
            </plugins></group></optionalFileGroups></installStep></installSteps></config>"#;
        let archive = zip(
            &f,
            "Pick.zip",
            &[
                ("fomod/ModuleConfig.xml", config),
                ("a.archive", "A"),
                ("b.archive", "B"),
            ],
        );
        let Prepared::NeedsChoices(request) = f
            .lib
            .prepare_archive(&f.game, &archive, ModMeta::default(), Some(&id))
            .unwrap()
        else {
            panic!("expected the installer to ask");
        };
        let choose_b = fomod::Selections(vec![vec![vec![1]]]);
        let outcome = f
            .lib
            .finish_fomod(&f.game, &request.token, &choose_b)
            .unwrap();
        let revision = &f.lib.revisions(&outcome.record.id).unwrap()[0];
        let choices = revision.fomod_choices.as_ref().unwrap();
        assert_eq!(choices.0[0].step, "S");
        assert_eq!(choices.0[0].groups[0].options, ["B"]);
        assert!(revision.archive_id.is_some());
    }

    #[test]
    fn sweeps_orphaned_archives_and_backs_up_the_database() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let a = zip(&f, "a.zip", &[("a.archive", "A")]);
        f.lib
            .install_archive(&f.game, &a, ModMeta::default(), Some(&id))
            .unwrap();
        for _ in 0..7 {
            f.lib.deploy(&f.game, &f.instance).unwrap();
        }
        let backups: Vec<_> = fs::read_dir(f.lib.root().join("db-backups"))
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(backups.len(), DB_BACKUPS_KEPT, "only the newest are kept");

        let root = f.lib.root().to_path_buf();
        fs::write(root.join("archives/stray.zip"), "x").unwrap();
        fs::write(root.join("archives/.incoming-new"), "x").unwrap();
        // Only files older than a few minutes are someone's leftovers.
        fs::File::options()
            .write(true)
            .open(root.join("archives/stray.zip"))
            .unwrap()
            .set_modified(SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();
        let stored = root.join(&f.lib.archive_usage().unwrap()[0].archive.path);
        fs::remove_file(&stored).unwrap();
        let lib = Library::open(&root).unwrap();
        assert!(!root.join("archives/stray.zip").exists());
        assert!(
            root.join("archives/.incoming-new").exists(),
            "maybe still being added"
        );
        assert!(
            lib.archive_usage().unwrap().is_empty(),
            "missing file, record dropped"
        );
    }

    #[test]
    fn backs_up_the_database_before_migrating() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("data");
        fs::create_dir_all(&root).unwrap();
        {
            let conn = rusqlite::Connection::open(root.join("library.db")).unwrap();
            conn.execute_batch(crate::store::BASE_SCHEMA).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }
        Library::open(&root).unwrap();
        let backups: Vec<String> = fs::read_dir(root.join("db-backups"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(backups[0].starts_with("pre-migration-"));
        let copy = root.join("db-backups").join(&backups[0]);
        assert_eq!(crate::store::Store::version_of(&copy).unwrap(), Some(2));
    }

    #[test]
    fn installs_extract_without_the_library_and_commit_in_any_order() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let a = zip(&f, "a.zip", &[("a.archive", "A")]);
        let b = zip(&f, "b.zip", &[("b.archive", "B")]);
        let job_a = f
            .lib
            .begin_install(&f.game, &a, ModMeta::default(), Some(&id))
            .unwrap();
        let job_b = f
            .lib
            .begin_install(&f.game, &b, ModMeta::default(), Some(&id))
            .unwrap();
        // The slow steps run on other threads, with no library at all.
        let game = f.game.clone();
        let (ea, eb) = std::thread::scope(|s| {
            let ha = s.spawn(|| job_a.extract(&game).unwrap());
            let hb = s.spawn(|| job_b.extract(&game).unwrap());
            (ha.join().unwrap(), hb.join().unwrap())
        });
        f.lib.complete_install(&f.game, eb).unwrap();
        f.lib.complete_install(&f.game, ea).unwrap();
        let loadout = f.lib.loadout(&f.game, &f.instance).unwrap();
        let names: Vec<&str> = loadout
            .mods
            .iter()
            .map(|m| m.record.name.as_str())
            .collect();
        assert_eq!(names, ["b", "a"], "in the order they were committed");
        assert_eq!(f.lib.archive_usage().unwrap().len(), 2);
        assert!(
            fs::read_dir(f.lib.root().join("tmp"))
                .unwrap()
                .next()
                .is_none(),
            "no scratch left behind"
        );
    }

    #[test]
    fn reinstalling_same_nexus_file_updates_in_place() {
        let mut f = fixture();
        let id = f.instance.id.clone();
        let meta = ModMeta {
            nexus_mod_id: Some(1),
            nexus_file_id: Some(2),
            ..Default::default()
        };
        let v1 = zip(&f, "v1.zip", &[("a.archive", "1")]);
        let v2 = zip(&f, "v2.zip", &[("a.archive", "2")]);
        let first = f
            .lib
            .install_archive(&f.game, &v1, meta.clone(), Some(&id))
            .unwrap();
        f.lib.deploy(&f.game, &f.instance).unwrap();
        let second = f
            .lib
            .install_archive(&f.game, &v2, meta, Some(&id))
            .unwrap();
        assert!(second.replaced_existing);
        assert_eq!(first.record.id, second.record.id);
        assert!(f.lib.loadout(&f.game, &f.instance).unwrap().pending_changes);
        f.lib.deploy(&f.game, &f.instance).unwrap();
        let deployed = f.instance.install_path.join("archive/pc/mod/a.archive");
        assert_eq!(fs::read_to_string(deployed).unwrap(), "2");
    }
}
