//! Bundled SQLite storage candidate.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::path::Path;
use std::time::Duration;

use godot_codex_index_store::{
    BuildState, DependencyEdge, DependencyResolution, GenerationBuilder, IncrementalBatch,
    IndexGeneration, IndexMetadata, IndexRead, IndexWriteTransaction, MigrationRunner,
    ResourceEntity, ResourceQuery, ResourceQueryResult, ResourceSelector, StoreError,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{BackendKind, FaultInjection, FaultPoint, SpikeStore};

const CURRENT_PHYSICAL_VERSION: u32 = 2;

/// SQLite candidate holding the project writer lease and connection.
pub struct SqliteStore {
    project_id: String,
    connection: Connection,
    _lock: File,
}

impl SqliteStore {
    /// Opens a normal v2 candidate store.
    pub fn open(project_root: &Path, project_id: &str) -> Result<Self, StoreError> {
        Self::open_with_version(project_root, project_id, CURRENT_PHYSICAL_VERSION)
    }

    /// Opens a new physical-v1 store for the migration scenario.
    pub fn open_with_version(
        project_root: &Path,
        project_id: &str,
        initial_version: u32,
    ) -> Result<Self, StoreError> {
        let codex = project_root.join(".godot").join("codex");
        fs::create_dir_all(&codex).map_err(io_error)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(codex.join("index.lock"))
            .map_err(io_error)?;
        lock.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => StoreError::StoreBusy,
            TryLockError::Error(error) => io_error(error),
        })?;
        let path = codex.join("index.sqlite");
        let connection = Connection::open(&path).map_err(sql_error)?;
        configure(&connection)?;
        initialize_schema(&connection, project_id, initial_version)?;
        validate_physical_version(&connection)?;
        verify_integrity(&connection)?;
        cleanup_staging(&connection)?;
        garbage_collect(&connection)?;
        let bound_project: String = connection
            .query_row(
                "SELECT project_id FROM store_metadata WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if bound_project != project_id {
            return Err(StoreError::ProjectMismatch);
        }
        Ok(Self {
            project_id: project_id.to_owned(),
            connection,
            _lock: lock,
        })
    }

    /// Opens and validates active data without acquiring the writer lease.
    pub fn read_generation(
        project_root: &Path,
        project_id: &str,
    ) -> Result<IndexGeneration, StoreError> {
        let path = project_root
            .join(".godot")
            .join("codex")
            .join("index.sqlite");
        let connection = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(sql_error)?;
        validate_physical_version(&connection)?;
        verify_integrity(&connection)?;
        let bound_project: String = connection
            .query_row(
                "SELECT project_id FROM store_metadata WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if bound_project != project_id {
            return Err(StoreError::ProjectMismatch);
        }
        load_active(&connection)
    }

    fn activate_with_version(
        &mut self,
        generation: &IndexGeneration,
        physical_version: u32,
        fault: Option<FaultInjection>,
    ) -> Result<IndexMetadata, StoreError> {
        if generation.project_id != self.project_id {
            return Err(StoreError::ProjectMismatch);
        }
        generation.validate()?;
        if let Some(metadata) = idempotent_activation(&self.connection, generation)? {
            return Ok(metadata);
        }
        hit(fault, FaultPoint::Capture)?;
        let previous = load_metadata(&self.connection).ok();
        let metadata = next_metadata(previous.as_ref(), generation);
        let transaction = self.connection.transaction().map_err(sql_error)?;
        insert_staging(&transaction, generation, physical_version, fault)?;
        transaction.commit().map_err(sql_error)?;
        hit(fault, FaultPoint::PreCommit)?;

        let activation = self.connection.transaction().map_err(sql_error)?;
        activation
            .execute(
                "UPDATE generations SET state = 'retired' WHERE state = 'active'",
                [],
            )
            .map_err(sql_error)?;
        activation
            .execute(
                "UPDATE generations SET state = 'active' WHERE generation_id = ?1",
                [&generation.generation_id],
            )
            .map_err(sql_error)?;
        activation
            .execute(
                "UPDATE store_metadata
                 SET physical_version = ?1, active_generation_id = ?2,
                     index_revision = ?3, metadata_json = ?4
                 WHERE id = 1",
                params![
                    physical_version,
                    generation.generation_id,
                    sql_u64(generation.index_revision)?,
                    to_json(&metadata)?
                ],
            )
            .map_err(sql_error)?;
        activation.commit().map_err(sql_error)?;
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(FULL);")
            .map_err(sql_error)?;
        hit(fault, FaultPoint::PostCommit)?;
        garbage_collect(&self.connection)?;
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .map_err(sql_error)?;
        Ok(metadata)
    }
}

