//! Whether each mod actually loaded, from the game's and frameworks' logs.
//!
//! Which log confirms which kind of mod, and how a load (or a failure)
//! reads, comes from the game definition (`[[logs]]` with `status`). A mod is
//! looked for by its folder names under the layout paths of that kind.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::game::GameDefinition;
use crate::health::LogReport;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub enum LoadStatus {
    /// A log says it loaded.
    Loaded,
    /// A log reports errors for it, or that it failed to load.
    Errors,
    /// The log that would confirm it doesn't mention it, is older than the
    /// last apply, or doesn't exist yet.
    NotSeen,
    /// No log can tell (archive and tweak mods load silently).
    CantVerify,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct ModStatus {
    pub status: LoadStatus,
    /// Which log said so, and how.
    pub detail: String,
}

/// A mod as the status check sees it.
pub struct StatusMod<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub files: &'a [String],
}

/// The newest file a log pattern matches, with its text and modification
/// time (Unix seconds).
fn newest_log(install_path: &Path, pattern: &str) -> Option<(String, String, i64)> {
    crate::health::log_paths_for(install_path, pattern)
        .into_iter()
        .filter_map(|p| {
            let modified = std::fs::metadata(&p)
                .ok()?
                .modified()
                .ok()?
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs() as i64;
            Some((p, modified))
        })
        .max_by_key(|(_, m)| *m)
        .and_then(|(p, modified)| {
            let text = crate::health::tail(&p, usize::MAX).ok()?.join("\n");
            let name = p.file_name()?.to_string_lossy().into_owned();
            Some((name, text, modified))
        })
}

/// Whether `pattern` matches `text` for the mod folder `name`.
pub fn matches(pattern: &str, name: &str, text: &str) -> bool {
    let (anchored, pattern) = match pattern.strip_prefix('^') {
        Some(rest) => (true, rest),
        None => (false, pattern),
    };
    let needle = pattern.replace("{name}", name).to_ascii_lowercase();
    if anchored {
        text.lines()
            .any(|l| l.to_ascii_lowercase().starts_with(&needle))
    } else {
        text.to_ascii_lowercase().contains(&needle)
    }
}

/// A mod's block in a log that logs one block per mod.
struct Block<'a> {
    text: &'a str,
    /// Another block or the end text followed, so the block is complete.
    ended: bool,
}

/// The blocks of `text` that start with `start` on a line naming one of
/// `names` as a path component (`\name\` or `\name.`).
fn blocks_of<'a>(
    text: &'a str,
    start: &str,
    end: Option<&str>,
    names: &[String],
) -> Vec<Block<'a>> {
    let mut starts: Vec<usize> = Vec::new();
    let mut end_at = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.contains(start) {
            starts.push(offset);
        } else if end_at.is_none() && end.is_some_and(|e| line.contains(e)) {
            end_at = Some(offset);
        }
        offset += line.len();
    }
    let mut out = Vec::new();
    for (i, &from) in starts.iter().enumerate() {
        let next = starts.get(i + 1).copied();
        let to = next.into_iter().chain(end_at.filter(|&e| e > from)).min();
        let block = &text[from..to.unwrap_or(text.len())];
        let first = block
            .lines()
            .next()
            .unwrap_or_default()
            .replace('/', "\\")
            .to_ascii_lowercase();
        let named = names.iter().any(|n| {
            let n = n.to_ascii_lowercase();
            first.contains(&format!("\\{n}\\")) || first.contains(&format!("\\{n}."))
        });
        if named {
            out.push(Block {
                text: block,
                ended: to.is_some(),
            });
        }
    }
    out
}

/// The folder names a mod has under the layout paths of `kind` (and DLL file
/// stems there, which loaders sometimes report instead).
fn names_for(game: &GameDefinition, kind: &str, files: &[String]) -> Vec<String> {
    let mut names = Vec::new();
    for file in files {
        let Some(layout) = game.layout_for(file).filter(|l| l.kind == kind) else {
            continue;
        };
        let rest = file[layout.path.len()..].trim_start_matches('/');
        let mut parts = rest.split('/');
        let (Some(first), Some(_)) = (parts.next(), parts.next()) else {
            continue;
        };
        for name in [first.to_string()].into_iter().chain(
            Path::new(rest)
                .extension()
                .filter(|e| e.eq_ignore_ascii_case("dll"))
                .and_then(|_| Path::new(rest).file_stem())
                .map(|s| s.to_string_lossy().into_owned()),
        ) {
            if !names.iter().any(|n: &String| n.eq_ignore_ascii_case(&name)) {
                names.push(name);
            }
        }
    }
    names
}

