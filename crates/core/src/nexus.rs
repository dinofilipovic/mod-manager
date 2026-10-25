//! Nexus Mods: `nxm://` links, the v1 REST API, downloads and API-key storage.
//!
//! Free accounts can only download through the website's "Mod Manager
//! Download" button, which opens an `nxm://` link carrying a short-lived key.
//! Premium accounts can also request download links directly (used for updates).

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::error::{Error, Result};

const API: &str = "https://api.nexusmods.com/v1";
/// Overrides the API address (a test server).
const API_ENV: &str = "MM_NEXUS_API_URL";
/// Requests kept back for downloads and the user's own actions: background
/// work (update checks, changelogs, requirement lookups) pauses below this.
const RESERVED_REQUESTS: u32 = 20;
const APP_NAME: &str = "ModManager";
const KEYRING_SERVICE: &str = "dev.local.modmanager";
const KEYRING_USER: &str = "nexus-api-key";

/// A parsed `nxm://<game>/mods/<mod>/files/<file>?key=..&expires=..` link.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct NxmLink {
    pub game_domain: String,
    pub mod_id: u32,
    pub file_id: u32,
    pub key: Option<String>,
    #[ts(type = "number | null")]
    pub expires: Option<u64>,
}

pub fn parse_nxm(link: &str) -> Result<NxmLink> {
    let bad = || Error::Nexus(format!("not a Nexus mod download link: {link}"));
    let url = url::Url::parse(link.trim()).map_err(|_| bad())?;
    if url.scheme() != "nxm" {
        return Err(bad());
    }
    let game_domain = url.host_str().ok_or_else(bad)?.to_ascii_lowercase();
    let segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
    match segments.as_slice() {
        ["mods", mod_id, "files", file_id] => {
            let query = |name: &str| {
                url.query_pairs()
                    .find(|(k, _)| k == name)
                    .map(|(_, v)| v.into_owned())
            };
            Ok(NxmLink {
                game_domain,
                mod_id: mod_id.parse().map_err(|_| bad())?,
                file_id: file_id.parse().map_err(|_| bad())?,
                key: query("key"),
                expires: query("expires").and_then(|e| e.parse().ok()),
            })
        }
        ["collections", ..] => Err(Error::Nexus(
            "Collections aren't supported yet; download the mods individually.".into(),
        )),
        _ => Err(bad()),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NexusUser {
    pub user_id: u64,
    pub name: String,
    pub is_premium: bool,
    pub profile_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NexusModInfo {
    pub mod_id: u32,
    pub name: Option<String>,
    pub summary: Option<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub picture_url: Option<String>,
    /// The game's Nexus domain (in MD5 search results, which span games).
    #[serde(default)]
    pub domain_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct NexusFile {
    pub file_id: u32,
    pub name: Option<String>,
    pub version: Option<String>,
    pub file_name: Option<String>,
    pub category_name: Option<String>,
    #[ts(type = "number | null")]
    pub size_kb: Option<u64>,
    /// Unix seconds.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub uploaded_timestamp: Option<i64>,
}

/// A mod updated within the period asked for (`updated.json`).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct UpdateEntry {
    pub mod_id: u32,
    /// Unix seconds of the newest file upload.
    pub latest_file_update: i64,
    pub latest_mod_activity: i64,
}

/// A mod's changelog: version -> lines.
pub type Changelog = BTreeMap<String, Vec<String>>;

/// A file found by its MD5 hash.
#[derive(Debug, Clone, Deserialize)]
pub struct Md5Match {
    #[serde(rename = "mod")]
    pub mod_info: NexusModInfo,
    pub file_details: NexusFile,
}

/// The `X-RL-*` headers of the latest Nexus response: 2,500 requests a day,
/// then 100 an hour once the daily ones are used up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct RateLimits {
    pub hourly_limit: u32,
    pub hourly_remaining: u32,
    /// Unix seconds.
    #[ts(type = "number | null")]
    pub hourly_reset: Option<i64>,
    pub daily_limit: u32,
    pub daily_remaining: u32,
    #[ts(type = "number | null")]
    pub daily_reset: Option<i64>,
    /// When these were read, Unix seconds.
    #[ts(type = "number")]
    pub observed_at: i64,
}

impl RateLimits {
    fn from_headers(headers: &reqwest::header::HeaderMap, now: i64) -> Option<Self> {
        let text = |name: &str| headers.get(name)?.to_str().ok().map(str::trim);
        let number = |name: &str| text(name)?.parse().ok();
        Some(Self {
            hourly_limit: number("x-rl-hourly-limit")?,
            hourly_remaining: number("x-rl-hourly-remaining")?,
            hourly_reset: text("x-rl-hourly-reset").and_then(parse_reset_time),
            daily_limit: number("x-rl-daily-limit")?,
            daily_remaining: number("x-rl-daily-remaining")?,
            daily_reset: text("x-rl-daily-reset").and_then(parse_reset_time),
            observed_at: now,
        })
    }

    /// Requests that can still be made now: the daily ones, then the hourly ones.
    pub fn available(&self) -> u32 {
        if self.daily_remaining > 0 {
            self.daily_remaining
        } else {
            self.hourly_remaining
        }
    }

    /// When more requests become available again, if known.
    pub fn next_reset(&self) -> Option<i64> {
        if self.daily_remaining > 0 {
            self.daily_reset
        } else {
            self.hourly_reset
        }
    }
}

/// Reads the reset times Nexus sends: `2019-02-01T12:00:00+00:00` (hourly)
/// and `2019-02-02 00:00:00 +0000` (daily). Returns Unix seconds.
pub fn parse_reset_time(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, rest) = text.split_at_checked(10)?;
    let rest = rest.trim_start_matches(['T', ' ']);
    let (time, offset) = rest.split_at_checked(8)?;
    let mut d = date.split('-').map(|p| p.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = time.split(':').map(|p| p.parse::<i64>());
    let (hh, mm, ss) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    let offset = offset.trim().replace(':', "");
    let offset_secs = if offset.is_empty() || offset == "Z" {
        0
    } else {
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let digits = offset.trim_start_matches(['+', '-']);
        let (oh, om) = digits.split_at_checked(2)?;
        sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60)
    };
    // Days since the epoch for a proleptic Gregorian date.
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hh * 3600 + mm * 60 + ss - offset_secs)
}

