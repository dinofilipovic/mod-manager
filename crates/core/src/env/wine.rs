//! Plain Wine prefixes (no CrossOver), as used on Linux.

use std::path::PathBuf;

use super::{Environment, EnvironmentKind, is_wine_prefix};

/// The prefix Wine uses by default: `$WINEPREFIX`, else `~/.wine`.
pub fn discover_default_prefix() -> Option<Environment> {
    if cfg!(windows) {
        return None;
    }
    let prefix = std::env::var_os("WINEPREFIX")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".wine")))?;
    is_wine_prefix(&prefix).then(|| Environment {
        id: format!("wine:{}", prefix.display()),
        name: prefix.display().to_string(),
        kind: EnvironmentKind::WinePrefix,
        prefix,
    })
}