/// The load status of each mod from the logs in the game folder.
/// `applied_at` is when the mods were last applied: older logs don't count.
/// `reports` are the parsed logs, whose errors attributed to a mod mark it.
pub fn statuses(
    game: &GameDefinition,
    install_path: &Path,
    mods: &[StatusMod],
    applied_at: Option<i64>,
    reports: &[LogReport],
) -> BTreeMap<String, ModStatus> {
    let logs: Vec<_> = game
        .logs
        .iter()
        .filter_map(|source| {
            let status = source.status.as_ref()?;
            Some((source, status, newest_log(install_path, &source.path)))
        })
        .collect();
    let mut out = BTreeMap::new();
    for m in mods {
        let reported_errors: Vec<&str> = reports
            .iter()
            .filter(|r| !r.stale)
            .filter(|r| {
                r.entries.iter().any(|e| {
                    e.severity == crate::health::Severity::Error
                        && e.mod_name.as_deref() == Some(m.name)
                })
            })
            .map(|r| r.name.as_str())
            .collect();
        let mut verdicts: Vec<ModStatus> = Vec::new();
        for (source, status, log) in &logs {
            let names = names_for(game, &status.kind, m.files);
            if names.is_empty() {
                continue;
            }
            let verdict = match log {
                None => ModStatus {
                    status: LoadStatus::NotSeen,
                    detail: format!(
                        "No {} log yet; start the game once with mods applied.",
                        source.name
                    ),
                },
                Some((_, _, modified)) if applied_at.is_some_and(|a| *modified < a) => ModStatus {
                    status: LoadStatus::NotSeen,
                    detail: format!(
                        "The {} log is from before the last apply; start the game to check again.",
                        source.name
                    ),
                },
                Some((file, text, _)) => {
                    let failed = status
                        .failed
                        .as_ref()
                        .is_some_and(|p| names.iter().any(|n| matches(p, n, text)));
                    let complete = status
                        .needs
                        .as_ref()
                        .is_none_or(|n| text.contains(n.as_str()));
                    let (loaded, ended_without) = match &status.block {
                        Some(start) => {
                            // Each block says it on its own, so a log cut
                            // short (a crash, a game still starting) counts
                            // for the plugins it got through.
                            let blocks = blocks_of(text, start, status.needs.as_deref(), &names);
                            let loaded = |b: &Block| matches(&status.loaded, "", b.text);
                            (
                                blocks.iter().any(loaded),
                                blocks.iter().any(|b| b.ended && !loaded(b)),
                            )
                        }
                        None => (
                            complete && names.iter().any(|n| matches(&status.loaded, n, text)),
                            false,
                        ),
                    };
                    let failed = failed || (!loaded && ended_without);
                    if failed {
                        ModStatus {
                            status: LoadStatus::Errors,
                            detail: format!("{file} says it failed to load."),
                        }
                    } else if loaded {
                        ModStatus {
                            status: LoadStatus::Loaded,
                            detail: format!("Loaded, says {file}."),
                        }
                    } else {
                        ModStatus {
                            status: LoadStatus::NotSeen,
                            detail: format!("{file} doesn't mention it."),
                        }
                    }
                }
            };
            verdicts.push(verdict);
        }
        let verdict = if let Some(log) = reported_errors.first() {
            ModStatus {
                status: LoadStatus::Errors,
                detail: format!("The {log} log has errors about it; see Health."),
            }
        } else if let Some(v) = [LoadStatus::Errors, LoadStatus::NotSeen, LoadStatus::Loaded]
            .iter()
            .find_map(|s| verdicts.iter().find(|v| v.status == *s))
        {
            v.clone()
        } else {
            ModStatus {
                status: LoadStatus::CantVerify,
                detail: "The game doesn't log loading this kind of mod.".into(),
            }
        };
        out.insert(m.id.to_string(), verdict);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns() {
        assert!(matches(
            "] Mod {name} loaded!",
            "nativeSettings",
            "[x] Mod NativeSettings loaded! ('C:\\\\...')"
        ));
        assert!(!matches(
            "] Mod {name} loaded!",
            "A",
            "[x] Mod Cool Mod A loaded! ('C:\\\\...')"
        ));
        assert!(matches(
            "^{name}\\",
            "CoolHud",
            "header\nCoolHud\\main.reds\n"
        ));
        assert!(!matches(
            "^{name}\\",
            "Cool",
            "header\nCoolHud\\main.reds\n"
        ));
    }
}
