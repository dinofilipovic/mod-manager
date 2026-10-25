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

/// Lowercase hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// SHA-256 of a file's contents, as lowercase hex.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use sha2::Digest;
    let mut file = fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let n = std::io::Read::read(&mut file, &mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Size and modification time (nanoseconds since the epoch) of a file.
pub fn size_and_mtime(path: &Path) -> std::io::Result<(u64, i64)> {
    let meta = fs::metadata(path)?;
    let mtime = meta
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64);
    Ok((meta.len(), mtime))
}

/// A sibling path for writing `path` in steps; never an existing file.
fn temp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()))
}

/// Copies `from` to `to` so that `to` only ever appears complete: the bytes go
/// to a temporary sibling that is then renamed. Replaces an existing `to`.
pub fn copy_atomic(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = temp_sibling(to);
    let result = reflink_copy::reflink_or_copy(from, &tmp).and_then(|_| fs::rename(&tmp, to));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Moves `from` to `to`, failing if `to` already exists, so a backup is never
/// overwritten. `to` only ever appears complete, also across volumes.
pub fn move_no_clobber(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::symlink_metadata(to).is_ok() {
        return Err(Error::new(
            ErrorKind::AlreadyExists,
            format!("{} already exists", to.display()),
        ));
    }
    // A hard link refuses to replace an existing file, and is atomic.
    match fs::hard_link(from, to) {
        Ok(()) => return fs::remove_file(from),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => return Err(e),
        Err(_) => {}
    }
    if fs::rename(from, to).is_ok() {
        return Ok(());
    }
    // Another volume (or no hard links): copy, then remove the original.
    let tmp = temp_sibling(to);
    let copied = reflink_copy::reflink_or_copy(from, &tmp).and_then(|_| fs::rename(&tmp, to));
    if let Err(e) = copied {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::remove_file(from)
}

/// Removes a file; a file that isn't there counts as removed.
pub fn remove_file_if_exists(path: &Path) -> std::io::Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
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

    #[test]
    fn moves_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b, c) = (
            dir.path().join("a"),
            dir.path().join("x/b"),
            dir.path().join("c"),
        );
        fs::write(&a, "original").unwrap();
        fs::write(&c, "other").unwrap();
        move_no_clobber(&a, &b).unwrap();
        assert!(!a.exists());
        assert_eq!(fs::read_to_string(&b).unwrap(), "original");
        assert!(move_no_clobber(&c, &b).is_err());
        assert_eq!(fs::read_to_string(&b).unwrap(), "original");
        assert!(c.exists(), "a failed move leaves the source");
        assert_eq!(
            sha256_file(&b).unwrap(),
            "0682c5f2076f099c34cfdd15a9e063849ed437a49677e6fcc5b4198c76575be5"
        );
    }
}