/// The latest rate-limit headers seen this run.
static RATE_LIMITS: std::sync::Mutex<Option<RateLimits>> = std::sync::Mutex::new(None);

pub fn rate_limits() -> Option<RateLimits> {
    RATE_LIMITS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Seeds the snapshot from an earlier run, unless this run has its own.
pub fn remember_rate_limits(limits: RateLimits) {
    let mut current = RATE_LIMITS.lock().unwrap_or_else(|p| p.into_inner());
    if current.is_none() {
        *current = Some(limits);
    }
}

/// Fails when background work should wait so the remaining requests are
/// kept for downloads and the user's own actions.
pub fn check_budget(now: i64) -> Result<()> {
    let Some(limits) = rate_limits() else {
        return Ok(());
    };
    // A reset since the snapshot means the budget is back.
    if limits.next_reset().is_some_and(|reset| reset <= now) {
        return Ok(());
    }
    let available = limits.available();
    if available > RESERVED_REQUESTS {
        return Ok(());
    }
    let when = limits
        .next_reset()
        .map(|r| format!(" in about {} minutes", ((r - now).max(60) + 59) / 60))
        .unwrap_or_default();
    Err(Error::Nexus(format!(
        "Paused to save your Nexus Mods requests: only {available} are left until they reset{when}. \
         Downloads still work; check for updates again after the reset."
    )))
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileUpdate {
    pub old_file_id: u32,
    pub new_file_id: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NexusFileList {
    pub files: Vec<NexusFile>,
    #[serde(default)]
    pub file_updates: Vec<FileUpdate>,
}

impl NexusFileList {
    /// Follows the update chain from `file_id` and returns the newest file if
    /// it differs from the one installed.
    pub fn newer_than(&self, file_id: u32) -> Option<&NexusFile> {
        let mut current = file_id;
        for _ in 0..self.file_updates.len() {
            match self.file_updates.iter().find(|u| u.old_file_id == current) {
                Some(update) => current = update.new_file_id,
                None => break,
            }
        }
        (current != file_id)
            .then(|| self.files.iter().find(|f| f.file_id == current))
            .flatten()
    }
}

/// A download in the app's queue. Kept in the library, so the queue
/// survives a restart.
#[derive(Debug, Clone, Default, Serialize, TS)]
#[ts(export)]
pub struct Download {
    pub id: String,
    pub game_id: String,
    pub game_domain: String,
    pub instance_id: Option<String>,
    pub name: String,
    pub version: Option<String>,
    pub file_name: Option<String>,
    pub nexus_mod_id: u32,
    pub nexus_file_id: u32,
    #[ts(type = "number")]
    pub bytes_done: u64,
    #[ts(type = "number | null")]
    pub bytes_total: Option<u64>,
    pub status: DownloadStatus,
    /// Failed attempts so far.
    pub attempts: u32,
    /// When the next attempt starts, Unix seconds.
    #[ts(type = "number | null")]
    pub retry_at: Option<i64>,
    /// Installed mod this download updates.
    pub replaces: Option<String>,
    /// The `nxm://` link's key and expiry (free accounts need them).
    #[serde(skip)]
    #[ts(skip)]
    pub key: Option<String>,
    #[serde(skip)]
    #[ts(skip)]
    pub expires: Option<u64>,
    /// The finished archive in the archive store.
    #[serde(skip)]
    #[ts(skip)]
    pub archive_id: Option<i64>,
    #[ts(type = "number")]
    pub created_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state")]
#[ts(export)]
pub enum DownloadStatus {
    #[default]
    Queued,
    Downloading,
    Installing,
    /// The mod has installer options; waiting for the user to choose.
    AwaitingChoices {
        token: String,
    },
    Done {
        mod_id: String,
        replaced_existing: bool,
        /// The installer's options were chosen like last time, without asking.
        replayed_choices: bool,
        warnings: Vec<String>,
    },
    Failed {
        message: String,
    },
}

#[derive(Debug, Deserialize)]
struct DownloadLink {
    #[serde(rename = "URI")]
    uri: String,
}

pub struct NexusClient {
    http: reqwest::blocking::Client,
    api_key: String,
    base: String,
}

impl NexusClient {
    pub fn new(api_key: &str, app_version: &str) -> Result<Self> {
        let base = std::env::var(API_ENV).unwrap_or_else(|_| API.to_string());
        Self::with_base_url(api_key, app_version, &base)
    }

    /// A client for another server with the same API (tests).
    pub fn with_base_url(api_key: &str, app_version: &str, base: &str) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .user_agent(format!("{APP_NAME}/{app_version}"))
            .connect_timeout(Duration::from_secs(15))
            .timeout(None)
            .build()
            .map_err(|e| Error::Nexus(e.to_string()))?;
        Ok(Self {
            http,
            api_key: api_key.trim().to_string(),
            base: base.trim_end_matches('/').to_string(),
        })
    }

    fn get<T: for<'de> Deserialize<'de>>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let response = self
            .http
            .get(format!("{}{path}", self.base))
            .header("apikey", &self.api_key)
            .header("Application-Name", APP_NAME)
            .header("Application-Version", env!("CARGO_PKG_VERSION"))
            .query(query)
            .timeout(Duration::from_secs(30))
            .send()
            .map_err(|e| {
                Error::Network(if e.is_timeout() {
                    "Nexus Mods didn't answer in time. Check your internet connection and try again.".into()
                } else {
                    format!("Couldn't reach Nexus Mods. Check your internet connection. ({e})")
                })
            })?;
        if let Some(limits) = RateLimits::from_headers(response.headers(), crate::library::now()) {
            *RATE_LIMITS.lock().unwrap_or_else(|p| p.into_inner()) = Some(limits);
        }
        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            let message = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
                .unwrap_or(body);
            if status.as_u16() == 429 {
                return Err(Error::Network(
                    "Nexus Mods rate limit reached: you've used this hour's requests. Try again after the hour; Settings shows when they reset.".into(),
                ));
            }
            if status.is_server_error() {
                return Err(Error::Network(format!(
                    "Nexus Mods had a problem ({status}). Try again in a few minutes."
                )));
            }
            return Err(Error::Nexus(match status.as_u16() {
                401 => "Nexus Mods didn't accept this API key. Copy the whole Personal API Key from your Nexus account page and try again.".into(),
                403 => format!("Nexus Mods refused the request: {message}"),
                404 => "Not found on Nexus Mods.".into(),
                code => format!("Nexus Mods error {code}: {message}"),
            }));
        }
        response
            .json()
            .map_err(|e| Error::Nexus(format!("unexpected response from Nexus Mods: {e}")))
    }

    pub fn validate(&self) -> Result<NexusUser> {
        self.get("/users/validate.json", &[])
    }

    pub fn mod_info(&self, game: &str, mod_id: u32) -> Result<NexusModInfo> {
        self.get(&format!("/games/{game}/mods/{mod_id}.json"), &[])
    }

    pub fn file_info(&self, game: &str, mod_id: u32, file_id: u32) -> Result<NexusFile> {
        self.get(
            &format!("/games/{game}/mods/{mod_id}/files/{file_id}.json"),
            &[],
        )
    }

    pub fn mod_files(&self, game: &str, mod_id: u32) -> Result<NexusFileList> {
        self.get(&format!("/games/{game}/mods/{mod_id}/files.json"), &[])
    }

    /// Mods of a game with new files or changes in the last `period`
    /// (`1d`, `1w` or `1m`).
    pub fn updated_mods(&self, game: &str, period: &str) -> Result<Vec<UpdateEntry>> {
        self.get(
            &format!("/games/{game}/mods/updated.json"),
            &[("period", period.to_string())],
        )
    }

    /// Files with this MD5 hash; empty when Nexus doesn't know it.
    pub fn md5_search(&self, game: &str, md5: &str) -> Result<Vec<Md5Match>> {
        match self.get(&format!("/games/{game}/mods/md5_search/{md5}.json"), &[]) {
            Err(Error::Nexus(m)) if m == "Not found on Nexus Mods." => Ok(Vec::new()),
            other => other,
        }
    }

    /// A mod's changelog: version -> lines.
    pub fn changelogs(&self, game: &str, mod_id: u32) -> Result<Changelog> {
        self.get(&format!("/games/{game}/mods/{mod_id}/changelogs.json"), &[])
    }

    /// A download URL. Free accounts need the key and expiry from an nxm link.
    pub fn download_url(
        &self,
        game: &str,
        mod_id: u32,
        file_id: u32,
        key: Option<&str>,
        expires: Option<u64>,
    ) -> Result<String> {
        let mut query = Vec::new();
        if let (Some(key), Some(expires)) = (key, expires) {
            query.push(("key", key.to_string()));
            query.push(("expires", expires.to_string()));
        }
        let links: Vec<DownloadLink> = self.get(
            &format!("/games/{game}/mods/{mod_id}/files/{file_id}/download_link.json"),
            &query,
        )?;
        links
            .into_iter()
            .next()
            .map(|l| l.uri)
            .ok_or_else(|| Error::Nexus("Nexus Mods returned no download servers.".into()))
    }

    /// Downloads `url` to `dest`, calling `progress(done, total)` as bytes
    /// arrive, and hashes the bytes on the way. Writes to `dest` + `.part`
    /// first so a failed download never looks complete; a `.part` left by an
    /// interrupted attempt is continued with an HTTP range request when the
    /// server supports it. Network problems are [`Error::Network`].
    /// `progress` returns false to stop (the `.part` file stays).
    pub fn download(
        &self,
        url: &str,
        dest: &Path,
        mut progress: impl FnMut(u64, Option<u64>) -> bool,
    ) -> Result<crate::archives::Hashes> {
        let part = part_path(dest);
        let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let mut request = self.http.get(url);
        if have > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let mut response = request.send().map_err(|e| {
            Error::Network(format!(
                "Couldn't reach the download server ({e}). It is retried automatically."
            ))
        })?;
        let status = response.status();
        let resuming = have > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
        if !status.is_success() {
            let message = format!("The download server answered {status}.");
            return Err(if status.is_server_error() || status.as_u16() == 429 {
                Error::Network(format!("{message} It is retried automatically."))
            } else {
                Error::Nexus(format!(
                    "{message} The download link may have expired; start the download again from Nexus Mods."
                ))
            });
        }
        let mut hasher = crate::archives::Hasher::default();
        let mut file = if resuming {
            // The bytes already there count towards the hashes.
            let mut existing = fs::File::open(&part).map_err(|e| Error::io(&part, e))?;
            let mut buffer = vec![0u8; 256 * 1024];
            loop {
                let n = existing
                    .read(&mut buffer)
                    .map_err(|e| Error::io(&part, e))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            fs::OpenOptions::new()
                .append(true)
                .open(&part)
                .map_err(|e| Error::io(&part, e))?
        } else {
            fs::File::create(&part).map_err(|e| Error::io(&part, e))?
        };
        let offset = if resuming { have } else { 0 };
        let total = response.content_length().map(|len| len + offset);
        let mut buffer = vec![0u8; 256 * 1024];
        let mut done = offset;
        loop {
            let n = response.read(&mut buffer).map_err(|e| {
                Error::Network(format!(
                    "The download was interrupted ({e}). It continues where it stopped."
                ))
            })?;
            if n == 0 {
                break;
            }
            file.write_all(&buffer[..n])
                .map_err(|e| Error::io(&part, e))?;
            hasher.update(&buffer[..n]);
            done += n as u64;
            if !progress(done, total) {
                return Err(Error::Nexus(CANCELLED.into()));
            }
        }
        file.flush().map_err(|e| Error::io(&part, e))?;
        drop(file);
        if total.is_some_and(|t| t != done) {
            return Err(Error::Network(
                "The download ended early. It continues where it stopped.".into(),
            ));
        }
        fs::rename(&part, dest).map_err(|e| Error::io(dest, e))?;
        Ok(hasher.finish())
    }
}

/// The error message of a download stopped by the user.
pub const CANCELLED: &str = "Download cancelled.";

/// Where a download is written until it is complete.
pub fn part_path(dest: &Path) -> std::path::PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    dest.with_file_name(name)
}

/// The API key in the OS credential store (Keychain, Credential Manager,
/// Secret Service), never in plain files.
pub mod api_key {
    use std::sync::Mutex;

    use super::*;

    /// The key after the first successful read. The OS may ask the user
    /// before letting the app read the keychain (always for unsigned dev
    /// builds), so it is read at most once per run. Holding the lock during
    /// the read also stops parallel callers from each triggering a prompt.
    static CACHE: Mutex<Option<Option<String>>> = Mutex::new(None);

    fn cache() -> std::sync::MutexGuard<'static, Option<Option<String>>> {
        CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn entry() -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| Error::Nexus(format!("credential store unavailable: {e}")))
    }

    pub fn load() -> Result<Option<String>> {
        let mut cache = cache();
        if let Some(cached) = cache.as_ref() {
            return Ok(cached.clone());
        }
        let key = match entry()?.get_password() {
            Ok(key) => Some(key),
            Err(keyring::Error::NoEntry) => None,
            // Not cached, so the next attempt asks again.
            Err(e) => {
                return Err(Error::Nexus(format!(
                    "The app couldn't read your Nexus API key from the system keychain. \
                     If a password prompt was declined, restart the app and choose Allow. ({e})"
                )));
            }
        };
        *cache = Some(key.clone());
        Ok(key)
    }

    pub fn save(key: &str) -> Result<()> {
        let key = key.trim();
        entry()?
            .set_password(key)
            .map_err(|e| Error::Nexus(format!("couldn't save the API key: {e}")))?;
        *cache() = Some(Some(key.to_string()));
        Ok(())
    }

    pub fn delete() -> Result<()> {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {
                *cache() = Some(None);
                Ok(())
            }
            Err(e) => Err(Error::Nexus(format!("couldn't remove the API key: {e}"))),
        }
    }
}

