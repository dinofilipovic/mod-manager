//! Cyberpunk 2077 `.archive` files (RDAR): checking the header and reading
//! the index of resources inside, to find mods changing the same resource.
//!
//! Format from WolvenKit (`WolvenKit.RED4/Archive/IO/ArchiveReader.cs`,
//! `Archive/Base/Header.cs`, `Archive/Base/Index.cs`), checked against the
//! game's own archives: a 40-byte little-endian header (`RDAR` magic,
//! version, index position and size, debug position and size, file size),
//! then at the index position a 28-byte table header and 56-byte file
//! entries, 16-byte segments and 8-byte dependencies.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// `RDAR`, read as a little-endian u32 (WolvenKit's `Header.MAGIC`, 1380009042).
pub const MAGIC: u32 = 0x5241_4452;
const HEADER_SIZE: u64 = 40;
const INDEX_HEADER_SIZE: u64 = 28;
const ENTRY_SIZE: u64 = 56;
const SEGMENT_SIZE: u64 = 16;
const DEPENDENCY_SIZE: u64 = 8;

/// What is wrong with an archive file, in words for the user.
pub type Problem = String;

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// The 64-bit name hashes of the resources in an archive.
pub fn resource_hashes(path: &Path) -> Result<Vec<u64>, Problem> {
    let mut file = File::open(path).map_err(|e| format!("can't be read ({e})"))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let mut header = [0u8; HEADER_SIZE as usize];
    let read = file.read(&mut header).map_err(|e| e.to_string())?;
    if read < 4 || u32_at(&header, 0) != MAGIC {
        return Err("isn't a Cyberpunk archive (it doesn't start with RDAR)".into());
    }
    if (read as u64) < HEADER_SIZE {
        return Err("is too short to be an archive".into());
    }
    let index_pos = u64_at(&header, 8);
    let index_size = u64::from(u32_at(&header, 16));
    let file_size = u64_at(&header, 32);
    if file_size != len {
        return Err(format!(
            "is incomplete or damaged: it should be {file_size} bytes but is {len}"
        ));
    }
    if index_pos
        .checked_add(index_size)
        .is_none_or(|end| end > len)
        || index_size < INDEX_HEADER_SIZE
    {
        return Err("has a damaged index".into());
    }
    file.seek(SeekFrom::Start(index_pos))
        .map_err(|e| e.to_string())?;
    let mut index = vec![0u8; index_size as usize];
    file.read_exact(&mut index).map_err(|e| e.to_string())?;
    let entries = u64::from(u32_at(&index, 16));
    let segments = u64::from(u32_at(&index, 20));
    let dependencies = u64::from(u32_at(&index, 24));
    let expected = INDEX_HEADER_SIZE
        + entries * ENTRY_SIZE
        + segments * SEGMENT_SIZE
        + dependencies * DEPENDENCY_SIZE;
    if expected != index_size {
        return Err("has a damaged index".into());
    }
    Ok((0..entries)
        .map(|i| u64_at(&index, (INDEX_HEADER_SIZE + i * ENTRY_SIZE) as usize))
        .collect())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A valid archive listing these resource hashes (no data segments).
    pub(crate) fn build(hashes: &[u64]) -> Vec<u8> {
        let index_size = INDEX_HEADER_SIZE + hashes.len() as u64 * ENTRY_SIZE;
        let index_pos = HEADER_SIZE + 4;
        let file_size = index_pos + index_size;
        let mut b = Vec::new();
        b.extend(MAGIC.to_le_bytes());
        b.extend(12u32.to_le_bytes());
        b.extend(index_pos.to_le_bytes());
        b.extend((index_size as u32).to_le_bytes());
        b.extend(0u64.to_le_bytes());
        b.extend(0u32.to_le_bytes());
        b.extend(file_size.to_le_bytes());
        b.extend(0u32.to_le_bytes()); // custom data length
        b.extend(0u32.to_le_bytes()); // file table offset
        b.extend(0u32.to_le_bytes()); // file table size
        b.extend(0u64.to_le_bytes()); // crc
        b.extend((hashes.len() as u32).to_le_bytes());
        b.extend(0u32.to_le_bytes()); // segments
        b.extend(0u32.to_le_bytes()); // dependencies
        for h in hashes {
            b.extend(h.to_le_bytes());
            b.extend([0u8; 48]);
        }
        b
    }

    #[test]
    fn reads_resource_hashes_and_rejects_damage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.archive");
        std::fs::write(&path, build(&[7, 42])).unwrap();
        assert_eq!(resource_hashes(&path).unwrap(), [7, 42]);

        let mut cut = build(&[7, 42]);
        cut.truncate(cut.len() - 10);
        std::fs::write(&path, &cut).unwrap();
        assert!(resource_hashes(&path).unwrap_err().contains("incomplete"));
        std::fs::write(&path, b"PK\x03\x04 a zip renamed").unwrap();
        assert!(resource_hashes(&path).unwrap_err().contains("RDAR"));
    }

    /// The game's own archives: `MM_GAME_ARCHIVES=<game>/archive/pc/content`.
    #[test]
    #[ignore = "needs the game's archives"]
    fn reads_the_games_archives() {
        let dir = std::env::var("MM_GAME_ARCHIVES").expect("set MM_GAME_ARCHIVES");
        let mut total = 0;
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            if entry.path().extension().is_some_and(|e| e == "archive") {
                let hashes = resource_hashes(&entry.path()).unwrap();
                assert!(!hashes.is_empty());
                total += hashes.len();
            }
        }
        println!("{total} resources");
        assert!(total > 1000);
    }
}
