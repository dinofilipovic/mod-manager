//! Makes the game folder match a set of desired mod files, and undoes it.
//!
//! Mods stay unpacked in the library; deployment places each file into the
//! game folder by the cheapest method that works: a copy-on-write clone
//! (APFS, Btrfs, XFS, ReFS), then a hard link, then a plain copy. Game files
//! that a mod replaces are moved to a backup folder and put back when the mod
//! is removed. Every placed file is recorded, so nothing else is touched.
//!
//! Each filesystem operation is written to the deploy journal before it
//! starts and marked done together with its effect on the deployment record.
//! An operation that fails, or that was cut short by a crash, is undone (a
//! new file) or finished (a replacement or removal) from what is on disk by
//! [`recover`], so the record always matches the game folder and an original
//! game file is never overwritten or lost.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use ts_rs::TS;

use crate::error::{Error, Result};
use crate::fs_util::{
    copy_atomic, move_no_clobber, remove_file_if_exists, resolve_case_insensitive, sha256_file,
    size_and_mtime,
};
use crate::store::{DeployedChange, DeployedFile, JournalEntry, JournalOp, Store};

/// Text and config files: never hard-linked, so editing them in the game
/// folder can't change the library's copy.
const TEXT_EXTENSIONS: &[&str] = &[
    "json", "yaml", "yml", "ini", "lua", "toml", "xml", "txt", "cfg",
];

/// A file that should be in the game folder after deployment.
#[derive(Debug, Clone)]
pub struct DesiredFile {
    pub target: String,
    pub mod_id: String,
    pub revision: u32,
    pub source: PathBuf,
    /// Recorded at install; computed while deploying when missing.
    pub sha256: Option<String>,
}

/// Where a deployment happens and what it may not remove.
pub struct Deployment<'a> {
    pub instance_id: &'a str,
    pub game_root: &'a Path,
    pub backup_root: &'a Path,
    /// User edits to deployed files are copied here before they are replaced.
    pub preserved_root: &'a Path,
    /// Folders never removed even if empty.
    pub protected_dirs: &'a [String],
    /// When the game last changed (an update), Unix seconds.
    pub game_updated_at: Option<i64>,
    /// The library's unpacked mods, `<mods_root>/<mod id>/<target>`.
    pub mods_root: &'a Path,
}

#[derive(Debug, Clone, Default, Serialize, TS)]
#[ts(export)]
pub struct DeployReport {
    pub added: u32,
    pub removed: u32,
    pub replaced: u32,
    pub unchanged: u32,
    /// Original game files moved aside because a mod replaces them.
    pub backed_up: u32,
    pub restored: u32,
    pub cloned: u32,
    pub linked: u32,
    pub copied: u32,
    /// Deployed files the game replaced (an update or file check); the game's
    /// version was kept and the outdated backup dropped.
    pub adopted: u32,
    /// Deployed files edited in the game folder, copied to the preserved
    /// folder before being replaced or removed.
    pub preserved: Vec<String>,
    pub warnings: Vec<String>,
}

/// How a deployed file differs from what was placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub enum DriftKind {
    /// Gone from the game folder.
    Missing,
    /// Changed in the game folder, most likely edited by hand.
    Modified,
    /// The game put its own file there (an update or a file check).
    ReplacedByGame,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct DriftedFile {
    pub target: String,
    pub mod_id: String,
    pub mod_name: Option<String>,
    pub kind: DriftKind,
}

/// Deployed files that no longer match what was placed.
#[derive(Debug, Clone, Default, Serialize, TS)]
#[ts(export)]
pub struct DeploymentCheck {
    pub checked: u32,
    pub drifted: Vec<DriftedFile>,
}

impl DeploymentCheck {
    /// Whether applying again would change the game folder: missing files
    /// come back and files the game replaced become the new originals.
    /// Hand edits alone don't count; they stay until the mod changes.
    pub fn needs_apply(&self) -> bool {
        self.drifted.iter().any(|d| d.kind != DriftKind::Modified)
    }
}

enum Placed {
    Cloned,
    Linked,
    Copied,
}

/// Compares the deployed files with the game folder.
pub fn verify(
    game_root: &Path,
    deployed: &[DeployedFile],
    game_updated_at: Option<i64>,
) -> DeploymentCheck {
    let drifted = deployed
        .iter()
        .filter_map(|d| {
            let kind = drift(d, &target_path(game_root, &d.target), game_updated_at)?;
            Some(DriftedFile {
                target: d.target.clone(),
                mod_id: d.mod_id.clone(),
                mod_name: None,
                kind,
            })
        })
        .collect();
    DeploymentCheck {
        checked: deployed.len() as u32,
        drifted,
    }
}

