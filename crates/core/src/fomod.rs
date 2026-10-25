//! FOMOD installers: `fomod/ModuleConfig.xml` inside a mod archive describes
//! a wizard of steps, groups and options, and which files each choice installs.
//!
//! Flow: [`find_installer`] parses the XML, the UI shows the wizard starting
//! from [`default_selections`] (asking [`visible_steps`] and [`option_kinds`]
//! as choices change), then [`validate`], [`resolve`] and [`expand`] turn the
//! choices into concrete files. C# script installers are not supported.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use walkdir::WalkDir;

use crate::error::{Error, Result};

/// A parsed `ModuleConfig.xml`.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct FomodInstaller {
    pub module_name: String,
    /// Header image, relative to the content root.
    pub image: Option<String>,
    pub info: Option<FomodInfo>,
    pub steps: Vec<InstallStep>,
    #[serde(skip)]
    #[ts(skip)]
    required_files: Vec<FileInstall>,
    #[serde(skip)]
    #[ts(skip)]
    conditional_installs: Vec<(Condition, Vec<FileInstall>)>,
}

/// Metadata from `fomod/info.xml`.
#[derive(Debug, Clone, Default, Serialize, TS)]
#[ts(export)]
pub struct FomodInfo {
    pub name: Option<String>,
    pub author: Option<String>,
    pub version: Option<String>,
    pub website: Option<String>,
    pub description: Option<String>,
}

/// One page of the wizard.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct InstallStep {
    pub name: String,
    pub groups: Vec<OptionGroup>,
    #[serde(skip)]
    #[ts(skip)]
    visible: Option<Condition>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct OptionGroup {
    pub name: String,
    pub kind: GroupKind,
    pub options: Vec<FomodOption>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub enum GroupKind {
    SelectExactlyOne,
    SelectAtMostOne,
    SelectAtLeastOne,
    SelectAll,
    SelectAny,
}

/// One choice (a `<plugin>` in the XML).
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct FomodOption {
    pub name: String,
    pub description: String,
    /// Relative to the content root.
    pub image: Option<String>,
    /// The kind before any flags are set; see [`option_kinds`] for the live one.
    pub kind: OptionKind,
    #[serde(skip)]
    #[ts(skip)]
    kind_patterns: Vec<(Condition, OptionKind)>,
    #[serde(skip)]
    #[ts(skip)]
    files: Vec<FileInstall>,
    #[serde(skip)]
    #[ts(skip)]
    flags: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub enum OptionKind {
    Required,
    Recommended,
    Optional,
    NotUsable,
    CouldBeUsable,
}

/// Chosen option indices: `selections[step][group]` lists the options picked
/// in that group. Missing entries count as nothing picked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Selections(#[ts(type = "Array<Array<Array<number>>>")] pub Vec<Vec<Vec<usize>>>);

impl Selections {
    fn get(&self, step: usize, group: usize) -> &[usize] {
        self.0
            .get(step)
            .and_then(|s| s.get(group))
            .map_or(&[], Vec::as_slice)
    }
}

/// A file or folder to install. `source` is relative to the content root,
/// `destination` to the install root; both `/`-separated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FileMapping {
    pub source: String,
    pub destination: String,
    pub priority: i32,
    pub is_folder: bool,
}

#[derive(Debug, Clone)]
struct FileInstall {
    mapping: FileMapping,
    always_install: bool,
    install_if_usable: bool,
}

#[derive(Debug, Clone)]
enum Condition {
    Flag {
        name: String,
        value: String,
    },
    /// File, game and FOMM version checks; we can't evaluate them, so they pass.
    Always,
    All(Vec<Condition>),
    Any(Vec<Condition>),
}

type Flags = HashMap<String, String>;

impl Condition {
    fn matches(&self, flags: &Flags) -> bool {
        match self {
            Condition::Flag { name, value } => {
                flags.get(name).map_or("", String::as_str) == value.as_str()
            }
            Condition::Always => true,
            Condition::All(items) => items.iter().all(|c| c.matches(flags)),
            Condition::Any(items) => items.iter().any(|c| c.matches(flags)),
        }
    }
}

impl FomodOption {
    fn kind_with(&self, flags: &Flags) -> OptionKind {
        self.kind_patterns
            .iter()
            .find(|(c, _)| c.matches(flags))
            .map_or(self.kind, |(_, k)| *k)
    }
}

/// Finds `fomod/ModuleConfig.xml` (any case) in `root` or one folder below
/// it. Returns the installer and the content root (the folder holding
/// `fomod/`), or `None` when the mod has no FOMOD installer.
pub fn find_installer(root: &Path) -> Result<Option<(FomodInstaller, PathBuf)>> {
    let mut candidates = vec![root.to_path_buf()];
    candidates.extend(
        sorted_entries(root)
            .into_iter()
            .filter(|p| p.is_dir() && !name_is(p, "__macosx") && !name_is(p, "fomod")),
    );
    for dir in candidates {
        let Some(fomod_dir) = child_ci(&dir, "fomod").filter(|p| p.is_dir()) else {
            continue;
        };
        let Some(config) = child_ci(&fomod_dir, "moduleconfig.xml").filter(|p| p.is_file()) else {
            continue;
        };
        let bytes = fs::read(&config).map_err(|source| Error::Io {
            path: config.clone(),
            source,
        })?;
        let mut installer = parse_module_config(&bytes)
            .map_err(|e| Error::Install(format!("couldn't read the FOMOD installer: {e}")))?;
        if let Some(info) = child_ci(&fomod_dir, "info.xml")
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| parse_info(&b))
        {
            if installer.module_name.is_empty() {
                installer.module_name = info.name.clone().unwrap_or_default();
            }
            installer.info = Some(info);
        }
        fix_image_case(&mut installer, &dir);
        return Ok(Some((installer, dir)));
    }
    Ok(None)
}