/// What a downloaded archive's file name tells us about the mod.
#[derive(Debug, PartialEq)]
pub struct ArchiveName {
    pub name: String,
    pub nexus_mod_id: Option<u32>,
    pub version: Option<String>,
}

/// Understands both Nexus download name formats:
/// - `Cool Mod-1234-1-2-1700000000.zip` (name, mod id, version, timestamp)
/// - `Cool Mod 1234 1.2 2026-10-22T12-57Z Q6xF6lly7.zip` (name, mod id,
///   version, ISO timestamp, random code)
///
/// Anything else is used as the name, with `_` read as spaces.
pub fn parse_archive_name(file_name: &str) -> ArchiveName {
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_name.to_string());

    let tokens: Vec<&str> = stem.split(' ').filter(|t| !t.is_empty()).collect();
    if let [rest @ .., mod_id, version, timestamp, _code] = tokens.as_slice()
        && !rest.is_empty()
        && is_iso_timestamp(timestamp)
        && is_version(version)
        && let Ok(mod_id) = mod_id.parse()
    {
        return ArchiveName {
            name: rest.join(" "),
            nexus_mod_id: Some(mod_id),
            version: Some((*version).to_string()),
        };
    }

    let parts: Vec<&str> = stem.split('-').collect();
    let first_numeric = parts
        .iter()
        .position(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    match first_numeric {
        // name-modid-v-e-r-sion-timestamp: the last part is a Unix timestamp.
        Some(i)
            if i > 0
                && parts.len() >= i + 3
                && parts
                    .last()
                    .is_some_and(|t| t.len() >= 9 && t.chars().all(|c| c.is_ascii_digit())) =>
        {
            let version_parts = &parts[i + 1..parts.len() - 1];
            ArchiveName {
                name: parts[..i].join("-").replace('_', " ").trim().to_string(),
                nexus_mod_id: parts[i].parse().ok(),
                version: (!version_parts.is_empty()).then(|| version_parts.join(".")),
            }
        }
        _ => ArchiveName {
            name: stem.replace('_', " ").trim().to_string(),
            nexus_mod_id: None,
            version: None,
        },
    }
}