/// How the file at `path` differs from the deployed record, if it does.
/// Size and time are compared first; the contents only when those differ.
pub fn drift(file: &DeployedFile, path: &Path, game_updated_at: Option<i64>) -> Option<DriftKind> {
    let Ok((size, mtime)) = size_and_mtime(path) else {
        return Some(DriftKind::Missing);
    };
    // Recorded before version 3: nothing to compare with.
    let (Some(want_size), Some(want_mtime)) = (file.size, file.mtime) else {
        return None;
    };
    if size == want_size && mtime == want_mtime {
        return None;
    }
    let actual = sha256_file(path).ok();
    if actual.is_some() && actual == file.sha256 {
        return None;
    }
    if actual.is_some() && actual == file.backup_sha256 {
        return Some(DriftKind::ReplacedByGame);
    }
    // Written by the game's update: changed after it was placed and no later
    // than when the update was noticed. Later changes are someone's edit.
    let changed_at = mtime.div_euclid(1_000_000_000);
    match (game_updated_at, file.placed_at) {
        (Some(updated), Some(placed)) if placed <= changed_at && changed_at <= updated => {
            Some(DriftKind::ReplacedByGame)
        }
        _ => Some(DriftKind::Modified),
    }
}

/// For deployments recorded before version 3 (no size or hash): whether the
/// file differs from the mod's file in the library, so an edit is kept.
fn legacy_edited(d: &Deployment, file: &DeployedFile, path: &Path) -> bool {
    if file.size.is_some() || !path.is_file() {
        return false;
    }
    let source = join_relative(&d.mods_root.join(&file.mod_id), &file.target);
    match (sha256_file(&source), sha256_file(path)) {
        (Ok(want), Ok(have)) => want != have,
        _ => false,
    }
}

/// Brings the game folder from the recorded deployment to `desired` (keyed
/// by lowercased target). Progress is recorded file by file, so after an
/// error the record still matches the disk and applying again continues.
pub fn deploy(
    d: &Deployment,
    desired: &BTreeMap<String, DesiredFile>,
    store: &mut Store,
) -> Result<DeployReport> {
    let mut report = DeployReport::default();
    let current = store.deployed_files(d.instance_id)?;
    let mut kept: HashSet<String> = HashSet::new();

    // Remove or refresh what is deployed now.
    for deployed in &current {
        let key = deployed.target.to_ascii_lowercase();
        let path = target_path(d.game_root, &deployed.target);
        let drifted = drift(deployed, &path, d.game_updated_at);
        if drifted == Some(DriftKind::ReplacedByGame) {
            let entry = journal_entry(d, JournalOp::Adopt, &deployed.target);
            run(d, store, entry, &mut report)?;
            report.adopted += 1;
            continue;
        }
        match desired.get(&key) {
            Some(want)
                if want.mod_id == deployed.mod_id
                    && want.revision == deployed.revision
                    && drifted != Some(DriftKind::Missing) =>
            {
                report.unchanged += 1;
                kept.insert(key);
            }
            Some(want) => {
                if drifted == Some(DriftKind::Modified) || legacy_edited(d, deployed, &path) {
                    preserve(d, &deployed.target, &path, &mut report)?;
                }
                let entry = JournalEntry {
                    source: Some(want.source.to_string_lossy().into_owned()),
                    mod_id: Some(want.mod_id.clone()),
                    revision: Some(want.revision),
                    sha256: want.sha256.clone(),
                    ..journal_entry(d, JournalOp::Replace, &deployed.target)
                };
                run(d, store, entry, &mut report)?;
                report.replaced += 1;
                kept.insert(key);
            }
            None => {
                if drifted == Some(DriftKind::Modified) || legacy_edited(d, deployed, &path) {
                    preserve(d, &deployed.target, &path, &mut report)?;
                }
                let entry = journal_entry(d, JournalOp::Remove, &deployed.target);
                run(d, store, entry, &mut report)?;
                report.removed += 1;
            }
        }
    }

    // Add what is new.
    for (key, want) in desired {
        if kept.contains(key) {
            continue;
        }
        let path = target_path(d.game_root, &want.target);
        let mut entry = JournalEntry {
            source: Some(want.source.to_string_lossy().into_owned()),
            mod_id: Some(want.mod_id.clone()),
            revision: Some(want.revision),
            sha256: want.sha256.clone(),
            ..journal_entry(d, JournalOp::Add, &want.target)
        };
        let occupied = fs::symlink_metadata(&path).is_ok();
        if join_relative(d.backup_root, &want.target).exists() {
            // A backup without a deployment record (left by a failed deploy
            // before the journal existed) holds the real original. It stays
            // the backup; a file in the game folder is set aside.
            entry.backup = Some(want.target.clone());
            if occupied {
                preserve(d, &want.target, &path, &mut report)?;
                remove_file_if_exists(&path).map_err(|e| Error::io(&path, e))?;
                report.warnings.push(format!(
                    "{}: kept the earlier backup of the original and moved the file in the game folder to the preserved folder",
                    want.target
                ));
            }
        } else if occupied {
            // A game or user file we didn't place: keep it to restore later.
            entry.backup = Some(want.target.clone());
            entry.moved_original = true;
            report.backed_up += 1;
        }
        run(d, store, entry, &mut report)?;
        report.added += 1;
    }

    store.set_deployed_at(d.instance_id, crate::library::now())?;
    store.clear_finished_journal()?;
    Ok(report)
}