impl SpikeStore for SqliteStore {
    fn kind(&self) -> BackendKind {
        BackendKind::Sqlite
    }

    fn activate(
        &mut self,
        generation: &IndexGeneration,
        fault: Option<FaultInjection>,
    ) -> Result<IndexMetadata, StoreError> {
        let version = self.physical_version().unwrap_or(CURRENT_PHYSICAL_VERSION);
        self.activate_with_version(generation, version, fault)
    }

    fn active_generation(&self) -> Result<IndexGeneration, StoreError> {
        validate_physical_version(&self.connection)?;
        verify_integrity(&self.connection)?;
        load_active(&self.connection)
    }

    fn direct(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError> {
        execute_query(&self.connection, query, false)
    }

    fn reverse(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError> {
        execute_query(&self.connection, query, true)
    }

    fn migrate_v2(&mut self) -> Result<IndexMetadata, StoreError> {
        if self.physical_version()? >= 2 {
            return load_metadata(&self.connection);
        }
        self.connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS resource_lookup (
                   generation_id TEXT NOT NULL,
                   kind TEXT NOT NULL,
                   value TEXT NOT NULL,
                   entity_id TEXT NOT NULL,
                   PRIMARY KEY (generation_id, kind, value),
                   FOREIGN KEY (generation_id) REFERENCES generations(generation_id)
                     ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS resource_lookup_entity
                   ON resource_lookup(generation_id, entity_id);",
            )
            .map_err(sql_error)?;
        let mut generation = load_active(&self.connection)?;
        generation.parent_generation_id = Some(generation.generation_id.clone());
        generation.index_revision += 1;
        generation.checkpoint.index_revision = generation.index_revision;
        generation.generation_id = format!("sqlite-migration-{:020}", generation.index_revision);
        generation.creation_reason = "physical_v1_to_v2".to_owned();
        generation.validation_digest.clear();
        generation.validation_digest = generation.compute_validation_digest();
        self.activate_with_version(&generation, 2, None)
    }

    fn physical_version(&self) -> Result<u32, StoreError> {
        validate_physical_version(&self.connection)
    }
}

/// Storage-neutral transaction adapter over the SQLite candidate.
pub struct SqliteTransaction<'a> {
    store: &'a mut SqliteStore,
    generation: Option<IndexGeneration>,
}

impl IndexWriteTransaction for SqliteTransaction<'_> {
    fn replace_generation(&mut self, generation: IndexGeneration) -> Result<(), StoreError> {
        generation.validate()?;
        self.generation = Some(generation);
        Ok(())
    }

    fn apply_incremental_batch(&mut self, batch: IncrementalBatch) -> Result<(), StoreError> {
        let current = self.generation.as_ref().ok_or_else(|| {
            StoreError::ValidationFailed("transaction generation missing".to_owned())
        })?;
        self.generation = Some(current.apply_incremental_batch(&batch)?);
        Ok(())
    }

    fn commit(mut self) -> Result<IndexMetadata, StoreError> {
        let generation = self.generation.take().ok_or_else(|| {
            StoreError::ValidationFailed("transaction generation missing".to_owned())
        })?;
        self.store.activate(&generation, None)
    }

    fn cancel(self) -> Result<(), StoreError> {
        Ok(())
    }
}

impl GenerationBuilder for SqliteStore {
    type Transaction<'a> = SqliteTransaction<'a>;

    fn begin_generation<'a>(
        &'a mut self,
        generation: &IndexGeneration,
    ) -> Result<Self::Transaction<'a>, StoreError> {
        generation.validate()?;
        Ok(SqliteTransaction {
            store: self,
            generation: Some(generation.clone()),
        })
    }
}

impl IndexRead for SqliteStore {
    fn metadata(&self) -> Result<IndexMetadata, StoreError> {
        load_metadata(&self.connection)
    }

    fn resource(&self, selector: &ResourceSelector) -> Result<ResourceEntity, StoreError> {
        execute_query(
            &self.connection,
            &ResourceQuery {
                selector: selector.clone(),
                limit: 1,
                offset: 0,
            },
            false,
        )
        .map(|result| result.resource)
    }

