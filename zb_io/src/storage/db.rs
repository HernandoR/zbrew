use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use zb_core::Error;

pub struct Database {
    conn: Connection,
}

/// Why a keg is registered in `installed_kegs`.
///
/// `Retained` covers everything a user asked to keep: explicit `zb install`
/// targets and the dependencies pulled in to satisfy them. `Transient` is a
/// keg materialized only so `zb run`/`zbx` could execute a command once.
///
/// The two are ordered by strength, and `record_install` only ever moves a
/// keg up: a `zbx` invocation can never demote something `zb install` put
/// there, so an explicitly installed formula is never hidden or reclaimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstallReason {
    #[default]
    Retained,
    Transient,
}

impl InstallReason {
    const RETAINED: &'static str = "retained";
    const TRANSIENT: &'static str = "transient";

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Retained => Self::RETAINED,
            Self::Transient => Self::TRANSIENT,
        }
    }

    /// Unknown values decay to `Retained`: a row written by a future zbrew
    /// with a reason this build does not know about must never be treated as
    /// disposable.
    fn from_str(value: &str) -> Self {
        match value {
            Self::TRANSIENT => Self::Transient,
            _ => Self::Retained,
        }
    }

    pub fn is_transient(self) -> bool {
        self == Self::Transient
    }
}

#[derive(Debug, Clone)]
pub struct InstalledKeg {
    pub name: String,
    pub version: String,
    pub store_key: String,
    pub installed_at: i64,
    pub reason: InstallReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoreRef {
    pub store_key: String,
    pub refcount: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct KegFileRecord {
    pub name: String,
    pub version: String,
    pub linked_path: String,
    pub target_path: String,
}

fn keg_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InstalledKeg> {
    let reason: String = row.get(4)?;
    Ok(InstalledKeg {
        name: row.get(0)?,
        version: row.get(1)?,
        store_key: row.get(2)?,
        installed_at: row.get(3)?,
        reason: InstallReason::from_str(&reason),
    })
}

impl Database {
    const SCHEMA_VERSION: u32 = 2;

    pub fn open(path: &Path) -> Result<Self, Error> {
        let conn = Connection::open(path).map_err(Error::store("failed to open database"))?;
        Self::migrate(&conn)?;
        Ok(Self { conn })
    }

    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self, Error> {
        let conn =
            Connection::open_in_memory().map_err(Error::store("failed to open in-memory db"))?;
        Self::migrate(&conn)?;
        Ok(Self { conn })
    }

    fn get_schema_version(conn: &Connection) -> Result<u32, Error> {
        let version: u32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(Error::store("failed to query schema version"))?;
        Ok(version)
    }

    fn set_schema_version(conn: &Connection, version: u32) -> Result<(), Error> {
        conn.execute(&format!("PRAGMA user_version = {}", version), [])
            .map_err(Error::store("failed to set schema version"))?;
        Ok(())
    }

    fn migrate(conn: &Connection) -> Result<(), Error> {
        let current_version = Self::get_schema_version(conn)?;

        if current_version > Self::SCHEMA_VERSION {
            return Err(Error::StoreCorruption {
                message: format!(
                    "database schema version {} is newer than supported version {}. \
                     Please upgrade zbrew",
                    current_version,
                    Self::SCHEMA_VERSION
                ),
            });
        }

        if current_version == Self::SCHEMA_VERSION {
            return Ok(());
        }

        for version in current_version..Self::SCHEMA_VERSION {
            let next_version = version + 1;
            Self::migrate_to_version(conn, next_version)?;
            Self::set_schema_version(conn, next_version)?;
        }

        Ok(())
    }

    fn migrate_to_version(conn: &Connection, version: u32) -> Result<(), Error> {
        match version {
            1 => Self::migrate_to_v1(conn),
            2 => Self::migrate_to_v2(conn),
            _ => Err(Error::StoreCorruption {
                message: format!("unknown migration version {}", version),
            }),
        }
    }

    fn migrate_to_v1(conn: &Connection) -> Result<(), Error> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS installed_kegs (
                name TEXT PRIMARY KEY,
                version TEXT NOT NULL,
                store_key TEXT NOT NULL,
                installed_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS store_refs (
                store_key TEXT PRIMARY KEY,
                refcount INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS keg_files (
                name TEXT NOT NULL,
                version TEXT NOT NULL,
                linked_path TEXT NOT NULL,
                target_path TEXT NOT NULL,
                PRIMARY KEY (name, linked_path)
            );
            ",
        )
        .map_err(Error::store("failed to create initial schema"))?;

        Ok(())
    }

    /// Add `installed_kegs.install_reason`.
    ///
    /// Every row that predates this column was written by `zb install` or by
    /// a dependency of one -- `zb run` was the only producer of disposable
    /// kegs and it could not mark them -- so defaulting to `retained` keeps
    /// existing installations visible and safe from `zb gc`. Kegs a previous
    /// `zbx` leaked stay listed, which is the conservative direction: the
    /// alternative would have `zb gc` delete packages the user believes they
    /// installed.
    fn migrate_to_v2(conn: &Connection) -> Result<(), Error> {
        if Self::has_column(conn, "installed_kegs", "install_reason")? {
            return Ok(());
        }

        conn.execute(
            &format!(
                "ALTER TABLE installed_kegs
                 ADD COLUMN install_reason TEXT NOT NULL DEFAULT '{}'",
                InstallReason::RETAINED
            ),
            [],
        )
        .map_err(Error::store("failed to add install_reason column"))?;

        Ok(())
    }

    fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, Error> {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({})", table))
            .map_err(Error::store("failed to inspect table"))?;

        let mut found = false;
        let mut rows = stmt
            .query([])
            .map_err(Error::store("failed to inspect table"))?;
        while let Some(row) = rows
            .next()
            .map_err(Error::store("failed to inspect table"))?
        {
            let name: String = row.get(1).map_err(Error::store("failed to read column"))?;
            if name == column {
                found = true;
                break;
            }
        }

        Ok(found)
    }

    pub(crate) fn transaction(&mut self) -> Result<InstallTransaction<'_>, Error> {
        let tx = self
            .conn
            .transaction()
            .map_err(Error::store("failed to start transaction"))?;

        Ok(InstallTransaction { tx })
    }

    pub(crate) fn get_installed(&self, name: &str) -> Option<InstalledKeg> {
        self.conn
            .query_row(
                "SELECT name, version, store_key, installed_at, install_reason
                 FROM installed_kegs WHERE name = ?1",
                params![name],
                keg_from_row,
            )
            .ok()
    }

    /// Every registered keg, transient ones included. Callers that report to
    /// the user filter on [`InstalledKeg::reason`]; callers that reason about
    /// what is on disk (doctor, `uninstall --all`, gc) must not.
    pub(crate) fn list_installed(&self) -> Result<Vec<InstalledKeg>, Error> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT name, version, store_key, installed_at, install_reason
                 FROM installed_kegs ORDER BY name",
            )
            .map_err(Error::store("failed to prepare statement"))?;

