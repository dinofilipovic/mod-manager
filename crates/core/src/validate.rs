//! Static checks of a mod's files at install: things that keep a mod from
//! loading, found before the game starts.
//!
//! Which checks run where comes from the game definition (`checks` on its
//! layout targets); these are the built-in checkers they can name. Checks
//! every game gets: files outside every known mod folder, paths that differ
//! only in case, paths too long for Windows and names Windows can't have.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::game::GameDefinition;

/// The built-in checkers a game definition's layout targets can name.
pub const CHECKERS: &[&str] = &["archive_header", "json", "yaml", "lua_entry", "pe64_dll"];

/// Windows' traditional path limit, which games and Wine still hit.
const MAX_PATH: usize = 260;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum CheckSeverity {
    /// The file (and so the mod) won't load.
    Error,
    /// Possibly a problem.
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct CheckResult {
    /// The checker, or `outside_layouts`, `case`, `path_length`, `windows_name`.
    pub check: String,
    pub severity: CheckSeverity,
    /// The file (or folder) it is about, game-relative.
    pub target: Option<String>,
    pub message: String,
}

/// Whether any result means the mod won't load.
pub fn wont_load(results: &[CheckResult]) -> bool {
    results.iter().any(|r| r.severity == CheckSeverity::Error)
}

/// Checks a mod's files: `files` are game-relative targets, found under
/// `root` (the mod's folder). `game_root_len` is the length of the game
/// folder's Windows path, for the path length check.
pub fn check_mod(
    game: &GameDefinition,
    root: &Path,
    files: &[String],
    game_root_len: usize,
) -> Vec<CheckResult> {
    let mut results = Vec::new();
    let error = |check: &str, target: &str, message: String| CheckResult {
        check: check.into(),
        severity: CheckSeverity::Error,
        target: Some(target.into()),
        message,
    };

    for target in files {
        let path = join(root, target);
        if let Some(layout) = game.layout_for(target) {
            let ext = Path::new(target)
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            for check in layout
                .checks
                .iter()
                .filter(|c| c.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)))
            {
                if let Err(problem) = run_file_check(&check.checker, &path) {
                    let mut result = error(&check.checker, target, format!("{target} {problem}"));
                    if check.warning_only {
                        result.severity = CheckSeverity::Warning;
                    }
                    results.push(result);
                }
            }
        }
        for component in target.split('/') {
            if let Some(problem) = windows_name_problem(component) {
                results.push(error(
                    "windows_name",
                    target,
                    format!("{target}: \"{component}\" {problem}; rename it or ask the author"),
                ));
                break;
            }
        }
        let length = game_root_len + 1 + target.len();
        if length >= MAX_PATH {
            results.push(CheckResult {
                check: "path_length".into(),
                severity: CheckSeverity::Warning,
                target: Some(target.clone()),
                message: format!(
                    "{target} is {length} characters long with the game folder; Windows may not open paths of {MAX_PATH} or more"
                ),
            });
        }
    }

    // Folder mods without their entry point (CET's init.lua).
    for layout in &game.layout {
        if !layout.checks.iter().any(|c| c.checker == "lua_entry") {
            continue;
        }
        let prefix = format!("{}/", layout.path.to_ascii_lowercase());
        let mut folders: BTreeMap<String, bool> = BTreeMap::new();
        for target in files {
            // Only files whose most specific layout is this one.
            if game.layout_for(target).map(|l| &l.path) != Some(&layout.path) {
                continue;
            }
            let Some(rest) = target
                .to_ascii_lowercase()
                .strip_prefix(&prefix)
                .map(String::from)
            else {
                continue;
            };
            let folder = target[prefix.len()..]
                .split('/')
                .next()
                .unwrap_or_default()
                .to_string();
            let entry = rest.split('/').nth(1) == Some("init.lua") && rest.split('/').count() == 2;
            *folders.entry(folder).or_default() |= entry;
        }
        for (folder, _) in folders.into_iter().filter(|(_, ok)| !ok) {
            let target = format!("{}/{folder}", layout.path);
            results.push(error(
                "lua_entry",
                &target,
                format!("{target} has no init.lua directly inside, so it isn't loaded; the mod may be packed one folder too deep"),
            ));
        }
    }

    // Paths that differ only in case become two folders on case-sensitive
    // disks, and fight each other on Windows.
    let mut seen: BTreeMap<String, String> = BTreeMap::new();
    let mut reported = BTreeSet::new();
    for target in files {
        let mut prefix = String::new();
        for part in target.split('/') {
            prefix = if prefix.is_empty() {
                part.to_string()
            } else {
                format!("{prefix}/{part}")
            };
            let key = prefix.to_ascii_lowercase();
            match seen.get(&key) {
                // The first folder where they part is enough.
                Some(other) if *other != prefix => {
                    if reported.insert(key.clone()) {
                        results.push(CheckResult {
                            check: "case".into(),
                            severity: CheckSeverity::Warning,
                            target: Some(prefix.clone()),
                            message: format!(
                                "{prefix} and {other} differ only in upper and lower case"
                            ),
                        });
                    }
                    break;
                }
                Some(_) => {}
                None => {
                    seen.insert(key, prefix.clone());
                }
            }
        }
    }

    if !files.is_empty() && files.iter().all(|f| game.layout_for(f).is_none()) {
        results.push(CheckResult {
            check: "outside_layouts".into(),
            severity: CheckSeverity::Warning,
            target: None,
            message: "None of its files are in a folder the game loads mods from. Check the mod's page for how it installs.".into(),
        });
    }
    results
}

