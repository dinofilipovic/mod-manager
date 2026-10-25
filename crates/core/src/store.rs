//! SQLite persistence for the mod library, profiles and deployment state.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use ts_rs::TS;

use crate::archives::{ArchiveRecord, ArchiveSource};
use crate::error::Result;
use crate::nexus::Changelog;

pub const SCHEMA_VERSION: i32 = 3;

/// The schema at version 2. New databases start from it and run the
/// migrations after it, so every database ends up with the same tables.
const BASE_VERSION: i32 = 2;

pub(crate) const BASE_SCHEMA: &str = "
CREATE TABLE mods (
    id TEXT PRIMARY KEY,
    game_id TEXT NOT NULL,
    name TEXT NOT NULL,
    version TEXT,
    author TEXT,
    summary TEXT,
    picture_url TEXT,
    nexus_mod_id INTEGER,
    nexus_file_id INTEGER,
    archive_name TEXT,
    kinds TEXT NOT NULL DEFAULT '[]',
    file_count INTEGER NOT NULL,
    size_bytes INTEGER NOT NULL,
    installed_at INTEGER NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    update_file_id INTEGER,
    update_version TEXT
);
CREATE INDEX mods_game ON mods(game_id);

CREATE TABLE mod_files (
    mod_id TEXT NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
    target TEXT NOT NULL,
    PRIMARY KEY (mod_id, target)
);

CREATE TABLE profiles (
    id TEXT PRIMARY KEY,
    instance_id TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE profile_mods (
    profile_id TEXT NOT NULL REFERENCES profiles(id) ON DELETE CASCADE,
    mod_id TEXT NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL,
    priority INTEGER NOT NULL,
    PRIMARY KEY (profile_id, mod_id)
);

CREATE TABLE instance_state (
    instance_id TEXT PRIMARY KEY,
    active_profile_id TEXT,
    deployed_at INTEGER
);

CREATE TABLE deployed_files (
    instance_id TEXT NOT NULL,
    target TEXT NOT NULL,
    mod_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    backup TEXT,
    PRIMARY KEY (instance_id, target)
);

CREATE TABLE game_state (
    instance_id TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    first_seen INTEGER NOT NULL,
    updated_at INTEGER
);

CREATE TABLE settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Upgrades from older databases: `(version, SQL)` runs when the database is
/// at that version or older.
const MIGRATIONS: &[(i32, &str)] = &[
    (
        1,
        "CREATE TABLE game_state (
            instance_id TEXT PRIMARY KEY,
            fingerprint TEXT NOT NULL,
            first_seen INTEGER NOT NULL,
            updated_at INTEGER
        );",
    ),
    (2, MIGRATION_3),
];

/// Version 3: deployment journal, file hashes, the archive store, mod
/// revisions, update tracking and the download queue.
const MIGRATION_3: &str = "
ALTER TABLE mods ADD COLUMN nexus_updated_at INTEGER;
ALTER TABLE mods ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
ALTER TABLE mods ADD COLUMN ignored_update_file_id INTEGER;

CREATE TABLE changelogs (
    game_domain TEXT NOT NULL,
    nexus_mod_id INTEGER NOT NULL,
    fetched_at INTEGER NOT NULL,
    entries TEXT NOT NULL,
    PRIMARY KEY (game_domain, nexus_mod_id)
);

ALTER TABLE mod_files ADD COLUMN size INTEGER;
ALTER TABLE mod_files ADD COLUMN sha256 TEXT;

ALTER TABLE deployed_files ADD COLUMN size INTEGER;
ALTER TABLE deployed_files ADD COLUMN mtime INTEGER;
ALTER TABLE deployed_files ADD COLUMN sha256 TEXT;
ALTER TABLE deployed_files ADD COLUMN backup_sha256 TEXT;
ALTER TABLE deployed_files ADD COLUMN placed_at INTEGER;

