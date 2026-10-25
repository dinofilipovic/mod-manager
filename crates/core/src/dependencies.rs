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
/// frameworks each mod needs by the game's rules (`needs`). The same thing
/// required from several sources is listed once.
pub fn resolve(
    game: &GameDefinition,
    mods: &[ModNode],
    recorded: &[Dependency],
    frameworks: &[FrameworkStatus],
    needs: &BTreeMap<String, Vec<String>>,
) -> DependencyReport {
    let by_id: HashMap<&str, &ModNode> = mods.iter().map(|m| (m.id, m)).collect();
    // A Nexus mod that is one of the game's frameworks is that framework.
    let normalize = |mut d: Dependency| {
        if d.kind == DependencyKind::NexusMod
            && let Some(f) = d
                .target
                .parse::<u32>()
                .ok()
                .and_then(|id| game.frameworks.iter().find(|f| f.nexus_mod_id == Some(id)))
        {
            d.kind = DependencyKind::Framework;
            d.target = f.id.clone();
        }
        d
    };
    let rules = needs.iter().flat_map(|(mod_id, wanted)| {
        wanted.iter().map(move |framework| Dependency {
            mod_id: mod_id.clone(),
            kind: DependencyKind::Framework,
            target: framework.clone(),
            min_version: None,
            optional: false,
            source: DependencySource::Toml,
            notes: None,
            url: None,
        })
    });
    // The user's word first, then Nexus, then the rules; required if any says so.
    let mut all: Vec<Dependency> = Vec::new();
    let mut sorted: Vec<Dependency> = recorded
        .iter()
        .cloned()
        .map(normalize)
        .chain(rules)
        .collect();
    sorted.sort_by_key(|d| match d.source {
        DependencySource::User => 0,
        DependencySource::Nexus => 1,
        DependencySource::Toml => 2,
    });
    for d in sorted {
        match all.iter_mut().find(|a| {
            a.mod_id == d.mod_id && a.kind == d.kind && a.target.eq_ignore_ascii_case(&d.target)
        }) {
            Some(existing) => {
                existing.optional &= d.optional;
                existing.min_version = existing.min_version.take().or(d.min_version);
                existing.notes = existing.notes.take().or(d.notes);
                existing.url = existing.url.take().or(d.url);
            }
            None => all.push(d),
        }
    }

    let mut report = DependencyReport::default();
    // Edges between enabled mods where one is the only thing meeting the
    // other's requirement: for "required by" and cycles.
    let mut edges: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut wanted_disabled: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for dep in &all {
        let Some(owner) = by_id.get(dep.mod_id.as_str()) else {
            continue;
        };
        let (status, providers) = check(game, mods, frameworks, dep, owner.id);
        // A mod doesn't need itself (a framework's own files match its rule).
        if providers.contains(&owner.id) {
            continue;
        }
        if owner.enabled && !dep.optional {
            if let [only] = providers.as_slice() {
                edges.entry(owner.id).or_default().insert(only);
            }
            if let DependencyState::Disabled { mod_id, .. } = &status.state {
                wanted_disabled
                    .entry(mod_id.clone())
                    .or_default()
                    .insert(owner.name.to_string());
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
    // A disabled mod that enabled mods are waiting for is needed by them too.
    for (mod_id, names) in wanted_disabled {
        report.required_by.entry(mod_id).or_default().extend(names);
    }
    report.cycles = cycles(&edges)
        .into_iter()
        .map(|group| {
            let mut names: Vec<String> = group
                .into_iter()
                .map(|id| by_id[id].name.to_string())
                .collect();
            names.sort();
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
    owner: &str,
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
            FrameworkState::Installed { mod_name, .. } => {
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
        // Another file of the mod's own Nexus page doesn't count.
        DependencyKind::NexusMod => {
            status.nexus_mod_id = dep.target.parse().ok();
            mods.iter()
                .filter(|m| m.id != owner)
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

/// Groups of mods that need each other in a circle: the strongly connected
/// components with more than one mod (Tarjan's algorithm, linear time).
fn cycles<'a>(edges: &BTreeMap<&'a str, BTreeSet<&'a str>>) -> Vec<Vec<&'a str>> {
    struct Tarjan<'a, 'e> {
        edges: &'e BTreeMap<&'a str, BTreeSet<&'a str>>,
        index: HashMap<&'a str, usize>,
        low: HashMap<&'a str, usize>,
        stack: Vec<&'a str>,
        on_stack: BTreeSet<&'a str>,
        groups: Vec<Vec<&'a str>>,
    }
    impl<'a> Tarjan<'a, '_> {
        fn visit(&mut self, node: &'a str) {
            let i = self.index.len();
            self.index.insert(node, i);
            self.low.insert(node, i);
            self.stack.push(node);
            self.on_stack.insert(node);
            for next in self.edges.get(node).into_iter().flatten().copied() {
                if !self.index.contains_key(next) {
                    self.visit(next);
                    let low = self.low[node].min(self.low[next]);
                    self.low.insert(node, low);
                } else if self.on_stack.contains(next) {
                    let low = self.low[node].min(self.index[next]);
                    self.low.insert(node, low);
                }
            }
            if self.low[node] == self.index[node] {
                let mut group = Vec::new();
                while let Some(member) = self.stack.pop() {
                    self.on_stack.remove(member);
                    group.push(member);
                    if member == node {
                        break;
                    }
                }
                if group.len() > 1 {
                    self.groups.push(group);
                }
            }
        }
    }
    let mut t = Tarjan {
        edges,
        index: HashMap::new(),
        low: HashMap::new(),
        stack: Vec::new(),
        on_stack: BTreeSet::new(),
        groups: Vec::new(),
    };
    for node in edges.keys().copied() {
        if !t.index.contains_key(node) {
            t.visit(node);
        }
    }
    t.groups
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
    let response = serde_json::from_str::<RequirementsResponse>(body).ok();
    // Errors about some mods (hidden, deleted) come with the others' data.
    let nodes = match response.and_then(|r| r.data) {
        Some(data) => data.legacy_mods_by_domain.nodes,
        None => {
            return Err(crate::error::Error::Nexus(
                crate::collections::graphql_error(body)
                    .unwrap_or_else(|| "unexpected response from Nexus Mods".into()),
            ));
        }
    };
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
/// A failing batch doesn't lose the others; only when all fail is it an error.
pub fn fetch_requirements(
    url: &str,
    api_key: Option<&str>,
    game_domain: &str,
    mod_ids: &[u32],
    app_version: &str,
) -> crate::error::Result<BTreeMap<u32, Vec<Dependency>>> {
    let mut all = BTreeMap::new();
    let mut failure = None;
    for batch in mod_ids.chunks(REQUIREMENTS_BATCH) {
        let ids: Vec<serde_json::Value> = batch
            .iter()
            .map(|id| serde_json::json!({ "gameDomain": game_domain, "modId": id }))
            .collect();
        let found = crate::collections::post_graphql(
            url,
            api_key,
            REQUIREMENTS_QUERY,
            serde_json::json!({ "ids": ids }),
            app_version,
        )
        .and_then(|body| parse_requirements(&body));
        match found {
            Ok(found) => all.extend(found),
            Err(e) => failure = Some(e),
        }
    }
    match failure {
        Some(e) if all.is_empty() && !mod_ids.is_empty() => Err(e),
        _ => Ok(all),
    }
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
        assert_eq!(
            report.required_by["lib"],
            ["hud"],
            "removing a disabled mod others wait for warns too"
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
    fn the_same_requirement_from_several_sources_is_listed_once() {
        let mut lib = Lib { mods: Vec::new() };
        // A RED4ext plugin needs RED4ext by the game's rules, and its Nexus
        // page says so too.
        lib.add("plugin", "1", Some(5), true, &["red4ext/plugins/P/P.dll"]);
        let report = lib.resolve(&[nexus("plugin", 2380)]);
        let red4ext: Vec<_> = report.requires["plugin"]
            .iter()
            .filter(|d| d.name == "RED4ext")
            .collect();
        assert_eq!(red4ext.len(), 1);
        assert_eq!(
            red4ext[0].source,
            DependencySource::Nexus,
            "the more specific source is shown"
        );
        assert_eq!(red4ext[0].state, DependencyState::Missing);
    }

    #[test]
    fn a_second_provider_means_nobody_depends_on_either() {
        let mut lib = Lib { mods: Vec::new() };
        lib.add("plugin", "1", Some(5), true, &["red4ext/plugins/P/P.dll"]);
        lib.add("r4a", "1", Some(2380), true, &["red4ext/RED4ext.dll"]);
        lib.add("r4b", "1", Some(2380), true, &["red4ext/RED4ext.dll"]);
        let report = lib.resolve(&[]);
        assert!(
            report.required_by.is_empty(),
            "disabling either keeps RED4ext"
        );
        // A requirement on its own Nexus page doesn't resolve to itself.
        let mut own = Lib { mods: Vec::new() };
        own.add("main", "1", Some(9), true, &["a.archive"]);
        let report = own.resolve(&[nexus("main", 9)]);
        assert_eq!(report.requires["main"][0].state, DependencyState::Missing);
    }

    #[test]
    fn partial_errors_keep_the_other_mods() {
        let body = r#"{"errors":[{"message":"Mod not found","extensions":{"code":"NOT_FOUND"}}],
            "data":{"legacyModsByDomain":{"nodes":[{"modId":5,"gameId":3333,"modRequirements":
            {"nexusRequirements":{"nodes":[{"modId":"2380","modName":"RED4ext","gameId":"3333",
            "externalRequirement":false,"notes":null,"url":null}]}}}]}}}"#;
        let found = parse_requirements(body).unwrap();
        assert_eq!(found[&5][0].target, "2380");
        assert!(parse_requirements(r#"{"errors":[{"message":"boom"}],"data":null}"#).is_err());
    }

    #[test]
    fn finds_cycles() {
        let mut lib = Lib { mods: Vec::new() };
        lib.add("a", "1", Some(1), true, &["a.archive"]);
        lib.add("b", "1", Some(2), true, &["b.archive"]);
        lib.add("c", "1", Some(3), true, &["c.archive"]);
        let report = lib.resolve(&[nexus("a", 2), nexus("b", 3), nexus("c", 1), nexus("b", 1)]);
        assert_eq!(
            report.cycles,
            [vec!["a", "b", "c"]],
            "one group, however many circles"
        );

        // A long chain without a circle is cheap and reports nothing.
        let mut chain = Lib { mods: Vec::new() };
        let mut deps = Vec::new();
        for i in 0..300u32 {
            chain.add(&format!("m{i}"), "1", Some(i + 1), true, &[]);
            for j in i + 1..(i + 6).min(300) {
                deps.push(nexus(&format!("m{i}"), j + 1));
            }
        }
        assert!(chain.resolve(&deps).cycles.is_empty());
    }
}
