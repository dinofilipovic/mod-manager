//! Data-driven game definitions. Each supported game is a TOML file in the
//! repository's `games/` folder; built-in ones are compiled into the binary.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{Error, Result};

const BUILTIN_GAMES: &[&str] = &[
    include_str!("../../../games/cyberpunk2077.toml"),
    include_str!("../../../games/witcher3.toml"),
    include_str!("../../../games/stardewvalley.toml"),
    include_str!("../../../games/bannerlord.toml"),
];

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct GameDefinition {
    pub id: String,
    pub name: String,
    /// Path of the main executable relative to the install folder.
    pub executable: String,
    #[serde(default)]
    pub stores: StoreIds,
    /// Windows paths to check when no store metadata points at the game.
    #[serde(default)]
    pub known_install_paths: Vec<String>,
    #[serde(default)]
    pub layout: Vec<LayoutTarget>,
    /// Folder names the game expects in lowercase.
    #[serde(default)]
    pub canonical_dirs: Vec<String>,
    /// Top-level game folders; an archive containing one is laid out from the game root.
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub wine: WineRequirements,
    #[serde(default)]
    pub launch: LaunchOptions,
    #[serde(default)]
    pub frameworks: Vec<Framework>,
    #[serde(default)]
    pub requirements: Vec<Requirement>,
    #[serde(default)]
    pub logs: Vec<LogSource>,
    pub load_order: Option<LoadOrderFile>,
    /// Windows path of the save folder; `*` matches one folder name (the user).
    pub saves: Option<String>,
}

/// A framework other mods depend on, such as a script extender.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct Framework {
    pub id: String,
    pub name: String,
    pub nexus_mod_id: Option<u32>,
    /// Game-relative files that are present when the framework is installed.
    pub provides: Vec<String>,
    /// Frameworks this one needs.
    #[serde(default)]
    pub requires: Vec<String>,
    /// The oldest version that works with each game version. Only values
    /// with a cited source (release notes, code) belong here.
    #[serde(default)]
    pub min_versions: Vec<MinVersion>,
}

/// From game version `game` on, the framework needs at least `version`.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct MinVersion {
    pub game: String,
    pub version: String,
}

impl Framework {
    /// The minimum version for this game version: the entry for the newest
    /// game version not after it.
    pub fn min_version_for(&self, game_version: &str) -> Option<&str> {
        use std::cmp::Ordering;
        self.min_versions
            .iter()
            .filter(|m| compare_game_versions(&m.game, game_version) != Ordering::Greater)
            .max_by(|a, b| compare_game_versions(&a.game, &b.game))
            .map(|m| m.version.as_str())
    }
}

/// Compares game versions as decimal numbers, the way game patches are
/// numbered: 2.3 < 2.31 < 2.5 < 2.51 (unlike mod versions, where 2.31 > 2.5).
pub fn compare_game_versions(a: &str, b: &str) -> std::cmp::Ordering {
    fn split(v: &str) -> (u64, String) {
        let v = v.trim().trim_start_matches(['v', 'V']);
        let (major, rest) = v.split_once('.').unwrap_or((v, ""));
        let fraction: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (major.parse().unwrap_or(0), fraction)
    }
    let ((major_a, mut frac_a), (major_b, mut frac_b)) = (split(a), split(b));
    let len = frac_a.len().max(frac_b.len());
    for f in [&mut frac_a, &mut frac_b] {
        while f.len() < len {
            f.push('0');
        }
    }
    major_a.cmp(&major_b).then(frac_a.cmp(&frac_b))
}

/// Mod files that need a framework: matched by extension or by folder.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub framework: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub paths: Vec<String>,
}

impl Requirement {
    pub fn matches(&self, target: &str) -> bool {
        let lower = target.to_ascii_lowercase();
        let ext_match = lower
            .rsplit_once('.')
            .is_some_and(|(_, ext)| self.extensions.iter().any(|e| e.eq_ignore_ascii_case(ext)));
        ext_match
            || self.paths.iter().any(|p| {
                let p = p.trim_end_matches('/').to_ascii_lowercase();
                lower.starts_with(&format!("{p}/"))
            })
    }
}

