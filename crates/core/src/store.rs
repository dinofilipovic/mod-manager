//! SQLite persistence for the mod library, profiles and deployment state.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use ts_rs::TS;

use crate::error::Result;

const SCHEMA_VERSION: i32 = 2;

const SCHEMA: &str = "
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
const MIGRATIONS: &[(i32, &str)] = &[(
    1,
    "CREATE TABLE game_state (
        instance_id TEXT PRIMARY KEY,
        fingerprint TEXT NOT NULL,
        first_seen INTEGER NOT NULL,
        updated_at INTEGER
    );",
)];

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

#[derive(Debug, Clone)]
pub struct DeployedFile {
    pub target: String,
    pub mod_id: String,
    pub revision: u32,
    /// Path of the original game file moved aside, relative to the backup folder.
    pub backup: Option<String>,
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
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version == 0 {
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if version < SCHEMA_VERSION {
            for (from, migration) in MIGRATIONS {
                if version <= *from {
                    conn.execute_batch(migration)?;
                }
            }
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        Ok(Self { conn })
    }

    // Mods

    pub fn insert_mod(&mut self, record: &ModRecord, files: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO mods (id, game_id, name, version, author, summary, picture_url, nexus_mod_id,
                nexus_file_id, archive_name, kinds, file_count, size_bytes, installed_at, revision,
                update_file_id, update_version)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET name = ?3, version = ?4, author = ?5, summary = ?6,
                picture_url = ?7, nexus_mod_id = ?8, nexus_file_id = ?9, archive_name = ?10, kinds = ?11,
                file_count = ?12, size_bytes = ?13, installed_at = ?14, revision = ?15,
                update_file_id = ?16, update_version = ?17",
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
            ],
        )?;
        tx.execute("DELETE FROM mod_files WHERE mod_id = ?1", [&record.id])?;
        {
            let mut stmt = tx.prepare("INSERT INTO mod_files (mod_id, target) VALUES (?1, ?2)")?;
            for target in files {
                stmt.execute(params![record.id, target])?;
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
            "SELECT target, mod_id, revision, backup FROM deployed_files WHERE instance_id = ?1",
        )?;
        let rows = stmt.query_map([instance_id], |r| {
            Ok(DeployedFile {
                target: r.get(0)?,
                mod_id: r.get(1)?,
                revision: r.get(2)?,
                backup: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

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
        {
            let mut stmt = tx.prepare(
                "INSERT INTO deployed_files (instance_id, target, mod_id, revision, backup) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for f in files {
                stmt.execute(params![
                    instance_id,
                    f.target,
                    f.mod_id,
                    f.revision,
                    f.backup
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO instance_state (instance_id, deployed_at) VALUES (?1, ?2)
             ON CONFLICT(instance_id) DO UPDATE SET deployed_at = ?2",
            params![instance_id, at],
        )?;
        tx.commit()?;
        Ok(())
    }

    // Game state

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
            conn.execute_batch(&SCHEMA.replace(
                "CREATE TABLE game_state (\n    instance_id TEXT PRIMARY KEY,\n    fingerprint TEXT NOT NULL,\n    first_seen INTEGER NOT NULL,\n    updated_at INTEGER\n);\n",
                "",
            ))
            .unwrap();
            conn.pragma_update(None, "user_version", 1).unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.observe_game("i", "a", 1).unwrap(), None);
    }
}
