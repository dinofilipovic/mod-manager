//! Decides where each file of an extracted mod goes in the game folder.
//!
//! Mod archives come in many shapes: laid out from the game root
//! (`archive/pc/mod/x.archive`), wrapped in extra folders
//! (`My Mod v1.2/archive/...`), or as loose files (`x.archive`, a CET mod
//! folder with `init.lua`). The rules come from the game definition, so this
//! code knows nothing about any particular game.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;
use ts_rs::TS;
use walkdir::WalkDir;

use crate::error::{Error, Result};
use crate::fs_util::resolve_case_insensitive;
use crate::game::GameDefinition;

/// Folders that installer tools put in archives but that are never game files.
const IGNORED_DIRS: &[&str] = &["fomod", "__macosx"];

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct InstallPlan {
    pub files: Vec<PlannedFile>,
    /// Files left out because they don't belong in the game (readmes, images).
    pub skipped: Vec<String>,
    /// Layout kinds the mod touches, e.g. `archive`, `cet`.
    pub kinds: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct PlannedFile {
    /// Path inside the extracted archive, `/`-separated.
    pub source: String,
    /// Path inside the game folder, `/`-separated.
    pub target: String,
}

/// Plans the install of the extracted mod at `root`. `mod_name` names the
/// mod's own folder where the game layout needs one.
pub fn plan_install(game: &GameDefinition, root: &Path, mod_name: &str) -> Result<InstallPlan> {
    let mut warnings = Vec::new();
    let mut all_files = list_files(root)?;
    if all_files
        .iter()
        .any(|f| first_component(f).eq_ignore_ascii_case("fomod"))
    {
        warnings.push(
            "This mod has a FOMOD installer with options; its default files were installed. \
             Check the mod page if it needs a specific option."
                .into(),
        );
    }
    all_files.retain(|f| {
        !f.split('/')
            .any(|c| IGNORED_DIRS.iter().any(|d| c.eq_ignore_ascii_case(d)))
            && !f.ends_with(".DS_Store")
    });
    if all_files.is_empty() {
        return Err(Error::Install("the archive contains no files".into()));
    }

    let prefix = content_prefix(game, root, &all_files);
    let files: Vec<&str> = all_files
        .iter()
        .filter_map(|f| f.strip_prefix(prefix.as_str()))
        .collect();
    let folder_name = sanitize_folder_name(mod_name);

    let mut planned: Vec<(String, String)> = Vec::new();
    let mut skipped = Vec::new();

    let has_root_layout = files
        .iter()
        .any(|f| f.contains('/') && is_root_marker(game, first_component(f)));
    if has_root_layout {
        for file in &files {
            if file.contains('/') && is_root_marker(game, first_component(file)) {
                planned.push((format!("{prefix}{file}"), (*file).to_string()));
            } else {
                skipped.push((*file).to_string());
            }
        }
    } else {
        let mut claimed = BTreeSet::new();
        // Folders marked by a file such as init.lua (CET) or info.json
        // (REDmod), or by a subfolder such as The Witcher 3's `content`.
        for target in game
            .layout
            .iter()
            .filter(|t| t.marker_file.is_some() || t.marker_folder.is_some())
        {
            let dirs = match (&target.marker_file, &target.marker_folder) {
                (Some(file), _) => marker_dirs(&files, file),
                (None, Some(folder)) => marker_folder_dirs(&files, folder),
                (None, None) => Vec::new(),
            };
            for dir in dirs {
                let name = if dir.is_empty() {
                    folder_name.clone()
                } else {
                    dir.rsplit('/').next().unwrap_or(&dir).to_string()
                };
                let dir_prefix = if dir.is_empty() {
                    String::new()
                } else {
                    format!("{dir}/")
                };
                for file in files.iter().filter(|f| f.starts_with(&dir_prefix)) {
                    if claimed.insert(*file) {
                        let rest = &file[dir_prefix.len()..];
                        planned.push((
                            format!("{prefix}{file}"),
                            format!("{}/{name}/{rest}", target.path),
                        ));
                    }
                }
            }
        }
        // Remaining files by extension.
        for file in files.iter().filter(|f| !claimed.contains(*f)) {
            let ext = Path::new(file)
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            let target = game
                .layout
                .iter()
                .find(|t| t.extensions.iter().any(|e| e.eq_ignore_ascii_case(&ext)));
            match target {
                Some(t) if t.flatten => {
                    let name = file.rsplit('/').next().unwrap_or(file);
                    planned.push((format!("{prefix}{file}"), format!("{}/{name}", t.path)));
                }
                Some(t) if t.per_mod_folder => {
                    planned.push((
                        format!("{prefix}{file}"),
                        format!("{}/{folder_name}/{file}", t.path),
                    ));
                }
                Some(t) => planned.push((format!("{prefix}{file}"), format!("{}/{file}", t.path))),
                None => skipped.push((*file).to_string()),
            }
        }
    }

    if planned.is_empty() {
        return Err(Error::Install(format!(
            "couldn't tell where this mod's files go. It contains: {}",
            files.iter().take(8).copied().collect::<Vec<_>>().join(", ")
        )));
    }

    // Normalize folder casing and drop duplicate targets (last one wins).
    let mut by_target: BTreeMap<String, PlannedFile> = BTreeMap::new();
    for (source, target) in planned {
        let target = normalize_target(game, &target);
        if let Some(previous) = by_target.insert(
            target.to_ascii_lowercase(),
            PlannedFile {
                source,
                target: target.clone(),
            },
        ) {
            warnings.push(format!(
                "{} appears twice in the archive; using {}",
                target, previous.source
            ));
        }
    }
    let files: Vec<PlannedFile> = by_target.into_values().collect();
    let kinds: BTreeSet<String> = files
        .iter()
        .filter_map(|f| game.layout_for(&f.target).map(|t| t.kind.clone()))
        .collect();

    Ok(InstallPlan {
        files,
        skipped,
        kinds: kinds.into_iter().collect(),
        warnings,
    })
}

/// All files under `root`, relative and `/`-separated, sorted.
fn list_files(root: &Path) -> Result<Vec<String>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry =
            entry.map_err(|e| Error::Install(format!("failed to read extracted files: {e}")))?;
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(root).unwrap_or(entry.path());
            let parts: Vec<String> = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            files.push(parts.join("/"));
        }
    }
    files.sort();
    Ok(files)
}

