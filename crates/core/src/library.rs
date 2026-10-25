//! The mod library: installed mods, per-install profiles (load orders) and
//! deployment. This is the main entry point the CLI and the app use.
//!
//! On disk (in the app's data folder):
//! - `library.db`              SQLite database
//! - `mods/<mod id>/...`       each mod unpacked in game-folder layout
//! - `downloads/`              archives downloaded from Nexus
//! - `backups/<instance>/...`  original game files replaced by mods
//! - `tmp/`                    extraction scratch space

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use ts_rs::TS;
use walkdir::WalkDir;

use crate::archive;
use crate::deploy::{self, DeployReport, DesiredFile};
use crate::detect::GameInstance;
use crate::error::{Error, Result};
use crate::fs_util::resolve_case_insensitive;
use crate::game::GameDefinition;
use crate::health;
use crate::installer::{self, InstallPlan};
use crate::nexus::{NexusMatch, parse_archive_name};
use crate::requirements::{self, FrameworkState, FrameworkStatus, MissingRequirement, ModFiles};
use crate::store::{ModRecord, Profile, ProfileEntry, Store};

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
    /// Frameworks installed before that update. Script extenders usually
    /// need a new version after a game patch.
    pub outdated_frameworks: Vec<String>,
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

/// Owner recorded for the generated load-order file in the deployment record.
pub const LOAD_ORDER_FILE_OWNER: &str = "generated:load-order";

pub struct Library {
    store: Store,
    root: PathBuf,
}

impl Library {
    /// The default data folder, e.g. `~/Library/Application Support/ModManager`.
    pub fn default_root() -> PathBuf {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ModManager")
    }

    pub fn open(root: &Path) -> Result<Self> {
        for dir in ["mods", "downloads", "backups", "tmp"] {
            let path = root.join(dir);
            fs::create_dir_all(&path).map_err(|e| Error::io(&path, e))?;
        }
        let store = Store::open(&root.join("library.db"))?;
        let library = Self {
            store,
            root: root.to_path_buf(),
        };
        library.clean_tmp();
        Ok(library)
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

    fn clean_tmp(&self) {
        if let Ok(entries) = fs::read_dir(self.root.join("tmp")) {
            for entry in entries.flatten() {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }

    // Installing

    /// Extracts and installs an archive. When `instance_id` is given, the mod
    /// is enabled at the end of that install's active profile.
    pub fn install_archive(
        &mut self,
        game: &GameDefinition,
        archive_path: &Path,
        mut meta: ModMeta,
        instance_id: Option<&str>,
    ) -> Result<InstallOutcome> {
        let scratch = self.root.join("tmp").join(uuid::Uuid::new_v4().to_string());
        let result = (|| {
            archive::extract(archive_path, &scratch)?;
            let archive_name = archive_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned());
            if let Some(parsed) = archive_name.as_deref().map(parse_archive_name) {
                meta.name = meta.name.or(Some(parsed.name));
                meta.version = meta.version.or(parsed.version);
                meta.nexus_mod_id = meta.nexus_mod_id.or(parsed.nexus_mod_id);
            }
            self.install_from_dir(game, &scratch, meta, archive_name, instance_id)
        })();
        let _ = fs::remove_dir_all(&scratch);
        result
    }

    fn install_from_dir(
        &mut self,
        game: &GameDefinition,
        extracted: &Path,
        meta: ModMeta,
        archive_name: Option<String>,
        instance_id: Option<&str>,
    ) -> Result<InstallOutcome> {
        let name = meta.name.clone().unwrap_or_else(|| "Unnamed mod".into());
        let plan: InstallPlan = installer::plan_install(game, extracted, &name)?;

        // Same Nexus file installed again, or an explicit update: replace it.
        let existing = match &meta.replaces {
            Some(id) => self.store.get_mod(id)?,
            None => match (meta.nexus_mod_id, meta.nexus_file_id) {
                (Some(mod_id), Some(file_id)) => self
                    .store
                    .find_mod_by_nexus_id(&game.id, mod_id)?
                    .filter(|m| m.nexus_file_id == Some(file_id)),
                _ => None,
            },
        };
        let id = existing
            .as_ref()
            .map_or_else(|| uuid::Uuid::new_v4().to_string(), |m| m.id.clone());

        // Stage into a fresh folder, then swap it in.
        let staging = self.root.join("tmp").join(format!("{id}.staging"));
        let mut size_bytes = 0u64;
        for file in &plan.files {
            let from = join_relative(extracted, &file.source);
            let to = join_relative(&staging, &file.target);
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
            }
            fs::rename(&from, &to)
                .or_else(|_| fs::copy(&from, &to).map(|_| ()))
                .map_err(|e| Error::io(&to, e))?;
            size_bytes += fs::metadata(&to).map(|m| m.len()).unwrap_or(0);
        }
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
            version: meta.version,
            author: meta.author,
            summary: meta.summary,
            picture_url: meta.picture_url,
            nexus_mod_id: meta.nexus_mod_id,
            nexus_file_id: meta.nexus_file_id,
            archive_name,
            kinds: plan.kinds.clone(),
            file_count: plan.files.len() as u32,
            size_bytes,
            installed_at: now(),
            revision: existing.as_ref().map_or(1, |m| m.revision + 1),
            update_file_id: None,
            update_version: None,
        };
        let targets: Vec<String> = plan.files.iter().map(|f| f.target.clone()).collect();
        self.store.insert_mod(&record, &targets)?;

        if existing.is_none()
            && let Some(instance_id) = instance_id
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
            warnings: plan.warnings,
            replaced_existing: existing.is_some(),
        })
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
        let analysis = {
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
            });

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

    /// Notes game updates and which installed frameworks predate the last one.
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
        let outdated_frameworks = match updated_at {
            Some(updated) => frameworks
                .iter()
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
            for target in self.store.mod_files(&record.id)? {
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
        let current = self.store.deployed_files(&instance.id)?;
        // Folders mods are dropped into stay even when empty; a framework's
        // own folder (no extensions or marker) is removed with it.
        let protected: Vec<String> = game
            .layout
            .iter()
            .filter(|t| !t.extensions.is_empty() || t.marker_file.is_some())
            .map(|t| t.path.clone())
            .chain(game.root_markers.iter().cloned())
            .collect();
        let (report, deployed) = deploy::deploy(
            &instance.install_path,
            &self.backup_dir(&instance.id),
            desired,
            &current,
            &protected,
        )?;
        self.store
            .set_deployed_files(&instance.id, &deployed, now())?;
        Ok(report)
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

pub(crate) fn now() -> i64 {
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