/// A log file the game or a framework writes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct LogSource {
    pub name: String,
    /// Game-relative path; `*` matches within one path component.
    pub path: String,
    pub framework: Option<String>,
    /// How the log confirms that mods loaded, if it does.
    pub status: Option<LogStatus>,
}

/// How a log tells that a mod of one kind loaded. Patterns contain `{name}`
/// (the mod's folder under its layout path, compared without regard to
/// case); a leading `^` anchors a pattern at the start of a line.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct LogStatus {
    /// The layout kind whose mods this log covers.
    pub kind: String,
    pub loaded: String,
    #[serde(default)]
    pub failed: Option<String>,
    /// Text the log must also contain for `loaded` to count (a run that
    /// finished).
    #[serde(default)]
    pub needs: Option<String>,
    /// For loaders that log one block per mod: the text starting a block,
    /// on a line that also has the mod's path. `loaded` is then looked for
    /// in the mod's block, and a block that ends without it means the mod
    /// didn't load.
    #[serde(default)]
    pub block: Option<String>,
}

/// A file through which the game takes an explicit archive load order.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct LoadOrderFile {
    pub file: String,
    pub extensions: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct StoreIds {
    pub steam_app_id: Option<String>,
    pub gog_id: Option<String>,
    /// Game domain on Nexus Mods, as used in its URLs and API.
    pub nexus_domain: Option<String>,
}

/// A folder inside the game where one kind of mod file is deployed.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct LayoutTarget {
    pub kind: String,
    pub path: String,
    /// File extensions that belong here when an archive has no game-root layout.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// A file whose folder is one mod of this kind (e.g. `init.lua` for CET).
    #[serde(default)]
    pub marker_file: Option<String>,
    /// A folder name whose parent folder is one mod of this kind (e.g.
    /// `content` for The Witcher 3's `modFoo/content/...`).
    #[serde(default)]
    pub marker_folder: Option<String>,
    /// Put matching files directly in `path`, dropping their folders.
    #[serde(default)]
    pub flatten: bool,
    /// Put matching files in `path/<mod name>/`.
    #[serde(default)]
    pub per_mod_folder: bool,
    #[serde(default)]
    pub description: String,
    /// Built-in checks run on files installed here (see `validate.rs`).
    #[serde(default)]
    pub checks: Vec<LayoutCheck>,
}