        let kegs = stmt
            .query_map([], keg_from_row)
            .map_err(Error::store("failed to query installed kegs"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::store("failed to collect results"))?;

        Ok(kegs)
    }

    #[cfg(test)]
    pub(crate) fn get_store_refcount(&self, store_key: &str) -> i64 {
        self.conn
            .query_row(
                "SELECT refcount FROM store_refs WHERE store_key = ?1",
                params![store_key],
                |row| row.get(0),
            )
            .unwrap_or(0)
    }

    pub(crate) fn get_unreferenced_store_keys(&self) -> Result<Vec<String>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT store_key FROM store_refs WHERE refcount <= 0")
            .map_err(Error::store("failed to prepare statement"))?;

        let keys = stmt
            .query_map([], |row| row.get(0))
            .map_err(Error::store("failed to query unreferenced keys"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::store("failed to collect results"))?;

        Ok(keys)
    }

    pub(crate) fn delete_store_ref(&self, store_key: &str) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM store_refs WHERE store_key = ?1",
                params![store_key],
            )
            .map_err(Error::store("failed to delete store ref"))?;
        Ok(())
    }

    pub(crate) fn list_store_refs(&self) -> Result<Vec<StoreRef>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT store_key, refcount FROM store_refs ORDER BY store_key")
            .map_err(Error::store("failed to prepare statement"))?;

        let refs = stmt
            .query_map([], |row| {
                Ok(StoreRef {
                    store_key: row.get(0)?,
                    refcount: row.get(1)?,
                })
            })
            .map_err(Error::store("failed to query store refs"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::store("failed to collect results"))?;

        Ok(refs)
    }

    pub(crate) fn list_keg_files(&self) -> Result<Vec<KegFileRecord>, Error> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT name, version, linked_path, target_path
                 FROM keg_files
                 ORDER BY name, version, linked_path",
            )
            .map_err(Error::store("failed to prepare statement"))?;

        let records = stmt
            .query_map([], |row| {
                Ok(KegFileRecord {
                    name: row.get(0)?,
                    version: row.get(1)?,
                    linked_path: row.get(2)?,
                    target_path: row.get(3)?,
                })
            })
            .map_err(Error::store("failed to query keg files"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(Error::store("failed to collect results"))?;

        Ok(records)
    }

    pub(crate) fn replace_store_refs(&self, store_refs: &[StoreRef]) -> Result<(), Error> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(Error::store("failed to start transaction"))?;

        tx.execute("DELETE FROM store_refs", [])
            .map_err(Error::store("failed to clear store refs"))?;

        {
            let mut stmt = tx
                .prepare("INSERT INTO store_refs (store_key, refcount) VALUES (?1, ?2)")
                .map_err(Error::store("failed to prepare statement"))?;

            for store_ref in store_refs {
                stmt.execute(params![store_ref.store_key, store_ref.refcount])
                    .map_err(Error::store("failed to insert store ref"))?;
            }
        }

        tx.commit()
            .map_err(Error::store("failed to commit transaction"))
    }

    pub(crate) fn count_stale_keg_file_records(&self) -> Result<usize, Error> {
        let count: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM keg_files
                 WHERE NOT EXISTS (
                     SELECT 1
                     FROM installed_kegs
                     WHERE installed_kegs.name = keg_files.name
                       AND installed_kegs.version = keg_files.version
                 )",
                [],
                |row| row.get(0),
            )
            .map_err(Error::store("failed to count stale keg file records"))?;
        Ok(count as usize)
    }

    pub(crate) fn prune_stale_keg_file_records(&self) -> Result<usize, Error> {
        self.conn
            .execute(
                "DELETE FROM keg_files
                 WHERE NOT EXISTS (
                     SELECT 1
                     FROM installed_kegs
                     WHERE installed_kegs.name = keg_files.name
                       AND installed_kegs.version = keg_files.version
                 )",
                [],
            )
            .map_err(Error::store("failed to prune stale keg file records"))
    }
}

