//! Update checks against Nexus Mods with as few requests as possible.
//!
//! One `updated.json` request lists the game's mods with new files since the
//! last check (period 1d, 1w or 1m, picked from when that was); only those
//! mods' file lists are fetched. More than a month since the last check, or
//! never checked, means every linked mod's file list is fetched once.
//!
//! The check runs without the library: [`UpdateCheck::snapshot`] copies what
//! it needs, [`run`] talks to Nexus, and `Library::apply_update_check`
//! records the result, so the library isn't locked during network calls.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use ts_rs::TS;

use crate::error::Result;
use crate::nexus::{NexusClient, NexusFile, NexusFileList};
use crate::store::ModRecord;

const DAY: i64 = 86_400;

/// What a check needs to know about the installed mods.
#[derive(Debug, Clone)]
pub struct UpdateCheck {
    pub game_id: String,
    pub game_domain: String,
    pub mods: Vec<ModRecord>,
    /// When this game was last checked, Unix seconds.
    pub last_checked_at: Option<i64>,
}

/// What a check found for one mod.
#[derive(Debug, Clone, PartialEq)]
pub struct ModUpdate {
    pub mod_id: String,
    /// The revision that was looked at; a newer one makes this result stale.
    pub revision: u32,
    /// The newest file upload seen for the mod, Unix seconds.
    pub nexus_updated_at: Option<i64>,
    /// A newer file, unless the mod is pinned or the file ignored.
    pub update: Option<(u32, Option<String>)>,
}

/// The result of a check, to record in the library.
#[derive(Debug, Clone)]
pub struct CheckOutcome {
    pub game_id: String,
    pub checked_at: i64,
    /// Only the mods whose files were looked at.
    pub mods: Vec<ModUpdate>,
    /// Requests made to Nexus.
    pub requests: u32,
}

/// A summary for the UI.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct UpdateSummary {
    /// Mods whose files were looked at.
    pub checked: u32,
    /// Mods with an update available now.
    pub available: u32,
    pub requests: u32,
    /// `None` for a full check, else `1d`, `1w` or `1m`.
    pub period: Option<String>,
}

/// One version's changelog lines.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct ChangelogVersion {
    pub version: String,
    pub lines: Vec<String>,
}

/// The changelog entries that matter for an update: versions after the
/// installed one up to the new one, newest first. Without an update, the
/// latest three.
pub fn relevant_changelog(
    entries: &BTreeMap<String, Vec<String>>,
    installed: Option<&str>,
    update: Option<&str>,
) -> Vec<ChangelogVersion> {
    let mut versions: Vec<ChangelogVersion> = entries
        .iter()
        .map(|(version, lines)| ChangelogVersion {
            version: version.clone(),
            lines: lines.clone(),
        })
        .collect();
    versions.sort_by(|a, b| compare_versions(&b.version, &a.version));
    match (installed, update) {
        (Some(installed), Some(update)) => versions
            .into_iter()
            .filter(|v| {
                compare_versions(&v.version, installed) == Ordering::Greater
                    && compare_versions(&v.version, update) != Ordering::Greater
            })
            .collect(),
        _ => versions.into_iter().take(3).collect(),
    }
}

/// The `updated.json` period covering everything since `last`, or `None`
/// when a full check is needed.
pub fn period_since(last: Option<i64>, now: i64) -> Option<&'static str> {
    // An hour of overlap: Nexus caches `updated.json` for a few minutes.
    let elapsed = now - last? + 3600;
    match elapsed {
        e if e < 0 => None,
        e if e <= DAY => Some("1d"),
        e if e <= 7 * DAY => Some("1w"),
        e if e <= 30 * DAY => Some("1m"),
        _ => None,
    }
}