fn journal_entry(d: &Deployment, op: JournalOp, target: &str) -> JournalEntry {
    JournalEntry {
        id: 0,
        instance_id: d.instance_id.to_string(),
        game_root: d.game_root.to_string_lossy().into_owned(),
        op,
        target: target.to_string(),
        source: None,
        mod_id: None,
        revision: None,
        sha256: None,
        backup: None,
        moved_original: false,
    }
}

/// Journals one operation, runs it and records its effect. If it fails, it
/// is undone or finished from the disk state before the error is returned.
fn run(
    d: &Deployment,
    store: &mut Store,
    mut entry: JournalEntry,
    report: &mut DeployReport,
) -> Result<()> {
    entry.id = store.journal_begin(&entry, crate::library::now())?;
    let result = execute(d, store, &entry, report);
    match result {
        Ok(change) => {
            let change = match &change {
                Some(file) => DeployedChange::Upsert(file),
                None => DeployedChange::Delete(&entry.target),
            };
            store.journal_finish(entry.id, d.instance_id, change)
        }
        Err(e) => {
            let recovered = recover(&entry, d.backup_root, d.protected_dirs, store);
            let note = match recovered {
                Ok(()) => "Nothing was lost; fix the problem and apply again.".to_string(),
                Err(r) => format!("Undoing it also failed ({r}); apply again to retry."),
            };
            Err(Error::Install(format!(
                "Applying stopped at {}: {e}. {note}",
                entry.target
            )))
        }
    }
}

/// Runs a journaled operation; returns the new record for its target, or
/// `None` when the target is no longer deployed.
fn execute(
    d: &Deployment,
    store: &Store,
    entry: &JournalEntry,
    report: &mut DeployReport,
) -> Result<Option<DeployedFile>> {
    let path = target_path(d.game_root, &entry.target);
    match entry.op {
        JournalOp::Add => {
            let mut backup_sha256 = None;
            if let Some(backup) = &entry.backup {
                let backup_path = join_relative(d.backup_root, backup);
                if entry.moved_original {
                    move_no_clobber(&path, &backup_path).map_err(|e| Error::io(&backup_path, e))?;
                }
                backup_sha256 =
                    Some(sha256_file(&backup_path).map_err(|e| Error::io(&backup_path, e))?);
            }
            let source = PathBuf::from(entry.source.as_deref().unwrap_or_default());
            count(report, place(&source, &path)?);
            Ok(Some(placed_record(
                entry,
                &source,
                &path,
                entry.backup.clone(),
                backup_sha256,
            )?))
        }
        JournalOp::Replace => {
            let previous = store
                .deployed_file(d.instance_id, &entry.target)?
                .unwrap_or_default();
            remove_file_if_exists(&path).map_err(|e| Error::io(&path, e))?;
            let source = PathBuf::from(entry.source.as_deref().unwrap_or_default());
            count(report, place(&source, &path)?);
            Ok(Some(placed_record(
                entry,
                &source,
                &path,
                previous.backup,
                previous.backup_sha256,
            )?))
        }
        JournalOp::Remove => {
            let previous = store
                .deployed_file(d.instance_id, &entry.target)?
                .unwrap_or_default();
            remove_file_if_exists(&path).map_err(|e| Error::io(&path, e))?;
            if let Some(backup) = &previous.backup {
                let backup_path = join_relative(d.backup_root, backup);
                if backup_path.exists() {
                    move_no_clobber(&backup_path, &path).map_err(|e| Error::io(&path, e))?;
                    report.restored += 1;
                } else {
                    report.warnings.push(format!(
                        "{}: the backup of the original game file is missing; verify the game files in its launcher",
                        entry.target
                    ));
                }
            }
            remove_empty_parents(d.game_root, &path, d.protected_dirs);
            Ok(None)
        }
        JournalOp::Adopt => {
            let previous = store
                .deployed_file(d.instance_id, &entry.target)?
                .unwrap_or_default();
            if let Some(backup) = &previous.backup {
                let backup_path = join_relative(d.backup_root, backup);
                remove_file_if_exists(&backup_path).map_err(|e| Error::io(&backup_path, e))?;
            }
            Ok(None)
        }
    }
}

