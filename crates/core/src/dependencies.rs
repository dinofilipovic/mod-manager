//! What each mod needs and whether it is there, per profile.
//!
//! Requirements come from three places: the game definition (framework
//! rules in `requirements.rs`), Nexus Mods (a mod page's requirements) and
//! the user. Each points at a framework, a Nexus mod or a mod in the
//! library; the resolver checks them against the profile's enabled mods and
//! reports what is missing, disabled, too old, optional, or a cycle.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::game::GameDefinition;
use crate::requirements::{FrameworkState, FrameworkStatus};
use crate::updates::compare_versions;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum DependencyKind {
    /// A framework of the game definition (`target` is its id).
    Framework,
    /// A mod on Nexus (`target` is its mod id).
    NexusMod,
    /// A mod in the library (`target` is its id).
    Mod,
    /// Something off Nexus, or on another game's Nexus pages (`target` is its name).
    External,
}

impl DependencyKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Framework => "framework",
            Self::NexusMod => "nexus_mod",
            Self::Mod => "mod",
            Self::External => "external",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "framework" => Self::Framework,
            "nexus_mod" => Self::NexusMod,
            "mod" => Self::Mod,
            _ => Self::External,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum DependencySource {
    /// The game definition's framework rules.
    Toml,
    /// The mod's requirements on Nexus Mods.
    Nexus,
    /// Added by the user.
    User,
}

impl DependencySource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Toml => "toml",
            Self::Nexus => "nexus",
            Self::User => "user",
        }
    }

    pub(crate) fn parse(s: &str) -> Self {
        match s {
            "toml" => Self::Toml,
            "nexus" => Self::Nexus,
            _ => Self::User,
        }
    }
}

/// A recorded requirement of a mod.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Dependency {
    pub mod_id: String,
    pub kind: DependencyKind,
    pub target: String,
    pub min_version: Option<String>,
    pub optional: bool,
    pub source: DependencySource,
    /// Shown with the requirement (from Nexus: the author's note).
    pub notes: Option<String>,
    /// Where to get it, for external requirements.
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[serde(tag = "state")]
#[ts(export)]
pub enum DependencyState {
    /// An enabled mod (or a manual install) provides it.
    Satisfied {
        by: String,
    },
    Missing,
    /// The library has it, but it's disabled in this profile.
    Disabled {
        mod_id: String,
        mod_name: String,
    },
    TooOld {
        have: String,
        need: String,
    },
    /// Something the app can't check (an external requirement).
    Unverifiable,
}

/// A requirement and how it stands in the active profile.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct DependencyStatus {
    pub kind: DependencyKind,
    pub target: String,
    /// What to call it: a framework's or mod's name.
    pub name: String,
    pub source: DependencySource,
    pub optional: bool,
    pub notes: Option<String>,
    pub url: Option<String>,
    /// Where to get it on Nexus.
    pub nexus_mod_id: Option<u32>,
    pub state: DependencyState,
}

impl DependencyStatus {
    /// Needed and not there (optional ones and ones that can't be checked
    /// don't count).
    pub fn is_unmet(&self) -> bool {
        !self.optional
            && !matches!(
                self.state,
                DependencyState::Satisfied { .. } | DependencyState::Unverifiable
            )
    }
}

/// A library mod as the resolver sees it.
pub struct ModNode<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub version: Option<&'a str>,
    pub nexus_mod_id: Option<u32>,
    pub enabled: bool,
    /// Frameworks whose files this mod contains.
    pub provides: Vec<String>,
}

#[derive(Debug, Default)]
pub struct DependencyReport {
    /// Requirements of each mod (enabled or not), by mod id.
    pub requires: BTreeMap<String, Vec<DependencyStatus>>,
    /// Enabled mods needing each mod, by mod id (names).
    pub required_by: BTreeMap<String, Vec<String>>,
    /// Groups of enabled mods that need each other in a circle (names).
    pub cycles: Vec<Vec<String>>,
}