    fn direct_dependencies(
        &self,
        query: &ResourceQuery,
    ) -> Result<ResourceQueryResult, StoreError> {
        self.direct(query)
    }

    fn reverse_owners(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError> {
        self.reverse(query)
    }
}

impl MigrationRunner for SqliteStore {
    fn migrate(&mut self, target_physical_version: u32) -> Result<IndexMetadata, StoreError> {
        if target_physical_version != 2 {
            return Err(StoreError::IncompatibleSchema);
        }
        self.migrate_v2()
    }
}

fn configure(connection: &Connection) -> Result<(), StoreError> {
    connection.busy_timeout(Duration::ZERO).map_err(sql_error)?;
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             PRAGMA foreign_keys=ON;
             PRAGMA temp_store=MEMORY;",
        )
        .map_err(sql_error)
}

fn initialize_schema(
    connection: &Connection,
    project_id: &str,
    initial_version: u32,
) -> Result<(), StoreError> {
    if !(1..=CURRENT_PHYSICAL_VERSION).contains(&initial_version) {
        return Err(StoreError::IncompatibleSchema);
    }
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS store_metadata (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               project_id TEXT NOT NULL,
               physical_version INTEGER NOT NULL,
               active_generation_id TEXT,
               index_revision INTEGER NOT NULL,
               metadata_json TEXT
             );
             CREATE TABLE IF NOT EXISTS generations (
               generation_id TEXT PRIMARY KEY,
               project_id TEXT NOT NULL,
               index_revision INTEGER NOT NULL,
               state TEXT NOT NULL,
               header_json TEXT NOT NULL,
               validation_digest TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS resources (
               generation_id TEXT NOT NULL,
               entity_id TEXT NOT NULL,
               uid TEXT,
               comparison_path TEXT NOT NULL,
               record_json TEXT NOT NULL,
               PRIMARY KEY (generation_id, entity_id),
               FOREIGN KEY (generation_id) REFERENCES generations(generation_id)
                 ON DELETE CASCADE
             );
             CREATE UNIQUE INDEX IF NOT EXISTS resources_uid
               ON resources(generation_id, uid) WHERE uid IS NOT NULL;
             CREATE UNIQUE INDEX IF NOT EXISTS resources_path
               ON resources(generation_id, comparison_path);
             CREATE TABLE IF NOT EXISTS source_documents (
               generation_id TEXT NOT NULL,
               entity_id TEXT NOT NULL,
               record_json TEXT NOT NULL,
               PRIMARY KEY (generation_id, entity_id),
               FOREIGN KEY (generation_id) REFERENCES generations(generation_id)
                 ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS dependency_edges (
               generation_id TEXT NOT NULL,
               edge_id TEXT NOT NULL,
               source_entity_id TEXT NOT NULL,
               target_entity_id TEXT,
               target_uid TEXT,
               target_path TEXT,
               record_json TEXT NOT NULL,
               PRIMARY KEY (generation_id, edge_id),
               FOREIGN KEY (generation_id) REFERENCES generations(generation_id)
                 ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS direct_edges
               ON dependency_edges(generation_id, source_entity_id);
             CREATE INDEX IF NOT EXISTS reverse_edges_entity
               ON dependency_edges(generation_id, target_entity_id);
             CREATE INDEX IF NOT EXISTS reverse_edges_uid
               ON dependency_edges(generation_id, target_uid);
             CREATE INDEX IF NOT EXISTS reverse_edges_path
               ON dependency_edges(generation_id, target_path);
             CREATE TABLE IF NOT EXISTS diagnostics (
               generation_id TEXT NOT NULL,
               diagnostic_id TEXT NOT NULL,
               record_json TEXT NOT NULL,
               PRIMARY KEY (generation_id, diagnostic_id),
               FOREIGN KEY (generation_id) REFERENCES generations(generation_id)
                 ON DELETE CASCADE
             );
             CREATE TABLE IF NOT EXISTS tombstones (
               generation_id TEXT NOT NULL,
               entity_id TEXT NOT NULL,
               record_json TEXT NOT NULL,
               PRIMARY KEY (generation_id, entity_id),
               FOREIGN KEY (generation_id) REFERENCES generations(generation_id)
                 ON DELETE CASCADE
             );",
        )
        .map_err(sql_error)?;
    if initial_version >= 2 {
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS resource_lookup (
                   generation_id TEXT NOT NULL,
                   kind TEXT NOT NULL,
                   value TEXT NOT NULL,
                   entity_id TEXT NOT NULL,
                   PRIMARY KEY (generation_id, kind, value),
                   FOREIGN KEY (generation_id) REFERENCES generations(generation_id)
                     ON DELETE CASCADE
                 );
                 CREATE INDEX IF NOT EXISTS resource_lookup_entity
                   ON resource_lookup(generation_id, entity_id);",
            )
            .map_err(sql_error)?;
    }
    connection
        .execute(
            "INSERT OR IGNORE INTO store_metadata
             (id, project_id, physical_version, active_generation_id, index_revision, metadata_json)
             VALUES (1, ?1, ?2, NULL, 0, NULL)",
            params![project_id, initial_version],
        )
        .map_err(sql_error)?;
    Ok(())
}

fn insert_staging(
    transaction: &Transaction<'_>,
    generation: &IndexGeneration,
    physical_version: u32,
    fault: Option<FaultInjection>,
) -> Result<(), StoreError> {
    let staging_threshold = (generation.resources.len()
        + generation.source_documents.len()
        + generation.dependencies.len()
        + generation.diagnostics.len()
        + generation.tombstones.len())
    .div_ceil(2);
    let mut staged_records = 0;
    let mut staging_triggered = false;
    let mut header = generation.clone();
    header.resources.clear();
    header.source_documents.clear();
    header.dependencies.clear();
    header.diagnostics.clear();
    header.tombstones.clear();
    transaction
        .execute(
            "INSERT INTO generations
             (generation_id, project_id, index_revision, state, header_json, validation_digest)
             VALUES (?1, ?2, ?3, 'staging', ?4, ?5)",
            params![
                generation.generation_id,
                generation.project_id,
                sql_u64(generation.index_revision)?,
                to_json(&header)?,
                generation.validation_digest
            ],
        )
        .map_err(sql_error)?;
    for resource in &generation.resources {
        transaction
            .execute(
                "INSERT INTO resources
                 (generation_id, entity_id, uid, comparison_path, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    generation.generation_id,
                    resource.entity_id,
                    resource.uid,
                    resource.comparison_path,
                    to_json(resource)?
                ],
            )
            .map_err(sql_error)?;
        advance_staging(
            &mut staged_records,
            staging_threshold,
            &mut staging_triggered,
            fault,
        )?;
        if physical_version >= 2 {
            for (kind, value) in [
                ("entity_id", Some(resource.entity_id.as_str())),
                ("uid", resource.uid.as_deref()),
                ("path", Some(resource.comparison_path.as_str())),
            ] {
                if let Some(value) = value {
                    transaction
                        .execute(
                            "INSERT INTO resource_lookup
                             (generation_id, kind, value, entity_id) VALUES (?1, ?2, ?3, ?4)",
                            params![generation.generation_id, kind, value, resource.entity_id],
                        )
                        .map_err(sql_error)?;
                }
            }
        }
    }
    for document in &generation.source_documents {
        transaction
            .execute(
                "INSERT INTO source_documents (generation_id, entity_id, record_json)
                 VALUES (?1, ?2, ?3)",
                params![
                    generation.generation_id,
                    document.entity_id,
                    to_json(document)?
                ],
            )
            .map_err(sql_error)?;
        advance_staging(
            &mut staged_records,
            staging_threshold,
            &mut staging_triggered,
            fault,
        )?;
    }
    for edge in &generation.dependencies {
        transaction
            .execute(
                "INSERT INTO dependency_edges
                 (generation_id, edge_id, source_entity_id, target_entity_id,
                  target_uid, target_path, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    generation.generation_id,
                    edge.edge_id,
                    edge.source_entity_id,
                    edge.target_entity_id,
                    edge.target_uid,
                    edge.target_comparison_path,
                    to_json(edge)?
                ],
            )
            .map_err(sql_error)?;
        advance_staging(
            &mut staged_records,
            staging_threshold,
            &mut staging_triggered,
            fault,
        )?;
    }
    for diagnostic in &generation.diagnostics {
        transaction
            .execute(
                "INSERT INTO diagnostics (generation_id, diagnostic_id, record_json)
                 VALUES (?1, ?2, ?3)",
                params![
                    generation.generation_id,
                    diagnostic.diagnostic_id,
                    to_json(diagnostic)?
                ],
            )
            .map_err(sql_error)?;
        advance_staging(
            &mut staged_records,
            staging_threshold,
            &mut staging_triggered,
            fault,
        )?;
    }
    for tombstone in &generation.tombstones {
        transaction
            .execute(
                "INSERT INTO tombstones (generation_id, entity_id, record_json)
                 VALUES (?1, ?2, ?3)",
                params![
                    generation.generation_id,
                    tombstone.entity_id,
                    to_json(tombstone)?
                ],
            )
            .map_err(sql_error)?;
        advance_staging(
            &mut staged_records,
            staging_threshold,
            &mut staging_triggered,
            fault,
        )?;
    }
    if !staging_triggered {
        hit(fault, FaultPoint::Staging)?;
    }
    Ok(())
}

fn advance_staging(
    staged_records: &mut usize,
    threshold: usize,
    triggered: &mut bool,
    fault: Option<FaultInjection>,
) -> Result<(), StoreError> {
    *staged_records += 1;
    if !*triggered && *staged_records >= threshold {
        *triggered = true;
        hit(fault, FaultPoint::Staging)?;
    }
    Ok(())
}

fn load_active(connection: &Connection) -> Result<IndexGeneration, StoreError> {
    let generation_id: Option<String> = connection
        .query_row(
            "SELECT active_generation_id FROM store_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    let generation_id = generation_id.ok_or(StoreError::NotReady)?;
    let header: String = connection
        .query_row(
            "SELECT header_json FROM generations
             WHERE generation_id = ?1 AND state = 'active'",
            [&generation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::CorruptStore("active generation row missing".to_owned()))?;
    let mut generation: IndexGeneration = from_json(&header)?;
    generation.resources = load_json_column(
        connection,
        "SELECT record_json FROM resources WHERE generation_id = ?1 ORDER BY entity_id",
        &generation_id,
    )?;
    generation.source_documents = load_json_column(
        connection,
        "SELECT record_json FROM source_documents WHERE generation_id = ?1 ORDER BY entity_id",
        &generation_id,
    )?;
    generation.dependencies = load_json_column(
        connection,
        "SELECT record_json FROM dependency_edges WHERE generation_id = ?1 ORDER BY edge_id",
        &generation_id,
    )?;
    generation.diagnostics = load_json_column(
        connection,
        "SELECT record_json FROM diagnostics WHERE generation_id = ?1 ORDER BY diagnostic_id",
        &generation_id,
    )?;
    generation.tombstones = load_json_column(
        connection,
        "SELECT record_json FROM tombstones WHERE generation_id = ?1 ORDER BY entity_id",
        &generation_id,
    )?;
    generation.canonicalize();
    generation.validate()?;
    Ok(generation)
}

fn execute_query(
    connection: &Connection,
    query: &ResourceQuery,
    reverse: bool,
) -> Result<ResourceQueryResult, StoreError> {
    if !(1..=200).contains(&query.limit) {
        return Err(StoreError::ValidationFailed(
            "invalid query limit".to_owned(),
        ));
    }
    let (generation_id, index_revision): (Option<String>, i64) = connection
        .query_row(
            "SELECT active_generation_id, index_revision FROM store_metadata WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    let generation_id = generation_id.ok_or(StoreError::NotReady)?;
    let (selector_column, selector_value) = match &query.selector {
        ResourceSelector::EntityId(value) => ("entity_id", value),
        ResourceSelector::Uid(value) => ("uid", value),
        ResourceSelector::ComparisonPath(value) => ("comparison_path", value),
    };
    let physical_version = validate_physical_version(connection)?;
    let resource_json: Option<String> = if physical_version >= 2 {
        let lookup_kind = match &query.selector {
            ResourceSelector::EntityId(_) => "entity_id",
            ResourceSelector::Uid(_) => "uid",
            ResourceSelector::ComparisonPath(_) => "path",
        };
        connection
            .query_row(
                "SELECT resources.record_json
                 FROM resource_lookup
                 JOIN resources ON resources.generation_id = resource_lookup.generation_id
                   AND resources.entity_id = resource_lookup.entity_id
                 WHERE resource_lookup.generation_id = ?1
                   AND resource_lookup.kind = ?2 AND resource_lookup.value = ?3",
                params![generation_id, lookup_kind, selector_value],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?
    } else {
        connection
            .query_row(
                &format!(
                    "SELECT record_json FROM resources
                     WHERE generation_id = ?1 AND {selector_column} = ?2"
                ),
                params![generation_id, selector_value],
                |row| row.get(0),
            )
            .optional()
            .map_err(sql_error)?
    };
    let resource: ResourceEntity = from_json(&resource_json.ok_or(StoreError::ResourceNotFound)?)?;
    let page_size = query
        .limit
        .checked_add(1)
        .ok_or_else(|| StoreError::ValidationFailed("query limit overflow".to_owned()))?;
    let offset = i64::try_from(query.offset)
        .map_err(|_| StoreError::ValidationFailed("query offset overflow".to_owned()))?;
    let mut edges = if reverse {
        let mut statement = connection
            .prepare(
                "SELECT record_json FROM dependency_edges
                 WHERE generation_id = ?1 AND
                   (target_entity_id = ?2 OR target_uid = ?3 OR target_path = ?4)
                 ORDER BY edge_id LIMIT ?5 OFFSET ?6",
            )
            .map_err(sql_error)?;
        statement
            .query_map(
                params![
                    generation_id,
                    resource.entity_id,
                    resource.uid,
                    resource.comparison_path,
                    i64::try_from(page_size).expect("query page size is bounded"),
                    offset,
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(sql_error)?
            .map(|row| from_json(&row.map_err(sql_error)?))
            .collect::<Result<Vec<DependencyEdge>, StoreError>>()?
    } else {
        let mut statement = connection
            .prepare(
                "SELECT record_json FROM dependency_edges
                 WHERE generation_id = ?1 AND source_entity_id = ?2
                 ORDER BY edge_id LIMIT ?3 OFFSET ?4",
            )
            .map_err(sql_error)?;
        statement
            .query_map(
                params![
                    generation_id,
                    resource.entity_id,
                    i64::try_from(page_size).expect("query page size is bounded"),
                    offset,
                ],
                |row| row.get::<_, String>(0),
            )
            .map_err(sql_error)?
            .map(|row| from_json(&row.map_err(sql_error)?))
            .collect::<Result<Vec<DependencyEdge>, StoreError>>()?
    };
    edges.sort_by(|left, right| left.edge_id.cmp(&right.edge_id));
    let has_more = edges.len() > query.limit;
    edges.truncate(query.limit);
    let exact = edges.iter().all(|edge| {
        edge.resolution == DependencyResolution::Resolved && edge.target_entity_id.is_some()
    });
    Ok(ResourceQueryResult {
        generation_id,
        index_revision: u64::try_from(index_revision)
            .map_err(|_| StoreError::CorruptStore("negative index revision".to_owned()))?,
        resource,
        edges,
        exact,
        has_more,
    })
}

fn load_metadata(connection: &Connection) -> Result<IndexMetadata, StoreError> {
    let json: Option<String> = connection
        .query_row(
            "SELECT metadata_json FROM store_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    from_json(&json.ok_or(StoreError::NotReady)?)
}

fn load_json_column<T: DeserializeOwned>(
    connection: &Connection,
    sql: &str,
    generation_id: &str,
) -> Result<Vec<T>, StoreError> {
    let mut statement = connection.prepare(sql).map_err(sql_error)?;
    let rows = statement
        .query_map([generation_id], |row| row.get::<_, String>(0))
        .map_err(sql_error)?;
    rows.map(|row| from_json(&row.map_err(sql_error)?))
        .collect()
}

fn cleanup_staging(connection: &Connection) -> Result<(), StoreError> {
    connection
        .execute_batch(
            "DELETE FROM resources WHERE generation_id IN
               (SELECT generation_id FROM generations WHERE state = 'staging');
             DELETE FROM source_documents WHERE generation_id IN
               (SELECT generation_id FROM generations WHERE state = 'staging');
             DELETE FROM dependency_edges WHERE generation_id IN
               (SELECT generation_id FROM generations WHERE state = 'staging');
             DELETE FROM diagnostics WHERE generation_id IN
               (SELECT generation_id FROM generations WHERE state = 'staging');
             DELETE FROM tombstones WHERE generation_id IN
               (SELECT generation_id FROM generations WHERE state = 'staging');
             DELETE FROM resource_lookup WHERE generation_id IN
               (SELECT generation_id FROM generations WHERE state = 'staging');
             DELETE FROM generations WHERE state = 'staging';",
        )
        .or_else(|error| {
            if error.to_string().contains("no such table: resource_lookup") {
                connection.execute_batch(
                    "DELETE FROM resources WHERE generation_id IN
                       (SELECT generation_id FROM generations WHERE state = 'staging');
                     DELETE FROM source_documents WHERE generation_id IN
                       (SELECT generation_id FROM generations WHERE state = 'staging');
                     DELETE FROM dependency_edges WHERE generation_id IN
                       (SELECT generation_id FROM generations WHERE state = 'staging');
                     DELETE FROM diagnostics WHERE generation_id IN
                       (SELECT generation_id FROM generations WHERE state = 'staging');
                     DELETE FROM tombstones WHERE generation_id IN
                       (SELECT generation_id FROM generations WHERE state = 'staging');
                     DELETE FROM generations WHERE state = 'staging';",
                )
            } else {
                Err(error)
            }
        })
        .map_err(sql_error)
}

fn garbage_collect(connection: &Connection) -> Result<(), StoreError> {
    let active_revision: i64 = connection
        .query_row(
            "SELECT index_revision FROM store_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if active_revision <= 2 {
        return Ok(());
    }
    let threshold = active_revision - 1;
    let cleanup_with_lookup = format!(
        "DELETE FROM resources WHERE generation_id IN
           (SELECT generation_id FROM generations WHERE state = 'retired' AND index_revision < {threshold});
         DELETE FROM source_documents WHERE generation_id IN
           (SELECT generation_id FROM generations WHERE state = 'retired' AND index_revision < {threshold});
         DELETE FROM dependency_edges WHERE generation_id IN
           (SELECT generation_id FROM generations WHERE state = 'retired' AND index_revision < {threshold});
         DELETE FROM diagnostics WHERE generation_id IN
           (SELECT generation_id FROM generations WHERE state = 'retired' AND index_revision < {threshold});
         DELETE FROM tombstones WHERE generation_id IN
           (SELECT generation_id FROM generations WHERE state = 'retired' AND index_revision < {threshold});
         DELETE FROM resource_lookup WHERE generation_id IN
           (SELECT generation_id FROM generations WHERE state = 'retired' AND index_revision < {threshold});
         DELETE FROM generations WHERE state = 'retired' AND index_revision < {threshold};"
    );
    connection
        .execute_batch(&cleanup_with_lookup)
        .or_else(|error| {
            if error.to_string().contains("no such table: resource_lookup") {
                connection.execute_batch(&cleanup_with_lookup.replace(
                    &format!(
                        "DELETE FROM resource_lookup WHERE generation_id IN\n           (SELECT generation_id FROM generations WHERE state = 'retired' AND index_revision < {threshold});\n         "
                    ),
                    "",
                ))
            } else {
                Err(error)
            }
        })
        .map_err(sql_error)
}

fn verify_integrity(connection: &Connection) -> Result<(), StoreError> {
    let result: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(sql_error)?;
    if result != "ok" {
        return Err(StoreError::CorruptStore(result));
    }
    let mut statement = connection
        .prepare("PRAGMA foreign_key_check")
        .map_err(sql_error)?;
    let mut rows = statement.query([]).map_err(sql_error)?;
    if rows.next().map_err(sql_error)?.is_some() {
        return Err(StoreError::CorruptStore(
            "foreign key integrity check failed".to_owned(),
        ));
    }
    Ok(())
}

fn validate_physical_version(connection: &Connection) -> Result<u32, StoreError> {
    let version: u32 = connection
        .query_row(
            "SELECT physical_version FROM store_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if (1..=CURRENT_PHYSICAL_VERSION).contains(&version) {
        Ok(version)
    } else {
        Err(StoreError::IncompatibleSchema)
    }
}

fn idempotent_activation(
    connection: &Connection,
    generation: &IndexGeneration,
) -> Result<Option<IndexMetadata>, StoreError> {
    let active_generation_id: Option<String> = connection
        .query_row(
            "SELECT active_generation_id FROM store_metadata WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if active_generation_id.as_deref() != Some(generation.generation_id.as_str()) {
        return Ok(None);
    }
    let persisted_digest: String = connection
        .query_row(
            "SELECT validation_digest FROM generations
             WHERE generation_id = ?1 AND state = 'active'",
            [&generation.generation_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if persisted_digest != generation.validation_digest {
        return Err(StoreError::ValidationFailed(
            "active generation identity reused with different content".to_owned(),
        ));
    }
    load_metadata(connection).map(Some)
}

fn next_metadata(previous: Option<&IndexMetadata>, generation: &IndexGeneration) -> IndexMetadata {
    let mut metadata = previous.cloned().unwrap_or(IndexMetadata {
        schema_version: generation.schema_version,
        reader_min_minor: 0,
        reader_max_minor: generation.schema_version.minor,
        project_id: generation.project_id.clone(),
        active_generation_id: None,
        index_revision: 0,
        build_state: BuildState::Empty,
        hash_algorithm: "sha256".to_owned(),
        full_rebuild_count: 0,
        incremental_commit_count: 0,
        quarantine_count: 0,
        failed_migration_count: 0,
    });
    metadata.active_generation_id = Some(generation.generation_id.clone());
    metadata.index_revision = generation.index_revision;
    metadata.build_state = BuildState::Ready;
    if generation.creation_reason == "full_snapshot" {
        metadata.full_rebuild_count += 1;
    } else {
        metadata.incremental_commit_count += 1;
    }
    metadata
}

fn hit(fault: Option<FaultInjection>, point: FaultPoint) -> Result<(), StoreError> {
    fault.map_or(Ok(()), |fault| fault.hit(point))
}

fn to_json<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|error| StoreError::StorageIo(error.to_string()))
}

fn from_json<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    serde_json::from_str(value).map_err(|error| StoreError::CorruptStore(error.to_string()))
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    if matches!(
        error,
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code: rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked,
                ..
            },
            _
        )
    ) {
        StoreError::StoreBusy
    } else {
        StoreError::StorageIo(error.to_string())
    }
}

fn io_error(error: std::io::Error) -> StoreError {
    StoreError::StorageIo(error.to_string())
}

fn sql_u64(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value)
        .map_err(|_| StoreError::ValidationFailed("revision exceeds SQLite i64".to_owned()))
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::dataset::{renamed_batch, renamed_generation, synthetic_generation};

    #[test]
    fn activation_and_reverse_query_are_generation_bound() {
        let temp = TempDir::new().expect("temp");
        let first = synthetic_generation(32, 96, 1);
        let mut store = SqliteStore::open(temp.path(), &first.project_id).expect("store");
        store.activate(&first, None).expect("first activation");
        let second = renamed_generation(&first, 2);
        store.activate(&second, None).expect("rename activation");
        assert_eq!(store.active_generation().expect("active"), second);
    }

    #[test]
    fn cancelled_precommit_preserves_active_generation() {
        let temp = TempDir::new().expect("temp");
        let first = synthetic_generation(8, 12, 1);
        let mut store = SqliteStore::open(temp.path(), &first.project_id).expect("store");
        store.activate(&first, None).expect("first activation");
        let second = renamed_generation(&first, 2);
        let result = store.activate(
            &second,
            Some(FaultInjection {
                point: FaultPoint::PreCommit,
                mode: crate::FaultMode::Cancel,
            }),
        );
        assert_eq!(result, Err(StoreError::Cancelled));
        assert_eq!(store.active_generation().expect("active"), first);
    }

    #[test]
    fn v1_to_v2_migration_preserves_logical_graph() {
        let temp = TempDir::new().expect("temp");
        let first = synthetic_generation(16, 32, 1);
        let mut store =
            SqliteStore::open_with_version(temp.path(), &first.project_id, 1).expect("v1");
        store.activate(&first, None).expect("activate v1");
        let before = store.active_generation().expect("before");
        store.migrate_v2().expect("migrate");
        let after = store.active_generation().expect("after");
        assert_eq!(store.physical_version().expect("version"), 2);
        assert_eq!(before.resources, after.resources);
        assert_eq!(before.source_documents, after.source_documents);
        assert_eq!(before.dependencies, after.dependencies);
        assert_eq!(before.diagnostics, after.diagnostics);
        assert_eq!(before.tombstones, after.tombstones);
    }

    #[test]
    fn storage_neutral_interfaces_drive_sqlite_candidate() {
        let temp = TempDir::new().expect("temp");
        let generation = synthetic_generation(8, 12, 1);
        let mut store = SqliteStore::open(temp.path(), &generation.project_id).expect("store");
        let transaction =
            GenerationBuilder::begin_generation(&mut store, &generation).expect("begin generation");
        IndexWriteTransaction::commit(transaction).expect("commit");
        let mut transaction =
            GenerationBuilder::begin_generation(&mut store, &generation).expect("begin batch");
        IndexWriteTransaction::apply_incremental_batch(
            &mut transaction,
            renamed_batch(&generation, 2),
        )
        .expect("apply batch");
        IndexWriteTransaction::commit(transaction).expect("commit batch");
        assert_eq!(
            IndexRead::metadata(&store)
                .expect("metadata")
                .index_revision,
            2
        );
    }
}