/// `2026-10-22T12-57Z` or `2026-10-22T12:57:00Z`.
fn is_iso_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[10] == b'T'
        && s.ends_with('Z')
}

fn is_version(s: &str) -> bool {
    let s = s.strip_prefix(['v', 'V']).unwrap_or(s);
    !s.is_empty()
        && s.chars().next().is_some_and(|c| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

/// A mod and (when it could be told) the exact file an archive came from.
#[derive(Debug, Clone)]
pub struct NexusMatch {
    pub mod_id: u32,
    pub file_id: Option<u32>,
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub picture_url: Option<String>,
}

/// The name to show for a downloaded file: the mod's name for its main
/// file, "Mod: File" for optional files, so variants stay distinguishable.
pub fn display_name(info: &NexusModInfo, file: Option<&NexusFile>) -> Option<String> {
    let mod_name = info.name.clone()?;
    let optional = file.and_then(|f| {
        let category = f.category_name.as_deref().unwrap_or("MAIN");
        (!category.eq_ignore_ascii_case("MAIN"))
            .then(|| f.name.clone())
            .flatten()
    });
    Some(match optional {
        Some(file_name) if !file_name.eq_ignore_ascii_case(&mod_name) => {
            format!("{mod_name}: {file_name}")
        }
        _ => mod_name,
    })
}

/// Picks the file an archive was downloaded as: by exact file name, then by
/// file title and version, then by a version only one file has.
pub fn match_file<'a>(
    files: &'a [NexusFile],
    archive_file_name: &str,
    parsed: &ArchiveName,
) -> Option<&'a NexusFile> {
    let same = |a: &Option<String>, b: &str| {
        a.as_deref()
            .is_some_and(|a| a.trim().eq_ignore_ascii_case(b.trim()))
    };
    if let Some(f) = files.iter().find(|f| same(&f.file_name, archive_file_name)) {
        return Some(f);
    }
    let version = parsed.version.as_deref()?;
    let with_version: Vec<&NexusFile> =
        files.iter().filter(|f| same(&f.version, version)).collect();
    if let Some(f) = with_version.iter().find(|f| same(&f.name, &parsed.name)) {
        return Some(f);
    }
    match with_version.as_slice() {
        [only] => Some(only),
        _ => None,
    }
}