/// Parses the bytes of a `ModuleConfig.xml` (UTF-8 or UTF-16, with or without BOM).
pub fn parse_module_config(bytes: &[u8]) -> std::result::Result<FomodInstaller, String> {
    let root = parse_xml(&decode(bytes))?;
    if !root.name.eq_ignore_ascii_case("config") {
        return Err(format!("expected <config>, found <{}>", root.name));
    }
    let steps = match root.child("installSteps") {
        Some(node) => ordered(node, node.children_named("installStep"))
            .into_iter()
            .map(parse_step)
            .collect(),
        None => Vec::new(),
    };
    let conditional_installs = root
        .child("conditionalFileInstalls")
        .and_then(|n| n.child("patterns"))
        .map(|patterns| {
            patterns
                .children_named("pattern")
                .map(|p| {
                    let condition = p
                        .child("dependencies")
                        .map_or(Condition::Always, parse_condition);
                    (condition, parse_files(p.child("files")))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(FomodInstaller {
        module_name: root
            .child("moduleName")
            .map(|n| n.text.trim().to_string())
            .unwrap_or_default(),
        image: root
            .child("moduleImage")
            .and_then(|n| n.attr("path"))
            .map(normalize_path)
            .filter(|p| !p.is_empty()),
        info: None,
        steps,
        required_files: parse_files(root.child("requiredInstallFiles")),
        conditional_installs,
    })
}

/// Default choices: Required and Recommended options; the first usable option
/// where a group needs one; everything in SelectAll groups.
pub fn default_selections(installer: &FomodInstaller) -> Selections {
    let mut selections = Selections::default();
    let mut flags = Flags::new();
    for step in &installer.steps {
        let mut chosen_step = Vec::new();
        for group in &step.groups {
            let kinds: Vec<OptionKind> =
                group.options.iter().map(|o| o.kind_with(&flags)).collect();
            let usable = |i: &usize| kinds[*i] != OptionKind::NotUsable;
            let mut chosen: Vec<usize> = if group.kind == GroupKind::SelectAll {
                (0..kinds.len()).collect()
            } else {
                (0..kinds.len())
                    .filter(|&i| matches!(kinds[i], OptionKind::Required | OptionKind::Recommended))
                    .collect()
            };
            match group.kind {
                GroupKind::SelectExactlyOne | GroupKind::SelectAtLeastOne if chosen.is_empty() => {
                    chosen.extend((0..kinds.len()).find(usable));
                }
                GroupKind::SelectExactlyOne | GroupKind::SelectAtMostOne => chosen.truncate(1),
                _ => {}
            }
            chosen_step.push(chosen);
        }
        if step.visible.as_ref().is_none_or(|c| c.matches(&flags)) {
            apply_flags(step, &chosen_step, &mut flags);
        }
        selections.0.push(chosen_step);
    }
    selections
}

/// Choices by name (step, group, options), so they can be matched against
/// another version of the installer. Only visible steps are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct FomodChoices(pub Vec<StepChoice>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct StepChoice {
    pub step: String,
    pub groups: Vec<GroupChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct GroupChoice {
    pub group: String,
    pub options: Vec<String>,
}

/// Earlier choices applied to an installer.
#[derive(Debug, Clone, PartialEq)]
pub struct Replay {
    /// The saved choices where they match, defaults elsewhere.
    pub selections: Selections,
    /// Every visible step and group matched by name with all its chosen
    /// options, and the result is a valid set of choices.
    pub complete: bool,
}

/// The selections as names.
pub fn name_choices(installer: &FomodInstaller, selections: &Selections) -> FomodChoices {
    let visible = visible_steps(installer, selections);
    FomodChoices(
        visible
            .into_iter()
            .map(|si| {
                let step = &installer.steps[si];
                StepChoice {
                    step: step.name.clone(),
                    groups: step
                        .groups
                        .iter()
                        .enumerate()
                        .map(|(gi, group)| GroupChoice {
                            group: group.name.clone(),
                            options: selections
                                .get(si, gi)
                                .iter()
                                .filter_map(|&oi| group.options.get(oi).map(|o| o.name.clone()))
                                .collect(),
                        })
                        .collect(),
                }
            })
            .collect(),
    )
}

/// Applies saved choices to an installer (possibly a newer version) by
/// matching step, group and option names, ignoring case and surrounding
/// spaces. Groups without a match keep their default.
pub fn replay(installer: &FomodInstaller, saved: &FomodChoices) -> Replay {
    let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    let mut selections = default_selections(installer);
    // Whether each group (`[step][group]`) matched with all its saved options.
    let mut matched: Vec<Vec<bool>> = Vec::new();
    for (si, step) in installer.steps.iter().enumerate() {
        let saved_step = saved.0.iter().find(|s| same(&s.step, &step.name));
        let mut step_matched = Vec::new();
        for (gi, group) in step.groups.iter().enumerate() {
            let saved_group =
                saved_step.and_then(|s| s.groups.iter().find(|g| same(&g.group, &group.name)));
            let Some(saved_group) = saved_group else {
                step_matched.push(false);
                continue;
            };
            let chosen: Vec<usize> = saved_group
                .options
                .iter()
                .filter_map(|name| group.options.iter().position(|o| same(&o.name, name)))
                .collect();
            step_matched.push(chosen.len() == saved_group.options.len());
            selections.0[si][gi] = chosen;
        }
        matched.push(step_matched);
    }
    // Steps hidden with these choices needn't have matched.
    let complete = visible_steps(installer, &selections)
        .into_iter()
        .all(|si| matched[si].iter().all(|m| *m))
        && validate(installer, &selections).is_ok();
    Replay {
        selections,
        complete,
    }
}

/// Indices of the steps shown for these selections. A step's visibility
/// depends on flags set by options chosen in earlier visible steps.
pub fn visible_steps(installer: &FomodInstaller, selections: &Selections) -> Vec<usize> {
    let eval = evaluate(installer, selections);
    (0..installer.steps.len())
        .filter(|&i| eval.visible[i])
        .collect()
}

/// The kind of every option (`[step][group][option]`) for these selections;
/// options with a dependency type change kind as flags change.
pub fn option_kinds(
    installer: &FomodInstaller,
    selections: &Selections,
) -> Vec<Vec<Vec<OptionKind>>> {
    evaluate(installer, selections).kinds
}

/// Checks the selections against each visible step's group rules.
pub fn validate(
    installer: &FomodInstaller,
    selections: &Selections,
) -> std::result::Result<(), String> {
    let eval = evaluate(installer, selections);
    for (si, step) in installer.steps.iter().enumerate() {
        if !eval.visible[si] {
            continue;
        }
        for (gi, group) in step.groups.iter().enumerate() {
            let chosen = selections.get(si, gi);
            let kinds = &eval.kinds[si][gi];
            let label = format!("\"{}\" in \"{}\"", group.name, step.name);
            for (n, &i) in chosen.iter().enumerate() {
                let Some(option) = group.options.get(i) else {
                    return Err(format!("{label}: there is no option {i}"));
                };
                if chosen[..n].contains(&i) {
                    return Err(format!("{label}: \"{}\" is chosen twice", option.name));
                }
                if kinds[i] == OptionKind::NotUsable {
                    return Err(format!("{label}: \"{}\" can't be chosen", option.name));
                }
            }
            let any_usable = kinds.iter().any(|k| *k != OptionKind::NotUsable);
            let count = chosen.len();
            let problem = match group.kind {
                GroupKind::SelectExactlyOne if count != 1 && any_usable => {
                    Some("choose exactly one option")
                }
                GroupKind::SelectAtMostOne if count > 1 => Some("choose at most one option"),
                GroupKind::SelectAtLeastOne if count == 0 && any_usable => {
                    Some("choose at least one option")
                }
                GroupKind::SelectAll
                    if count
                        != kinds
                            .iter()
                            .filter(|k| **k != OptionKind::NotUsable)
                            .count() =>
                {
                    Some("all options must be chosen")
                }
                _ => None,
            };
            if let Some(problem) = problem {
                return Err(format!("{label}: {problem}"));
            }
        }
    }
    Ok(())
}

/// Everything to install for these selections: required files, files of the
/// options chosen in visible steps (plus `alwaysInstall`/`installIfUsable`
/// ones), and conditional installs matching the final flags. Sorted by
/// priority, lowest first, so later mappings overwrite earlier ones.
pub fn resolve(installer: &FomodInstaller, selections: &Selections) -> Vec<FileMapping> {
    let eval = evaluate(installer, selections);
    let mut out: Vec<FileMapping> = installer
        .required_files
        .iter()
        .map(|f| f.mapping.clone())
        .collect();
    for (si, step) in installer.steps.iter().enumerate() {
        if !eval.visible[si] {
            continue;
        }
        for (gi, group) in step.groups.iter().enumerate() {
            let chosen = selections.get(si, gi);
            for (oi, option) in group.options.iter().enumerate() {
                let selected = chosen.contains(&oi);
                let usable = eval.kinds[si][gi][oi] != OptionKind::NotUsable;
                out.extend(
                    option
                        .files
                        .iter()
                        .filter(|f| selected || f.always_install || (f.install_if_usable && usable))
                        .map(|f| f.mapping.clone()),
                );
            }
        }
    }
    for (condition, files) in &installer.conditional_installs {
        if condition.matches(&eval.flags) {
            out.extend(files.iter().map(|f| f.mapping.clone()));
        }
    }
    out.sort_by_key(|m| m.priority);
    out
}

/// Turns mappings into `(source, target)` file pairs: `source` is the file's
/// real path under `content_root`, `target` is relative to the install root.
/// Sources match case-insensitively; a later mapping wins a target over an
/// earlier one (compared case-insensitively). Missing sources are skipped.
pub fn expand(content_root: &Path, mappings: &[FileMapping]) -> Vec<(String, String)> {
    let files: Vec<(String, String)> = WalkDir::new(content_root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let rel = e.path().strip_prefix(content_root).ok()?;
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let path = parts.join("/");
            Some((path.to_lowercase(), path))
        })
        .collect();

    let mut by_target: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut add = |source: &str, target: String| {
        if target.is_empty() || target.split('/').any(|c| c == "..") {
            return;
        }
        by_target.insert(target.to_lowercase(), (source.to_string(), target));
    };
    for mapping in mappings {
        let source = mapping.source.to_lowercase();
        if mapping.is_folder {
            let depth = if source.is_empty() {
                0
            } else {
                source.split('/').count()
            };
            for (lower, actual) in &files {
                let inside = if source.is_empty() {
                    !lower.starts_with("fomod/")
                } else {
                    lower
                        .strip_prefix(source.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
                };
                if inside {
                    let rest: Vec<&str> = actual.split('/').skip(depth).collect();
                    add(actual, join(&mapping.destination, &rest.join("/")));
                }
            }
        } else if let Some((_, actual)) = files.iter().find(|(lower, _)| *lower == source) {
            add(actual, mapping.destination.clone());
        }
    }
    by_target.into_values().collect()
}

struct Evaluation {
    visible: Vec<bool>,
    kinds: Vec<Vec<Vec<OptionKind>>>,
    /// Flags after the last step.
    flags: Flags,
}

fn evaluate(installer: &FomodInstaller, selections: &Selections) -> Evaluation {
    let mut flags = Flags::new();
    let mut visible = Vec::new();
    let mut kinds = Vec::new();
    for (si, step) in installer.steps.iter().enumerate() {
        let shown = step.visible.as_ref().is_none_or(|c| c.matches(&flags));
        kinds.push(
            step.groups
                .iter()
                .map(|g| g.options.iter().map(|o| o.kind_with(&flags)).collect())
                .collect(),
        );
        if shown {
            let chosen: Vec<Vec<usize>> = (0..step.groups.len())
                .map(|gi| selections.get(si, gi).to_vec())
                .collect();
            apply_flags(step, &chosen, &mut flags);
        }
        visible.push(shown);
    }
    Evaluation {
        visible,
        kinds,
        flags,
    }
}

fn apply_flags(step: &InstallStep, chosen: &[Vec<usize>], flags: &mut Flags) {
    for (group, chosen) in step.groups.iter().zip(chosen) {
        for option in chosen.iter().filter_map(|&i| group.options.get(i)) {
            for (name, value) in &option.flags {
                flags.insert(name.clone(), value.clone());
            }
        }
    }
}

fn parse_step(node: &Node) -> InstallStep {
    let groups = match node.child("optionalFileGroups") {
        Some(groups) => ordered(groups, groups.children_named("group"))
            .into_iter()
            .map(parse_group)
            .collect(),
        None => Vec::new(),
    };
    InstallStep {
        name: node.attr("name").unwrap_or_default().to_string(),
        groups,
        visible: node.child("visible").map(parse_condition),
    }
}

fn parse_group(node: &Node) -> OptionGroup {
    let kind = match node.attr("type").unwrap_or_default() {
        "SelectExactlyOne" => GroupKind::SelectExactlyOne,
        "SelectAtMostOne" => GroupKind::SelectAtMostOne,
        "SelectAtLeastOne" => GroupKind::SelectAtLeastOne,
        "SelectAll" => GroupKind::SelectAll,
        _ => GroupKind::SelectAny,
    };
    let options = match node.child("plugins") {
        Some(plugins) => ordered(plugins, plugins.children_named("plugin"))
            .into_iter()
            .map(parse_option)
            .collect(),
        None => Vec::new(),
    };
    OptionGroup {
        name: node.attr("name").unwrap_or_default().to_string(),
        kind,
        options,
    }
}

fn parse_option(node: &Node) -> FomodOption {
    let descriptor = node.child("typeDescriptor");
    let (kind, kind_patterns) = match descriptor.and_then(|d| d.child("dependencyType")) {
        Some(dep) => {
            let default = dep
                .child("defaultType")
                .map_or(OptionKind::Optional, parse_kind);
            let patterns = dep
                .child("patterns")
                .map(|ps| {
                    ps.children_named("pattern")
                        .map(|p| {
                            let condition = p
                                .child("dependencies")
                                .map_or(Condition::Always, parse_condition);
                            let kind = p.child("type").map_or(default, parse_kind);
                            (condition, kind)
                        })
                        .collect()
                })
                .unwrap_or_default();
            (default, patterns)
        }
        None => (
            descriptor
                .and_then(|d| d.child("type"))
                .map_or(OptionKind::Optional, parse_kind),
            Vec::new(),
        ),
    };
    let flags = node
        .child("conditionFlags")
        .map(|f| {
            f.children_named("flag")
                .map(|flag| {
                    (
                        flag.attr("name").unwrap_or_default().to_string(),
                        flag.text.trim().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    FomodOption {
        name: node.attr("name").unwrap_or_default().to_string(),
        description: node
            .child("description")
            .map(|d| d.text.trim().to_string())
            .unwrap_or_default(),
        image: node
            .child("image")
            .and_then(|i| i.attr("path"))
            .map(normalize_path)
            .filter(|p| !p.is_empty()),
        kind,
        kind_patterns,
        files: parse_files(node.child("files")),
        flags,
    }
}

fn parse_kind(node: &Node) -> OptionKind {
    match node.attr("name").unwrap_or_default() {
        "Required" => OptionKind::Required,
        "Recommended" => OptionKind::Recommended,
        "NotUsable" => OptionKind::NotUsable,
        "CouldBeUsable" => OptionKind::CouldBeUsable,
        _ => OptionKind::Optional,
    }
}

/// A `compositeDependency` element (`<dependencies>`, `<visible>`).
fn parse_condition(node: &Node) -> Condition {
    let items = node
        .children
        .iter()
        .filter_map(|child| match child.name.as_str() {
            "flagDependency" => Some(Condition::Flag {
                name: child.attr("flag").unwrap_or_default().to_string(),
                value: child.attr("value").unwrap_or_default().to_string(),
            }),
            "dependencies" => Some(parse_condition(child)),
            n if n.ends_with("Dependency") => Some(Condition::Always),
            _ => None,
        })
        .collect();
    if node.attr("operator") == Some("Or") {
        Condition::Any(items)
    } else {
        Condition::All(items)
    }
}

fn parse_files(node: Option<&Node>) -> Vec<FileInstall> {
    let Some(node) = node else {
        return Vec::new();
    };
    node.children
        .iter()
        .filter(|c| c.name == "file" || c.name == "folder")
        .map(|c| {
            let is_folder = c.name == "folder";
            let source = normalize_path(c.attr("source").unwrap_or_default());
            let raw_destination = c.attr("destination").unwrap_or_default();
            let mut destination = normalize_path(raw_destination);
            if !is_folder && (destination.is_empty() || raw_destination.ends_with(['/', '\\'])) {
                let name = source.rsplit('/').next().unwrap_or_default();
                destination = join(&destination, name);
            }
            let flag = |name: &str| c.attr(name).is_some_and(|v| v == "true" || v == "1");
            FileInstall {
                mapping: FileMapping {
                    source,
                    destination,
                    priority: c
                        .attr("priority")
                        .and_then(|p| p.trim().parse().ok())
                        .unwrap_or(0),
                    is_folder,
                },
                always_install: flag("alwaysInstall"),
                install_if_usable: flag("installIfUsable"),
            }
        })
        .collect()
}

/// Applies the element's `order` attribute (default `Ascending`) to its items.
fn ordered<'a>(parent: &Node, items: impl Iterator<Item = &'a Node>) -> Vec<&'a Node> {
    let mut items: Vec<&Node> = items.collect();
    let key = |n: &&Node| n.attr("name").unwrap_or_default().to_lowercase();
    match parent.attr("order").unwrap_or("Ascending") {
        "Explicit" => {}
        "Descending" => {
            items.sort_by_key(key);
            items.reverse();
        }
        _ => items.sort_by_key(key),
    }
    items
}

fn parse_info(bytes: &[u8]) -> Option<FomodInfo> {
    let root = parse_xml(&decode(bytes)).ok()?;
    let field = |name: &str| {
        root.child(name)
            .map(|n| n.text.trim().to_string())
            .filter(|t| !t.is_empty())
    };
    Some(FomodInfo {
        name: field("Name"),
        author: field("Author"),
        version: field("Version"),
        website: field("Website"),
        description: field("Description"),
    })
}

/// Rewrites image paths to their real case on disk so the UI can load them.
fn fix_image_case(installer: &mut FomodInstaller, content_root: &Path) {
    let fix = |image: &mut Option<String>| {
        if let Some(path) = image
            && let Some(real) = real_path(content_root, path)
        {
            *path = real;
        }
    };
    fix(&mut installer.image);
    for option in installer
        .steps
        .iter_mut()
        .flat_map(|s| &mut s.groups)
        .flat_map(|g| &mut g.options)
    {
        fix(&mut option.image);
    }
}

/// `relative` resolved component by component, ignoring case.
fn real_path(root: &Path, relative: &str) -> Option<String> {
    let mut dir = root.to_path_buf();
    let mut parts = Vec::new();
    for part in relative.split('/') {
        dir = child_ci(&dir, part)?;
        parts.push(dir.file_name()?.to_string_lossy().into_owned());
    }
    Some(parts.join("/"))
}

/// The entry of `dir` named `name`, ignoring case, with its real name (an
/// exact match first). Listing rather than `exists()` because case-insensitive
/// file systems would hand back the name we asked for.
fn child_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = sorted_entries(dir);
    let exact = entries
        .iter()
        .position(|p| p.file_name().is_some_and(|n| n == name));
    let index = exact.or_else(|| entries.iter().position(|p| name_is(p, name)))?;
    entries.into_iter().nth(index)
}

fn sorted_entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    entries.sort();
    entries
}

fn name_is(path: &Path, name: &str) -> bool {
    path.file_name()
        .is_some_and(|n| n.to_string_lossy().to_lowercase() == name.to_lowercase())
}

/// `\` to `/`, no leading `/`, no empty or `.` components.
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
        .split('/')
        .map(str::trim)
        .filter(|c| !c.is_empty() && *c != ".")
        .collect::<Vec<_>>()
        .join("/")
}

fn join(dir: &str, rest: &str) -> String {
    match (dir.is_empty(), rest.is_empty()) {
        (true, _) => rest.to_string(),
        (_, true) => dir.to_string(),
        _ => format!("{dir}/{rest}"),
    }
}

/// Bytes to text, honoring UTF-8/UTF-16 BOMs (and BOM-less UTF-16).
fn decode(bytes: &[u8]) -> String {
    let utf16 = |rest: &[u8], le: bool| {
        let (pairs, _) = rest.as_chunks::<2>();
        let units: Vec<u16> = pairs
            .iter()
            .map(|&pair| {
                if le {
                    u16::from_le_bytes(pair)
                } else {
                    u16::from_be_bytes(pair)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, true),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, false),
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [b'<', 0, ..] => utf16(bytes, true),
        [0, b'<', ..] => utf16(bytes, false),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// Minimal element tree; names are local (no namespace prefix).
#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    text: String,
}

impl Node {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn child(&self, name: &str) -> Option<&Node> {
        self.children
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children
            .iter()
            .filter(move |c| c.name.eq_ignore_ascii_case(name))
    }
}

fn parse_xml(text: &str) -> std::result::Result<Node, String> {
    let mut reader = Reader::from_str(text);
    let mut stack = vec![Node::default()];
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("{e} (at byte {})", reader.error_position()))?;
        match event {
            Event::Start(e) => stack.push(element(&e)?),
            Event::Empty(e) => {
                let node = element(&e)?;
                stack.last_mut().expect("document node").children.push(node);
            }
            Event::End(_) => {
                if stack.len() < 2 {
                    return Err("unexpected closing tag".into());
                }
                let node = stack.pop().expect("checked above");
                stack.last_mut().expect("checked above").children.push(node);
            }
            Event::Text(t) => stack
                .last_mut()
                .expect("document node")
                .text
                .push_str(&t.xml10_content()),
            Event::CData(t) => stack
                .last_mut()
                .expect("document node")
                .text
                .push_str(&t.into_inner()),
            Event::GeneralRef(r) => {
                let resolved = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => quick_xml::escape::resolve_predefined_entity(&r)
                        .unwrap_or_default()
                        .to_string(),
                };
                stack
                    .last_mut()
                    .expect("document node")
                    .text
                    .push_str(&resolved);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err("the XML ends before all elements are closed".into());
    }
    stack
        .pop()
        .and_then(|doc| doc.children.into_iter().next())
        .ok_or_else(|| "the XML is empty".into())
}

fn element(start: &BytesStart) -> std::result::Result<Node, String> {
    let mut attrs = Vec::new();
    for attr in start.attributes().with_checks(false) {
        let attr = attr.map_err(|e| e.to_string())?;
        let value = attr
            .normalized_value(XmlVersion::Implicit1_0)
            .map_err(|e| e.to_string())?;
        attrs.push((
            attr.key.local_name().as_ref().to_string(),
            value.into_owned(),
        ));
    }
    Ok(Node {
        name: start.local_name().as_ref().to_string(),
        attrs,
        ..Node::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_STEPS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<config xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <moduleName>Two Steps</moduleName>
  <moduleImage path="fomod\header.png"/>
  <requiredInstallFiles>
    <folder source="Core" destination="" />
    <file source="docs\readme.txt" />
  </requiredInstallFiles>
  <installSteps order="Explicit">
    <installStep name="Main">
      <optionalFileGroups order="Explicit">
        <group name="Style" type="SelectExactlyOne">
          <plugins order="Explicit">
            <plugin name="Light">
              <description>Light &amp; airy</description>
              <image path="Images/Light.png"/>
              <files><folder source="Light" destination="archive/pc/mod" priority="1"/></files>
              <typeDescriptor><type name="Optional"/></typeDescriptor>
            </plugin>
            <plugin name="Dark">
              <description>Dark</description>
              <conditionFlags><flag name="dark">On</flag></conditionFlags>
              <files><folder source="Dark" destination="archive/pc/mod" priority="1"/></files>
              <typeDescriptor><type name="Optional"/></typeDescriptor>
            </plugin>
            <plugin name="Broken">
              <description/>
              <typeDescriptor><type name="NotUsable"/></typeDescriptor>
            </plugin>
          </plugins>
        </group>
        <group name="Extras" type="SelectAny">
          <plugins order="Explicit">
            <plugin name="Extra">
              <description/>
              <files><file source="extra/x.archive" destination="archive/pc/mod/"/></files>
              <typeDescriptor>
                <dependencyType>
                  <defaultType name="Optional"/>
                  <patterns>
                    <pattern>
                      <dependencies operator="And"><flagDependency flag="dark" value="On"/></dependencies>
                      <type name="Recommended"/>
                    </pattern>
                  </patterns>
                </dependencyType>
              </typeDescriptor>
            </plugin>
          </plugins>
        </group>
      </optionalFileGroups>
    </installStep>
    <installStep name="Dark Options">
      <visible><flagDependency flag="dark" value="On"/></visible>
      <optionalFileGroups>
        <group name="Shade" type="SelectExactlyOne">
          <plugins>
            <plugin name="Pitch">
              <description/>
              <conditionFlags><flag name="pitch">On</flag></conditionFlags>
              <typeDescriptor><type name="Optional"/></typeDescriptor>
            </plugin>
            <plugin name="Dim">
              <description/>
              <typeDescriptor><type name="Recommended"/></typeDescriptor>
            </plugin>
          </plugins>
        </group>
      </optionalFileGroups>
    </installStep>
  </installSteps>
  <conditionalFileInstalls>
    <patterns>
      <pattern>
        <dependencies operator="Or">
          <flagDependency flag="pitch" value="On"/>
          <dependencies operator="And">
            <flagDependency flag="dark" value="On"/>
            <fileDependency file="x.esp" state="Active"/>
          </dependencies>
        </dependencies>
        <files><file source="patch/p.archive" destination="archive/pc/mod/p.archive" priority="5"/></files>
      </pattern>
      <pattern>
        <dependencies><flagDependency flag="pitch" value="On"/></dependencies>
        <files><file source="patch/pitch.archive" destination="archive/pc/mod/pitch.archive"/></files>
      </pattern>
    </patterns>
  </conditionalFileInstalls>
</config>"#;

    fn installer() -> FomodInstaller {
        parse_module_config(TWO_STEPS.as_bytes()).unwrap()
    }

    fn sel(v: Vec<Vec<Vec<usize>>>) -> Selections {
        Selections(v)
    }

    fn write(root: &Path, files: &[&str]) {
        for f in files {
            let path = root.join(f);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"x").unwrap();
        }
    }

    #[test]
    fn parses_utf16_with_bom() {
        let xml = TWO_STEPS.replace("UTF-8", "UTF-16");
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
        let i = parse_module_config(&bytes).unwrap();
        assert_eq!(i.module_name, "Two Steps");
        assert_eq!(i.image.as_deref(), Some("fomod/header.png"));
        assert_eq!(i.steps.len(), 2);
        let style = &i.steps[0].groups[0];
        assert_eq!(style.kind, GroupKind::SelectExactlyOne);
        assert_eq!(style.options[0].description, "Light & airy");
        assert_eq!(style.options[0].image.as_deref(), Some("Images/Light.png"));
        assert_eq!(style.options[2].kind, OptionKind::NotUsable);

        let mut be = vec![0xFE, 0xFF];
        be.extend(xml.encode_utf16().flat_map(u16::to_be_bytes));
        assert_eq!(parse_module_config(&be).unwrap().steps.len(), 2);
    }

    #[test]
    fn steps_and_plugins_sort_ascending_by_default() {
        let i = installer();
        // Step 2 has no order attribute, so its plugins sort by name.
        let names: Vec<&str> = i.steps[1].groups[0]
            .options
            .iter()
            .map(|o| o.name.as_str())
            .collect();
        assert_eq!(names, ["Dim", "Pitch"]);
    }

    #[test]
    fn flags_control_step_visibility_and_option_kinds() {
        let i = installer();
        let light = sel(vec![vec![vec![0], vec![]], vec![vec![0]]]);
        assert_eq!(visible_steps(&i, &light), [0]);
        assert_eq!(
            option_kinds(&i, &light)[1][0],
            [OptionKind::Recommended, OptionKind::Optional]
        );

        let dark = sel(vec![vec![vec![1], vec![]], vec![vec![0]]]);
        assert_eq!(visible_steps(&i, &dark), [0, 1]);
        // Flags only affect later steps, so step 0's Extra stays Optional.
        assert_eq!(option_kinds(&i, &dark)[0][1], [OptionKind::Optional]);
    }

    #[test]
    fn defaults_pick_first_usable_and_recommended() {
        let i = installer();
        let d = default_selections(&i);
        // Light is first usable; Dark is unset so step 2 is hidden but still filled.
        assert_eq!(d, sel(vec![vec![vec![0], vec![]], vec![vec![0]]]));
        assert!(validate(&i, &d).is_ok());
    }

    #[test]
    fn choices_replay_by_name() {
        let i = installer();
        let dark = sel(vec![vec![vec![1], vec![]], vec![vec![1]]]);
        let named = name_choices(&i, &dark);
        assert_eq!(named.0.len(), 2, "both steps are visible");
        let again = replay(&i, &named);
        assert!(again.complete);
        assert_eq!(again.selections, dark);

        // A newer version renamed an option: what matches is preselected.
        let renamed =
            parse_module_config(TWO_STEPS.replace("Pitch", "Pitch Black").as_bytes()).unwrap();
        let partial = replay(&renamed, &named);
        assert!(!partial.complete);
        assert_eq!(
            partial.selections.0[0][0],
            [1],
            "the first step still matches"
        );

        // Light hides step 2, which then needn't match anything.
        let light = sel(vec![vec![vec![0], vec![]], vec![vec![0]]]);
        let named = name_choices(&i, &light);
        assert_eq!(named.0.len(), 1);
        assert!(replay(&i, &named).complete);
    }

    #[test]
    fn validation_errors() {
        let i = installer();
        let err = validate(&i, &sel(vec![vec![vec![], vec![]]])).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        let err = validate(&i, &sel(vec![vec![vec![0, 1], vec![]]])).unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
        let err = validate(&i, &sel(vec![vec![vec![2], vec![]]])).unwrap_err();
        assert!(err.contains("can't be chosen"), "{err}");
        let err = validate(&i, &sel(vec![vec![vec![7], vec![]]])).unwrap_err();
        assert!(err.contains("no option 7"), "{err}");
        // Hidden step 2 isn't checked.
        assert!(validate(&i, &sel(vec![vec![vec![0], vec![]]])).is_ok());
        // Visible step 2 is.
        let err = validate(&i, &sel(vec![vec![vec![1], vec![]]])).unwrap_err();
        assert!(err.contains("Dark Options"), "{err}");
    }

    #[test]
    fn resolve_combines_required_options_and_conditionals() {
        let i = installer();
        let light = resolve(&i, &sel(vec![vec![vec![0], vec![0]], vec![vec![1]]]));
        let dests: Vec<(&str, &str)> = light
            .iter()
            .map(|m| (m.source.as_str(), m.destination.as_str()))
            .collect();
        assert_eq!(
            dests,
            [
                ("Core", ""),
                ("docs/readme.txt", "readme.txt"),
                ("extra/x.archive", "archive/pc/mod/x.archive"),
                ("Light", "archive/pc/mod"),
            ]
        );

        // Dark + Pitch (index 1 after sorting): both conditional patterns match.
        let dark = resolve(&i, &sel(vec![vec![vec![1], vec![]], vec![vec![1]]]));
        let sources: Vec<&str> = dark.iter().map(|m| m.source.as_str()).collect();
        assert_eq!(
            sources,
            [
                "Core",
                "docs/readme.txt",
                "patch/pitch.archive",
                "Dark",
                "patch/p.archive"
            ]
        );
        // Dark + Dim: the nested And (flag + lenient fileDependency) matches.
        let dim = resolve(&i, &sel(vec![vec![vec![1], vec![]], vec![vec![0]]]));
        assert!(dim.iter().any(|m| m.source == "patch/p.archive"));
        assert!(!dim.iter().any(|m| m.source == "patch/pitch.archive"));
        // Hidden step selections don't set flags.
        let hidden = resolve(&i, &sel(vec![vec![vec![0], vec![]], vec![vec![1]]]));
        assert!(!hidden.iter().any(|m| m.source.starts_with("patch/")));
    }

    #[test]
    fn expand_matches_case_and_later_mappings_win() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            &[
                "core/A.archive",
                "core/sub/B.txt",
                "DOCS/ReadMe.txt",
                "light/A.archive",
                "fomod/ModuleConfig.xml",
            ],
        );
        let i = installer();
        let mappings = resolve(&i, &sel(vec![vec![vec![0], vec![]]]));
        let files = expand(dir.path(), &mappings);
        assert_eq!(
            files,
            [
                ("core/A.archive".to_string(), "A.archive".to_string()),
                ("light/A.archive".into(), "archive/pc/mod/A.archive".into()),
                ("DOCS/ReadMe.txt".into(), "readme.txt".into()),
                ("core/sub/B.txt".into(), "sub/B.txt".into()),
            ]
        );

        // Same target, case differs: the later mapping wins.
        let over = expand(
            dir.path(),
            &[
                FileMapping {
                    source: "Core".into(),
                    destination: "out".into(),
                    priority: 0,
                    is_folder: true,
                },
                FileMapping {
                    source: "LIGHT/a.archive".into(),
                    destination: "OUT/a.archive".into(),
                    priority: 1,
                    is_folder: false,
                },
            ],
        );
        assert_eq!(
            over,
            [
                ("light/A.archive".to_string(), "OUT/a.archive".to_string()),
                ("core/sub/B.txt".into(), "out/sub/B.txt".into()),
            ]
        );
    }

    #[test]
    fn empty_destinations() {
        let xml = r#"<config><moduleName>x</moduleName><requiredInstallFiles>
            <file source="\deep\path\a.dll" destination=""/>
            <file source="b.dll"/>
            <folder source="data\"/>
            <folder source="" destination="all"/>
        </requiredInstallFiles></config>"#;
        let i = parse_module_config(xml.as_bytes()).unwrap();
        let m = resolve(&i, &Selections::default());
        let pairs: Vec<(&str, &str)> = m
            .iter()
            .map(|m| (m.source.as_str(), m.destination.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("deep/path/a.dll", "a.dll"),
                ("b.dll", "b.dll"),
                ("data", ""),
                ("", "all")
            ]
        );

        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &["Data/x.esp", "fomod/info.xml"]);
        let files = expand(dir.path(), &m[2..]);
        assert_eq!(
            files,
            [
                ("Data/x.esp".to_string(), "all/Data/x.esp".to_string()),
                ("Data/x.esp".into(), "x.esp".into()),
            ]
        );
    }

    #[test]
    fn finds_installer_in_wrapper_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(find_installer(dir.path()).unwrap().is_none());
        write(
            dir.path(),
            &["My Mod/FOMOD/moduleconfig.XML", "My Mod/images/light.PNG"],
        );
        fs::write(
            dir.path().join("My Mod/FOMOD/moduleconfig.XML"),
            [&[0xEF, 0xBB, 0xBF][..], TWO_STEPS.as_bytes()].concat(),
        )
        .unwrap();
        fs::write(
            dir.path().join("My Mod/FOMOD/Info.xml"),
            "<fomod><Name>Info Name</Name><Author>Someone</Author></fomod>",
        )
        .unwrap();
        let (i, content_root) = find_installer(dir.path()).unwrap().unwrap();
        assert_eq!(content_root, dir.path().join("My Mod"));
        assert_eq!(i.module_name, "Two Steps");
        assert_eq!(i.info.unwrap().author.as_deref(), Some("Someone"));
        // Image path fixed to the real case on disk.
        assert_eq!(
            i.steps[0].groups[0].options[0].image.as_deref(),
            Some("images/light.PNG")
        );
    }
}
