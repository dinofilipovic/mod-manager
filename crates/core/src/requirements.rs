//! Which frameworks (script extenders and the like) each enabled mod needs,
//! and which of those are missing. Rules come from the game definition:
//! mod files match `requirements` by extension or folder, and a framework is
//! present when an enabled mod (or a manual install) provides its files.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use ts_rs::TS;

use crate::game::{Framework, GameDefinition};

/// A mod and its game-relative files, as seen by the analysis.
pub struct ModFiles<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub enabled: bool,
    pub files: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(tag = "state")]
#[ts(export)]
pub enum FrameworkState {
    /// An enabled mod provides it.
    Installed {
        mod_name: String,
    },
    /// Its files are in the game folder but weren't placed by this app.
    InstalledManually,
    /// A mod in the library provides it, but it's disabled in this profile.
    Disabled {
        mod_id: String,
        mod_name: String,
    },
    NotInstalled,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct FrameworkStatus {
    pub id: String,
    pub name: String,
    pub nexus_mod_id: Option<u32>,
    pub state: FrameworkState,
    /// Enabled mods that need it.
    pub needed_by: Vec<String>,
    /// The installed version, when it can be told.
    pub version: Option<String>,
    /// The oldest version that works with this game version, when known.
    pub min_version: Option<String>,
    /// Installed, but older than `min_version`.
    pub too_old: bool,
}

impl FrameworkStatus {
    pub fn is_available(&self) -> bool {
        matches!(
            self.state,
            FrameworkState::Installed { .. } | FrameworkState::InstalledManually
        )
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct MissingRequirement {
    pub framework_id: String,
    pub name: String,
    pub nexus_mod_id: Option<u32>,
    /// Set when the library has it but it's disabled: enabling fixes it.
    pub disabled_mod_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct Analysis {
    pub frameworks: Vec<FrameworkStatus>,
    /// Missing requirements per enabled mod id.
    pub missing: BTreeMap<String, Vec<MissingRequirement>>,
    /// The frameworks each enabled mod needs (directly or through another
    /// framework), by id.
    pub needs: BTreeMap<String, Vec<String>>,
}

/// Whether a mod's files include the framework's.
pub fn provides(framework: &Framework, files: &[String]) -> bool {
    framework
        .provides
        .iter()
        .any(|p| files.iter().any(|f| f.eq_ignore_ascii_case(p)))
}

/// `installed_manually(framework)` tells whether the framework's files are in
/// the game folder without having been placed by this app.
pub fn analyze(
    game: &GameDefinition,
    mods: &[ModFiles],
    installed_manually: impl Fn(&Framework) -> bool,
) -> Analysis {
    // What each enabled mod needs, including what its own frameworks need.
    let mut needs: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for m in mods.iter().filter(|m| m.enabled) {
        let own: BTreeSet<&str> = game
            .frameworks
            .iter()
            .filter(|f| provides(f, m.files))
            .map(|f| f.id.as_str())
            .collect();
        let mut wanted: BTreeSet<&str> = game
            .requirements
            .iter()
            .filter(|r| m.files.iter().any(|f| r.matches(f)))
            .map(|r| r.framework.as_str())
            .chain(
                own.iter()
                    .filter_map(|id| game.framework(id))
                    .flat_map(|f| f.requires.iter().map(String::as_str)),
            )
            .collect();
        // Frameworks need what they build on, transitively.
        let mut queue: Vec<&str> = wanted.iter().copied().collect();
        while let Some(id) = queue.pop() {
            for dep in game
                .framework(id)
                .map(|f| f.requires.as_slice())
                .unwrap_or_default()
            {
                if wanted.insert(dep) {
                    queue.push(dep);
                }
            }
        }
        wanted.retain(|id| !own.contains(id));
        needs.insert(m.id, wanted);
    }

    let frameworks: Vec<FrameworkStatus> = game
        .frameworks
        .iter()
        .map(|f| {
            let enabled = mods.iter().find(|m| m.enabled && provides(f, m.files));
            let disabled = mods.iter().find(|m| !m.enabled && provides(f, m.files));
            let state = match (enabled, disabled) {
                (Some(m), _) => FrameworkState::Installed {
                    mod_name: m.name.to_string(),
                },
                _ if installed_manually(f) => FrameworkState::InstalledManually,
                (None, Some(m)) => FrameworkState::Disabled {
                    mod_id: m.id.to_string(),
                    mod_name: m.name.to_string(),
                },
                (None, None) => FrameworkState::NotInstalled,
            };
            let needed_by = mods
                .iter()
                .filter(|m| needs.get(m.id).is_some_and(|n| n.contains(f.id.as_str())))
                .map(|m| m.name.to_string())
                .collect();
            FrameworkStatus {
                id: f.id.clone(),
                name: f.name.clone(),
                nexus_mod_id: f.nexus_mod_id,
                state,
                needed_by,
                version: None,
                min_version: None,
                too_old: false,
            }
        })
        .collect();

    let needs_by_mod = needs
        .iter()
        .map(|(id, wanted)| {
            (
                id.to_string(),
                wanted.iter().map(|w| w.to_string()).collect(),
            )
        })
        .collect();
    let missing = needs
        .into_iter()
        .map(|(mod_id, wanted)| {
            let list = wanted
                .into_iter()
                .filter_map(|id| frameworks.iter().find(|f| f.id == id))
                .filter(|f| !f.is_available())
                .map(|f| MissingRequirement {
                    framework_id: f.id.clone(),
                    name: f.name.clone(),
                    nexus_mod_id: f.nexus_mod_id,
                    disabled_mod_id: match &f.state {
                        FrameworkState::Disabled { mod_id, .. } => Some(mod_id.clone()),
                        _ => None,
                    },
                })
                .collect();
            (mod_id.to_string(), list)
        })
        .collect();

    Analysis {
        frameworks,
        missing,
        needs: needs_by_mod,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::find_builtin;

    fn files(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reports_missing_and_disabled_frameworks() {
        let game = find_builtin("cyberpunk2077").unwrap();
        let script = files(&["r6/scripts/Cool/main.reds", "archive/pc/mod/cool.xl"]);
        let cet = files(&["bin/x64/plugins/cyber_engine_tweaks.asi"]);
        let axl = files(&["red4ext/plugins/ArchiveXL/ArchiveXL.dll"]);
        let mods = [
            ModFiles {
                id: "cool",
                name: "Cool",
                enabled: true,
                files: &script,
            },
            ModFiles {
                id: "cet",
                name: "CET",
                enabled: true,
                files: &cet,
            },
            ModFiles {
                id: "axl",
                name: "ArchiveXL",
                enabled: false,
                files: &axl,
            },
        ];
        let a = analyze(&game, &mods, |_| false);

        let missing: Vec<(&str, bool)> = a.missing["cool"]
            .iter()
            .map(|m| (m.framework_id.as_str(), m.disabled_mod_id.is_some()))
            .collect();
        // .xl needs ArchiveXL (disabled), which needs RED4ext; .reds needs redscript.
        assert_eq!(
            missing,
            [
                ("archivexl", true),
                ("red4ext", false),
                ("redscript", false)
            ]
        );
        assert!(a.missing["cet"].is_empty());

        let state = |id: &str| {
            a.frameworks
                .iter()
                .find(|f| f.id == id)
                .unwrap()
                .state
                .clone()
        };
        assert_eq!(
            state("cet"),
            FrameworkState::Installed {
                mod_name: "CET".into()
            }
        );
        assert!(matches!(
            state("archivexl"),
            FrameworkState::Disabled { .. }
        ));
    }

    #[test]
    fn manual_installs_count_and_frameworks_need_their_base() {
        let game = find_builtin("cyberpunk2077").unwrap();
        let tweak = files(&["red4ext/plugins/TweakXL/TweakXL.dll"]);
        let mods = [ModFiles {
            id: "txl",
            name: "TweakXL",
            enabled: true,
            files: &tweak,
        }];
        let a = analyze(&game, &mods, |f| f.id == "red4ext");
        assert!(
            a.missing["txl"].is_empty(),
            "RED4ext installed by hand satisfies TweakXL"
        );
        let a = analyze(&game, &mods, |_| false);
        assert_eq!(a.missing["txl"][0].framework_id, "red4ext");
    }
}