/// Checks the mods linked to Nexus files. Pinned mods are looked at too (to
/// keep their dates current) but never get an update. `before_request` runs
/// before each request and can stop the check (the request budget).
pub fn run(
    client: &NexusClient,
    check: &UpdateCheck,
    now: i64,
    before_request: &dyn Fn() -> Result<()>,
) -> Result<CheckOutcome> {
    let linked: Vec<&ModRecord> = check
        .mods
        .iter()
        .filter(|m| m.nexus_mod_id.is_some() && m.nexus_file_id.is_some())
        .collect();
    let mut requests = 0;
    let period = period_since(check.last_checked_at, now);
    let candidates: Vec<&ModRecord> = match period {
        None => linked,
        Some(period) => {
            before_request()?;
            let updated: HashMap<u32, i64> = client
                .updated_mods(&check.game_domain, period)?
                .into_iter()
                .map(|e| (e.mod_id, e.latest_file_update))
                .collect();
            requests += 1;
            linked
                .into_iter()
                .filter(|m| match (m.nexus_updated_at, m.nexus_mod_id) {
                    (None, _) => true,
                    (Some(seen), Some(id)) => updated.get(&id).is_some_and(|&t| t > seen),
                    _ => false,
                })
                .collect()
        }
    };

    // One request per Nexus mod, even when several installed mods are files of it.
    let mut lists: BTreeMap<u32, NexusFileList> = BTreeMap::new();
    let mut mods = Vec::new();
    for m in candidates {
        let (Some(nexus_mod), Some(file_id)) = (m.nexus_mod_id, m.nexus_file_id) else {
            continue;
        };
        let files = match lists.entry(nexus_mod) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                before_request()?;
                requests += 1;
                e.insert(client.mod_files(&check.game_domain, nexus_mod)?)
            }
        };
        let update = newer_file(files, file_id, m.version.as_deref())
            .filter(|_| !m.pinned)
            .filter(|f| Some(f.file_id) != m.ignored_update_file_id)
            .map(|f| (f.file_id, f.version.clone()));
        mods.push(ModUpdate {
            mod_id: m.id.clone(),
            revision: m.revision,
            nexus_updated_at: files
                .files
                .iter()
                .filter_map(|f| f.uploaded_timestamp)
                .max(),
            update,
        });
    }
    Ok(CheckOutcome {
        game_id: check.game_id.clone(),
        checked_at: now,
        mods,
        requests,
    })
}

/// The file that replaces `file_id`: the end of the author's update chain,
/// or when there is none and the installed file is a main file, the newest
/// main file with a higher version (newer upload breaks ties). When the
/// installed file is gone from Nexus, `installed_version` is compared.
pub fn newer_file<'a>(
    files: &'a NexusFileList,
    file_id: u32,
    installed_version: Option<&str>,
) -> Option<&'a NexusFile> {
    if let Some(newer) = files.newer_than(file_id) {
        return Some(newer);
    }
    let installed = files.files.iter().find(|f| f.file_id == file_id);
    let category = installed.and_then(|f| f.category_name.as_deref());
    // Optional files have their own lines; only main files are compared.
    if !matches!(
        category,
        None | Some("MAIN") | Some("OLD_VERSION") | Some("ARCHIVED")
    ) {
        return None;
    }
    let current = installed
        .and_then(|f| f.version.as_deref())
        .or(installed_version)?;
    files
        .files
        .iter()
        .filter(|f| f.category_name.as_deref() == Some("MAIN") && f.file_id != file_id)
        .filter(|f| {
            f.version
                .as_deref()
                .is_some_and(|v| compare_versions(v, current) == Ordering::Greater)
        })
        .max_by(|a, b| {
            compare_versions(
                a.version.as_deref().unwrap_or_default(),
                b.version.as_deref().unwrap_or_default(),
            )
            .then(a.uploaded_timestamp.cmp(&b.uploaded_timestamp))
        })
}

