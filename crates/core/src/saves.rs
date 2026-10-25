//! Backups of a game's save folder: zip snapshots taken by hand or
//! automatically before launching, and restoring one of them.
//!
//! Backups live in the library's `save-backups/<instance>/` folder and are
//! named `<unix seconds>-<reason>.zip`, so listing needs no database.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use ts_rs::TS;
use walkdir::WalkDir;

use crate::env::Environment;
use crate::error::{Error, Result};
use crate::health::wildcard_match;

/// Automatic backups kept per install; older ones are deleted.
const KEEP_AUTOMATIC: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub enum BackupReason {
    /// Taken automatically before the game was launched.
    BeforeLaunch,
    Manual,
    /// The saves that were replaced by restoring another backup.
    BeforeRestore,
}

impl BackupReason {
    fn slug(self) -> &'static str {
        match self {
            Self::BeforeLaunch => "auto",
            Self::Manual => "manual",
            Self::BeforeRestore => "before-restore",
        }
    }

    fn from_slug(slug: &str) -> Option<Self> {
        [Self::BeforeLaunch, Self::Manual, Self::BeforeRestore]
            .into_iter()
            .find(|r| r.slug() == slug)
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct SaveBackup {
    /// The backup's file name; identifies it.
    pub id: String,
    #[ts(type = "number")]
    pub created_at: i64,
    pub reason: BackupReason,
    #[ts(type = "number")]
    pub size_bytes: u64,
}

/// Finds the save folder from a Windows path pattern such as
/// `C:\users\*\Saved Games\CD Projekt Red\Cyberpunk 2077`, where `*` matches
/// within one folder name (the Windows user, `crossover` in CrossOver).
pub fn find_save_dir(env: &Environment, pattern: &str) -> Option<PathBuf> {
    let (drive, rest) = pattern.split_once(':')?;
    let mut candidates = vec![env.windows_to_host(&format!("{drive}:\\"))?];
    for part in rest.split(['\\', '/']).filter(|p| !p.is_empty()) {
        candidates = candidates
            .iter()
            .flat_map(|dir| fs::read_dir(dir).into_iter().flatten().flatten())
            .filter(|e| e.path().is_dir() && wildcard_match(part, &e.file_name().to_string_lossy()))
            .map(|e| e.path())
            .collect();
    }
    // With several users, the one with the most recent saves.
    candidates.into_iter().max_by_key(|dir| newest_mtime(dir))
}

/// Newest modification time of any file in `dir`, in Unix seconds.
pub fn newest_mtime(dir: &Path) -> i64 {
    WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok()?.modified().ok())
        .filter_map(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .max()
        .unwrap_or(0)
}

/// Backups of one install, newest first.
pub fn list(backup_dir: &Path) -> Vec<SaveBackup> {
    let mut backups: Vec<SaveBackup> = fs::read_dir(backup_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let id = entry.file_name().to_string_lossy().into_owned();
            let stem = id.strip_suffix(".zip")?;
            let (time, reason) = stem.split_once('-')?;
            Some(SaveBackup {
                created_at: time.parse().ok()?,
                reason: BackupReason::from_slug(reason)?,
                size_bytes: entry.metadata().ok()?.len(),
                id,
            })
        })
        .collect();
    backups.sort_by_key(|b| std::cmp::Reverse(b.created_at));
    backups
}

