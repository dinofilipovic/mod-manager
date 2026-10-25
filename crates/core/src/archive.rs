//! Extracts mod archives (zip, 7z, rar). The format is detected from the
//! file's first bytes, not its extension, since Nexus file names are not
//! always accurate.

use std::fs::File;
use std::io::Read;
use std::path::{Component, Path};

use crate::error::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    SevenZip,
    Rar,
}

pub fn detect_format(path: &Path) -> Result<ArchiveFormat> {
    let mut magic = [0u8; 8];
    let read = File::open(path)
        .and_then(|mut f| f.read(&mut magic))
        .map_err(|e| Error::io(path, e))?;
    let magic = &magic[..read];
    if magic.starts_with(b"PK\x03\x04") || magic.starts_with(b"PK\x05\x06") {
        Ok(ArchiveFormat::Zip)
    } else if magic.starts_with(b"7z\xBC\xAF\x27\x1C") {
        Ok(ArchiveFormat::SevenZip)
    } else if magic.starts_with(b"Rar!\x1A\x07") {
        Ok(ArchiveFormat::Rar)
    } else {
        Err(Error::Archive(format!(
            "{} is not a zip, 7z or rar archive",
            path.display()
        )))
    }
}

/// Extracts `archive` into the empty directory `dest`. Entries that would
/// land outside `dest` are rejected.
pub fn extract(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).map_err(|e| Error::io(dest, e))?;
    let fail = |e: &dyn std::fmt::Display| {
        Error::Archive(format!("failed to extract {}: {e}", archive.display()))
    };
    match detect_format(archive)? {
        ArchiveFormat::Zip => {
            let file = File::open(archive).map_err(|e| Error::io(archive, e))?;
            // `extract` skips entries whose names escape the destination.
            zip::ZipArchive::new(file)
                .and_then(|mut zip| zip.extract(dest))
                .map_err(|e| fail(&e))
        }
        ArchiveFormat::SevenZip => {
            // The default extract function rejects `..` components.
            sevenz_rust2::decompress_file(archive, dest).map_err(|e| fail(&e))
        }
        ArchiveFormat::Rar => extract_rar(archive, dest).map_err(|e| fail(&e)),
    }
}

fn extract_rar(archive: &Path, dest: &Path) -> std::result::Result<(), String> {
    let mut open = unrar::Archive::new(archive)
        .open_for_processing()
        .map_err(|e| e.to_string())?;
    while let Some(header) = open.read_header().map_err(|e| e.to_string())? {
        let entry = header.entry();
        if !is_safe_relative(&entry.filename) {
            return Err(format!("unsafe entry path {}", entry.filename.display()));
        }
        open = if entry.is_file() {
            header.extract_with_base(dest)
        } else {
            header.skip()
        }
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn is_safe_relative(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    pub(crate) fn write_zip(path: &Path, files: &[(&str, &[u8])]) {
        let mut zip = zip::ZipWriter::new(File::create(path).unwrap());
        for (name, data) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn extracts_zip() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("mod.zip");
        write_zip(&archive, &[("archive/pc/mod/a.archive", b"x")]);
        let out = dir.path().join("out");
        extract(&archive, &out).unwrap();
        assert!(out.join("archive/pc/mod/a.archive").is_file());
    }

    #[test]
    fn rejects_unknown_format() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mod.zip");
        std::fs::write(&file, b"not an archive").unwrap();
        assert!(matches!(detect_format(&file), Err(Error::Archive(_))));
    }
}