/// Checks every requirement: the recorded ones (`recorded`) and the
/// frameworks each enabled mod needs by the game's rules (`needs`).
pub fn resolve(
    game: &GameDefinition,
    mods: &[ModNode],
    recorded: &[Dependency],
    frameworks: &[FrameworkStatus],
    needs: &BTreeMap<String, Vec<String>>,
) -> DependencyReport {
    let by_id: HashMap<&str, &ModNode> = mods.iter().map(|m| (m.id, m)).collect();
    let mut all: Vec<Dependency> = recorded.to_vec();
    for (mod_id, wanted) in needs {
        for framework in wanted {
            let already = all.iter().any(|d| {
                d.mod_id == *mod_id && d.kind == DependencyKind::Framework && d.target == *framework
            });
            if !already {
                all.push(Dependency {
                    mod_id: mod_id.clone(),
                    kind: DependencyKind::Framework,
                    target: framework.clone(),
                    min_version: None,
                    optional: false,
                    source: DependencySource::Toml,
                    notes: None,
                    url: None,
                });
            }
        }
    }

    let mut report = DependencyReport::default();
    // Edges between enabled mods, for "required by" and cycles.
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for dep in &all {
        let Some(owner) = by_id.get(dep.mod_id.as_str()) else {
            continue;
        };
        let (status, providers) = check(game, mods, frameworks, dep);
        // A mod doesn't need itself (a framework's own files match its rule).
        if providers.contains(&owner.id) {
            continue;
        }
        if owner.enabled {
            for p in providers
                .iter()
                .filter(|p| by_id.get(*p).is_some_and(|m| m.enabled))
            {
                edges.entry(owner.id).or_default().insert(p);
            }
        }
        report
            .requires
            .entry(dep.mod_id.clone())
            .or_default()
            .push(status);
    }
    for (owner, targets) in &edges {
        for target in targets {
            report
                .required_by
                .entry(target.to_string())
                .or_default()
                .push(by_id[owner].name.to_string());
        }
    }
    report.cycles = cycles(&edges)
        .into_iter()
        .map(|c| {
            let mut names: Vec<String> =
                c.into_iter().map(|id| by_id[id].name.to_string()).collect();
            // Start at the first name alphabetically, keeping the order around the circle.
            let first = names
                .iter()
                .enumerate()
                .min_by_key(|(_, n)| n.as_str())
                .map(|(i, _)| i);
            names.rotate_left(first.unwrap_or(0));
            names
        })
        .collect();
    report.cycles.sort();
    report
}

