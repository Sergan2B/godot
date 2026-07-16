//! Spike adapter over the production immutable segment store.

use std::path::Path;

use godot_codex_index_store::{
    GenerationBuilder, IndexGeneration, IndexMetadata, ResourceQuery, ResourceQueryResult,
    SegmentFaultInjection, SegmentFaultMode, SegmentFaultPoint,
    SegmentStore as ProductionSegmentStore, SegmentTransaction, StoreError,
};

use crate::{BackendKind, FaultInjection, FaultMode, FaultPoint, SpikeStore};

/// Compatibility wrapper preserving the original spike evidence version namespace.
pub struct SegmentStore {
    inner: ProductionSegmentStore,
}

impl SegmentStore {
    pub fn open(project_root: &Path, project_id: &str) -> Result<Self, StoreError> {
        Ok(Self {
            inner: ProductionSegmentStore::open(project_root, project_id)?,
        })
    }

    pub fn open_with_version(
        project_root: &Path,
        project_id: &str,
        spike_version: u32,
    ) -> Result<Self, StoreError> {
        let production_version = match spike_version {
            1 => 0,
            2 => 1,
            _ => return Err(StoreError::IncompatibleSchema),
        };
        Ok(Self {
            inner: ProductionSegmentStore::open_with_version(
                project_root,
                project_id,
                production_version,
            )?,
        })
    }

    pub fn read_generation(
        project_root: &Path,
        project_id: &str,
    ) -> Result<IndexGeneration, StoreError> {
        ProductionSegmentStore::read_generation(project_root, project_id)
    }
}

impl SpikeStore for SegmentStore {
    fn kind(&self) -> BackendKind {
        BackendKind::Segment
    }

    fn activate(
        &mut self,
        generation: &IndexGeneration,
        fault: Option<FaultInjection>,
    ) -> Result<IndexMetadata, StoreError> {
        self.inner.activate(generation, fault.map(map_fault))
    }

    fn active_generation(&self) -> Result<IndexGeneration, StoreError> {
        self.inner.active_generation()
    }

    fn direct(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError> {
        self.inner.direct(query)
    }

    fn reverse(&self, query: &ResourceQuery) -> Result<ResourceQueryResult, StoreError> {
        self.inner.reverse(query)
    }

    fn migrate_v2(&mut self) -> Result<IndexMetadata, StoreError> {
        self.inner.migrate_current()
    }

    fn physical_version(&self) -> Result<u32, StoreError> {
        self.inner.physical_version().map(|version| version + 1)
    }
}

impl GenerationBuilder for SegmentStore {
    type Transaction<'a> = SegmentTransaction<'a>;

    fn begin_generation<'a>(
        &'a mut self,
        generation: &IndexGeneration,
    ) -> Result<Self::Transaction<'a>, StoreError> {
        GenerationBuilder::begin_generation(&mut self.inner, generation)
    }
}

fn map_fault(fault: FaultInjection) -> SegmentFaultInjection {
    SegmentFaultInjection {
        point: match fault.point {
            FaultPoint::Capture => SegmentFaultPoint::Capture,
            FaultPoint::Staging => SegmentFaultPoint::Staging,
            FaultPoint::PreCommit => SegmentFaultPoint::PreCommit,
            FaultPoint::PostCommit => SegmentFaultPoint::PostCommit,
        },
        mode: match fault.mode {
            FaultMode::Cancel => SegmentFaultMode::Cancel,
            FaultMode::Crash => SegmentFaultMode::Crash,
        },
    }
}

#[cfg(test)]
mod tests {
    use godot_codex_index_store::{GenerationBuilder, IndexRead, IndexWriteTransaction};
    use tempfile::TempDir;

    use super::*;
    use crate::dataset::{renamed_batch, synthetic_generation};

    #[test]
    fn storage_neutral_interfaces_drive_production_segment_store() {
        let temp = TempDir::new().expect("temp");
        let generation = synthetic_generation(8, 12, 1);
        let mut store = SegmentStore::open(temp.path(), &generation.project_id).expect("store");
        let transaction =
            GenerationBuilder::begin_generation(&mut store, &generation).expect("begin");
        IndexWriteTransaction::commit(transaction).expect("commit");
        let mut transaction =
            GenerationBuilder::begin_generation(&mut store, &generation).expect("begin batch");
        IndexWriteTransaction::apply_incremental_batch(
            &mut transaction,
            renamed_batch(&generation, 2),
        )
        .expect("batch");
        IndexWriteTransaction::commit(transaction).expect("commit batch");
        assert_eq!(IndexRead::metadata(&store.inner).unwrap().index_revision, 2);
    }
}
