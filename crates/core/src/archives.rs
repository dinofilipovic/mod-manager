//! The archive store: every mod archive the library installed from, kept as
//! `archives/<sha256>.<ext>` so a mod can be reinstalled, rolled back or
//! installed with other options without downloading it again. Archives are
//! deduplicated by content; a retention setting decides how many are kept.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use md5::Md5;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use ts_rs::TS;

use crate::error::{Error, Result};
use crate::fs_util::hex;

/// Where an archive came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum ArchiveSource {
    /// Downloaded from Nexus Mods by the app.
    Nexus,
    /// Added by the user from a file on disk.
    Manual,
}

impl ArchiveSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Nexus => "nexus",
            Self::Manual => "manual",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        if s == "nexus" {
            Self::Nexus
        } else {
            Self::Manual
        }
    }
}

/// An archive in the store.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct ArchiveRecord {
    #[ts(type = "number")]
    pub id: i64,
    pub sha256: String,
    pub md5: String,
    #[ts(type = "number")]
    pub size: u64,
    /// Relative to the library folder.
    pub path: String,
    pub original_name: Option<String>,
    pub nexus_mod_id: Option<u32>,
    pub nexus_file_id: Option<u32>,
    pub source: ArchiveSource,
    #[ts(type = "number")]
    pub added_at: i64,
}

/// How many archives to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum ArchiveRetention {
    KeepAll,
    /// The archives of each mod's latest revisions (how many is a setting,
    /// 2 by default: enough to roll back once).
    #[default]
    LastRevisions,
    /// Deleted once installed; reinstalling or rolling back needs a new download.
    DeleteAfterInstall,
}

impl ArchiveRetention {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::KeepAll => "all",
            Self::LastRevisions => "last",
            Self::DeleteAfterInstall => "none",
        }
    }

    pub(crate) fn parse(s: Option<&str>) -> Self {
        match s {
            Some("all") => Self::KeepAll,
            Some("none") => Self::DeleteAfterInstall,
            _ => Self::LastRevisions,
        }
    }
}

/// An archive and the mods installed from it, for the archive list.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ArchiveUsage {
    pub archive: ArchiveRecord,
    /// Names of the mods with a revision installed from this archive.
    pub mods: Vec<String>,
    /// No mod revision uses it: "clean unused archives" deletes it.
    pub unused: bool,
}

/// Disk space the archives of one mod take.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ModArchiveSize {
    pub mod_id: String,
    pub mod_name: String,
    pub archives: u32,
    #[ts(type = "number")]
    pub bytes: u64,
}

/// Content hashes and size of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hashes {
    pub sha256: String,
    pub md5: String,
    pub size: u64,
}

/// Incremental SHA-256 and MD5, for hashing data while it streams past
/// (a download, a copy) instead of reading it again afterwards.
#[derive(Default)]
pub struct Hasher {
    sha256: Sha256,
    md5: Md5,
    size: u64,
}

impl Hasher {
    pub fn update(&mut self, data: &[u8]) {
        self.sha256.update(data);
        self.md5.update(data);
        self.size += data.len() as u64;
    }

    pub fn finish(self) -> Hashes {
        Hashes {
            sha256: hex(&self.sha256.finalize()),
            md5: hex(&self.md5.finalize()),
            size: self.size,
        }
    }
}

/// Hashes `from`, writing the bytes to `to` on the way when given.
pub fn hash_copy(from: &Path, to: Option<&Path>) -> std::io::Result<Hashes> {
    let mut input = fs::File::open(from)?;
    let mut output = to.map(fs::File::create).transpose()?;
    let mut hasher = Hasher::default();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        if let Some(out) = output.as_mut() {
            out.write_all(&buffer[..n])?;
        }
    }
    if let Some(out) = output.as_mut() {
        out.flush()?;
    }
    Ok(hasher.finish())
}

/// The file extension an archive is stored under, from its name or contents.
pub fn archive_extension(path: &Path, original_name: Option<&str>) -> String {
    let from_name = original_name
        .and_then(|n| Path::new(n).extension())
        .or_else(|| path.extension())
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .filter(|e| matches!(e.as_str(), "zip" | "7z" | "rar"));
    from_name.unwrap_or_else(|| match crate::archive::detect_format(path) {
        Ok(crate::archive::ArchiveFormat::SevenZip) => "7z".into(),
        Ok(crate::archive::ArchiveFormat::Rar) => "rar".into(),
        _ => "zip".into(),
    })
}