fn placed_record(
    entry: &JournalEntry,
    source: &Path,
    path: &Path,
    backup: Option<String>,
    backup_sha256: Option<String>,
) -> Result<DeployedFile> {
    let (size, mtime) = size_and_mtime(path).map_err(|e| Error::io(path, e))?;
    let sha256 = match &entry.sha256 {
        Some(hash) => hash.clone(),
        None => sha256_file(source).map_err(|e| Error::io(source, e))?,
    };
    Ok(DeployedFile {
        target: entry.target.clone(),
        mod_id: entry.mod_id.clone().unwrap_or_default(),
        revision: entry.revision.unwrap_or_default(),
        backup,
        size: Some(size),
        mtime: Some(mtime),
        sha256: Some(sha256),
        backup_sha256,
        placed_at: Some(crate::library::now()),
    })
}

/// Brings an interrupted operation to a consistent end from what is on
/// disk: a new file is taken away again (with the original put back), a
/// replacement, removal or adoption is finished. Records the result.
pub fn recover(
    entry: &JournalEntry,
    backup_root: &Path,
    protected_dirs: &[String],
    store: &mut Store,
) -> Result<()> {
    let game_root = Path::new(&entry.game_root);
    let path = target_path(game_root, &entry.target);
    let io = |p: &Path| {
        let p = p.to_path_buf();
        move |e| Error::io(p, e)
    };
    let previous = store.deployed_file(&entry.instance_id, &entry.target)?;
    match entry.op {
        JournalOp::Add => {
            let backup_path = entry.backup.as_ref().map(|b| join_relative(backup_root, b));
            match backup_path {
                // Not moved yet means the original is still in place.
                Some(backup_path) if entry.moved_original && !backup_path.exists() => {}
                // The original goes back into the game folder, also when it
                // was an earlier backup kept as is.
                Some(backup_path) if backup_path.exists() => {
                    remove_file_if_exists(&path).map_err(io(&path))?;
                    move_no_clobber(&backup_path, &path).map_err(io(&path))?;
                }
                _ => {
                    remove_file_if_exists(&path).map_err(io(&path))?;
                    remove_empty_parents(game_root, &path, protected_dirs);
                }
            }
            store.journal_finish(entry.id, &entry.instance_id, DeployedChange::Nothing)
        }
        JournalOp::Replace => {
            let previous = previous.unwrap_or_default();
            let source = PathBuf::from(entry.source.as_deref().unwrap_or_default());
            remove_file_if_exists(&path).map_err(io(&path))?;
            if source.is_file() {
                place(&source, &path)?;
                let record = placed_record(
                    entry,
                    &source,
                    &path,
                    previous.backup,
                    previous.backup_sha256,
                )?;
                store.journal_finish(
                    entry.id,
                    &entry.instance_id,
                    DeployedChange::Upsert(&record),
                )
            } else {
                // The new file is gone from the library: end up removed.
                restore_backup(&previous, backup_root, &path)?;
                remove_empty_parents(game_root, &path, protected_dirs);
                store.journal_finish(
                    entry.id,
                    &entry.instance_id,
                    DeployedChange::Delete(&entry.target),
                )
            }
        }
        JournalOp::Remove => {
            let previous = previous.unwrap_or_default();
            match &previous.backup {
                // Already restored when the backup is gone.
                Some(backup) if !join_relative(backup_root, backup).exists() => {}
                _ => {
                    remove_file_if_exists(&path).map_err(io(&path))?;
                    restore_backup(&previous, backup_root, &path)?;
                }
            }
            remove_empty_parents(game_root, &path, protected_dirs);
            store.journal_finish(
                entry.id,
                &entry.instance_id,
                DeployedChange::Delete(&entry.target),
            )
        }
        JournalOp::Adopt => {
            if let Some(backup) = previous.and_then(|p| p.backup) {
                let backup_path = join_relative(backup_root, &backup);
                remove_file_if_exists(&backup_path).map_err(io(&backup_path))?;
            }
            store.journal_finish(
                entry.id,
                &entry.instance_id,
                DeployedChange::Delete(&entry.target),
            )
        }
    }
}

fn restore_backup(previous: &DeployedFile, backup_root: &Path, path: &Path) -> Result<()> {
    if let Some(backup) = &previous.backup {
        let backup_path = join_relative(backup_root, backup);
        if backup_path.exists() {
            move_no_clobber(&backup_path, path).map_err(|e| Error::io(path, e))?;
        }
    }
    Ok(())
}