/// Zips the save folder into a new backup. Returns `None` when there is
/// nothing to back up.
pub fn create(
    save_dir: &Path,
    backup_dir: &Path,
    reason: BackupReason,
    now: i64,
) -> Result<Option<SaveBackup>> {
    let files: Vec<PathBuf> = WalkDir::new(save_dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .collect();
    if files.is_empty() {
        return Ok(None);
    }
    fs::create_dir_all(backup_dir).map_err(|e| Error::io(backup_dir, e))?;
    // Unique even when two backups happen in the same second.
    let mut created_at = now;
    while backup_dir
        .join(format!("{created_at}-{}.zip", reason.slug()))
        .exists()
    {
        created_at += 1;
    }
    let id = format!("{created_at}-{}.zip", reason.slug());
    let path = backup_dir.join(&id);
    let part = backup_dir.join(format!("{id}.part"));

    let write = || -> io::Result<()> {
        let mut zip = zip::ZipWriter::new(File::create(&part)?);
        // Saves are already compressed; storing them is fast.
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for file in &files {
            let name = file
                .strip_prefix(save_dir)
                .map_err(io::Error::other)?
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            zip.start_file(name, options).map_err(io::Error::other)?;
            io::copy(&mut File::open(file)?, &mut zip)?;
        }
        zip.finish().map_err(io::Error::other)?.flush()
    };
    if let Err(e) = write() {
        let _ = fs::remove_file(&part);
        return Err(Error::io(&path, e));
    }
    fs::rename(&part, &path).map_err(|e| Error::io(&path, e))?;
    prune(backup_dir);
    Ok(list(backup_dir).into_iter().find(|b| b.id == id))
}

/// Takes an automatic backup unless the saves haven't changed since the
/// newest backup.
pub fn auto_backup(save_dir: &Path, backup_dir: &Path, now: i64) -> Result<Option<SaveBackup>> {
    let newest_backup = list(backup_dir).first().map_or(0, |b| b.created_at);
    if newest_mtime(save_dir) <= newest_backup {
        return Ok(None);
    }
    create(save_dir, backup_dir, BackupReason::BeforeLaunch, now)
}

/// Replaces the saves with a backup's contents. The current saves are backed
/// up first, so a restore can always be undone.
pub fn restore(save_dir: &Path, backup_dir: &Path, id: &str, now: i64) -> Result<()> {
    let backup = list(backup_dir)
        .into_iter()
        .find(|b| b.id == id)
        .ok_or_else(|| Error::NotFound(format!("save backup {id}")))?;
    create(save_dir, backup_dir, BackupReason::BeforeRestore, now)?;

    // Extract next to the save folder, then swap, so a failure leaves saves intact.
    let staging = save_dir.with_extension("restoring");
    let _ = fs::remove_dir_all(&staging);
    crate::archive::extract(&backup_dir.join(&backup.id), &staging)?;
    let old = save_dir.with_extension("replaced");
    let _ = fs::remove_dir_all(&old);
    if save_dir.exists() {
        fs::rename(save_dir, &old).map_err(|e| Error::io(save_dir, e))?;
    }
    if let Err(e) = fs::rename(&staging, save_dir) {
        let _ = fs::rename(&old, save_dir);
        return Err(Error::io(save_dir, e));
    }
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

pub fn delete(backup_dir: &Path, id: &str) -> Result<()> {
    if !list(backup_dir).iter().any(|b| b.id == id) {
        return Err(Error::NotFound(format!("save backup {id}")));
    }
    let path = backup_dir.join(id);
    fs::remove_file(&path).map_err(|e| Error::io(&path, e))
}

/// Keeps the newest automatic backups; manual ones are never deleted.
fn prune(backup_dir: &Path) {
    let automatic: Vec<SaveBackup> = list(backup_dir)
        .into_iter()
        .filter(|b| b.reason != BackupReason::Manual)
        .collect();
    for old in automatic.iter().skip(KEEP_AUTOMATIC) {
        let _ = fs::remove_file(backup_dir.join(&old.id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::EnvironmentKind;

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_save_folder_in_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let saves = tmp
            .path()
            .join("drive_c/users/crossover/Saved Games/CD Projekt Red/Cyberpunk 2077");
        write(&saves.join("ManualSave-1/sav.dat"), "x");
        fs::create_dir_all(tmp.path().join("drive_c/users/Public")).unwrap();
        let env = Environment {
            id: "t".into(),
            name: "t".into(),
            kind: EnvironmentKind::WinePrefix,
            prefix: tmp.path().to_path_buf(),
        };
        let found = find_save_dir(
            &env,
            r"C:\users\*\Saved Games\CD Projekt Red\Cyberpunk 2077",
        )
        .unwrap();
        assert!(found.ends_with("crossover/Saved Games/CD Projekt Red/Cyberpunk 2077"));
    }

    #[test]
    fn backs_up_restores_and_prunes() {
        let tmp = tempfile::tempdir().unwrap();
        let saves = tmp.path().join("saves");
        let backups = tmp.path().join("backups");
        write(&saves.join("Save-1/sav.dat"), "one");

        let first = create(&saves, &backups, BackupReason::Manual, 100)
            .unwrap()
            .unwrap();
        write(&saves.join("Save-1/sav.dat"), "two");
        write(&saves.join("Save-2/sav.dat"), "new");

        restore(&saves, &backups, &first.id, 200).unwrap();
        assert_eq!(
            fs::read_to_string(saves.join("Save-1/sav.dat")).unwrap(),
            "one"
        );
        assert!(!saves.join("Save-2").exists());
        let reasons: Vec<BackupReason> = list(&backups).iter().map(|b| b.reason).collect();
        assert_eq!(reasons, [BackupReason::BeforeRestore, BackupReason::Manual]);

        for i in 0..12 {
            create(&saves, &backups, BackupReason::BeforeLaunch, 300 + i).unwrap();
        }
        let all = list(&backups);
        assert_eq!(
            all.iter()
                .filter(|b| b.reason == BackupReason::Manual)
                .count(),
            1
        );
        assert_eq!(all.len(), 1 + KEEP_AUTOMATIC);
    }

    #[test]
    fn auto_backup_skips_unchanged_saves() {
        let tmp = tempfile::tempdir().unwrap();
        let saves = tmp.path().join("saves");
        let backups = tmp.path().join("backups");
        write(&saves.join("a.dat"), "x");
        let far_future = 4_000_000_000;
        assert!(auto_backup(&saves, &backups, far_future).unwrap().is_some());
        assert!(
            auto_backup(&saves, &backups, far_future + 10)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn empty_save_folder_is_not_backed_up() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("saves")).unwrap();
        assert!(
            create(
                &tmp.path().join("saves"),
                &tmp.path().join("b"),
                BackupReason::Manual,
                1
            )
            .unwrap()
            .is_none()
        );
    }
}
