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
            // `files` names are directly inside a mod's folder in the layout.
            let in_mod_folder = target[layout.path.len()..]
                .trim_start_matches('/')
                .split_once('/')
                .map(|(_, name)| name)
                .filter(|name| !name.contains('/'));
            let mut ran = BTreeSet::new();
            for check in layout.checks.iter().filter(|c| {
                c.files
                    .iter()
                    .any(|f| in_mod_folder.is_some_and(|n| n.eq_ignore_ascii_case(f)))
                    || c.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext))
            }) {
                if !ran.insert(check.checker.as_str()) {
                    continue;
                }
                if let Err(problem) = run_file_check(&check.checker, &path) {
                    let mut result = error(
                        &check.checker,
                        target,
                        format!("{target} {}", problem.message),
                    );
                    if check.warning_only || problem.harmless {
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
        // Per folder: init.lua directly inside, and the subfolders below it
        // (with whether each has init.lua directly inside).
        let mut folders: BTreeMap<String, (bool, BTreeMap<String, bool>)> = BTreeMap::new();
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
            let parts: Vec<&str> = rest.split('/').collect();
            // Loose files next to the mod folders aren't a mod folder.
            if parts.len() < 2 {
                continue;
            }
            let folder = target[prefix.len()..]
                .split('/')
                .next()
                .unwrap_or_default()
                .to_string();
            let found = folders.entry(folder).or_default();
            match parts.len() {
                2 => found.0 |= parts[1] == "init.lua",
                _ => {
                    *found.1.entry(parts[1].to_string()).or_default() |=
                        parts.len() == 3 && parts[2] == "init.lua"
                }
            }
        }
        for (folder, (direct, subfolders)) in folders {
            let target = format!("{}/{folder}", layout.path);
            if direct {
                continue;
            }
            // Packed one folder too deep: the whole folder is one subfolder
            // that is a mod by itself. An add-on with a Lua package next to
            // other files is not.
            let wrapped = subfolders.len() == 1 && subfolders.values().all(|&entry| entry);
            results.push(if wrapped {
                error(
                    "lua_entry",
                    &target,
                    format!("{target} has no init.lua directly inside, so it isn't loaded; the mod is packed one folder too deep"),
                )
            } else {
                // Add-ons put files into another mod's folder (Appearance
                // Menu Mod's collabs, translations).
                CheckResult {
                    check: "lua_entry".into(),
                    severity: CheckSeverity::Warning,
                    target: Some(target.clone()),
                    message: format!("{target} has no init.lua; fine if it adds to another CET mod, otherwise it isn't loaded"),
                }
            });
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

/// Why a file failed a check.
struct FileProblem {
    /// Completes "<file> ...".
    message: String,
    /// The game tolerates it, so it's only worth a warning.
    harmless: bool,
}

impl From<String> for FileProblem {
    fn from(message: String) -> Self {
        FileProblem {
            message,
            harmless: false,
        }
    }
}

/// Runs a file checker.
fn run_file_check(checker: &str, path: &Path) -> Result<(), FileProblem> {
    let unreadable = |e: std::io::Error| FileProblem::from(format!("can't be read ({e})"));
    match checker {
        "archive_header" => Ok(crate::archive_index::resource_hashes(path).map(|_| ())?),
        "json" => {
            let bytes = std::fs::read(path).map_err(unreadable)?;
            // Many tools write a byte order mark first.
            let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
            serde_json::from_slice::<serde_json::Value>(bytes)
                .map(|_| ())
                .map_err(|e| format!("isn't valid JSON ({e})").into())
        }
        "yaml" => {
            let text = std::fs::read_to_string(path).map_err(unreadable)?;
            yaml_rust2::YamlLoader::load_from_str(text.trim_start_matches('\u{feff}'))
                .map(|_| ())
                .map_err(|e| {
                    let message = e.to_string();
                    FileProblem {
                        // yaml-cpp, which TweakXL and ArchiveXL use, keeps
                        // one of the values.
                        harmless: message.contains("duplicated key"),
                        message: format!("isn't valid YAML ({message})"),
                    }
                })
        }
        "pe64_dll" => {
            use std::io::{Read, Seek, SeekFrom};
            let mut file = std::fs::File::open(path).map_err(unreadable)?;
            let mut head = Vec::new();
            (&mut file)
                .take(0x40)
                .read_to_end(&mut head)
                .map_err(unreadable)?;
            // The PE header can start anywhere the DOS header points: read
            // its 24 bytes from there and put them right after the DOS
            // header, so only 88 bytes are ever held.
            let len = file.metadata().map_err(unreadable)?.len();
            if head.len() == 0x40 && &head[..2] == b"MZ" {
                let pe = u64::from(u32::from_le_bytes(head[0x3C..0x40].try_into().unwrap()));
                if pe >= 0x40 && pe + 24 <= len {
                    file.seek(SeekFrom::Start(pe)).map_err(unreadable)?;
                    file.take(24).read_to_end(&mut head).map_err(unreadable)?;
                    head[0x3C..0x40].copy_from_slice(&0x40u32.to_le_bytes());
                }
            }
            Ok(check_pe64_dll(&head)?)
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
    fn what_the_game_tolerates_is_only_a_warning() {
        let game = find_builtin("cyberpunk2077").unwrap();
        let dir = tempfile::tempdir().unwrap();
        // A PE header past the first 4 KiB is still a DLL.
        let mut far = vec![0u8; 0x2100];
        far[..2].copy_from_slice(b"MZ");
        far[0x3C..0x40].copy_from_slice(&0x2000u32.to_le_bytes());
        far[0x2000..0x2004].copy_from_slice(b"PE\0\0");
        far[0x2004..0x2006].copy_from_slice(&0x8664u16.to_le_bytes());
        far[0x2016..0x2018].copy_from_slice(&0x2022u16.to_le_bytes());
        let files = write(
            dir.path(),
            &[
                // An add-on for another CET mod, and a loose readme.
                (
                    "bin/x64/plugins/cyber_engine_tweaks/mods/AppearanceMenuMod/Collabs/Props/props.lua",
                    b"return {}",
                ),
                ("bin/x64/plugins/cyber_engine_tweaks/mods/readme.txt", b"hi"),
                // An add-on shipping a Lua package.
                (
                    "bin/x64/plugins/cyber_engine_tweaks/mods/Host/addon/lib/init.lua",
                    b"return {}",
                ),
                // The game only reads info.json directly in the mod's folder.
                ("mods/Redmod/extra/info.json", b"{"),
                (
                    "r6/tweaks/Dup/dup.yaml",
                    b"Items.A:\n  price: 1\n  price: 2\n",
                ),
                ("mods/Redmod/info.json", b"{\"name\":\"Redmod\"}"),
                ("mods/Redmod/custom_sounds/config.json", b""),
                ("red4ext/plugins/Far/Far.dll", far.as_slice()),
            ],
        );
        let results = check_mod(&game, dir.path(), &files, 60);
        let mut found = checks(&results);
        found.sort_by_key(|(c, _)| *c);
        assert_eq!(
            found,
            [
                ("json", CheckSeverity::Warning),
                ("json", CheckSeverity::Warning),
                ("lua_entry", CheckSeverity::Warning),
                ("lua_entry", CheckSeverity::Warning),
                ("yaml", CheckSeverity::Warning),
            ],
            "{results:#?}"
        );
        assert!(!wont_load(&results));

        // A PE offset pointing far past the end is just not a DLL (and
        // nothing that large is read or allocated).
        let mut bogus = vec![0u8; 0x40];
        bogus[..2].copy_from_slice(b"MZ");
        bogus[0x3C..0x40].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        let files = write(dir.path(), &[("red4ext/plugins/Bogus/Bogus.dll", &bogus)]);
        let results = check_mod(&game, dir.path(), &files, 60);
        assert_eq!(checks(&results), [("pe64_dll", CheckSeverity::Error)]);

        // But a REDmod's info.json must parse.
        let files = write(dir.path(), &[("mods/Broken/info.json", b"{")]);
        assert_eq!(
            checks(&check_mod(&game, dir.path(), &files, 60)),
            [("json", CheckSeverity::Error)]
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
