//! Reads the log files the game and its frameworks write, picks out errors and
//! warnings, and attributes each to the mod it mentions where possible.
//! Locations come from the game definition's `logs`.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use ts_rs::TS;

use crate::game::{GameDefinition, LogSource};

/// Only the end of large logs is read; the latest session is at the bottom.
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;
/// Most entries kept per log for display.
const MAX_ENTRIES: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[ts(export)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct LogEntry {
    pub line: u32,
    pub severity: Severity,
    pub text: String,
    /// The mod the line mentions, if one could be told.
    pub mod_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct LogReport {
    pub name: String,
    pub framework: Option<String>,
    /// Game-relative path, `/`-separated.
    pub relative_path: String,
    pub path: PathBuf,
    #[ts(type = "number")]
    pub modified_at: i64,
    #[ts(type = "number")]
    pub size_bytes: u64,
    /// Written before the mods were last applied, so it may be out of date.
    pub stale: bool,
    pub error_count: u32,
    pub warning_count: u32,
    /// The latest errors and warnings, oldest first.
    pub entries: Vec<LogEntry>,
}

/// Words that tie a log line to a mod: its folder names and archive names.
pub struct ModHints {
    pub name: String,
    pub tokens: Vec<String>,
}

const GENERIC_TOKENS: &[&str] = &[
    "mods", "mod", "scripts", "plugins", "config", "data", "init", "main", "archive", "tweaks",
];

/// Distinctive names in a mod's files: the folder right below each layout
/// target (`r6/scripts/<Name>`, `.../mods/<Name>`) and archive file names.
pub fn hints_for(game: &GameDefinition, mod_name: &str, files: &[String]) -> ModHints {
    let mut tokens: Vec<String> = Vec::new();
    for file in files {
        let Some(target) = game.layout_for(file) else {
            continue;
        };
        let rest = &file[target.path.len().min(file.len())..].trim_start_matches('/');
        let token = if target.flatten {
            rest.rsplit_once('.').map_or(*rest, |(stem, _)| stem)
        } else {
            rest.split('/').next().unwrap_or_default()
        };
        let token = token.to_ascii_lowercase();
        if token.len() >= 4 && !GENERIC_TOKENS.contains(&token.as_str()) && !tokens.contains(&token)
        {
            tokens.push(token);
        }
    }
    let name = mod_name.to_ascii_lowercase();
    if name.len() >= 5 && !tokens.contains(&name) {
        tokens.push(name);
    }
    ModHints {
        name: mod_name.to_string(),
        tokens,
    }
}

/// Every existing log of the game, newest first within each source.
pub fn read_logs(
    game: &GameDefinition,
    install_path: &Path,
    hints: &[ModHints],
    applied_at: Option<i64>,
) -> Vec<LogReport> {
    game.logs
        .iter()
        .flat_map(|source| {
            expand(install_path, &source.path)
                .into_iter()
                .filter_map(move |path| read_log(source, install_path, path, hints, applied_at))
        })
        .collect()
}

/// The last `max_lines` lines of a log, for viewing it in full.
pub fn tail(path: &Path, max_lines: usize) -> std::io::Result<Vec<String>> {
    let (text, _) = read_end(path)?;
    let lines: Vec<String> = text.lines().map(String::from).collect();
    Ok(lines[lines.len().saturating_sub(max_lines)..].to_vec())
}

/// Game-relative paths of every log the game definition knows about.
pub fn log_paths(game: &GameDefinition, install_path: &Path) -> Vec<PathBuf> {
    game.logs
        .iter()
        .flat_map(|s| expand(install_path, &s.path))
        .collect()
}

fn read_log(
    source: &LogSource,
    install_path: &Path,
    path: PathBuf,
    hints: &[ModHints],
    applied_at: Option<i64>,
) -> Option<LogReport> {
    let meta = fs::metadata(&path).ok()?;
    let modified_at = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let relative_path = path
        .strip_prefix(install_path)
        .ok()?
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    let (text, first_line) = read_end(&path).ok()?;

    // A log inside a mod's own folder belongs to that mod.
    let path_mod = hint_match(hints, &relative_path.to_ascii_lowercase());
    let mut entries = Vec::new();
    let (mut errors, mut warnings) = (0u32, 0u32);
    for (i, line) in text.lines().enumerate() {
        let Some(severity) = classify(line) else {
            continue;
        };
        match severity {
            Severity::Error => errors += 1,
            Severity::Warning => warnings += 1,
        }
        entries.push(LogEntry {
            line: first_line + i as u32,
            severity,
            text: line.trim().chars().take(500).collect(),
            mod_name: hint_match(hints, &line.to_ascii_lowercase()).or_else(|| path_mod.clone()),
        });
    }
    let keep_from = entries.len().saturating_sub(MAX_ENTRIES);
    Some(LogReport {
        name: source.name.clone(),
        framework: source.framework.clone(),
        relative_path,
        size_bytes: meta.len(),
        stale: applied_at.is_some_and(|applied| modified_at < applied),
        modified_at,
        path,
        error_count: errors,
        warning_count: warnings,
        entries: entries.split_off(keep_from),
    })
}

/// Error and warning markers used by redscript (`[ERROR - ...]`), spdlog-based
/// logs such as RED4ext and CET (`[error]`, `[warning]`) and REDmod.
fn classify(line: &str) -> Option<Severity> {
    let l = line.to_ascii_lowercase();
    if l.contains("[error")
        || l.contains("[critical")
        || l.contains("[fatal")
        || l.contains(" error !")
    {
        Some(Severity::Error)
    } else if l.contains("[warn") {
        Some(Severity::Warning)
    } else {
        None
    }
}

fn hint_match(hints: &[ModHints], haystack: &str) -> Option<String> {
    hints
        .iter()
        .find(|h| h.tokens.iter().any(|t| haystack.contains(t.as_str())))
        .map(|h| h.name.clone())
}

/// Reads the whole file, or its last `MAX_READ_BYTES` for large ones.
/// Returns the text and the (1-based) number of its first line when known.
fn read_end(path: &Path) -> std::io::Result<(String, u32)> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let mut bytes = Vec::new();
    if len > MAX_READ_BYTES {
        file.seek(SeekFrom::Start(len - MAX_READ_BYTES))?;
        file.read_to_end(&mut bytes)?;
        // Drop the partial first line.
        let start = bytes.iter().position(|&b| b == b'\n').map_or(0, |p| p + 1);
        bytes.drain(..start);
        Ok((String::from_utf8_lossy(&bytes).into_owned(), 0))
    } else {
        file.read_to_end(&mut bytes)?;
        Ok((String::from_utf8_lossy(&bytes).into_owned(), 1))
    }
}

/// Expands a pattern where `*` matches within one path component, matching
/// literal components without regard to case.
fn expand(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut current = vec![root.to_path_buf()];
    for part in pattern.split('/').filter(|p| !p.is_empty()) {
        let mut next = Vec::new();
        for dir in &current {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if wildcard_match(part, &name) {
                    next.push(entry.path());
                }
            }
        }
        current = next;
    }
    current.retain(|p| p.is_file());
    current.sort();
    current
}