/// How a file enters the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ingest {
    /// The file is the app's own (a finished download): move it.
    Move,
    /// The file is the user's: clone it (copy-on-write) or copy it.
    Copy,
}

/// Puts `path` into the store folder `dir` as `<sha256>.<ext>`. Returns the
/// hashes and the stored path relative to `library_root`. When `known` hashes
/// are given (computed while downloading), the file isn't read again.
pub fn put(
    library_root: &Path,
    path: &Path,
    original_name: Option<&str>,
    mode: Ingest,
    known: Option<Hashes>,
) -> Result<(Hashes, String)> {
    let dir = library_root.join("archives");
    fs::create_dir_all(&dir).map_err(|e| Error::io(&dir, e))?;
    let ext = archive_extension(path, original_name);
    let tmp = dir.join(format!(".incoming-{}", uuid::Uuid::new_v4()));
    let hashes = match (mode, known) {
        (Ingest::Move, Some(hashes)) => {
            move_or_copy(path, &tmp)?;
            hashes
        }
        (Ingest::Move, None) => {
            let hashes = hash_copy(path, None).map_err(|e| Error::io(path, e))?;
            move_or_copy(path, &tmp)?;
            hashes
        }
        (Ingest::Copy, _) => {
            // A clone costs nothing on APFS/Btrfs; otherwise copy and hash at once.
            if reflink_copy::reflink(path, &tmp).is_ok() {
                hash_copy(&tmp, None)
            } else {
                hash_copy(path, Some(&tmp))
            }
            .map_err(|e| {
                let _ = fs::remove_file(&tmp);
                Error::io(path, e)
            })?
        }
    };
    let relative = format!("archives/{}.{ext}", hashes.sha256);
    let dest = library_root.join(&relative);
    if dest.exists() {
        // Same content already stored.
        let _ = fs::remove_file(&tmp);
    } else {
        fs::rename(&tmp, &dest).map_err(|e| Error::io(&dest, e))?;
    }
    Ok((hashes, relative))
}

fn move_or_copy(from: &Path, to: &Path) -> Result<()> {
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    fs::copy(from, to).map_err(|e| Error::io(to, e))?;
    fs::remove_file(from).map_err(|e| Error::io(from, e))
}

/// Files in the store folder that no record points at (left by a crash).
pub fn orphan_files(library_root: &Path, known: &[String]) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(library_root.join("archives")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter(|e| {
            let relative = format!("archives/{}", e.file_name().to_string_lossy());
            !known.contains(&relative)
        })
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_while_copying_and_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Mod-1-1-0.zip");
        fs::write(&file, b"PK\x03\x04 pretend zip").unwrap();
        let (hashes, path) = put(dir.path(), &file, None, Ingest::Copy, None).unwrap();
        assert_eq!(hashes.size, 16);
        assert_eq!(hashes.md5.len(), 32);
        assert_eq!(path, format!("archives/{}.zip", hashes.sha256));
        assert!(file.exists(), "the user's file stays");
        assert_eq!(
            fs::read(dir.path().join(&path)).unwrap(),
            fs::read(&file).unwrap()
        );

        // The same bytes under another name land on the same stored file.
        let copy = dir.path().join("download.part");
        fs::copy(&file, &copy).unwrap();
        let (again, path2) = put(dir.path(), &copy, Some("Mod.zip"), Ingest::Move, None).unwrap();
        assert_eq!(again, hashes);
        assert_eq!(path2, path);
        assert!(!copy.exists(), "moved");
        assert_eq!(
            fs::read_dir(dir.path().join("archives")).unwrap().count(),
            1
        );
    }

    #[test]
    fn finds_orphaned_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("archives")).unwrap();
        fs::write(dir.path().join("archives/a.zip"), "a").unwrap();
        fs::write(dir.path().join("archives/b.zip"), "b").unwrap();
        let orphans = orphan_files(dir.path(), &["archives/a.zip".into()]);
        assert_eq!(orphans, [dir.path().join("archives/b.zip")]);
    }
}
