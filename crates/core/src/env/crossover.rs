//! CrossOver bottles. Each bottle is a Wine prefix in CrossOver's bottle
//! folder with a `cxbottle.conf` describing it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::{Environment, EnvironmentKind, is_wine_prefix};
use crate::error::{Error, Result};

/// Where CrossOver keeps bottles. `CX_BOTTLE_PATH` (colon-separated, the same
/// variable CrossOver itself reads) overrides the platform default.
pub fn default_bottle_dirs() -> Vec<PathBuf> {
    if let Some(paths) = std::env::var_os("CX_BOTTLE_PATH") {
        return std::env::split_paths(&paths).collect();
    }
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    if cfg!(target_os = "macos") {
        vec![home.join("Library/Application Support/CrossOver/Bottles")]
    } else if cfg!(target_os = "linux") {
        vec![home.join(".cxoffice")]
    } else {
        Vec::new()
    }
}

/// Lists the bottles in one bottle folder. A missing folder means no bottles.
pub fn discover_bottles(dir: &Path) -> Result<Vec<Environment>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::io(dir, e)),
    };
    let mut bottles: Vec<Environment> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.join("cxbottle.conf").is_file() && is_wine_prefix(path))
        .map(|path| load_bottle(&path))
        .collect();
    bottles.sort_by_key(|b| b.name.to_lowercase());
    Ok(bottles)
}

fn load_bottle(path: &Path) -> Environment {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut conf = fs::read_to_string(path.join("cxbottle.conf"))
        .map(|text| parse_conf_section(&text, "Bottle"))
        .unwrap_or_default();
    Environment {
        id: format!("crossover:{name}"),
        name,
        kind: EnvironmentKind::CrossOverBottle {
            bottle_id: conf.remove("BottleID"),
            template: conf.remove("Template"),
            wine_arch: conf.remove("WineArch"),
            crossover_version: conf.remove("Version"),
        },
        prefix: path.to_path_buf(),
    }
}

/// Parses the `"Key" = "Value"` lines of one `[Section]` of a CrossOver config file.
fn parse_conf_section(text: &str, section: &str) -> HashMap<String, String> {
    let mut values = HashMap::new();
    let mut in_section = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = header == section;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let unquote = |s: &str| s.trim().trim_matches('"').to_string();
            values.insert(unquote(key), unquote(value));
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONF: &str = r#";; comment
[Bottle]
"MenuRoot" = "/Windows Applications"
"BottleID" = "23906E30"
"Version" = "26.3.0.39832"
"Template" = "win10_64"
"WineArch" = "win64"
[EnvironmentVariables]
"Template" = "not this one"
"#;

    #[test]
    fn parses_bottle_section_only() {
        let values = parse_conf_section(CONF, "Bottle");
        assert_eq!(values["Template"], "win10_64");
        assert_eq!(values["Version"], "26.3.0.39832");
        assert_eq!(values.len(), 5);
    }

    #[test]
    fn discovers_bottles_with_metadata() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["Steam", "Not a bottle"] {
            fs::create_dir_all(dir.path().join(name).join("drive_c")).unwrap();
            fs::write(dir.path().join(name).join("system.reg"), "").unwrap();
        }
        fs::write(dir.path().join("Steam/cxbottle.conf"), CONF).unwrap();

        let bottles = discover_bottles(dir.path()).unwrap();
        assert_eq!(bottles.len(), 1);
        assert_eq!(bottles[0].id, "crossover:Steam");
        let EnvironmentKind::CrossOverBottle { template, .. } = &bottles[0].kind else {
            panic!("expected a CrossOver bottle");
        };
        assert_eq!(template.as_deref(), Some("win10_64"));
    }

    #[test]
    fn missing_bottle_dir_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            discover_bottles(&dir.path().join("nope"))
                .unwrap()
                .is_empty()
        );
    }
}