fn wildcard_match(pattern: &str, name: &str) -> bool {
    let (pattern, name) = (pattern.to_ascii_lowercase(), name.to_ascii_lowercase());
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == name;
    }
    let mut rest = name.as_str();
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(r) = rest.strip_prefix(part) else {
                return false;
            };
            rest = r;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else {
            let Some(pos) = rest.find(part) else {
                return false;
            };
            rest = &rest[pos + part.len()..];
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::find_builtin;

    #[test]
    fn reads_errors_and_attributes_them_to_mods() {
        let game = find_builtin("cyberpunk2077").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |rel: &str, text: &str| {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        };
        write(
            "r6/logs/redscript_rCURRENT.log",
            "[INFO - x] Compiling\n[ERROR - x] At r6/scripts/CoolHud/main.reds:1: unknown type\n[WARN - x] field already defined\n",
        );
        write(
            "bin/x64/plugins/cyber_engine_tweaks/mods/MyCet/MyCet.log",
            "[info] ok\n[error] attempt to index nil\n",
        );
        let hints = vec![
            hints_for(&game, "Cool HUD", &["r6/scripts/CoolHud/main.reds".into()]),
            hints_for(
                &game,
                "My CET Mod",
                &["bin/x64/plugins/cyber_engine_tweaks/mods/MyCet/init.lua".into()],
            ),
        ];
        let logs = read_logs(&game, root, &hints, None);
        assert_eq!(logs.len(), 2);

        let redscript = logs.iter().find(|l| l.name == "redscript").unwrap();
        assert_eq!((redscript.error_count, redscript.warning_count), (1, 1));
        assert_eq!(redscript.entries[0].line, 2);
        assert_eq!(redscript.entries[0].mod_name.as_deref(), Some("Cool HUD"));

        let cet = logs.iter().find(|l| l.name == "CET mod").unwrap();
        assert_eq!(
            cet.entries[0].mod_name.as_deref(),
            Some("My CET Mod"),
            "attributed by folder"
        );
    }

    #[test]
    fn wildcards() {
        assert!(wildcard_match("*.log", "Mod.LOG"));
        assert!(wildcard_match("red4ext*.log", "red4ext-2026.log"));
        assert!(!wildcard_match("*.log", "mod.txt"));
        assert!(wildcard_match("mods", "Mods"));
    }
}