/// Copies an edited deployed file to the preserved folder, unless an
/// identical copy is already there.
fn preserve(d: &Deployment, target: &str, path: &Path, report: &mut DeployReport) -> Result<()> {
    let mut dest = join_relative(d.preserved_root, target);
    if dest.exists() {
        let same = sha256_file(&dest).ok().zip(sha256_file(path).ok());
        if same.is_some_and(|(a, b)| a == b) {
            return Ok(());
        }
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        dest.set_file_name(format!("{name}.{}", crate::library::now()));
    }
    copy_atomic(path, &dest).map_err(|e| Error::io(&dest, e))?;
    report.preserved.push(target.to_string());
    Ok(())
}

fn count(report: &mut DeployReport, placed: Placed) {
    match placed {
        Placed::Cloned => report.cloned += 1,
        Placed::Linked => report.linked += 1,
        Placed::Copied => report.copied += 1,
    }
}

/// The on-disk path for `target`, reusing existing folders whose names differ
/// only in case so we don't create `Archive/` next to `archive/`.
pub fn target_path(game_root: &Path, target: &str) -> PathBuf {
    let parts: Vec<&str> = target.split('/').filter(|p| !p.is_empty()).collect();
    let mut path = game_root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        let is_last = i + 1 == parts.len();
        path = match resolve_case_insensitive(&path, part) {
            Some(existing) if is_last || existing.is_dir() => existing,
            _ => path.join(part),
        };
    }
    path
}

fn join_relative(base: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .filter(|p| !p.is_empty())
        .fold(base.to_path_buf(), |path, part| path.join(part))
}

fn is_text_file(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| TEXT_EXTENSIONS.contains(&e.as_str()))
}

