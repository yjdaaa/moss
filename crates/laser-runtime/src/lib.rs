pub mod config;
pub mod db;
pub mod migration;
pub mod root;
pub mod store;

pub use config::LaserSettings;
pub use db::{MaterialConflictPolicy, RuntimeDb, SchemaError};
pub use migration::{
    ConflictPolicy, MigrationCounts, MigrationError, MigrationOptions, MigrationReport,
    normalize_material_version, remap_path, remap_path_to, run_migration, scan_uploads, sha256_file,
};
pub use root::{RuntimeRoot, RuntimeRootError};
pub use store::{MemoryStore, RecordKind, RuntimeStore, SqliteStore, StoredRecord};

pub fn packaging_sqlite_probe() -> rusqlite::Result<i64> {
    let connection = rusqlite::Connection::open_in_memory()?;
    connection.execute("CREATE TABLE probe (value INTEGER NOT NULL)", [])?;
    connection.execute("INSERT INTO probe (value) VALUES (42)", [])?;
    connection.query_row("SELECT value FROM probe", [], |row| row.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_sqlite_reads_and_writes() {
        assert_eq!(packaging_sqlite_probe().unwrap(), 42);
    }

    #[test]
    fn runtime_schema_seeds_twelve_builtin_cells() {
        let db = RuntimeDb::open_memory().unwrap();
        let materials: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM materials", [], |row| row.get(0))
            .unwrap();
        let cells: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM material_params", [], |row| row.get(0))
            .unwrap();
        assert_eq!(materials, 4);
        assert_eq!(cells, 12);
        db.migrate().unwrap();
        assert_eq!(
            db.connection
                .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn history_schema_keeps_replacement_metadata() {
        let db = RuntimeDb::open_memory().unwrap();
        let columns: Vec<String> = db
            .connection
            .prepare("PRAGMA table_info(material_params_history)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "replaced_at"));
        assert!(columns.iter().any(|column| column == "reason"));
    }

    #[test]
    fn restart_recovery_fails_running_and_pending_jobs() {
        let db = RuntimeDb::open_memory().unwrap();
        db.connection.execute(
            "INSERT INTO jobs(job_id,status,created_at,payload) VALUES
             ('running-job','running',unixepoch(),'{}'),
             ('pending-job','pending',unixepoch(),'{}')",
            [],
        ).unwrap();
        assert_eq!(db.recover_after_restart().unwrap(), 2);
        let running: String = db.connection.query_row(
            "SELECT status FROM jobs WHERE job_id='running-job'", [], |row| row.get(0)
        ).unwrap();
        let pending: String = db.connection.query_row(
            "SELECT status FROM jobs WHERE job_id='pending-job'", [], |row| row.get(0)
        ).unwrap();
        assert_eq!(running, "failed");
        assert_eq!(pending, "failed");
    }

    #[test]
    fn material_import_handles_conflicts_history_and_tombstones() {
        let db = RuntimeDb::open_memory().unwrap();
        let cell_count: i64 = db
            .connection
            .query_row("SELECT COUNT(*) FROM material_params", [], |row| row.get(0))
            .unwrap();
        assert_eq!(cell_count, 12);

        let modified_json = serde_json::json!({
            "version": 1,
            "materials": {
                "椴木": {
                    "aliases": ["椴木新别名"],
                    "thicknesses": {
                        "3": {
                            "engrave": {
                                "raster": {
                                    "laser_min_power": 10,
                                    "laser_max_power": 999,
                                    "feed_rate": 2000,
                                    "travel_rate": 3000,
                                    "threshold": -1,
                                    "passes": 1,
                                    "pixel_size_mm": 0.1
                                }
                            }
                        }
                    }
                }
            }
        });

        let counts_keep = db
            .import_material_json(
                &modified_json,
                crate::db::MaterialConflictPolicy::Keep,
                1000.0,
            )
            .unwrap();
        assert_eq!(counts_keep.conflicts, 1);
        let power_keep: i32 = db
            .connection
            .query_row(
                "SELECT laser_max_power FROM material_params WHERE material_name='椴木' AND thickness_key='3' AND engraving_mode='raster'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(power_keep, 420);

        let counts_overwrite = db
            .import_material_json(
                &modified_json,
                crate::db::MaterialConflictPolicy::Overwrite,
                2000.0,
            )
            .unwrap();
        assert_eq!(counts_overwrite.conflicts, 1);
        let power_overwritten: i32 = db
            .connection
            .query_row(
                "SELECT laser_max_power FROM material_params WHERE material_name='椴木' AND thickness_key='3' AND engraving_mode='raster'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(power_overwritten, 999);

        let history_count: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM material_params_history WHERE material_name='椴木' AND reason='import_overwrite'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(history_count, 1);

        let alias_exists: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM material_aliases WHERE alias='椴木新别名'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alias_exists, 1);

        let tombstone_json = serde_json::json!({
            "version": 1,
            "materials": {
                "椴木": {
                    "thicknesses": {
                        "3": {
                            "engrave": {
                                "raster": null
                            }
                        }
                    }
                }
            }
        });
        let counts_tomb = db
            .import_material_json(
                &tombstone_json,
                crate::db::MaterialConflictPolicy::Keep,
                3000.0,
            )
            .unwrap();
        assert_eq!(counts_tomb.tombstones, 1);
        let raster_remaining: i64 = db
            .connection
            .query_row(
                "SELECT COUNT(*) FROM material_params WHERE material_name='椴木' AND thickness_key='3' AND engraving_mode='raster'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raster_remaining, 0);
    }
}