fn join(root: &Path, target: &str) -> std::path::PathBuf {
    target
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

/// Runs a file checker; the error completes "<file> ...".
fn run_file_check(checker: &str, path: &Path) -> Result<(), String> {
    match checker {
        "archive_header" => crate::archive_index::resource_hashes(path).map(|_| ()),
        "json" => {
            let bytes = std::fs::read(path).map_err(|e| format!("can't be read ({e})"))?;
            // Many tools write a byte order mark first.
            let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
            serde_json::from_slice::<serde_json::Value>(bytes)
                .map(|_| ())
                .map_err(|e| format!("isn't valid JSON ({e})"))
        }
        "yaml" => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("can't be read ({e})"))?;
            yaml_rust2::YamlLoader::load_from_str(text.trim_start_matches('\u{feff}'))
                .map(|_| ())
                .map_err(|e| format!("isn't valid YAML ({e})"))
        }
        "pe64_dll" => {
            let mut head = vec![0u8; 4096];
            let read = std::io::Read::read(
                &mut std::fs::File::open(path).map_err(|e| format!("can't be read ({e})"))?,
                &mut head,
            )
            .map_err(|e| format!("can't be read ({e})"))?;
            check_pe64_dll(&head[..read])
        }
        // Folder checks run separately.
        _ => Ok(()),
    }
}

/// A 64-bit Windows DLL: `MZ`, a PE header, machine x64 (0x8664) and the
/// DLL flag (0x2000) in the file header's characteristics.
pub fn check_pe64_dll(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() < 0x40 || &bytes[..2] != b"MZ" {
        return Err("isn't a Windows DLL".into());
    }
    let pe = u32::from_le_bytes(bytes[0x3C..0x40].try_into().unwrap()) as usize;
    if bytes.len() < pe + 24 || &bytes[pe..pe + 4] != b"PE\0\0" {
        return Err("isn't a Windows DLL".into());
    }
    let machine = u16::from_le_bytes([bytes[pe + 4], bytes[pe + 5]]);
    let characteristics = u16::from_le_bytes([bytes[pe + 22], bytes[pe + 23]]);
    if machine != 0x8664 {
        return Err("is a 32-bit DLL; the game only loads 64-bit ones".into());
    }
    if characteristics & 0x2000 == 0 {
        return Err("is a program, not a DLL".into());
    }
    Ok(())
}