/// The `a/b/` prefix of wrapper folders that hold the real content. Stops at
/// a game root folder or a folder that is itself a mod (contains a marker file).
fn content_prefix(game: &GameDefinition, root: &Path, files: &[String]) -> String {
    let markers: Vec<&str> = game
        .layout
        .iter()
        .filter_map(|t| t.marker_file.as_deref())
        .collect();
    let marker_folders: Vec<&str> = game
        .layout
        .iter()
        .filter_map(|t| t.marker_folder.as_deref())
        .collect();
    let mut prefix = String::new();
    loop {
        let rest: Vec<&str> = files
            .iter()
            .filter_map(|f| f.strip_prefix(prefix.as_str()))
            .collect();
        let firsts: BTreeSet<&str> = rest.iter().map(|f| first_component(f)).collect();
        let [only] = firsts.into_iter().collect::<Vec<_>>()[..] else {
            break;
        };
        let all_nested = rest.iter().all(|f| f.contains('/'));
        let is_mod_dir = markers
            .iter()
            .any(|m| root.join(&prefix).join(only).join(m).is_file())
            || marker_folders.iter().any(|m| {
                resolve_case_insensitive(&root.join(&prefix).join(only), m)
                    .is_some_and(|p| p.is_dir())
            });
        if !all_nested || is_root_marker(game, only) || is_mod_dir {
            break;
        }
        prefix = format!("{prefix}{only}/");
    }
    prefix
}

/// Folders (relative, `""` for the top) that directly contain `marker`,
/// keeping only the outermost ones.
fn marker_dirs(files: &[&str], marker: &str) -> Vec<String> {
    let mut dirs: Vec<String> = files
        .iter()
        .filter(|f| {
            f.rsplit('/')
                .next()
                .is_some_and(|n| n.eq_ignore_ascii_case(marker))
        })
        .map(|f| {
            f.rsplit_once('/')
                .map_or(String::new(), |(dir, _)| dir.to_string())
        })
        .collect();
    dirs.sort_by_key(|d| d.len());
    let mut outermost: Vec<String> = Vec::new();
    for dir in dirs {
        let nested = outermost
            .iter()
            .any(|o| o.is_empty() || dir.starts_with(&format!("{o}/")));
        if !nested {
            outermost.push(dir);
        }
    }
    outermost
}

/// Folders (relative, `""` for the top) that directly contain a subfolder
/// named `marker`, keeping only the outermost ones.
fn marker_folder_dirs(files: &[&str], marker: &str) -> Vec<String> {
    let mut dirs: Vec<String> = files
        .iter()
        .filter_map(|f| {
            let parts: Vec<&str> = f.split('/').collect();
            let at = parts[..parts.len() - 1]
                .iter()
                .position(|p| p.eq_ignore_ascii_case(marker))?;
            Some(parts[..at].join("/"))
        })
        .collect();
    dirs.sort_by_key(|d| d.len());
    dirs.dedup();
    let mut outermost: Vec<String> = Vec::new();
    for dir in dirs {
        if !outermost
            .iter()
            .any(|o| o.is_empty() || dir.starts_with(&format!("{o}/")))
        {
            outermost.push(dir);
        }
    }
    outermost
}