/// The state of one requirement, and the mods that provide it.
fn check<'a>(
    game: &GameDefinition,
    mods: &'a [ModNode],
    frameworks: &[FrameworkStatus],
    dep: &Dependency,
) -> (DependencyStatus, Vec<&'a str>) {
    let mut status = DependencyStatus {
        kind: dep.kind,
        target: dep.target.clone(),
        name: dep.target.clone(),
        source: dep.source,
        optional: dep.optional,
        notes: dep.notes.clone(),
        url: dep.url.clone(),
        nexus_mod_id: None,
        state: DependencyState::Missing,
    };
    // A Nexus mod that is one of the game's frameworks is checked as that
    // framework, so a manual install counts.
    let framework_id = match dep.kind {
        DependencyKind::Framework => Some(dep.target.clone()),
        DependencyKind::NexusMod => dep.target.parse::<u32>().ok().and_then(|id| {
            game.frameworks
                .iter()
                .find(|f| f.nexus_mod_id == Some(id))
                .map(|f| f.id.clone())
        }),
        _ => None,
    };
    if let Some(framework_id) = framework_id {
        let providers: Vec<&str> = mods
            .iter()
            .filter(|m| m.enabled && m.provides.contains(&framework_id))
            .map(|m| m.id)
            .collect();
        let Some(f) = frameworks.iter().find(|f| f.id == framework_id) else {
            return (status, providers);
        };
        status.name = f.name.clone();
        status.nexus_mod_id = f.nexus_mod_id;
        status.state = match &f.state {
            _ if f.too_old => DependencyState::TooOld {
                have: f.version.clone().unwrap_or_default(),
                need: f.min_version.clone().unwrap_or_default(),
            },
            FrameworkState::Installed { mod_name } => {
                too_old(f.version.as_deref(), dep.min_version.as_deref()).unwrap_or(
                    DependencyState::Satisfied {
                        by: mod_name.clone(),
                    },
                )
            }
            FrameworkState::InstalledManually => {
                too_old(f.version.as_deref(), dep.min_version.as_deref()).unwrap_or(
                    DependencyState::Satisfied {
                        by: "installed manually".into(),
                    },
                )
            }
            FrameworkState::Disabled { mod_id, mod_name } => DependencyState::Disabled {
                mod_id: mod_id.clone(),
                mod_name: mod_name.clone(),
            },
            FrameworkState::NotInstalled => DependencyState::Missing,
        };
        return (status, providers);
    }

    let candidates: Vec<&ModNode> = match dep.kind {
        DependencyKind::NexusMod => {
            status.nexus_mod_id = dep.target.parse().ok();
            mods.iter()
                .filter(|m| m.nexus_mod_id.is_some() && m.nexus_mod_id == status.nexus_mod_id)
                .collect()
        }
        DependencyKind::Mod => mods.iter().filter(|m| m.id == dep.target).collect(),
        DependencyKind::External | DependencyKind::Framework => {
            status.state = DependencyState::Unverifiable;
            return (status, Vec::new());
        }
    };
    if let Some(m) = candidates.first() {
        status.name = m.name.to_string();
        status.nexus_mod_id = status.nexus_mod_id.or(m.nexus_mod_id);
    }
    let enabled: Vec<&&ModNode> = candidates.iter().filter(|m| m.enabled).collect();
    status.state = match (enabled.first(), candidates.first()) {
        (Some(m), _) => {
            too_old(m.version, dep.min_version.as_deref()).unwrap_or(DependencyState::Satisfied {
                by: m.name.to_string(),
            })
        }
        (None, Some(m)) => DependencyState::Disabled {
            mod_id: m.id.to_string(),
            mod_name: m.name.to_string(),
        },
        (None, None) => DependencyState::Missing,
    };
    (status, enabled.iter().map(|m| m.id).collect())
}

fn too_old(have: Option<&str>, need: Option<&str>) -> Option<DependencyState> {
    let (have, need) = (have?, need?);
    (compare_versions(have, need) == std::cmp::Ordering::Less).then(|| DependencyState::TooOld {
        have: have.to_string(),
        need: need.to_string(),
    })
}

/// Every cycle in the graph, each once, starting at its smallest id.
fn cycles<'a>(edges: &BTreeMap<&'a str, BTreeSet<&'a str>>) -> Vec<Vec<&'a str>> {
    let mut found: BTreeSet<Vec<&str>> = BTreeSet::new();
    fn walk<'a>(
        node: &'a str,
        edges: &BTreeMap<&'a str, BTreeSet<&'a str>>,
        path: &mut Vec<&'a str>,
        found: &mut BTreeSet<Vec<&'a str>>,
    ) {
        if let Some(start) = path.iter().position(|n| *n == node) {
            let mut cycle = path[start..].to_vec();
            let min = cycle
                .iter()
                .enumerate()
                .min_by_key(|(_, n)| **n)
                .map(|(i, _)| i);
            cycle.rotate_left(min.unwrap_or(0));
            found.insert(cycle);
            return;
        }
        // Long chains can't hide a cycle shorter than the mod count.
        if path.len() > edges.len() {
            return;
        }
        path.push(node);
        for next in edges.get(node).into_iter().flatten() {
            walk(next, edges, path, found);
        }
        path.pop();
    }
    for node in edges.keys() {
        walk(node, edges, &mut Vec::new(), &mut found);
    }
    found.into_iter().collect()
}