/// Compares version strings part by part: numbers as numbers, the rest as
/// text, so `1.10` is newer than `1.9`. A leading `v` is ignored and
/// missing parts count as zero.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    fn parts(v: &str) -> Vec<String> {
        let v = v.trim().trim_start_matches(['v', 'V']);
        let mut out = Vec::new();
        let mut current = String::new();
        let mut digits = None;
        for c in v.chars() {
            if !c.is_ascii_alphanumeric() {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
                digits = None;
                continue;
            }
            let is_digit = c.is_ascii_digit();
            if digits.is_some_and(|d| d != is_digit) && !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            digits = Some(is_digit);
            current.push(c.to_ascii_lowercase());
        }
        if !current.is_empty() {
            out.push(current);
        }
        out
    }
    let (a, b) = (parts(a), parts(b));
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).map_or("0", String::as_str);
        let y = b.get(i).map_or("0", String::as_str);
        let order = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            // A number part beats a text part (1.0.1 > 1.0.beta).
            (Ok(_), Err(_)) => Ordering::Greater,
            (Err(_), Ok(_)) => Ordering::Less,
            _ => x.cmp(y),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert_eq!(compare_versions("1.10", "1.9"), Ordering::Greater);
        assert_eq!(compare_versions("v2.0", "2.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0.1", "1.0"), Ordering::Greater);
        assert_eq!(compare_versions("1.0b", "1.0a"), Ordering::Greater);
        assert_eq!(compare_versions("0.5.31", "0.5.32"), Ordering::Less);
    }

    #[test]
    fn picks_the_changelog_between_versions() {
        let entries: BTreeMap<String, Vec<String>> = [
            ("1.0", "first"),
            ("1.1", "second"),
            ("1.2", "third"),
            ("1.10", "tenth"),
        ]
        .iter()
        .map(|(v, l)| (v.to_string(), vec![l.to_string()]))
        .collect();
        let picked: Vec<String> = relevant_changelog(&entries, Some("1.0"), Some("1.2"))
            .into_iter()
            .map(|v| v.version)
            .collect();
        assert_eq!(picked, ["1.2", "1.1"]);
        assert_eq!(relevant_changelog(&entries, None, None)[0].version, "1.10");
    }

    #[test]
    fn picks_periods() {
        let now = 100 * DAY;
        assert_eq!(period_since(None, now), None);
        assert_eq!(period_since(Some(now - 3600), now), Some("1d"));
        assert_eq!(
            period_since(Some(now - DAY + 1800), now),
            Some("1w"),
            "an hour of overlap"
        );
        assert_eq!(period_since(Some(now - 3 * DAY), now), Some("1w"));
        assert_eq!(period_since(Some(now - 20 * DAY), now), Some("1m"));
        assert_eq!(period_since(Some(now - 40 * DAY), now), None);
    }

    #[test]
    fn falls_back_to_the_newest_main_file() {
        let list: NexusFileList = serde_json::from_str(
            r#"{"files":[
                {"file_id":1,"version":"1.0","category_name":"MAIN","uploaded_timestamp":10},
                {"file_id":2,"version":"1.2","category_name":"MAIN","uploaded_timestamp":20},
                {"file_id":3,"version":"1.2","category_name":"MAIN","uploaded_timestamp":30},
                {"file_id":4,"version":"9.0","category_name":"OPTIONAL","uploaded_timestamp":40},
                {"file_id":5,"version":"0.9","category_name":"MAIN","uploaded_timestamp":50}],
              "file_updates":[]}"#,
        )
        .unwrap();
        assert_eq!(
            newer_file(&list, 1, None).unwrap().file_id,
            3,
            "same version: newer upload"
        );
        assert!(newer_file(&list, 3, None).is_none());
        assert!(
            newer_file(&list, 4, None).is_none(),
            "optional files aren't compared"
        );
        // The installed file was deleted from Nexus: its version still counts.
        assert_eq!(newer_file(&list, 99, Some("1.1")).unwrap().file_id, 3);
        assert!(newer_file(&list, 99, None).is_none());
    }
}
