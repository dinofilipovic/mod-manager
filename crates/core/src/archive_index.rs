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

/// Two mods' archives changing the same game resources.
#[derive(Debug, Clone, PartialEq, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ResourceConflict {
    pub winner_mod_id: String,
    pub winner_mod: String,
    pub winner_archive: String,
    pub loser_mod_id: String,
    pub loser_mod: String,
    pub loser_archive: String,
    /// Resources both change; the winner's version is used.
    pub resources: u32,
}

/// An archive of an enabled mod, in load order (later mods win in the list).
pub struct ModArchive<'a> {
    pub mod_id: &'a str,
    pub mod_name: &'a str,
    pub list_position: usize,
    /// The file name in the game's mod folder.
    pub name: &'a str,
    pub hashes: &'a [u64],
}

/// Which archives change the same resources, and which one the game uses:
/// by file name the first one loaded (alphabetical) wins; with the list
/// order the game loads the lowest mod first, so it wins.
pub fn conflicts(archives: &[ModArchive], by_list: bool) -> Vec<ResourceConflict> {
    let mut owners: std::collections::HashMap<u64, Vec<usize>> = std::collections::HashMap::new();
    for (i, a) in archives.iter().enumerate() {
        for h in a.hashes {
            owners.entry(*h).or_default().push(i);
        }
    }
    let wins = |a: &ModArchive, b: &ModArchive| {
        if by_list {
            a.list_position > b.list_position
        } else {
            a.name.to_ascii_lowercase() < b.name.to_ascii_lowercase()
        }
    };
    let mut pairs: std::collections::BTreeMap<(usize, usize), u32> =
        std::collections::BTreeMap::new();
    for list in owners.values().filter(|l| l.len() > 1) {
        let winner = list
            .iter()
            .copied()
            .reduce(|best, i| {
                if wins(&archives[i], &archives[best]) {
                    i
                } else {
                    best
                }
            })
            .unwrap_or_default();
        for &loser in list.iter().filter(|&&i| i != winner) {
            if archives[loser].mod_id != archives[winner].mod_id {
                *pairs.entry((winner, loser)).or_default() += 1;
            }
        }
    }
    let mut out: Vec<ResourceConflict> = pairs
        .into_iter()
        .map(|((w, l), resources)| ResourceConflict {
            winner_mod_id: archives[w].mod_id.to_string(),
            winner_mod: archives[w].mod_name.to_string(),
            winner_archive: archives[w].name.to_string(),
            loser_mod_id: archives[l].mod_id.to_string(),
            loser_mod: archives[l].mod_name.to_string(),
            loser_archive: archives[l].name.to_string(),
            resources,
        })
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.resources));
    out
}

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
    let mut index_header = [0u8; INDEX_HEADER_SIZE as usize];
    file.read_exact(&mut index_header)
        .map_err(|e| e.to_string())?;
    let entries = u64::from(u32_at(&index_header, 16));
    let segments = u64::from(u32_at(&index_header, 20));
    let dependencies = u64::from(u32_at(&index_header, 24));
    let expected = INDEX_HEADER_SIZE
        + entries * ENTRY_SIZE
        + segments * SEGMENT_SIZE
        + dependencies * DEPENDENCY_SIZE;
    if expected != index_size {
        return Err("has a damaged index".into());
    }
    // Entry by entry, so a large index isn't read into memory at once.
    let mut reader = std::io::BufReader::new(file);
    let mut entry = [0u8; ENTRY_SIZE as usize];
    let mut hashes = Vec::with_capacity(entries.min(1 << 20) as usize);
    for _ in 0..entries {
        reader.read_exact(&mut entry).map_err(|e| e.to_string())?;
        hashes.push(u64_at(&entry, 0));
    }
    Ok(hashes)
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

    #[test]
    fn finds_resource_conflicts_and_their_winner() {
        let a = [1u64, 2, 3];
        let b = [3u64, 4];
        let c = [2u64, 3];
        let archives = [
            ModArchive {
                mod_id: "alpha",
                mod_name: "Alpha",
                list_position: 0,
                name: "zz_alpha.archive",
                hashes: &a,
            },
            ModArchive {
                mod_id: "beta",
                mod_name: "Beta",
                list_position: 1,
                name: "aa_beta.archive",
                hashes: &b,
            },
            ModArchive {
                mod_id: "gamma",
                mod_name: "Gamma",
                list_position: 2,
                name: "mm_gamma.archive",
                hashes: &c,
            },
        ];
        // By file name: aa_beta loads first and wins resource 3; mm_gamma beats zz_alpha on 2.
        let by_name = conflicts(&archives, false);
        let summary: Vec<(&str, &str, u32)> = by_name
            .iter()
            .map(|c| (c.winner_mod.as_str(), c.loser_mod.as_str(), c.resources))
            .collect();
        assert_eq!(summary.len(), 3);
        assert!(summary.contains(&("Beta", "Alpha", 1)));
        assert!(summary.contains(&("Beta", "Gamma", 1)));
        assert!(summary.contains(&("Gamma", "Alpha", 1)));
        // By list: the lowest mod (Gamma) wins both its resources.
        let by_list = conflicts(&archives, true);
        assert!(by_list.iter().all(|c| c.winner_mod != "Alpha"));
        assert!(
            by_list
                .iter()
                .any(|c| c.winner_mod == "Gamma" && c.loser_mod == "Alpha" && c.resources == 2)
        );
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
