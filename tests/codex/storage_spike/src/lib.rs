//! Reproducible SQLite-versus-segment storage spike for decision D-05.

pub mod dataset;
pub mod evidence;
pub mod runner;
pub mod segment;
#[cfg(feature = "sqlite")]
pub mod sqlite;

use std::path::Path;

use godot_codex_index_store::{
    IndexGeneration, IndexMetadata, ResourceQuery, ResourceQueryResult, StoreError,
};
use serde::{Deserialize, Serialize};

/// Stable backend names used by evidence and CLI arguments.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    /// Bundled SQLite candidate.
    Sqlite,
    /// Immutable content-addressed segment candidate.
    Segment,
}

impl BackendKind {
    /// Stable evidence spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sqlite => "sqlite",
            Self::Segment => "segment",
        }
    }
}

/// Named cancellation/crash boundaries shared by both backends.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultPoint {
    /// Before the first staging write.
    Capture,
    /// After half of normalized records have been staged.
    Staging,
    /// After validation/flush and before activation.
    PreCommit,
    /// After durable activation but before acknowledgement/cleanup.
    PostCommit,
}

/// Behavior triggered at a selected fault boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaultMode {
    /// Return `cancelled` and unwind normally.
    Cancel,
    /// Terminate the worker immediately without destructors.
    Crash,
}

/// One optional failpoint used only by the spike harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FaultInjection {
    /// Boundary to trigger.
    pub point: FaultPoint,
    /// Graceful or abrupt behavior.
    pub mode: FaultMode,
}

impl FaultInjection {
    /// Triggers this fault when `point` matches.
    pub fn hit(self, point: FaultPoint) -> Result<(), StoreError> {
        if self.point != point {
            return Ok(());
        }
        match self.mode {
            FaultMode::Cancel => Err(StoreError::Cancelled),
            FaultMode::Crash => {
                if let Some(path) = std::env::var_os("CODEX_STORAGE_SPIKE_FAULT_READY") {
                    let mut ready =
                        std::fs::File::create(path).unwrap_or_else(|_| std::process::exit(87));
                    use std::io::Write as _;
                    ready
                        .write_all(format!("{point:?}").as_bytes())
                        .unwrap_or_else(|_| std::process::exit(87));
                    ready.sync_all().unwrap_or_else(|_| std::process::exit(87));
                    loop {
                        std::thread::park_timeout(std::time::Duration::from_secs(60));
                    }
                }
                std::process::exit(86)
            }
        }
    }
}

/// Common physical-store behavior exercised by the spike runner.
pub trait SpikeStore {
    /// Candidate kind.
    fn kind(&self) -> BackendKind;
    /// Stages, validates, and atomically activates one generation.
    fn activate(
        &mut self,
        generation: &IndexGeneration,
        fault: Option<FaultInjection>,
    ) -> Result<IndexMetadata, StoreError>;
    /// Loads and validates the active logical generation.
    fn active_generation(&self) -> Result<IndexGeneration, StoreError>;
    /// Executes a direct query.
    fn direct(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError>;
    /// Executes a reverse query.
    fn reverse(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError>;
    /// Migrates physical v1 to v2 through a new generation.
    fn migrate_v2(&mut self) -> Result<IndexMetadata, StoreError>;
    /// Current physical format version.
    fn physical_version(&self) -> Result<u32, StoreError>;
}

/// Opens one backend with an exclusive writer lease.
pub fn open_store(
    kind: BackendKind,
    root: &Path,
    project_id: &str,
) -> Result<Box<dyn SpikeStore>, StoreError> {
    match kind {
        #[cfg(feature = "sqlite")]
        BackendKind::Sqlite => Ok(Box::new(sqlite::SqliteStore::open(root, project_id)?)),
        #[cfg(not(feature = "sqlite"))]
        BackendKind::Sqlite => Err(StoreError::StorageIo(
            "sqlite feature is disabled".to_owned(),
        )),
        BackendKind::Segment => Ok(Box::new(segment::SegmentStore::open(root, project_id)?)),
    }
}
