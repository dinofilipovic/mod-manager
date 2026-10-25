use std::fs;
use std::path::{Path, PathBuf};

/// Resolves `relative` under `base`, matching each path component without
/// regard to case. Accepts `/` and `\` as separators.
///
/// Windows (and macOS by default) are case-insensitive, but Linux filesystems
/// are not, and paths from Steam manifests or the registry often differ in case
/// from what is on disk. Returns `None` if any component does not exist.
pub fn resolve_case_insensitive(base: &Path, relative: &str) -> Option<PathBuf> {
    let mut current = base.to_path_buf();
    for component in relative.split(['/', '\\']).filter(|c| !c.is_empty()) {
        let direct = current.join(component);
        if direct.exists() {
            current = direct;
            continue;
        }
        let found = fs::read_dir(&current).ok()?.flatten().find(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .eq_ignore_ascii_case(component)
        })?;
        current = found.path();
    }
    Some(current)
}

/// Canonicalizes a path, falling back to the path itself if that fails.
pub fn canonical_or_self(path: PathBuf) -> PathBuf {
    fs::canonicalize(&path).unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_mismatched_case() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("Bin/X64")).unwrap();
        fs::write(dir.path().join("Bin/X64/Game.exe"), b"").unwrap();

        let found = resolve_case_insensitive(dir.path(), "bin\\x64/game.exe").unwrap();
        assert!(found.is_file());
        assert!(resolve_case_insensitive(dir.path(), "bin/x64/missing.exe").is_none());
    }
}