pub(crate) struct InstallTransaction<'a> {
    tx: Transaction<'a>,
}

impl<'a> InstallTransaction<'a> {
    /// Register `name` as installed.
    ///
    /// `reason` is a floor, never a ceiling: recording a keg as
    /// [`InstallReason::Transient`] leaves an existing `Retained` row
    /// retained, while recording it as `Retained` promotes a transient row.
    /// `zb install` re-records its whole dependency closure, so a dependency
    /// a previous `zbx` left behind is promoted the moment something the user
    /// wants to keep needs it -- which is what makes it safe for `zb gc` to
    /// delete transient kegs outright.
    pub(crate) fn record_install(
        &self,
        name: &str,
        version: &str,
        store_key: &str,
        reason: InstallReason,
    ) -> Result<(), Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        let previous_store_key: Option<String> = self
            .tx
            .query_row(
                "SELECT store_key FROM installed_kegs WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::store("failed to query previous store key"))?;

        self.tx
            .execute(
                "INSERT INTO installed_kegs (name, version, store_key, installed_at, install_reason)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(name) DO UPDATE SET
                     version = excluded.version,
                     store_key = excluded.store_key,
                     installed_at = excluded.installed_at,
                     install_reason = CASE
                         WHEN excluded.install_reason = ?6 THEN installed_kegs.install_reason
                         ELSE excluded.install_reason
                     END",
                params![
                    name,
                    version,
                    store_key,
                    now,
                    reason.as_str(),
                    InstallReason::TRANSIENT
                ],
            )
            .map_err(Error::store("failed to record install"))?;

        match previous_store_key.as_deref() {
            Some(previous) if previous == store_key => {}
            other => {
                if let Some(previous) = other {
                    self.tx
                        .execute(
                            "UPDATE store_refs SET refcount = refcount - 1 WHERE store_key = ?1",
                            params![previous],
                        )
                        .map_err(Error::store("failed to decrement previous store ref"))?;
                }

                self.tx
                    .execute(
                        "INSERT INTO store_refs (store_key, refcount) VALUES (?1, 1)
                         ON CONFLICT(store_key) DO UPDATE SET refcount = refcount + 1",
                        params![store_key],
                    )
                    .map_err(Error::store("failed to increment store ref"))?;
            }
        }

        Ok(())
    }