impl NexusClient {
    /// Looks up the mod an archive was downloaded from, using the Nexus mod
    /// id in its file name. `Ok(None)` when the name carries no mod id.
    pub fn identify(
        &self,
        game_domain: &str,
        archive_file_name: &str,
    ) -> Result<Option<NexusMatch>> {
        let parsed = parse_archive_name(archive_file_name);
        let Some(mod_id) = parsed.nexus_mod_id else {
            return Ok(None);
        };
        let info = self.mod_info(game_domain, mod_id)?;
        let files = self.mod_files(game_domain, mod_id)?;
        let file = match_file(&files.files, archive_file_name, &parsed);
        Ok(Some(NexusMatch {
            mod_id,
            file_id: file.map(|f| f.file_id),
            name: display_name(&info, file).unwrap_or(parsed.name.clone()),
            version: file
                .and_then(|f| f.version.clone())
                .or(parsed.version)
                .or(info.version),
            author: info.author,
            summary: info.summary,
            picture_url: info.picture_url,
        }))
    }
}

impl NexusClient {
    /// Finds the file an archive was downloaded as by its MD5 hash, among
    /// the files of `game_domain`.
    pub fn identify_by_md5(&self, game_domain: &str, md5: &str) -> Result<Option<NexusMatch>> {
        let found = self.md5_search(game_domain, md5)?;
        let Some(hit) = found.into_iter().find(|m| {
            m.mod_info
                .domain_name
                .as_deref()
                .is_none_or(|d| d.eq_ignore_ascii_case(game_domain))
        }) else {
            return Ok(None);
        };
        let file = hit.file_details;
        Ok(Some(NexusMatch {
            mod_id: hit.mod_info.mod_id,
            file_id: Some(file.file_id),
            name: display_name(&hit.mod_info, Some(&file))
                .unwrap_or_else(|| format!("Mod {}", hit.mod_info.mod_id)),
            version: file.version.clone().or(hit.mod_info.version.clone()),
            author: hit.mod_info.author,
            summary: hit.mod_info.summary,
            picture_url: hit.mod_info.picture_url,
        }))
    }
}