/// The mod-page requirements of up to 50 mods per request (v2 API). The
/// query is kept in `tests/fixtures/nexus/requirements.graphql` too.
pub const REQUIREMENTS_QUERY: &str = "\
query ModRequirements($ids: [CompositeDomainWithIdInput!]!) {
  legacyModsByDomain(ids: $ids, count: 50) {
    nodes {
      modId
      gameId
      modRequirements {
        nexusRequirements {
          nodes { modId modName gameId externalRequirement notes url }
        }
      }
    }
  }
}
";

/// Mods per requirements request.
pub const REQUIREMENTS_BATCH: usize = 50;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequirementsResponse {
    data: Option<RequirementsData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequirementsData {
    legacy_mods_by_domain: NodePage<RequirementsMod>,
}

#[derive(Deserialize)]
struct NodePage<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequirementsMod {
    mod_id: u32,
    game_id: u32,
    mod_requirements: Option<ModRequirements>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModRequirements {
    nexus_requirements: NodePage<NexusRequirement>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct NexusRequirement {
    mod_id: String,
    mod_name: String,
    game_id: String,
    external_requirement: bool,
    notes: Option<String>,
    url: Option<String>,
}

/// Turns a requirements response into each mod's requirements (by Nexus mod
/// id), as `nexus` dependencies with an empty `mod_id` to fill in. Off-site
/// requirements and ones on another game's pages become `External`.
pub fn parse_requirements(body: &str) -> crate::error::Result<BTreeMap<u32, Vec<Dependency>>> {
    if let Some(message) = crate::collections::graphql_error(body) {
        return Err(crate::error::Error::Nexus(message));
    }
    let response: RequirementsResponse = serde_json::from_str(body).map_err(|e| {
        crate::error::Error::Nexus(format!("unexpected response from Nexus Mods: {e}"))
    })?;
    let nodes = response
        .data
        .map(|d| d.legacy_mods_by_domain.nodes)
        .unwrap_or_default();
    Ok(nodes
        .into_iter()
        .map(|m| {
            let deps = m
                .mod_requirements
                .map(|r| r.nexus_requirements.nodes)
                .unwrap_or_default()
                .into_iter()
                .map(|r| {
                    let same_game = r.game_id.parse::<u32>().ok() == Some(m.game_id);
                    let nexus_id = r.mod_id.parse::<u32>().ok().filter(|id| *id > 0);
                    let non_empty = |s: Option<String>| s.filter(|s| !s.trim().is_empty());
                    let (kind, target) = match nexus_id {
                        Some(id) if same_game && !r.external_requirement => {
                            (DependencyKind::NexusMod, id.to_string())
                        }
                        _ => (DependencyKind::External, r.mod_name.clone()),
                    };
                    Dependency {
                        mod_id: String::new(),
                        kind,
                        target,
                        min_version: None,
                        optional: false,
                        source: DependencySource::Nexus,
                        notes: non_empty(r.notes),
                        url: non_empty(r.url).or_else(|| {
                            // A requirement on another game's pages links there.
                            nexus_id.filter(|_| !same_game).map(|id| {
                                format!("https://www.nexusmods.com/games/{}/mods/{id}", r.game_id)
                            })
                        }),
                    }
                })
                .collect();
            (m.mod_id, deps)
        })
        .collect())
}

/// Asks Nexus for the requirements of `mod_ids` (of one game), in batches.
pub fn fetch_requirements(
    url: &str,
    api_key: Option<&str>,
    game_domain: &str,
    mod_ids: &[u32],
    app_version: &str,
) -> crate::error::Result<BTreeMap<u32, Vec<Dependency>>> {
    let mut all = BTreeMap::new();
    for batch in mod_ids.chunks(REQUIREMENTS_BATCH) {
        let ids: Vec<serde_json::Value> = batch
            .iter()
            .map(|id| serde_json::json!({ "gameDomain": game_domain, "modId": id }))
            .collect();
        let body = crate::collections::post_graphql(
            url,
            api_key,
            REQUIREMENTS_QUERY,
            serde_json::json!({ "ids": ids }),
            app_version,
        )?;
        all.extend(parse_requirements(&body)?);
    }
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::find_builtin;
    use crate::requirements::{self, ModFiles};

    struct TestMod {
        id: String,
        version: Option<String>,
        nexus: Option<u32>,
        enabled: bool,
        files: Vec<String>,
    }

    struct Lib {
        mods: Vec<TestMod>,
    }

    impl Lib {
        fn add(
            &mut self,
            id: &str,
            version: &str,
            nexus: Option<u32>,
            enabled: bool,
            files: &[&str],
        ) {
            self.mods.push(TestMod {
                id: id.into(),
                version: Some(version.to_string()).filter(|v| !v.is_empty()),
                nexus,
                enabled,
                files: files.iter().map(|f| f.to_string()).collect(),
            });
        }

        fn resolve(&self, recorded: &[Dependency]) -> DependencyReport {
            let game = find_builtin("cyberpunk2077").unwrap();
            let files: Vec<ModFiles> = self
                .mods
                .iter()
                .map(|m| ModFiles {
                    id: &m.id,
                    name: &m.id,
                    enabled: m.enabled,
                    files: &m.files,
                })
                .collect();
            let analysis = requirements::analyze(&game, &files, |_| false);
            let nodes: Vec<ModNode> = self
                .mods
                .iter()
                .map(|m| ModNode {
                    id: &m.id,
                    name: &m.id,
                    version: m.version.as_deref(),
                    nexus_mod_id: m.nexus,
                    enabled: m.enabled,
                    provides: game
                        .frameworks
                        .iter()
                        .filter(|f| requirements::provides(f, &m.files))
                        .map(|f| f.id.clone())
                        .collect(),
                })
                .collect();
            resolve(
                &game,
                &nodes,
                recorded,
                &analysis.frameworks,
                &analysis.needs,
            )
        }
    }

    fn nexus(mod_id: &str, target: u32) -> Dependency {
        Dependency {
            mod_id: mod_id.into(),
            kind: DependencyKind::NexusMod,
            target: target.to_string(),
            min_version: None,
            optional: false,
            source: DependencySource::Nexus,
            notes: None,
            url: None,
        }
    }

    fn state<'a>(report: &'a DependencyReport, mod_id: &str, name: &str) -> &'a DependencyState {
        &report.requires[mod_id]
            .iter()
            .find(|d| d.name == name || d.target == name)
            .unwrap_or_else(|| panic!("{mod_id} has no requirement {name}"))
            .state
    }

    #[test]
    fn reports_each_state() {
        let mut lib = Lib { mods: Vec::new() };
        lib.add("hud", "1.0", Some(10), true, &["r6/scripts/Hud/main.reds"]);
        lib.add(
            "lib",
            "1.0",
            Some(20),
            false,
            &["archive/pc/mod/lib.archive"],
        );
        lib.add(
            "old",
            "0.9",
            Some(30),
            true,
            &["archive/pc/mod/old.archive"],
        );
        let recorded = vec![
            nexus("hud", 20),
            Dependency {
                min_version: Some("1.2".into()),
                ..nexus("hud", 30)
            },
            Dependency {
                optional: true,
                ..nexus("hud", 40)
            },
            Dependency {
                kind: DependencyKind::External,
                target: "Some tool".into(),
                ..nexus("hud", 0)
            },
        ];
        let report = lib.resolve(&recorded);
        // The framework rule: .reds needs redscript, not installed.
        assert_eq!(
            state(&report, "hud", "redscript"),
            &DependencyState::Missing
        );
        assert_eq!(
            state(&report, "hud", "lib"),
            &DependencyState::Disabled {
                mod_id: "lib".into(),
                mod_name: "lib".into()
            },
            "enabling it fixes it"
        );
        assert_eq!(
            state(&report, "hud", "old"),
            &DependencyState::TooOld {
                have: "0.9".into(),
                need: "1.2".into()
            }
        );
        let optional = report.requires["hud"]
            .iter()
            .find(|d| d.target == "40")
            .unwrap();
        assert_eq!(optional.state, DependencyState::Missing);
        assert!(!optional.is_unmet(), "optional ones don't block");
        assert_eq!(
            state(&report, "hud", "Some tool"),
            &DependencyState::Unverifiable
        );
        assert_eq!(report.required_by["old"], ["hud"]);
        assert!(
            !report.required_by.contains_key("lib"),
            "disabled mods aren't needed by anything yet"
        );
    }

    #[test]
    fn nexus_requirements_on_frameworks_count_manual_installs_and_providers() {
        let mut lib = Lib { mods: Vec::new() };
        lib.add(
            "axl",
            "1.28.0",
            Some(4198),
            true,
            &["red4ext/plugins/ArchiveXL/ArchiveXL.dll"],
        );
        lib.add("r4", "1.31.0", Some(2380), true, &["red4ext/RED4ext.dll"]);
        // ArchiveXL's Nexus page requires RED4ext (2380).
        let report = lib.resolve(&[nexus("axl", 2380)]);
        assert_eq!(
            state(&report, "axl", "RED4ext"),
            &DependencyState::Satisfied { by: "r4".into() }
        );
        assert_eq!(report.required_by["r4"], ["axl"]);
        assert!(
            !report.requires.contains_key("r4"),
            "a framework doesn't need itself"
        );
    }

    #[test]
    fn parses_the_recorded_requirements_response() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/nexus");
        let query = std::fs::read_to_string(dir.join("requirements.graphql")).unwrap();
        assert_eq!(
            query.trim(),
            REQUIREMENTS_QUERY.trim(),
            "the fixture is the query in use"
        );
        let body = std::fs::read_to_string(dir.join("requirements_response.json")).unwrap();
        let reqs = parse_requirements(&body).unwrap();
        let targets = |id: u32| -> Vec<(DependencyKind, String)> {
            reqs[&id]
                .iter()
                .map(|d| (d.kind, d.target.clone()))
                .collect()
        };
        assert_eq!(targets(4198), [(DependencyKind::NexusMod, "2380".into())]);
        assert_eq!(
            targets(4885),
            [
                (DependencyKind::NexusMod, "2380".into()),
                (DependencyKind::NexusMod, "1511".into()),
                (DependencyKind::NexusMod, "4198".into())
            ]
        );
        assert!(reqs[&107].is_empty());
        let external = &reqs[&1856][0];
        assert_eq!(external.kind, DependencyKind::External);
        assert_eq!(
            external.target,
            "Cyber Engine Tweaks Version 1.11.4 (Or Higher)"
        );
        assert!(
            external
                .url
                .as_deref()
                .unwrap()
                .starts_with("https://github.com/")
        );
        assert_eq!(
            external.notes.as_deref(),
            Some("Must have for mod to work.")
        );
        assert!(reqs[&4198][0].notes.is_none(), "empty notes are dropped");
    }

    #[test]
    fn finds_cycles() {
        let mut lib = Lib { mods: Vec::new() };
        lib.add("a", "1", Some(1), true, &["a.archive"]);
        lib.add("b", "1", Some(2), true, &["b.archive"]);
        lib.add("c", "1", Some(3), true, &["c.archive"]);
        let report = lib.resolve(&[nexus("a", 2), nexus("b", 3), nexus("c", 1), nexus("b", 1)]);
        assert_eq!(report.cycles, [vec!["a", "b"], vec!["a", "b", "c"]]);
    }
}