    pub(crate) fn record_linked_file(
        &self,
        name: &str,
        version: &str,
        linked_path: &str,
        target_path: &str,
    ) -> Result<(), Error> {
        self.tx
            .execute(
                "INSERT OR REPLACE INTO keg_files (name, version, linked_path, target_path)
                 VALUES (?1, ?2, ?3, ?4)",
                params![name, version, linked_path, target_path],
            )
            .map_err(Error::store("failed to record linked file"))?;

        Ok(())
    }

    pub(crate) fn record_uninstall(&self, name: &str) -> Result<Option<String>, Error> {
        // Get the store_key before removing
        let store_key: Option<String> = self
            .tx
            .query_row(
                "SELECT store_key FROM installed_kegs WHERE name = ?1",
                params![name],
                |row| row.get(0),
            )
            .ok();

        // Remove installed keg record
        self.tx
            .execute("DELETE FROM installed_kegs WHERE name = ?1", params![name])
            .map_err(Error::store("failed to remove install record"))?;

        self.tx
            .execute("DELETE FROM keg_files WHERE name = ?1", params![name])
            .map_err(Error::store("failed to remove keg files records"))?;

        // Decrement store ref if we had one
        if let Some(ref key) = store_key {
            self.tx
                .execute(
                    "UPDATE store_refs SET refcount = refcount - 1 WHERE store_key = ?1",
                    params![key],
                )
                .map_err(Error::store("failed to decrement store ref"))?;
        }

        Ok(store_key)
    }

    pub(crate) fn delete_installed_record(&self, name: &str) -> Result<(), Error> {
        self.tx
            .execute("DELETE FROM installed_kegs WHERE name = ?1", params![name])
            .map_err(Error::store("failed to remove install record"))?;

        self.clear_keg_file_records(name)
    }

    pub(crate) fn clear_keg_file_records(&self, name: &str) -> Result<(), Error> {
        self.tx
            .execute("DELETE FROM keg_files WHERE name = ?1", params![name])
            .map_err(Error::store("failed to clear keg files records"))?;

        Ok(())
    }

    pub(crate) fn commit(self) -> Result<(), Error> {
        self.tx
            .commit()
            .map_err(Error::store("failed to commit transaction"))
    }