/// The page of a mod on the Nexus website.
pub fn mod_page_url(game_domain: &str, mod_id: u32) -> String {
    format!("https://www.nexusmods.com/{game_domain}/mods/{mod_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nxm_links() {
        let link = parse_nxm(
            "nxm://cyberpunk2077/mods/107/files/12345?key=abc&expires=1700000000&user_id=9",
        )
        .unwrap();
        assert_eq!(
            link,
            NxmLink {
                game_domain: "cyberpunk2077".into(),
                mod_id: 107,
                file_id: 12345,
                key: Some("abc".into()),
                expires: Some(1700000000),
            }
        );
        assert!(parse_nxm("https://nexusmods.com").is_err());
        assert!(
            parse_nxm("nxm://cyberpunk2077/collections/abc/revisions/1")
                .unwrap_err()
                .to_string()
                .contains("Collections")
        );
    }

    #[test]
    fn names_from_nexus_archives() {
        let parsed = parse_archive_name(
            "CET 1.38.0 - Game version 2.5 107 1.38.0 2026-10-22T12-57Z Q6xF6lly7.zip",
        );
        assert_eq!(
            parsed,
            ArchiveName {
                name: "CET 1.38.0 - Game version 2.5".into(),
                nexus_mod_id: Some(107),
                version: Some("1.38.0".into()),
            }
        );
        let parsed = parse_archive_name("Cool Mod-1234-1-2-1700000000.zip");
        assert_eq!(parsed.name, "Cool Mod");
        assert_eq!(parsed.nexus_mod_id, Some(1234));
        assert_eq!(parsed.version.as_deref(), Some("1.2"));
        let plain = parse_archive_name("Just_A_Mod.7z");
        assert_eq!(plain.name, "Just A Mod");
        assert_eq!(plain.nexus_mod_id, None);
        assert_eq!(parse_archive_name("Mod-2-final.zip").name, "Mod-2-final");
    }

    #[test]
    fn matches_downloaded_file() {
        let files: Vec<NexusFile> = serde_json::from_str(
            r#"[{"file_id":10,"name":"CET 1.37.0","version":"1.37.0","file_name":"old.zip","category_name":"OLD_VERSION"},
                {"file_id":11,"name":"CET 1.38.0 - Game version 2.5","version":"1.38.0","file_name":"cet.zip","category_name":"MAIN"},
                {"file_id":12,"name":"CET source","version":"1.38.0","file_name":"src.zip","category_name":"MISCELLANEOUS"}]"#,
        )
        .unwrap();
        let archive = "CET 1.38.0 - Game version 2.5 107 1.38.0 2026-10-22T12-57Z Q6xF6lly7.zip";
        let parsed = parse_archive_name(archive);
        assert_eq!(match_file(&files, archive, &parsed).unwrap().file_id, 11);
        assert_eq!(match_file(&files, "src.zip", &parsed).unwrap().file_id, 12);
        let unknown = parse_archive_name("Thing 107 9.9 2026-10-22T12-57Z abc.zip");
        assert!(match_file(&files, "x.zip", &unknown).is_none());
    }

    #[test]
    fn names_optional_files_after_the_mod() {
        let info: NexusModInfo =
            serde_json::from_str(r#"{"mod_id":1,"name":"HD Reworked"}"#).unwrap();
        let main: NexusFile =
            serde_json::from_str(r#"{"file_id":1,"name":"HD Ultra","category_name":"MAIN"}"#)
                .unwrap();
        let optional: NexusFile =
            serde_json::from_str(r#"{"file_id":2,"name":"Balanced","category_name":"OPTIONAL"}"#)
                .unwrap();
        assert_eq!(display_name(&info, Some(&main)).unwrap(), "HD Reworked");
        assert_eq!(
            display_name(&info, Some(&optional)).unwrap(),
            "HD Reworked: Balanced"
        );
    }

    #[test]
    fn follows_update_chain() {
        let list: NexusFileList = serde_json::from_str(
            r#"{"files":[{"file_id":1,"version":"1"},{"file_id":2,"version":"2"},{"file_id":3,"version":"3"}],
                "file_updates":[{"old_file_id":1,"new_file_id":2},{"old_file_id":2,"new_file_id":3}]}"#,
        )
        .unwrap();
        assert_eq!(list.newer_than(1).unwrap().file_id, 3);
        assert!(list.newer_than(3).is_none());
    }
}
