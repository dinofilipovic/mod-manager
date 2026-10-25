//! Mods installed by hand: files in the game's mod folders that no mod in
//! the library placed, grouped the way the layout rules group a mod's files,
//! so each group can be adopted as a mod (`Library::adopt`).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use walkdir::WalkDir;

use crate::fs_util::resolve_case_insensitive;
use crate::game::GameDefinition;

/// Files that look like one hand-installed mod.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct UnownedGroup {
    /// The layout kind, e.g. `archive` or `cet`.
    pub kind: String,
    /// A name for the mod: its folder, or the file name without extensions.
    pub name: String,
    /// Game-relative paths as they are on disk.
    pub files: Vec<String>,
    #[ts(type = "number")]
    pub size_bytes: u64,
}

/// Groups the files in the game's mod folders that aren't in `owned`
/// (lowercased game-relative paths of deployed files).
pub fn unowned_groups(
    game: &GameDefinition,
    install_path: &Path,
    owned: &HashSet<String>,
) -> Vec<UnownedGroup> {
    let load_order = game
        .load_order
        .as_ref()
        .map(|o| o.file.to_ascii_lowercase());
    let mut groups: BTreeMap<(String, String), UnownedGroup> = BTreeMap::new();
    for layout in game
        .layout
        .iter()
        .filter(|t| !t.extensions.is_empty() || t.marker_file.is_some())
    {
        let Some(dir) = resolve_case_insensitive(install_path, &layout.path) else {
            continue;
        };
        for entry in WalkDir::new(&dir).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let Ok(relative) = entry.path().strip_prefix(install_path) else {
                continue;
            };
            let target = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let key = target.to_ascii_lowercase();
            // Ours already, the generated load order, or a more specific
            // layout's file.
            if owned.contains(&key)
                || load_order.as_deref() == Some(key.as_str())
                || game.layout_for(&target).map(|l| &l.path) != Some(&layout.path)
            {
                continue;
            }
            let rest = &target[layout.path.len()..].trim_start_matches('/');
            let mut parts = rest.split('/');
            let first = parts.next().unwrap_or_default();
            let name = if parts.next().is_some() && !layout.flatten {
                first.to_string()
            } else {
                // A loose file: its name up to the first dot, so
                // `x.archive` and `x.archive.xl` go together.
                first.split('.').next().unwrap_or(first).to_string()
            };
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            let group = groups
                .entry((layout.kind.clone(), name.to_ascii_lowercase()))
                .or_insert_with(|| UnownedGroup {
                    kind: layout.kind.clone(),
                    name: name.clone(),
                    files: Vec::new(),
                    size_bytes: 0,
                });
            group.files.push(target);
            group.size_bytes += size;
        }
    }
    groups
        .into_values()
        .map(|mut g| {
            g.files.sort();
            g
        })
        .collect()
}