    // Transaction is rolled back automatically when dropped without commit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_and_list() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "abc123", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        let installed = db.list_installed().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name, "foo");
        assert_eq!(installed[0].version, "1.0.0");
        assert_eq!(installed[0].store_key, "abc123");
    }

    #[test]
    fn rollback_leaves_no_partial_state() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "abc123", InstallReason::Retained)
                .unwrap();
            // Don't commit - transaction will be rolled back when dropped
        }

        let installed = db.list_installed().unwrap();
        assert!(installed.is_empty());

        // Store ref should also not exist
        assert_eq!(db.get_store_refcount("abc123"), 0);
    }

    #[test]
    fn uninstall_decrements_refcount() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "shared123", InstallReason::Retained)
                .unwrap();
            tx.record_install("bar", "2.0.0", "shared123", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        assert_eq!(db.get_store_refcount("shared123"), 2);

        {
            let tx = db.transaction().unwrap();
            tx.record_uninstall("foo").unwrap();
            tx.commit().unwrap();
        }

        assert_eq!(db.get_store_refcount("shared123"), 1);
        assert!(db.get_installed("foo").is_none());
        assert!(db.get_installed("bar").is_some());
    }

    #[test]
    fn get_unreferenced_store_keys() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "key1", InstallReason::Retained)
                .unwrap();
            tx.record_install("bar", "2.0.0", "key2", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        // Uninstall both
        {
            let tx = db.transaction().unwrap();
            tx.record_uninstall("foo").unwrap();
            tx.record_uninstall("bar").unwrap();
            tx.commit().unwrap();
        }

        let unreferenced = db.get_unreferenced_store_keys().unwrap();
        assert_eq!(unreferenced.len(), 2);
        assert!(unreferenced.contains(&"key1".to_string()));
        assert!(unreferenced.contains(&"key2".to_string()));
    }

    #[test]
    fn linked_files_are_recorded() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "abc123", InstallReason::Retained)
                .unwrap();
            tx.record_linked_file(
                "foo",
                "1.0.0",
                "/opt/homebrew/bin/foo",
                "/opt/zbrew/cellar/foo/1.0.0/bin/foo",
            )
            .unwrap();
            tx.commit().unwrap();
        }

        // Verify via uninstall that removes records
        {
            let tx = db.transaction().unwrap();
            tx.record_uninstall("foo").unwrap();
            tx.commit().unwrap();
        }

        assert!(db.get_installed("foo").is_none());
    }

    #[test]
    fn reinstall_with_same_store_key_does_not_leak_refcount() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "samekey", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        assert_eq!(db.get_store_refcount("samekey"), 1);

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "samekey", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        assert_eq!(db.get_store_refcount("samekey"), 1);
    }

    #[test]
    fn reinstall_with_new_store_key_moves_refcount() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "oldkey", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        assert_eq!(db.get_store_refcount("oldkey"), 1);

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.1.0", "newkey", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        assert_eq!(db.get_store_refcount("oldkey"), 0);
        assert_eq!(db.get_store_refcount("newkey"), 1);

        let installed = db.get_installed("foo").unwrap();
        assert_eq!(installed.version, "1.1.0");
        assert_eq!(installed.store_key, "newkey");
    }

    #[test]
    fn delete_store_ref_removes_unreferenced_entry() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "gc_key", InstallReason::Retained)
                .unwrap();
            tx.record_uninstall("foo").unwrap();
            tx.commit().unwrap();
        }

        assert_eq!(db.get_unreferenced_store_keys().unwrap(), vec!["gc_key"]);
        db.delete_store_ref("gc_key").unwrap();
        assert!(db.get_unreferenced_store_keys().unwrap().is_empty());
    }

    #[test]
    fn record_install_propagates_query_errors() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("foo", "1.0.0", "oldkey", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        db.conn
            .execute(
                "UPDATE installed_kegs
                 SET store_key = CAST(X'80' AS BLOB)
                 WHERE name = 'foo'",
                [],
            )
            .unwrap();

        let tx = db.transaction().unwrap();
        let err = tx
            .record_install("foo", "1.1.0", "newkey", InstallReason::Retained)
            .unwrap_err();
        assert!(matches!(err, Error::StoreCorruption { .. }));
        assert!(
            err.to_string()
                .contains("failed to query previous store key")
        );
    }

    #[test]
    fn new_database_starts_at_current_version() {
        let db = Database::in_memory().expect("failed to create database");
        let version = Database::get_schema_version(&db.conn).expect("failed to get version");
        assert_eq!(version, Database::SCHEMA_VERSION);
    }

    #[test]
    fn migration_is_idempotent() {
        let db = Database::in_memory().expect("failed to create database");
        Database::migrate(&db.conn).expect("first migration failed");
        Database::migrate(&db.conn).expect("second migration failed");
        let version = Database::get_schema_version(&db.conn).expect("failed to get version");
        assert_eq!(version, Database::SCHEMA_VERSION);
    }

    #[test]
    fn rejects_future_schema_version() {
        let conn = Connection::open_in_memory().expect("failed to open connection");
        Database::set_schema_version(&conn, 999).expect("failed to set version");
        let err = Database::migrate(&conn).unwrap_err();
        assert!(matches!(err, Error::StoreCorruption { .. }));
        assert!(err.to_string().contains("newer than supported version"));
    }

    #[test]
    fn migration_preserves_existing_data() {
        let conn = Connection::open_in_memory().expect("failed to open connection");

        conn.execute_batch(
            "CREATE TABLE installed_kegs (
                name TEXT PRIMARY KEY,
                version TEXT NOT NULL,
                store_key TEXT NOT NULL,
                installed_at INTEGER NOT NULL
            );
            INSERT INTO installed_kegs VALUES ('test', '1.0.0', 'key123', 1234567890);",
        )
        .expect("failed to create old schema");

        Database::migrate(&conn).expect("migration failed");

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM installed_kegs", [], |row| row.get(0))
            .expect("failed to count rows");
        assert_eq!(count, 1);

        let name: String = conn
            .query_row("SELECT name FROM installed_kegs", [], |row| row.get(0))
            .expect("failed to query data");
        assert_eq!(name, "test");
    }

    /// A v1 database -- one written by zbrew 0.4.0 or earlier -- must open
    /// without losing rows, and every row it carries must come back retained.
    #[test]
    fn v1_database_migrates_to_v2_with_retained_rows() {
        let conn = Connection::open_in_memory().expect("failed to open connection");

        Database::migrate_to_v1(&conn).expect("failed to create v1 schema");
        Database::set_schema_version(&conn, 1).expect("failed to set version");
        conn.execute_batch(
            "INSERT INTO installed_kegs VALUES ('jq', '1.7.1', 'key123', 1234567890);
             INSERT INTO store_refs VALUES ('key123', 1);",
        )
        .expect("failed to seed v1 data");

        Database::migrate(&conn).expect("migration failed");
        assert_eq!(
            Database::get_schema_version(&conn).unwrap(),
            Database::SCHEMA_VERSION
        );

        let db = Database { conn };
        let installed = db.list_installed().unwrap();
        assert_eq!(installed.len(), 1);
        assert_eq!(installed[0].name, "jq");
        assert_eq!(installed[0].store_key, "key123");
        assert_eq!(installed[0].reason, InstallReason::Retained);
        assert_eq!(db.get_store_refcount("key123"), 1);
    }

    #[test]
    fn transient_install_is_recorded_as_transient() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("jq", "1.7.1", "key", InstallReason::Transient)
                .unwrap();
            tx.commit().unwrap();
        }

        assert!(db.get_installed("jq").unwrap().reason.is_transient());
    }

    #[test]
    fn explicit_install_promotes_a_transient_keg() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("jq", "1.7.1", "key", InstallReason::Transient)
                .unwrap();
            tx.record_install("jq", "1.7.1", "key", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        assert_eq!(
            db.get_installed("jq").unwrap().reason,
            InstallReason::Retained
        );
    }

    /// The direction that matters for safety: nothing `zbx` does may make an
    /// explicitly installed formula disappear from `zb list` or become fair
    /// game for `zb gc`.
    #[test]
    fn transient_install_never_demotes_a_retained_keg() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("jq", "1.7.1", "key", InstallReason::Retained)
                .unwrap();
            tx.record_install("jq", "1.7.1", "key", InstallReason::Transient)
                .unwrap();
            tx.commit().unwrap();
        }

        assert_eq!(
            db.get_installed("jq").unwrap().reason,
            InstallReason::Retained
        );
    }

    #[test]
    fn reinstall_updates_version_without_touching_the_reason() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("jq", "1.7.0", "old", InstallReason::Transient)
                .unwrap();
            tx.record_install("jq", "1.7.1", "new", InstallReason::Transient)
                .unwrap();
            tx.commit().unwrap();
        }

        let keg = db.get_installed("jq").unwrap();
        assert_eq!(keg.version, "1.7.1");
        assert!(keg.reason.is_transient());
    }

    #[test]
    fn unknown_reason_values_decay_to_retained() {
        let mut db = Database::in_memory().unwrap();

        {
            let tx = db.transaction().unwrap();
            tx.record_install("jq", "1.7.1", "key", InstallReason::Transient)
                .unwrap();
            tx.commit().unwrap();
        }

        db.conn
            .execute(
                "UPDATE installed_kegs SET install_reason = 'from-the-future'",
                [],
            )
            .unwrap();

        assert_eq!(
            db.get_installed("jq").unwrap().reason,
            InstallReason::Retained
        );
    }
}
