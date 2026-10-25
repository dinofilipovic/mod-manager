//! Makes the game folder match a set of desired mod files, and undoes it.
//!
//! Mods stay unpacked in the library; deployment places each file into the
//! game folder by the cheapest method that works: a copy-on-write clone
//! (APFS, Btrfs, XFS, ReFS), then a hard link, then a plain copy. Game files
//! that a mod replaces are moved to a backup folder and put back when the mod
//! is removed. Every placed file is recorded, so nothing else is touched.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use ts_rs::TS;

use crate::error::{Error, Result};
use crate::fs_util::resolve_case_insensitive;
use crate::store::DeployedFile;

/// A file that should be in the game folder after deployment.
#[derive(Debug, Clone)]
pub struct DesiredFile {
    pub target: String,
    pub mod_id: String,
    pub revision: u32,
    pub source: PathBuf,
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
}

enum Placed {
    Cloned,
    Linked,
    Copied,
}

/// Brings `game_root` from the `current` deployment to `desired` (keyed by
/// lowercased target). `protected_dirs` are folders never removed even if
/// empty. Returns the report and the new deployment record.
pub fn deploy(
    game_root: &Path,
    backup_root: &Path,
    desired: &BTreeMap<String, DesiredFile>,
    current: &[DeployedFile],
    protected_dirs: &[String],
) -> Result<(DeployReport, Vec<DeployedFile>)> {
    let mut report = DeployReport::default();
    let mut result = Vec::new();
    let current: BTreeMap<String, &DeployedFile> = current
        .iter()
        .map(|f| (f.target.to_ascii_lowercase(), f))
        .collect();

    // Remove or refresh what is deployed now.
    for (key, deployed) in &current {
        let path = target_path(game_root, &deployed.target);
        match desired.get(key) {
            Some(want)
                if want.mod_id == deployed.mod_id
                    && want.revision == deployed.revision
                    && path.exists() =>
            {
                report.unchanged += 1;
                result.push(DeployedFile {
                    target: deployed.target.clone(),
                    mod_id: want.mod_id.clone(),
                    revision: want.revision,
                    backup: deployed.backup.clone(),
                });
            }
            Some(want) => {
                remove_file_if_exists(&path)?;
                count(&mut report, place(&want.source, &path)?);
                report.replaced += 1;
                result.push(DeployedFile {
                    target: deployed.target.clone(),
                    mod_id: want.mod_id.clone(),
                    revision: want.revision,
                    backup: deployed.backup.clone(),
                });
            }
            None => {
                remove_file_if_exists(&path)?;
                report.removed += 1;
                if let Some(backup) = &deployed.backup {
                    let backup_path = backup_root.join(backup);
                    if backup_path.exists() {
                        move_file(&backup_path, &path)?;
                        report.restored += 1;
                    }
                }
                remove_empty_parents(game_root, &path, protected_dirs);
            }
        }
    }

    // Add what is new.
    for (key, want) in desired {
        if current.contains_key(key) {
            continue;
        }
        let path = target_path(game_root, &want.target);
        let mut backup = None;
        if path.exists() {
            // A game or user file we didn't place: keep it to restore later.
            let backup_path = backup_root.join(&want.target);
            move_file(&path, &backup_path)?;
            backup = Some(want.target.clone());
            report.backed_up += 1;
        }
        count(&mut report, place(&want.source, &path)?);
        report.added += 1;
        result.push(DeployedFile {
            target: want.target.clone(),
            mod_id: want.mod_id.clone(),
            revision: want.revision,
            backup,
        });
    }

    Ok((report, result))
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

fn place(source: &Path, dest: &Path) -> Result<Placed> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    if reflink_copy::reflink(source, dest).is_ok() {
        return Ok(Placed::Cloned);
    }
    if fs::hard_link(source, dest).is_ok() {
        return Ok(Placed::Linked);
    }
    fs::copy(source, dest).map_err(|e| Error::io(dest, e))?;
    Ok(Placed::Copied)
}

fn move_file(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    }
    if fs::rename(from, to).is_err() {
        // Different volume: copy, then remove.
        fs::copy(from, to).map_err(|e| Error::io(to, e))?;
        fs::remove_file(from).map_err(|e| Error::io(from, e))?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(Error::io(path, e)),
        _ => Ok(()),
    }
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
        staging: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let game = tmp.path().join("game");
        let backup = tmp.path().join("backup");
        let staging = tmp.path().join("staging");
        fs::create_dir_all(game.join("bin/x64")).unwrap();
        fs::write(game.join("bin/x64/game.exe"), b"exe").unwrap();
        Fixture {
            _tmp: tmp,
            game,
            backup,
            staging,
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
        let f = fixture();
        let want = desired(vec![stage(&f, "a", "archive/pc/mod/a.archive", "A")]);
        let protected = vec!["archive/pc/mod".to_string()];

        let (report, state) = deploy(&f.game, &f.backup, &want, &[], &protected).unwrap();
        assert_eq!(report.added, 1);
        assert_eq!(
            fs::read_to_string(f.game.join("archive/pc/mod/a.archive")).unwrap(),
            "A"
        );

        let (report, state) = deploy(&f.game, &f.backup, &want, &state, &protected).unwrap();
        assert_eq!(report.unchanged, 1);

        let (report, state) =
            deploy(&f.game, &f.backup, &BTreeMap::new(), &state, &protected).unwrap();
        assert_eq!(report.removed, 1);
        assert!(state.is_empty());
        assert!(!f.game.join("archive/pc/mod/a.archive").exists());
        assert!(
            f.game.join("archive/pc/mod").is_dir(),
            "protected folder kept"
        );
    }

    #[test]
    fn backs_up_and_restores_game_files() {
        let f = fixture();
        let want = desired(vec![stage(&f, "a", "bin/x64/game.exe", "modded")]);

        let (report, state) = deploy(&f.game, &f.backup, &want, &[], &[]).unwrap();
        assert_eq!(report.backed_up, 1);
        assert_eq!(
            fs::read_to_string(f.game.join("bin/x64/game.exe")).unwrap(),
            "modded"
        );

        let (report, _) = deploy(&f.game, &f.backup, &BTreeMap::new(), &state, &[]).unwrap();
        assert_eq!(report.restored, 1);
        assert_eq!(
            fs::read_to_string(f.game.join("bin/x64/game.exe")).unwrap(),
            "exe"
        );
    }

    #[test]
    fn switching_owner_replaces_file() {
        let f = fixture();
        let a = desired(vec![stage(&f, "a", "r6/tweaks/x.yaml", "A")]);
        let b = desired(vec![stage(&f, "b", "r6/tweaks/x.yaml", "B")]);

        let (_, state) = deploy(&f.game, &f.backup, &a, &[], &[]).unwrap();
        let (report, state) = deploy(&f.game, &f.backup, &b, &state, &[]).unwrap();
        assert_eq!(report.replaced, 1);
        assert_eq!(state[0].mod_id, "b");
        assert_eq!(
            fs::read_to_string(f.game.join("r6/tweaks/x.yaml")).unwrap(),
            "B"
        );
    }

    #[test]
    fn reuses_existing_folder_case() {
        let f = fixture();
        fs::create_dir_all(f.game.join("Archive/PC")).unwrap();
        let want = desired(vec![stage(&f, "a", "archive/pc/mod/a.archive", "A")]);
        deploy(&f.game, &f.backup, &want, &[], &[]).unwrap();
        let top: Vec<String> = fs::read_dir(&f.game)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.eq_ignore_ascii_case("archive"))
            .collect();
        assert_eq!(top, ["Archive"], "no second folder differing only in case");
    }
}