fn place(source: &Path, dest: &Path) -> Result<Placed> {
    if !source.is_file() {
        return Err(Error::Install(format!(
            "the mod's file {} is missing from the library; reinstall the mod",
            source.display()
        )));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    if reflink_copy::reflink(source, dest).is_ok() {
        return Ok(Placed::Cloned);
    }
    if !is_text_file(dest) && fs::hard_link(source, dest).is_ok() {
        return Ok(Placed::Linked);
    }
    fs::copy(source, dest).map_err(|e| Error::io(dest, e))?;
    Ok(Placed::Copied)
}

/// Removes folders left empty by removing `file`, stopping at the game root
/// and at protected folders.
fn remove_empty_parents(game_root: &Path, file: &Path, protected_dirs: &[String]) {
    let mut dir = file.parent();
    while let Some(d) = dir {
        let Ok(relative) = d.strip_prefix(game_root) else {
            break;
        };
        let relative = relative
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let protected = relative.is_empty()
            || protected_dirs.iter().any(|p| {
                let p = p.to_ascii_lowercase();
                p == relative || p.starts_with(&format!("{relative}/"))
            });
        if protected || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        game: PathBuf,
        backup: PathBuf,
        preserved: PathBuf,
        staging: PathBuf,
        store: Store,
        protected: Vec<String>,
        game_updated_at: Option<i64>,
    }

    impl Fixture {
        fn deployment(&self) -> Deployment<'_> {
            Deployment {
                instance_id: "i",
                game_root: &self.game,
                backup_root: &self.backup,
                preserved_root: &self.preserved,
                protected_dirs: &self.protected,
                game_updated_at: self.game_updated_at,
                mods_root: &self.staging,
            }
        }

        fn deploy(&mut self, want: &BTreeMap<String, DesiredFile>) -> Result<DeployReport> {
            let d = Deployment {
                instance_id: "i",
                game_root: &self.game,
                backup_root: &self.backup,
                preserved_root: &self.preserved,
                protected_dirs: &self.protected,
                game_updated_at: self.game_updated_at,
                mods_root: &self.staging,
            };
            deploy(&d, want, &mut self.store)
        }

        fn read(&self, target: &str) -> String {
            fs::read_to_string(self.game.join(target)).unwrap()
        }
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("game");
        fs::create_dir_all(game.join("bin/x64")).unwrap();
        fs::write(game.join("bin/x64/game.exe"), b"exe").unwrap();
        Fixture {
            backup: tmp.path().join("backup"),
            preserved: tmp.path().join("preserved"),
            staging: tmp.path().join("staging"),
            store: Store::open_in_memory().unwrap(),
            protected: vec!["archive/pc/mod".into()],
            game_updated_at: None,
            game,
            _tmp: tmp,
        }
    }

    fn stage(f: &Fixture, mod_id: &str, target: &str, data: &str) -> DesiredFile {
        let source = f.staging.join(mod_id).join(target);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, data).unwrap();
        DesiredFile {
            target: target.into(),
            mod_id: mod_id.into(),
            revision: 1,
            source,
            sha256: None,
        }
    }

    fn desired(files: Vec<DesiredFile>) -> BTreeMap<String, DesiredFile> {
        files
            .into_iter()
            .map(|f| (f.target.to_ascii_lowercase(), f))
            .collect()
    }

    #[test]
    fn deploys_then_purges_cleanly() {
        let mut f = fixture();
        let want = desired(vec![stage(&f, "a", "archive/pc/mod/a.archive", "A")]);

        let report = f.deploy(&want).unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(f.read("archive/pc/mod/a.archive"), "A");

        let report = f.deploy(&want).unwrap();
        assert_eq!(report.unchanged, 1);

        let report = f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(report.removed, 1);
        assert!(f.store.deployed_files("i").unwrap().is_empty());
        assert!(!f.game.join("archive/pc/mod/a.archive").exists());
        assert!(
            f.game.join("archive/pc/mod").is_dir(),
            "protected folder kept"
        );
        assert!(f.store.pending_journal().unwrap().is_empty());
    }

    #[test]
    fn backs_up_and_restores_game_files() {
        let mut f = fixture();
        let want = desired(vec![stage(&f, "a", "bin/x64/game.exe", "modded")]);

        let report = f.deploy(&want).unwrap();
        assert_eq!(report.backed_up, 1);
        assert_eq!(f.read("bin/x64/game.exe"), "modded");
        let record = &f.store.deployed_files("i").unwrap()[0];
        assert!(record.backup_sha256.is_some(), "backups are always hashed");

        let report = f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(report.restored, 1);
        assert_eq!(f.read("bin/x64/game.exe"), "exe");
    }

    #[test]
    fn switching_owner_replaces_file() {
        let mut f = fixture();
        let a = desired(vec![stage(&f, "a", "r6/tweaks/x.yaml", "A")]);
        let b = desired(vec![stage(&f, "b", "r6/tweaks/x.yaml", "B")]);

        f.deploy(&a).unwrap();
        let report = f.deploy(&b).unwrap();
        assert_eq!(report.replaced, 1);
        assert_eq!(f.store.deployed_files("i").unwrap()[0].mod_id, "b");
        assert_eq!(f.read("r6/tweaks/x.yaml"), "B");
    }

    #[test]
    fn reuses_existing_folder_case() {
        let mut f = fixture();
        fs::create_dir_all(f.game.join("Archive/PC")).unwrap();
        let want = desired(vec![stage(&f, "a", "archive/pc/mod/a.archive", "A")]);
        f.deploy(&want).unwrap();
        let top: Vec<String> = fs::read_dir(&f.game)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.eq_ignore_ascii_case("archive"))
            .collect();
        assert_eq!(top, ["Archive"], "no second folder differing only in case");
    }

    #[test]
    fn text_files_are_never_hard_linked() {
        let mut f = fixture();
        let want = desired(vec![stage(&f, "a", "r6/config/x.json", "{}")]);
        let report = f.deploy(&want).unwrap();
        assert_eq!(report.linked, 0);
        fs::write(f.game.join("r6/config/x.json"), "{\"edited\":1}").unwrap();
        assert_eq!(
            fs::read_to_string(f.staging.join("a/r6/config/x.json")).unwrap(),
            "{}",
            "editing the game copy leaves the library copy alone"
        );
    }

    #[test]
    fn failure_mid_deploy_keeps_record_and_original() {
        let mut f = fixture();
        let a = stage(&f, "a", "bin/x64/game.exe", "modded");
        let mut b = stage(&f, "a", "r6/scripts/b.reds", "B");
        b.source = f.staging.join("missing");
        let want = desired(vec![a, b]);
        let err = f.deploy(&want).unwrap_err().to_string();
        assert!(err.contains("r6/scripts/b.reds"), "{err}");
        let deployed = f.store.deployed_files("i").unwrap();
        assert_eq!(deployed.len(), 1, "the finished file is recorded");
        assert!(f.store.pending_journal().unwrap().is_empty());
        assert!(
            !f.game.join("r6").exists(),
            "the failed file left no folders"
        );

        f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(f.read("bin/x64/game.exe"), "exe");
    }

    #[test]
    fn verify_reports_each_drift_kind() {
        let mut f = fixture();
        let want = desired(vec![
            stage(&f, "a", "archive/pc/mod/gone.archive", "G"),
            stage(&f, "a", "archive/pc/mod/edited.archive", "E"),
            stage(&f, "a", "bin/x64/game.exe", "modded"),
            stage(&f, "a", "archive/pc/mod/same.archive", "S"),
        ]);
        f.deploy(&want).unwrap();
        fs::remove_file(f.game.join("archive/pc/mod/gone.archive")).unwrap();
        // Copies keep the original bytes but get a new time: not drift.
        let same = f.game.join("archive/pc/mod/same.archive");
        fs::remove_file(&same).unwrap();
        fs::write(&same, "S").unwrap();
        // Rewritten rather than edited in place: hard links share the inode.
        let edited = f.game.join("archive/pc/mod/edited.archive");
        fs::remove_file(&edited).unwrap();
        fs::write(&edited, "edited by hand").unwrap();
        // The launcher's file check puts the original executable back.
        let exe = f.game.join("bin/x64/game.exe");
        fs::remove_file(&exe).unwrap();
        fs::write(&exe, "exe").unwrap();

        let check = verify(&f.game, &f.store.deployed_files("i").unwrap(), None);
        let kinds: Vec<(&str, DriftKind)> = check
            .drifted
            .iter()
            .map(|d| (d.target.as_str(), d.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("archive/pc/mod/edited.archive", DriftKind::Modified),
                ("archive/pc/mod/gone.archive", DriftKind::Missing),
                ("bin/x64/game.exe", DriftKind::ReplacedByGame),
            ]
        );
        assert!(check.needs_apply());
        let _ = f.deployment();
    }

    #[test]
    fn game_patch_over_a_deployed_file_becomes_the_new_original() {
        let mut f = fixture();
        let want = desired(vec![stage(&f, "a", "bin/x64/game.exe", "modded")]);
        f.deploy(&want).unwrap();
        // A patch writes a new executable, then the game is seen updated.
        let exe = f.game.join("bin/x64/game.exe");
        fs::remove_file(&exe).unwrap();
        fs::write(&exe, "patched").unwrap();
        f.game_updated_at = Some(crate::library::now() + 1);
        let check = verify(
            &f.game,
            &f.store.deployed_files("i").unwrap(),
            f.game_updated_at,
        );
        assert_eq!(check.drifted[0].kind, DriftKind::ReplacedByGame);

        // Purge keeps the patched file and drops the outdated backup.
        let report = f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(f.read("bin/x64/game.exe"), "patched");
        assert!(!f.backup.join("bin/x64/game.exe").exists());

        // The next deploy backs up the patched file as the original.
        f.game_updated_at = None;
        let report = f.deploy(&want).unwrap();
        assert_eq!(report.backed_up, 1);
        assert_eq!(
            fs::read_to_string(f.backup.join("bin/x64/game.exe")).unwrap(),
            "patched"
        );
        f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(f.read("bin/x64/game.exe"), "patched");
    }

    #[test]
    fn hand_edits_after_a_game_update_keep_the_original_backup() {
        let mut f = fixture();
        let want = desired(vec![stage(&f, "a", "bin/x64/game.exe", "modded")]);
        f.deploy(&want).unwrap();
        // The game updated (noticed now), then the user edits the deployed file.
        f.game_updated_at = Some(crate::library::now() - 1);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let exe = f.game.join("bin/x64/game.exe");
        fs::remove_file(&exe).unwrap();
        fs::write(&exe, "edited").unwrap();
        let check = verify(
            &f.game,
            &f.store.deployed_files("i").unwrap(),
            f.game_updated_at,
        );
        assert_eq!(check.drifted[0].kind, DriftKind::Modified);

        let report = f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(report.adopted, 0);
        assert_eq!(report.preserved, ["bin/x64/game.exe"]);
        assert_eq!(
            f.read("bin/x64/game.exe"),
            "exe",
            "the real original is back"
        );
    }

    #[test]
    fn orphaned_backup_with_an_empty_target_is_restored() {
        let mut f = fixture();
        // A failed deploy moved the original aside and nothing is at the target.
        fs::create_dir_all(f.backup.join("bin/x64")).unwrap();
        fs::rename(
            f.game.join("bin/x64/game.exe"),
            f.backup.join("bin/x64/game.exe"),
        )
        .unwrap();
        let want = desired(vec![stage(&f, "a", "bin/x64/game.exe", "modded")]);
        f.deploy(&want).unwrap();
        assert_eq!(f.read("bin/x64/game.exe"), "modded");
        f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(f.read("bin/x64/game.exe"), "exe");

        // Same start, but placing fails: the original goes back into the game.
        fs::rename(
            f.game.join("bin/x64/game.exe"),
            f.backup.join("bin/x64/game.exe"),
        )
        .unwrap();
        let mut broken = stage(&f, "a", "bin/x64/game.exe", "modded");
        broken.source = f.staging.join("missing");
        assert!(f.deploy(&desired(vec![broken])).is_err());
        assert_eq!(f.read("bin/x64/game.exe"), "exe");
        assert!(!f.backup.join("bin/x64/game.exe").exists());
    }

    #[test]
    fn edits_to_files_deployed_before_version_3_are_preserved() {
        let mut f = fixture();
        let a = stage(&f, "a", "r6/config/x.json", "{}");
        fs::create_dir_all(f.game.join("r6/config")).unwrap();
        fs::write(f.game.join("r6/config/x.json"), "{\"mine\":1}").unwrap();
        // A version 2 record: no size, time or hash.
        f.store
            .set_deployed_files(
                "i",
                &[DeployedFile {
                    target: a.target.clone(),
                    mod_id: "a".into(),
                    revision: 1,
                    ..Default::default()
                }],
                0,
            )
            .unwrap();
        let report = f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(report.preserved, ["r6/config/x.json"]);
    }

    #[test]
    fn user_edits_are_preserved_before_removal() {
        let mut f = fixture();
        let want = desired(vec![stage(&f, "a", "r6/config/x.json", "{}")]);
        f.deploy(&want).unwrap();
        let path = f.game.join("r6/config/x.json");
        fs::write(&path, "{\"mine\":1}").unwrap();
        let report = f.deploy(&want).unwrap();
        assert_eq!(
            report.unchanged, 1,
            "hand edits stay while the mod is unchanged"
        );

        let report = f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(report.preserved, ["r6/config/x.json"]);
        assert_eq!(
            fs::read_to_string(f.preserved.join("r6/config/x.json")).unwrap(),
            "{\"mine\":1}"
        );
        assert!(!path.exists());
    }

    #[test]
    fn orphaned_backup_is_kept_as_the_original() {
        let mut f = fixture();
        // A failed deploy from before the journal: original in backups, a
        // mod's file in the game folder, nothing recorded.
        fs::create_dir_all(f.backup.join("bin/x64")).unwrap();
        fs::write(f.backup.join("bin/x64/game.exe"), "exe").unwrap();
        fs::write(f.game.join("bin/x64/game.exe"), "stray mod file").unwrap();

        let want = desired(vec![stage(&f, "a", "bin/x64/game.exe", "modded")]);
        let report = f.deploy(&want).unwrap();
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(
            fs::read_to_string(f.preserved.join("bin/x64/game.exe")).unwrap(),
            "stray mod file"
        );
        f.deploy(&BTreeMap::new()).unwrap();
        assert_eq!(f.read("bin/x64/game.exe"), "exe");
    }

    fn crash(f: &mut Fixture, entry: JournalEntry) {
        f.store.journal_begin(&entry, 0).unwrap();
        let backup = f.backup.clone();
        let protected = f.protected.clone();
        for pending in f.store.pending_journal().unwrap() {
            recover(&pending, &backup, &protected, &mut f.store).unwrap();
        }
        assert!(f.store.pending_journal().unwrap().is_empty());
    }

    #[test]
    fn crash_after_backing_up_puts_the_original_back() {
        let mut f = fixture();
        let a = stage(&f, "a", "bin/x64/game.exe", "modded");
        // The original was moved aside and the mod's file half written.
        fs::create_dir_all(f.backup.join("bin/x64")).unwrap();
        fs::rename(
            f.game.join("bin/x64/game.exe"),
            f.backup.join("bin/x64/game.exe"),
        )
        .unwrap();
        fs::write(f.game.join("bin/x64/game.exe"), "mod").unwrap();
        let entry = JournalEntry {
            source: Some(a.source.to_string_lossy().into_owned()),
            mod_id: Some("a".into()),
            revision: Some(1),
            backup: Some("bin/x64/game.exe".into()),
            moved_original: true,
            ..journal_entry(&f.deployment(), JournalOp::Add, "bin/x64/game.exe")
        };
        crash(&mut f, entry);
        assert_eq!(f.read("bin/x64/game.exe"), "exe");
        assert!(f.store.deployed_files("i").unwrap().is_empty());
    }

    #[test]
    fn crash_mid_removal_finishes_it() {
        let mut f = fixture();
        let want = desired(vec![stage(&f, "a", "bin/x64/game.exe", "modded")]);
        f.deploy(&want).unwrap();
        // The mod's file was deleted, the original not yet moved back.
        fs::remove_file(f.game.join("bin/x64/game.exe")).unwrap();
        let entry = journal_entry(&f.deployment(), JournalOp::Remove, "bin/x64/game.exe");
        crash(&mut f, entry);
        assert_eq!(f.read("bin/x64/game.exe"), "exe");
        assert!(f.store.deployed_files("i").unwrap().is_empty());
    }

    #[test]
    fn crash_mid_replace_finishes_it() {
        let mut f = fixture();
        f.deploy(&desired(vec![stage(&f, "a", "r6/x.reds", "A")]))
            .unwrap();
        let b = stage(&f, "b", "r6/x.reds", "B");
        fs::remove_file(f.game.join("r6/x.reds")).unwrap();
        let entry = JournalEntry {
            source: Some(b.source.to_string_lossy().into_owned()),
            mod_id: Some("b".into()),
            revision: Some(1),
            ..journal_entry(&f.deployment(), JournalOp::Replace, "r6/x.reds")
        };
        crash(&mut f, entry);
        assert_eq!(f.read("r6/x.reds"), "B");
        assert_eq!(f.store.deployed_files("i").unwrap()[0].mod_id, "b");
    }
}