/// Why Windows can't have this path component, if it can't.
pub fn windows_name_problem(component: &str) -> Option<String> {
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if let Some(c) = component
        .chars()
        .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\') || (*c as u32) < 32)
    {
        return Some(format!(
            "contains a character Windows doesn't allow ({c:?})"
        ));
    }
    if component.ends_with(['.', ' ']) {
        return Some("ends with a dot or space, which Windows drops".into());
    }
    let stem = component.split('.').next().unwrap_or(component);
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
        return Some(format!("is the reserved Windows name {stem}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::find_builtin;

    fn pe(machine: u16, characteristics: u16) -> Vec<u8> {
        let mut b = vec![0u8; 512];
        b[..2].copy_from_slice(b"MZ");
        b[0x3C..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        b[0x80..0x84].copy_from_slice(b"PE\0\0");
        b[0x84..0x86].copy_from_slice(&machine.to_le_bytes());
        b[0x96..0x98].copy_from_slice(&characteristics.to_le_bytes());
        b
    }

    fn write(root: &Path, files: &[(&str, &[u8])]) -> Vec<String> {
        for (target, data) in files {
            let path = join(root, target);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
        }
        files.iter().map(|(t, _)| t.to_string()).collect()
    }

    fn checks(results: &[CheckResult]) -> Vec<(&str, CheckSeverity)> {
        results
            .iter()
            .map(|r| (r.check.as_str(), r.severity))
            .collect()
    }

    #[test]
    fn good_files_pass() {
        let game = find_builtin("cyberpunk2077").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let archive = crate::archive_index::tests::build(&[1, 2]);
        let dll = pe(0x8664, 0x2022);
        let files = write(
            dir.path(),
            &[
                ("archive/pc/mod/good.archive", archive.as_slice()),
                (
                    "archive/pc/mod/good.archive.xl",
                    b"resource:\n  patch: {}\n",
                ),
                ("r6/tweaks/Good/good.yaml", b"Items.Good:\n  price: 5\n"),
                (
                    "bin/x64/plugins/cyber_engine_tweaks/mods/Good/init.lua",
                    b"return {}",
                ),
                (
                    "bin/x64/plugins/cyber_engine_tweaks/mods/Good/data/config.json",
                    b"\xEF\xBB\xBF{\"a\":1}",
                ),
                ("red4ext/plugins/Good/Good.dll", dll.as_slice()),
                ("mods/GoodRedmod/info.json", b"{\"name\":\"GoodRedmod\"}"),
            ],
        );
        let results = check_mod(&game, dir.path(), &files, 60);
        assert!(results.is_empty(), "{results:#?}");
    }

    #[test]
    fn broken_files_are_caught() {
        let game = find_builtin("cyberpunk2077").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let mut truncated = crate::archive_index::tests::build(&[1]);
        truncated.truncate(30);
        let long = format!("archive/pc/mod/{}.archive", "x".repeat(200));
        let files = write(
            dir.path(),
            &[
                ("archive/pc/mod/broken.archive", truncated.as_slice()),
                ("archive/pc/mod/broken.xl", b"resource: [unclosed\n"),
                ("r6/tweaks/Bad/bad.yaml", b"key: \"unterminated\n"),
                (
                    "bin/x64/plugins/cyber_engine_tweaks/mods/Deep/Deep/init.lua",
                    b"return {}",
                ),
                (
                    "bin/x64/plugins/cyber_engine_tweaks/mods/Ok/init.lua",
                    b"return {}",
                ),
                (
                    "bin/x64/plugins/cyber_engine_tweaks/mods/Ok/settings.json",
                    b"{nope",
                ),
                ("red4ext/plugins/Old/Old.dll", pe(0x14c, 0x2102).as_slice()),
                ("red4ext/plugins/Exe/Exe.dll", pe(0x8664, 0x0022).as_slice()),
                ("red4ext/plugins/Fake/Fake.dll", b"not a dll"),
                ("r6/scripts/What?/main.reds", b"x"),
                ("r6/scripts/Aux/main.reds", b"x"),
                ("r6/scripts/Case/a.reds", b"x"),
                ("r6/Scripts/Case/b.reds", b"x"),
                (
                    long.as_str(),
                    crate::archive_index::tests::build(&[]).as_slice(),
                ),
            ],
        );
        let results = check_mod(&game, dir.path(), &files, 60);
        let found = checks(&results);
        let count = |c: &str| found.iter().filter(|(k, _)| *k == c).count();
        assert_eq!(count("archive_header"), 1);
        assert_eq!(count("yaml"), 2, "the .xl and the tweak");
        assert_eq!(count("json"), 1);
        assert_eq!(
            results.iter().find(|r| r.check == "json").unwrap().severity,
            CheckSeverity::Warning,
            "a CET mod's settings file doesn't stop it loading"
        );
        assert_eq!(count("pe64_dll"), 3, "32-bit, not a DLL, not a PE file");
        assert_eq!(
            count("lua_entry"),
            1,
            "Deep has init.lua one folder too deep"
        );
        assert_eq!(count("windows_name"), 2, "? and AUX");
        assert_eq!(count("case"), 1, "r6/scripts vs r6/Scripts");
        assert_eq!(count("path_length"), 1);
        assert!(wont_load(&results));
        let deep = results.iter().find(|r| r.check == "lua_entry").unwrap();
        assert_eq!(
            deep.target.as_deref(),
            Some("bin/x64/plugins/cyber_engine_tweaks/mods/Deep")
        );
        assert!(
            results
                .iter()
                .find(|r| r.check == "pe64_dll" && r.message.contains("32-bit"))
                .is_some()
        );
    }

    #[test]
    fn files_outside_every_mod_folder_are_flagged() {
        let game = find_builtin("cyberpunk2077").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let files = write(
            dir.path(),
            &[("engine/config/platform/pc/tweak.ini", b"[x]")],
        );
        let results = check_mod(&game, dir.path(), &files, 60);
        assert_eq!(
            checks(&results),
            [("outside_layouts", CheckSeverity::Warning)]
        );
        assert!(!wont_load(&results));
    }
}