/// A built-in checker and the files it applies to.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct LayoutCheck {
    /// `archive_header`, `json`, `yaml`, `lua_entry` or `pe64_dll`.
    pub checker: String,
    /// File extensions it looks at; `lua_entry` checks mod folders and needs none.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// File names it looks at directly inside a mod's folder
    /// (`<path>/<mod>/<name>`), whatever their extension. A file only gets
    /// the first check of each checker that matches it.
    #[serde(default)]
    pub files: Vec<String>,
    /// A problem found here doesn't keep the mod from loading (say, a
    /// settings file the mod rewrites).
    #[serde(default)]
    pub warning_only: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct WineRequirements {
    #[serde(default)]
    pub dll_overrides: Vec<DllOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct DllOverride {
    pub dll: String,
    /// Wine load order, e.g. `native,builtin`.
    pub mode: String,
    #[serde(default)]
    pub reason: String,
    /// The framework that loads through it; when set, a missing override only
    /// matters once that framework is installed.
    #[serde(default)]
    pub framework: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(deny_unknown_fields)]
pub struct LaunchOptions {
    /// Arguments the game needs for mods to load (Cyberpunk's `-modded`).
    #[serde(default)]
    pub modded_args: Vec<String>,
}

impl GameDefinition {
    /// The layout target a game-relative path falls under, if any. The most
    /// specific (longest) matching path wins.
    pub fn layout_for(&self, target: &str) -> Option<&LayoutTarget> {
        let target = target.to_ascii_lowercase();
        self.layout
            .iter()
            .filter(|t| {
                let path = t.path.to_ascii_lowercase();
                target == path || target.starts_with(&format!("{path}/"))
            })
            .max_by_key(|t| t.path.len())
    }

    pub fn framework(&self, id: &str) -> Option<&Framework> {
        self.frameworks.iter().find(|f| f.id == id)
    }

    pub fn from_toml(text: &str) -> Result<Self> {
        let game: Self = toml::from_str(text)?;
        game.validate()?;
        Ok(game)
    }

    fn validate(&self) -> Result<()> {
        let invalid = |message: String| Error::InvalidGame {
            id: self.id.clone(),
            message,
        };
        if self.id.is_empty()
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(invalid(
                "id must be non-empty and use only letters, digits, '-' or '_'".into(),
            ));
        }
        if self.executable.is_empty() {
            return Err(invalid("executable must not be empty".into()));
        }
        let relative_paths =
            std::iter::once(&self.executable).chain(self.layout.iter().map(|t| &t.path));
        for path in relative_paths {
            if is_unsafe_relative(path) {
                return Err(invalid(format!(
                    "path '{path}' must be relative and stay inside the game folder"
                )));
            }
        }
        for check in self.layout.iter().flat_map(|t| &t.checks) {
            if !crate::validate::CHECKERS.contains(&check.checker.as_str()) {
                return Err(invalid(format!("unknown checker '{}'", check.checker)));
            }
        }
        let known = |id: &str| self.frameworks.iter().any(|f| f.id == id);
        let references = self
            .requirements
            .iter()
            .map(|r| &r.framework)
            .chain(self.frameworks.iter().flat_map(|f| &f.requires))
            .chain(self.logs.iter().filter_map(|l| l.framework.as_ref()));
        for id in references {
            if !known(id) {
                return Err(invalid(format!("unknown framework '{id}'")));
            }
        }
        Ok(())
    }
}

fn is_unsafe_relative(path: &str) -> bool {
    path.starts_with(['/', '\\'])
        || path.contains(':')
        || path.split(['/', '\\']).any(|c| c == "..")
}

/// Returns the game definitions compiled into the binary.
pub fn builtin_games() -> Result<Vec<GameDefinition>> {
    BUILTIN_GAMES
        .iter()
        .map(|text| GameDefinition::from_toml(text))
        .collect()
}

pub fn find_builtin(id: &str) -> Result<GameDefinition> {
    builtin_games()?
        .into_iter()
        .find(|g| g.id == id)
        .ok_or_else(|| Error::UnknownGame(id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_games_parse() {
        let cp = find_builtin("cyberpunk2077").unwrap();
        assert_eq!(cp.stores.steam_app_id.as_deref(), Some("1091500"));
        assert!(
            cp.layout
                .iter()
                .any(|t| t.kind == "archive" && t.path == "archive/pc/mod")
        );
        assert_eq!(cp.wine.dll_overrides.len(), 2);
        assert!(cp.framework("redscript").is_some());
        assert!(cp.load_order.is_some());
        let cet = cp.framework("cet").unwrap();
        assert_eq!(cet.min_version_for("2.5"), Some("1.38.0"));
        assert_eq!(
            cet.min_version_for("2.51"),
            Some("1.38.0"),
            "until a newer entry"
        );
        assert_eq!(
            cet.min_version_for("2.31"),
            None,
            "no data for older patches"
        );
        use std::cmp::Ordering::*;
        assert_eq!(compare_game_versions("2.31", "2.5"), Less);
        assert_eq!(compare_game_versions("2.5", "2.50"), Equal);
        assert_eq!(compare_game_versions("3.0", "2.51"), Greater);
    }

    #[test]
    fn rejects_paths_outside_game_folder() {
        let text = r#"
            id = "bad"
            name = "Bad"
            executable = "../evil.exe"
        "#;
        assert!(matches!(
            GameDefinition::from_toml(text),
            Err(Error::InvalidGame { .. })
        ));
    }
}