CREATE TABLE deploy_journal (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instance_id TEXT NOT NULL,
    game_root TEXT NOT NULL,
    op TEXT NOT NULL,
    target TEXT NOT NULL,
    source TEXT,
    mod_id TEXT,
    revision INTEGER,
    sha256 TEXT,
    backup TEXT,
    moved_original INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE archives (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    sha256 TEXT NOT NULL UNIQUE,
    md5 TEXT NOT NULL,
    size INTEGER NOT NULL,
    path TEXT NOT NULL,
    original_name TEXT,
    nexus_mod_id INTEGER,
    nexus_file_id INTEGER,
    source TEXT NOT NULL,
    added_at INTEGER NOT NULL
);

CREATE TABLE mod_revisions (
    mod_id TEXT NOT NULL REFERENCES mods(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL,
    archive_id INTEGER REFERENCES archives(id) ON DELETE SET NULL,
    fomod_choices TEXT,
    version TEXT,
    nexus_file_id INTEGER,
    installed_at INTEGER NOT NULL,
    PRIMARY KEY (mod_id, revision)
);
";

/// An installed mod in the library. Mods belong to a game and are shared by
/// all installs (instances) of that game.
#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct ModRecord {
    pub id: String,
    pub game_id: String,
    pub name: String,
    pub version: Option<String>,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub picture_url: Option<String>,
    pub nexus_mod_id: Option<u32>,
    pub nexus_file_id: Option<u32>,
    pub archive_name: Option<String>,
    pub kinds: Vec<String>,
    pub file_count: u32,
    #[ts(type = "number")]
    pub size_bytes: u64,
    /// Unix seconds.
    #[ts(type = "number")]
    pub installed_at: i64,
    /// Bumped when the mod's files are replaced, so deployment refreshes them.
    pub revision: u32,
    /// Newer Nexus file, set by the update check.
    pub update_file_id: Option<u32>,
    pub update_version: Option<String>,
    /// The newest file upload seen on Nexus at the last check, Unix seconds.
    #[ts(type = "number | null")]
    pub nexus_updated_at: Option<i64>,
    /// Kept at this version: update checks skip it.
    pub pinned: bool,
    /// A newer file the user chose not to install.
    pub ignored_update_file_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
pub struct Profile {
    pub id: String,
    pub instance_id: String,
    pub name: String,
}

/// A mod's place in a profile.
#[derive(Debug, Clone)]
pub struct ProfileEntry {
    pub mod_id: String,
    pub enabled: bool,
    pub priority: i64,
}

#[derive(Debug, Clone, Default)]
pub struct DeployedFile {
    pub target: String,
    pub mod_id: String,
    pub revision: u32,
    /// Path of the original game file moved aside, relative to the backup folder.
    pub backup: Option<String>,
    /// The placed file as it was right after placing it. `None` for files
    /// deployed before version 3, which can't be checked for drift.
    pub size: Option<u64>,
    /// Nanoseconds since the epoch.
    pub mtime: Option<i64>,
    pub sha256: Option<String>,
    pub backup_sha256: Option<String>,
    /// Unix seconds.
    pub placed_at: Option<i64>,
}

/// One filesystem operation of a deployment, recorded before it starts.
#[derive(Debug, Clone, PartialEq)]
pub struct JournalEntry {
    pub id: i64,
    pub instance_id: String,
    pub game_root: String,
    pub op: JournalOp,
    pub target: String,
    /// Library file placed by `Add` and `Replace`.
    pub source: Option<String>,
    pub mod_id: Option<String>,
    pub revision: Option<u32>,
    pub sha256: Option<String>,
    /// Backup path relative to the instance's backup folder (`Add`).
    pub backup: Option<String>,
    /// `Add` moves the original game file to `backup` first.
    pub moved_original: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalOp {
    /// Place a new file, moving an original game file aside first.
    Add,
    /// Put another mod's file (or a newer revision) where a deployed one is.
    Replace,
    /// Take a deployed file away and put the original back.
    Remove,
    /// Keep the file the game put over a deployed one; drop the stale backup.
    Adopt,
}

impl JournalOp {
    fn as_str(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Replace => "replace",
            Self::Remove => "remove",
            Self::Adopt => "adopt",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "add" => Self::Add,
            "replace" => Self::Replace,
            "remove" => Self::Remove,
            _ => Self::Adopt,
        }
    }
}

/// What finishing a journal entry does to the deployment record.
pub enum DeployedChange<'a> {
    Upsert(&'a DeployedFile),
    Delete(&'a str),
    Nothing,
}

/// One install of a mod: which archive it came from and the installer
/// options chosen, so it can be installed again exactly.
#[derive(Debug, Clone, PartialEq, Serialize, TS)]
#[ts(export)]
pub struct ModRevision {
    pub mod_id: String,
    pub revision: u32,
    /// `None` once the archive was deleted (retention or cleanup).
    #[ts(type = "number | null")]
    pub archive_id: Option<i64>,
    pub fomod_choices: Option<crate::fomod::FomodChoices>,
    pub version: Option<String>,
    /// The Nexus file this revision was, so rolling back restores it.
    pub nexus_file_id: Option<u32>,
    #[ts(type = "number")]
    pub installed_at: i64,
}

/// A mod file with its size and hash, recorded at install.
#[derive(Debug, Clone, PartialEq)]
pub struct ModFile {
    pub target: String,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        // The deploy journal's intent rows must be on disk before the file
        // operation they describe, also across a power loss.
        conn.pragma_update(None, "synchronous", "FULL")?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            // One transaction: a crash leaves the database as it was.
            let tx = conn.unchecked_transaction()?;
            let version = if version == 0 {
                tx.execute_batch(BASE_SCHEMA)?;
                BASE_VERSION
            } else {
                version
            };
            for (from, migration) in MIGRATIONS {
                if version <= *from {
                    tx.execute_batch(migration)?;
                }
            }
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    /// The schema version of the database at `path`, without changing it;
    /// `None` for a new database.
    pub fn version_of(path: &Path) -> Result<Option<i32>> {
        if !path.is_file() {
            return Ok(None);
        }
        let conn = Connection::open(path)?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        Ok((version > 0).then_some(version))
    }

    /// Writes a consistent copy of the database file at `src` to `dest`,
    /// including what is still in its write-ahead log, without migrating it.
    pub fn backup_file(src: &Path, dest: &Path) -> Result<()> {
        let conn = Connection::open(src)?;
        conn.execute("VACUUM INTO ?1", [dest.to_string_lossy()])?;
        Ok(())
    }

    // Mods

    pub fn insert_mod(&mut self, record: &ModRecord, files: &[ModFile]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO mods (id, game_id, name, version, author, summary, picture_url, nexus_mod_id,
                nexus_file_id, archive_name, kinds, file_count, size_bytes, installed_at, revision,
                update_file_id, update_version, nexus_updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
             ON CONFLICT(id) DO UPDATE SET name = ?3, version = ?4, author = ?5, summary = ?6,
                picture_url = ?7, nexus_mod_id = ?8, nexus_file_id = ?9, archive_name = ?10, kinds = ?11,
                file_count = ?12, size_bytes = ?13, installed_at = ?14, revision = ?15,
                update_file_id = ?16, update_version = ?17, nexus_updated_at = ?18",
            params![
                record.id,
                record.game_id,
                record.name,
                record.version,
                record.author,
                record.summary,
                record.picture_url,
                record.nexus_mod_id,
                record.nexus_file_id,
                record.archive_name,
                serde_json::to_string(&record.kinds).unwrap_or_else(|_| "[]".into()),
                record.file_count,
                record.size_bytes as i64,
                record.installed_at,
                record.revision,
                record.update_file_id,
                record.update_version,
                record.nexus_updated_at,
            ],
        )?;
        tx.execute("DELETE FROM mod_files WHERE mod_id = ?1", [&record.id])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO mod_files (mod_id, target, size, sha256) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for f in files {
                stmt.execute(params![
                    record.id,
                    f.target,
                    f.size.map(|s| s as i64),
                    f.sha256
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn mods_for_game(&self, game_id: &str) -> Result<Vec<ModRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM mods WHERE game_id = ?1 ORDER BY installed_at")?;
        let rows = stmt.query_map([game_id], row_to_mod)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn get_mod(&self, id: &str) -> Result<Option<ModRecord>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM mods WHERE id = ?1", [id], row_to_mod)
            .optional()?)
    }

    pub fn find_mod_by_nexus_id(
        &self,
        game_id: &str,
        nexus_mod_id: u32,
    ) -> Result<Option<ModRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM mods WHERE game_id = ?1 AND nexus_mod_id = ?2 ORDER BY installed_at DESC LIMIT 1",
                params![game_id, nexus_mod_id],
                row_to_mod,
            )
            .optional()?)
    }

    pub fn mod_files(&self, mod_id: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT target FROM mod_files WHERE mod_id = ?1 ORDER BY target")?;
        let rows = stmt.query_map([mod_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// A mod's files with the size and hash recorded at install.
    pub fn mod_file_details(&self, mod_id: &str) -> Result<Vec<ModFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT target, size, sha256 FROM mod_files WHERE mod_id = ?1 ORDER BY target",
        )?;
        let rows = stmt.query_map([mod_id], |r| {
            Ok(ModFile {
                target: r.get(0)?,
                size: r.get::<_, Option<i64>>(1)?.map(|s| s as u64),
                sha256: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_mod(&self, id: &str) -> Result<()> {
        self.conn.execute("DELETE FROM mods WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Links a mod to its Nexus page and file, without touching its files.
    #[allow(clippy::too_many_arguments)]
    pub fn set_nexus_metadata(
        &self,
        id: &str,
        name: &str,
        version: Option<&str>,
        author: Option<&str>,
        summary: Option<&str>,
        picture_url: Option<&str>,
        nexus_mod_id: u32,
        nexus_file_id: Option<u32>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE mods SET name = ?2, version = COALESCE(?3, version), author = ?4, summary = ?5,
                picture_url = ?6, nexus_mod_id = ?7, nexus_file_id = ?8 WHERE id = ?1",
            params![
                id,
                name,
                version,
                author,
                summary,
                picture_url,
                nexus_mod_id,
                nexus_file_id
            ],
        )?;
        Ok(())
    }

    /// Records what an update check found for a mod.
    pub fn set_mod_checked(
        &self,
        id: &str,
        nexus_updated_at: Option<i64>,
        update: Option<(u32, Option<&str>)>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE mods SET nexus_updated_at = COALESCE(?2, nexus_updated_at),
                update_file_id = ?3, update_version = ?4 WHERE id = ?1",
            params![
                id,
                nexus_updated_at,
                update.map(|u| u.0),
                update.and_then(|u| u.1)
            ],
        )?;
        Ok(())
    }

    pub fn set_mod_pinned(&self, id: &str, pinned: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE mods SET pinned = ?2,
                update_file_id = CASE WHEN ?2 THEN NULL ELSE update_file_id END,
                update_version = CASE WHEN ?2 THEN NULL ELSE update_version END
             WHERE id = ?1",
            params![id, pinned],
        )?;
        Ok(())
    }

    /// Skips the pending update file (and the update), or forgets a skip.
    pub fn ignore_mod_update(&self, id: &str, file_id: Option<u32>) -> Result<()> {
        self.conn.execute(
            "UPDATE mods SET ignored_update_file_id = ?2,
                update_file_id = CASE WHEN update_file_id = ?2 THEN NULL ELSE update_file_id END,
                update_version = CASE WHEN update_file_id = ?2 THEN NULL ELSE update_version END
             WHERE id = ?1",
            params![id, file_id],
        )?;
        Ok(())
    }

    // Changelogs

    pub fn changelog(
        &self,
        game_domain: &str,
        nexus_mod_id: u32,
    ) -> Result<Option<(i64, Changelog)>> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT fetched_at, entries FROM changelogs WHERE game_domain = ?1 AND nexus_mod_id = ?2",
                params![game_domain, nexus_mod_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.map(|(at, json)| (at, serde_json::from_str(&json).unwrap_or_default())))
    }

    pub fn set_changelog(
        &self,
        game_domain: &str,
        nexus_mod_id: u32,
        fetched_at: i64,
        entries: &Changelog,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO changelogs (game_domain, nexus_mod_id, fetched_at, entries) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(game_domain, nexus_mod_id) DO UPDATE SET fetched_at = ?3, entries = ?4",
            params![
                game_domain,
                nexus_mod_id,
                fetched_at,
                serde_json::to_string(entries).unwrap_or_else(|_| "{}".into())
            ],
        )?;
        Ok(())
    }

    pub fn rename_mod(&self, id: &str, name: &str) -> Result<()> {
        self.conn
            .execute("UPDATE mods SET name = ?2 WHERE id = ?1", params![id, name])?;
        Ok(())
    }

    pub fn set_mod_update(
        &self,
        id: &str,
        file_id: Option<u32>,
        version: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE mods SET update_file_id = ?2, update_version = ?3 WHERE id = ?1",
            params![id, file_id, version],
        )?;
        Ok(())
    }

    // Profiles

    pub fn profiles(&self, instance_id: &str) -> Result<Vec<Profile>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, instance_id, name FROM profiles WHERE instance_id = ?1 ORDER BY created_at, name",
        )?;
        let rows = stmt.query_map([instance_id], |r| {
            Ok(Profile {
                id: r.get(0)?,
                instance_id: r.get(1)?,
                name: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn insert_profile(&self, profile: &Profile, created_at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO profiles (id, instance_id, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![profile.id, profile.instance_id, profile.name, created_at],
        )?;
        Ok(())
    }

    pub fn rename_profile(&self, id: &str, name: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE profiles SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        Ok(())
    }

    pub fn delete_profile(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM profiles WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn profile_entries(&self, profile_id: &str) -> Result<Vec<ProfileEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT mod_id, enabled, priority FROM profile_mods WHERE profile_id = ?1 ORDER BY priority",
        )?;
        let rows = stmt.query_map([profile_id], |r| {
            Ok(ProfileEntry {
                mod_id: r.get(0)?,
                enabled: r.get(1)?,
                priority: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Replaces all entries of a profile.
    pub fn set_profile_entries(
        &mut self,
        profile_id: &str,
        entries: &[ProfileEntry],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM profile_mods WHERE profile_id = ?1",
            [profile_id],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO profile_mods (profile_id, mod_id, enabled, priority) VALUES (?1, ?2, ?3, ?4)",
            )?;
            for e in entries {
                stmt.execute(params![profile_id, e.mod_id, e.enabled, e.priority])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // Instance state

    pub fn active_profile_id(&self, instance_id: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT active_profile_id FROM instance_state WHERE instance_id = ?1",
                [instance_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    pub fn set_active_profile(&self, instance_id: &str, profile_id: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO instance_state (instance_id, active_profile_id) VALUES (?1, ?2)
             ON CONFLICT(instance_id) DO UPDATE SET active_profile_id = ?2",
            params![instance_id, profile_id],
        )?;
        Ok(())
    }

    pub fn deployed_at(&self, instance_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT deployed_at FROM instance_state WHERE instance_id = ?1",
                [instance_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    pub fn deployed_files(&self, instance_id: &str) -> Result<Vec<DeployedFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT target, mod_id, revision, backup, size, mtime, sha256, backup_sha256, placed_at
             FROM deployed_files WHERE instance_id = ?1 ORDER BY target",
        )?;
        let rows = stmt.query_map([instance_id], row_to_deployed)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn deployed_file(&self, instance_id: &str, target: &str) -> Result<Option<DeployedFile>> {
        Ok(self
            .conn
            .query_row(
                "SELECT target, mod_id, revision, backup, size, mtime, sha256, backup_sha256, placed_at
                 FROM deployed_files WHERE instance_id = ?1 AND target = ?2",
                params![instance_id, target],
                row_to_deployed,
            )
            .optional()?)
    }

    /// Replaces the whole deployment record of an install (tests and imports).
    pub fn set_deployed_files(
        &mut self,
        instance_id: &str,
        files: &[DeployedFile],
        at: i64,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM deployed_files WHERE instance_id = ?1",
            [instance_id],
        )?;
        for f in files {
            upsert_deployed(&tx, instance_id, f)?;
        }
        tx.execute(
            "INSERT INTO instance_state (instance_id, deployed_at) VALUES (?1, ?2)
             ON CONFLICT(instance_id) DO UPDATE SET deployed_at = ?2",
            params![instance_id, at],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_deployed_at(&self, instance_id: &str, at: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO instance_state (instance_id, deployed_at) VALUES (?1, ?2)
             ON CONFLICT(instance_id) DO UPDATE SET deployed_at = ?2",
            params![instance_id, at],
        )?;
        Ok(())
    }

    // Deployment journal

    /// Records an operation as pending before it touches the disk.
    pub fn journal_begin(&self, entry: &JournalEntry, now: i64) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO deploy_journal (instance_id, game_root, op, target, source, mod_id, revision,
                sha256, backup, moved_original, state, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'pending', ?11)",
            params![
                entry.instance_id,
                entry.game_root,
                entry.op.as_str(),
                entry.target,
                entry.source,
                entry.mod_id,
                entry.revision,
                entry.sha256,
                entry.backup,
                entry.moved_original,
                now
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Marks an operation done and records its effect, in one transaction.
    pub fn journal_finish(
        &mut self,
        id: i64,
        instance_id: &str,
        change: DeployedChange,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        match change {
            DeployedChange::Upsert(file) => upsert_deployed(&tx, instance_id, file)?,
            DeployedChange::Delete(target) => {
                tx.execute(
                    "DELETE FROM deployed_files WHERE instance_id = ?1 AND target = ?2",
                    params![instance_id, target],
                )?;
            }
            DeployedChange::Nothing => {}
        }
        tx.execute(
            "UPDATE deploy_journal SET state = 'done' WHERE id = ?1",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Operations that started but never finished (the app stopped mid-way).
    pub fn pending_journal(&self) -> Result<Vec<JournalEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, instance_id, game_root, op, target, source, mod_id, revision, sha256, backup,
                moved_original FROM deploy_journal WHERE state = 'pending' ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(JournalEntry {
                id: r.get(0)?,
                instance_id: r.get(1)?,
                game_root: r.get(2)?,
                op: JournalOp::parse(&r.get::<_, String>(3)?),
                target: r.get(4)?,
                source: r.get(5)?,
                mod_id: r.get(6)?,
                revision: r.get(7)?,
                sha256: r.get(8)?,
                backup: r.get(9)?,
                moved_original: r.get(10)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Forgets finished operations.
    pub fn clear_finished_journal(&self) -> Result<()> {
        self.conn
            .execute("DELETE FROM deploy_journal WHERE state = 'done'", [])?;
        Ok(())
    }

    // Game state

    /// When the game was last seen changing (an update), without recording anything.
    pub fn game_updated_at(&self, instance_id: &str) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row(
                "SELECT updated_at FROM game_state WHERE instance_id = ?1",
                [instance_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Records the game's current fingerprint and returns when it last
    /// changed (an update or repair), if a change has been seen.
    pub fn observe_game(
        &self,
        instance_id: &str,
        fingerprint: &str,
        now: i64,
    ) -> Result<Option<i64>> {
        let previous: Option<(String, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT fingerprint, updated_at FROM game_state WHERE instance_id = ?1",
                [instance_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        match previous {
            None => {
                self.conn.execute(
                    "INSERT INTO game_state (instance_id, fingerprint, first_seen) VALUES (?1, ?2, ?3)",
                    params![instance_id, fingerprint, now],
                )?;
                Ok(None)
            }
            Some((old, updated_at)) if old == fingerprint => Ok(updated_at),
            Some(_) => {
                self.conn.execute(
                    "UPDATE game_state SET fingerprint = ?2, updated_at = ?3 WHERE instance_id = ?1",
                    params![instance_id, fingerprint, now],
                )?;
                Ok(Some(now))
            }
        }
    }

    // Archive store

    pub fn insert_archive(&self, a: &ArchiveRecord) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO archives (sha256, md5, size, path, original_name, nexus_mod_id, nexus_file_id,
                source, added_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                a.sha256,
                a.md5,
                a.size as i64,
                a.path,
                a.original_name,
                a.nexus_mod_id,
                a.nexus_file_id,
                a.source.as_str(),
                a.added_at
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Fills in the Nexus file of an archive when it wasn't known yet.
    pub fn link_archive(
        &self,
        id: i64,
        nexus_mod_id: Option<u32>,
        nexus_file_id: Option<u32>,
        original_name: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE archives SET nexus_mod_id = COALESCE(nexus_mod_id, ?2),
                nexus_file_id = COALESCE(nexus_file_id, ?3),
                original_name = COALESCE(original_name, ?4) WHERE id = ?1",
            params![id, nexus_mod_id, nexus_file_id, original_name],
        )?;
        Ok(())
    }

    pub fn archive(&self, id: i64) -> Result<Option<ArchiveRecord>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM archives WHERE id = ?1", [id], row_to_archive)
            .optional()?)
    }

    pub fn archive_by_sha256(&self, sha256: &str) -> Result<Option<ArchiveRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM archives WHERE sha256 = ?1",
                [sha256],
                row_to_archive,
            )
            .optional()?)
    }

    pub fn archives(&self) -> Result<Vec<ArchiveRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM archives ORDER BY added_at, id")?;
        let rows = stmt.query_map([], row_to_archive)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn delete_archive(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM archives WHERE id = ?1", [id])?;
        Ok(())
    }

    // Revisions

    pub fn insert_revision(&self, r: &ModRevision) -> Result<()> {
        self.conn.execute(
            "INSERT INTO mod_revisions (mod_id, revision, archive_id, fomod_choices, version, installed_at,
                nexus_file_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(mod_id, revision) DO UPDATE SET archive_id = ?3, fomod_choices = ?4,
                version = ?5, installed_at = ?6, nexus_file_id = ?7",
            params![
                r.mod_id,
                r.revision,
                r.archive_id,
                r.fomod_choices
                    .as_ref()
                    .and_then(|c| serde_json::to_string(c).ok()),
                r.version,
                r.installed_at,
                r.nexus_file_id
            ],
        )?;
        Ok(())
    }

    /// A mod's revisions, newest first.
    pub fn revisions(&self, mod_id: &str) -> Result<Vec<ModRevision>> {
        let mut stmt = self.conn.prepare(
            "SELECT mod_id, revision, archive_id, fomod_choices, version, installed_at, nexus_file_id
             FROM mod_revisions WHERE mod_id = ?1 ORDER BY revision DESC",
        )?;
        let rows = stmt.query_map([mod_id], row_to_revision)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Every revision of every mod (for working out which archives are used).
    pub fn all_revisions(&self) -> Result<Vec<ModRevision>> {
        let mut stmt = self.conn.prepare(
            "SELECT mod_id, revision, archive_id, fomod_choices, version, installed_at, nexus_file_id
             FROM mod_revisions ORDER BY mod_id, revision DESC",
        )?;
        let rows = stmt.query_map([], row_to_revision)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // Settings

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: Option<&str>) -> Result<()> {
        match value {
            Some(value) => self.conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
                params![key, value],
            )?,
            None => self.conn.execute("DELETE FROM settings WHERE key = ?1", [key])?,
        };
        Ok(())
    }
}

fn row_to_archive(r: &rusqlite::Row) -> rusqlite::Result<ArchiveRecord> {
    Ok(ArchiveRecord {
        id: r.get("id")?,
        sha256: r.get("sha256")?,
        md5: r.get("md5")?,
        size: r.get::<_, i64>("size")? as u64,
        path: r.get("path")?,
        original_name: r.get("original_name")?,
        nexus_mod_id: r.get("nexus_mod_id")?,
        nexus_file_id: r.get("nexus_file_id")?,
        source: ArchiveSource::parse(&r.get::<_, String>("source")?),
        added_at: r.get("added_at")?,
    })
}

fn row_to_revision(r: &rusqlite::Row) -> rusqlite::Result<ModRevision> {
    let choices: Option<String> = r.get(3)?;
    Ok(ModRevision {
        mod_id: r.get(0)?,
        revision: r.get(1)?,
        archive_id: r.get(2)?,
        fomod_choices: choices.and_then(|c| serde_json::from_str(&c).ok()),
        version: r.get(4)?,
        installed_at: r.get(5)?,
        nexus_file_id: r.get(6)?,
    })
}

fn row_to_deployed(r: &rusqlite::Row) -> rusqlite::Result<DeployedFile> {
    Ok(DeployedFile {
        target: r.get(0)?,
        mod_id: r.get(1)?,
        revision: r.get(2)?,
        backup: r.get(3)?,
        size: r.get::<_, Option<i64>>(4)?.map(|s| s as u64),
        mtime: r.get(5)?,
        sha256: r.get(6)?,
        backup_sha256: r.get(7)?,
        placed_at: r.get(8)?,
    })
}

fn upsert_deployed(
    conn: &rusqlite::Connection,
    instance_id: &str,
    f: &DeployedFile,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO deployed_files (instance_id, target, mod_id, revision, backup, size, mtime,
            sha256, backup_sha256, placed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(instance_id, target) DO UPDATE SET mod_id = ?3, revision = ?4, backup = ?5,
            size = ?6, mtime = ?7, sha256 = ?8, backup_sha256 = ?9, placed_at = ?10",
        params![
            instance_id,
            f.target,
            f.mod_id,
            f.revision,
            f.backup,
            f.size.map(|s| s as i64),
            f.mtime,
            f.sha256,
            f.backup_sha256,
            f.placed_at
        ],
    )?;
    Ok(())
}

fn row_to_mod(r: &rusqlite::Row) -> rusqlite::Result<ModRecord> {
    let kinds: String = r.get("kinds")?;
    Ok(ModRecord {
        id: r.get("id")?,
        game_id: r.get("game_id")?,
        name: r.get("name")?,
        version: r.get("version")?,
        author: r.get("author")?,
        summary: r.get("summary")?,
        picture_url: r.get("picture_url")?,
        nexus_mod_id: r.get("nexus_mod_id")?,
        nexus_file_id: r.get("nexus_file_id")?,
        archive_name: r.get("archive_name")?,
        kinds: serde_json::from_str(&kinds).unwrap_or_default(),
        file_count: r.get("file_count")?,
        size_bytes: r.get::<_, i64>("size_bytes")? as u64,
        installed_at: r.get("installed_at")?,
        revision: r.get("revision")?,
        update_file_id: r.get("update_file_id")?,
        update_version: r.get("update_version")?,
        nexus_updated_at: r.get("nexus_updated_at")?,
        pinned: r.get("pinned")?,
        ignored_update_file_id: r.get("ignored_update_file_id")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_game_updates() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(store.observe_game("i", "a", 10).unwrap(), None);
        assert_eq!(store.observe_game("i", "a", 20).unwrap(), None);
        assert_eq!(store.observe_game("i", "b", 30).unwrap(), Some(30));
        assert_eq!(store.observe_game("i", "b", 40).unwrap(), Some(30));
    }

    #[test]
    fn migrates_version_1_databases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(&BASE_SCHEMA.replace(
                "CREATE TABLE game_state (\n    instance_id TEXT PRIMARY KEY,\n    fingerprint TEXT NOT NULL,\n    first_seen INTEGER NOT NULL,\n    updated_at INTEGER\n);\n",
                "",
            ))
            .unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.observe_game("i", "a", 1).unwrap(), None);
    }

    #[test]
    fn migrates_version_2_databases_keeping_all_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(BASE_SCHEMA).unwrap();
            conn.execute_batch(
                "INSERT INTO mods (id, game_id, name, version, nexus_mod_id, nexus_file_id, archive_name,
                    kinds, file_count, size_bytes, installed_at, revision, update_file_id, update_version)
                 VALUES ('m1', 'g', 'Mod One', '1.0', 10, 100, 'one.zip', '[\"archive\"]', 1, 5, 1000, 2, 101, '1.1');
                 INSERT INTO mod_files (mod_id, target) VALUES ('m1', 'archive/pc/mod/one.archive');
                 INSERT INTO profiles (id, instance_id, name, created_at) VALUES ('p1', 'g-1', 'Default', 1);
                 INSERT INTO profile_mods (profile_id, mod_id, enabled, priority) VALUES ('p1', 'm1', 1, 0);
                 INSERT INTO instance_state (instance_id, active_profile_id, deployed_at) VALUES ('g-1', 'p1', 2000);
                 INSERT INTO deployed_files (instance_id, target, mod_id, revision, backup)
                    VALUES ('g-1', 'archive/pc/mod/one.archive', 'm1', 2, NULL);
                 INSERT INTO game_state (instance_id, fingerprint, first_seen, updated_at) VALUES ('g-1', 'fp', 1, 3000);
                 INSERT INTO settings (key, value) VALUES ('archive_order:g-1', 'list');",
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }
        assert_eq!(Store::version_of(&path).unwrap(), Some(2));
        let store = Store::open(&path).unwrap();
        assert_eq!(Store::version_of(&path).unwrap(), Some(SCHEMA_VERSION));

        let m = store.get_mod("m1").unwrap().unwrap();
        assert_eq!(
            (m.name.as_str(), m.version.as_deref(), m.revision),
            ("Mod One", Some("1.0"), 2)
        );
        assert_eq!(
            (m.nexus_mod_id, m.nexus_file_id, m.update_file_id),
            (Some(10), Some(100), Some(101))
        );
        assert_eq!(m.kinds, ["archive"]);
        assert_eq!(
            store.mod_file_details("m1").unwrap(),
            [ModFile {
                target: "archive/pc/mod/one.archive".into(),
                size: None,
                sha256: None
            }]
        );
        assert_eq!(store.profiles("g-1").unwrap()[0].name, "Default");
        assert_eq!(store.profile_entries("p1").unwrap()[0].mod_id, "m1");
        assert_eq!(
            store.active_profile_id("g-1").unwrap().as_deref(),
            Some("p1")
        );
        assert_eq!(store.deployed_at("g-1").unwrap(), Some(2000));
        let deployed = store.deployed_files("g-1").unwrap();
        assert_eq!(
            (deployed[0].mod_id.as_str(), deployed[0].revision),
            ("m1", 2)
        );
        assert_eq!(deployed[0].size, None, "old rows have no metadata to check");
        assert_eq!(store.game_updated_at("g-1").unwrap(), Some(3000));
        assert_eq!(
            store.setting("archive_order:g-1").unwrap().as_deref(),
            Some("list")
        );
        assert!(store.pending_journal().unwrap().is_empty());
    }
}