fn first_component(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

fn is_root_marker(game: &GameDefinition, name: &str) -> bool {
    game.root_markers
        .iter()
        .any(|m| m.eq_ignore_ascii_case(name))
}

/// Lowercases folder names the game expects in lowercase; leaves the rest.
pub fn normalize_target(game: &GameDefinition, target: &str) -> String {
    let parts: Vec<&str> = target.split('/').filter(|p| !p.is_empty()).collect();
    let last = parts.len().saturating_sub(1);
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            let canonical = game
                .canonical_dirs
                .iter()
                .find(|d| d.eq_ignore_ascii_case(part));
            match canonical {
                Some(dir) if i < last => dir.clone(),
                _ => (*part).to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// A folder name that is valid on every OS.
pub fn sanitize_folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').trim();
    if trimmed.is_empty() {
        "mod".into()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::game::find_builtin;

    fn plan(files: &[&str]) -> InstallPlan {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            let path = dir.path().join(f);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }
        plan_install(
            &find_builtin("cyberpunk2077").unwrap(),
            dir.path(),
            "Cool Mod",
        )
        .unwrap()
    }

    fn targets(plan: &InstallPlan) -> Vec<&str> {
        plan.files.iter().map(|f| f.target.as_str()).collect()
    }

    #[test]
    fn root_layout_inside_wrapper_folder() {
        let p = plan(&[
            "Cool Mod v1/Archive/PC/Mod/cool.archive",
            "Cool Mod v1/readme.txt",
        ]);
        assert_eq!(targets(&p), ["archive/pc/mod/cool.archive"]);
        assert_eq!(p.files[0].source, "Cool Mod v1/Archive/PC/Mod/cool.archive");
        assert_eq!(p.skipped, ["readme.txt"]);
        assert_eq!(p.kinds, ["archive"]);
    }

    #[test]
    fn loose_archive_files_are_flattened() {
        let p = plan(&["files/a.archive", "files/b.xl", "preview.png"]);
        assert_eq!(
            targets(&p),
            ["archive/pc/mod/a.archive", "archive/pc/mod/b.xl"]
        );
    }

    #[test]
    fn cet_mod_folder_is_kept() {
        let p = plan(&["MyCetMod/init.lua", "MyCetMod/modules/ui.lua"]);
        assert_eq!(
            targets(&p),
            [
                "bin/x64/plugins/cyber_engine_tweaks/mods/MyCetMod/init.lua",
                "bin/x64/plugins/cyber_engine_tweaks/mods/MyCetMod/modules/ui.lua",
            ]
        );
        assert_eq!(p.kinds, ["cet"]);
    }

    #[test]
    fn cet_files_at_top_use_mod_name() {
        let p = plan(&["init.lua"]);
        assert_eq!(
            targets(&p),
            ["bin/x64/plugins/cyber_engine_tweaks/mods/Cool Mod/init.lua"]
        );
    }

    #[test]
    fn loose_redscript_goes_in_mod_folder() {
        let p = plan(&["script.reds"]);
        assert_eq!(targets(&p), ["r6/scripts/Cool Mod/script.reds"]);
    }

    #[test]
    fn redmod_folder() {
        let p = plan(&["MyRedMod/info.json", "MyRedMod/archives/x.archive"]);
        assert_eq!(
            targets(&p),
            [
                "mods/MyRedMod/archives/x.archive",
                "mods/MyRedMod/info.json"
            ]
        );
    }

    #[test]
    fn script_extender_is_classified() {
        let p = plan(&[
            "bin/x64/version.dll",
            "bin/x64/plugins/cyber_engine_tweaks.asi",
            "bin/x64/plugins/cyber_engine_tweaks/fonts/a.ttf",
        ]);
        assert_eq!(p.kinds, ["cet"]);
    }

    #[test]
    fn witcher_mod_folders_are_found_by_content_folder() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "Pack v2/modBetterUI/content/blob0.bundle",
            "Pack v2/modBetterUI/content/scripts/x.ws",
            "Pack v2/readme.txt",
        ] {
            let path = dir.path().join(f);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }
        let plan =
            plan_install(&find_builtin("witcher3").unwrap(), dir.path(), "Better UI").unwrap();
        assert_eq!(
            targets(&plan),
            [
                "Mods/modBetterUI/content/blob0.bundle",
                "Mods/modBetterUI/content/scripts/x.ws"
            ]
        );
    }

    #[test]
    fn unknown_layout_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("readme.txt"), b"x").unwrap();
        let err =
            plan_install(&find_builtin("cyberpunk2077").unwrap(), dir.path(), "x").unwrap_err();
        assert!(err.to_string().contains("readme.txt"));
    }

    #[test]
    fn sanitizes_folder_names() {
        assert_eq!(sanitize_folder_name("A/B: C?"), "A_B_ C_");
        assert_eq!(sanitize_folder_name("..."), "mod");
    }
}
