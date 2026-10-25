//! Running directly on Windows: paths are host paths and the registry is the
//! real one.

use std::collections::BTreeMap;
use std::path::PathBuf;

use super::{Environment, EnvironmentKind, Hive};

pub fn discover() -> Option<Environment> {
    cfg!(windows).then(|| Environment {
        id: "native".into(),
        name: "Windows".into(),
        kind: EnvironmentKind::Native,
        prefix: PathBuf::from("C:\\"),
    })
}

/// String values of a registry key, checking the 32-bit view too.
#[cfg(windows)]
pub fn registry_key(hive: Hive, key: &str) -> Option<BTreeMap<String, String>> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WOW64_32KEY};
    let root = RegKey::predef(match hive {
        Hive::LocalMachine => HKEY_LOCAL_MACHINE,
        Hive::CurrentUser => HKEY_CURRENT_USER,
    });
    let opened = root
        .open_subkey_with_flags(key, KEY_READ)
        .or_else(|_| root.open_subkey_with_flags(key, KEY_READ | KEY_WOW64_32KEY))
        .ok()?;
    Some(
        opened
            .enum_values()
            .flatten()
            .filter_map(|(name, _)| {
                let value: String = opened.get_value(&name).ok()?;
                Some((name.to_ascii_lowercase(), value))
            })
            .collect(),
    )
}

#[cfg(not(windows))]
pub fn registry_key(_hive: Hive, _key: &str) -> Option<BTreeMap<String, String>> {
    None
}
