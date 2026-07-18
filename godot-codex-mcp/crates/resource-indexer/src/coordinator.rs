use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use godot_codex_bridge_client::{
    BridgeClient, BridgeError, ProjectContextObservation, ResourceDeltaBatch,
    ResourceDeltaOperation, ResourceDeltaPoll, ResourceSnapshotTransfer, SceneDeltaBatch,
    SceneDeltaOperation, SceneDeltaPoll, SceneDiagnostic, SceneObservation, SceneSnapshot,
};
use godot_codex_index_store::{IndexReadSnapshot, SegmentIndexReader, SegmentStore, StoreError};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::watch;

use crate::{IndexerError, ResourceNormalizer, ResourceSnapshotSpool, SceneNormalizer};

const DELTA_POLL_INTERVAL: Duration = Duration::from_millis(100);
const RETRY_MIN: Duration = Duration::from_millis(200);
const RETRY_MAX: Duration = Duration::from_secs(5);

/// Safe public reason why a committed generation cannot be served as current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceIndexStaleReason {
    StartupValidation,
    BridgeDisconnected,
    JournalGap,
    Rebuilding,
    StoreUnavailable,
}

/// Observable freshness state used by the MCP boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResourceIndexStatus {
    ProjectNotBound,
    NotReady,
    Current {
        project_id: String,
        editor_session_id: String,
        generation_id: String,
        index_revision: u64,
        resource_revision: u64,
        project_revision: u64,
    },
    NotCurrent {
        reason: ResourceIndexStaleReason,
    },
    CapabilityUnavailable,
}

/// Stable availability failures consumed by resource MCP tools.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ResourceIndexReadError {
    #[error("project_not_bound")]
    ProjectNotBound,
    #[error("index_not_ready")]
    NotReady,
    #[error("index_not_current")]
    NotCurrent,
    #[error("capability_unavailable")]
    CapabilityUnavailable,
}

/// Safe public reason why scene facts cannot be served as current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SceneIndexStaleReason {
    StartupValidation,
    BridgeDisconnected,
    JournalGap,
    ResourceChanged,
    Rebuilding,
    StoreUnavailable,
}

/// Observable freshness of the independently checkpointed scene domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SceneIndexStatus {
    ProjectNotBound,
    NotReady,
    Current {
        project_id: String,
        editor_session_id: String,
        generation_id: String,
        index_revision: u64,
        resource_revision: u64,
        scene_graph_revision: u64,
    },
    NotCurrent {
        reason: SceneIndexStaleReason,
    },
    CapabilityUnavailable,
}

/// Stable availability failures consumed by scene MCP tools.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SceneIndexReadError {
    #[error("project_not_bound")]
    ProjectNotBound,
    #[error("index_not_ready")]
    NotReady,
    #[error("index_not_current")]
    NotCurrent,
    #[error("capability_unavailable")]
    CapabilityUnavailable,
}

struct ResourceIndexState {
    status: ResourceIndexStatus,
    reader: Option<SegmentIndexReader>,
}

struct SceneIndexState {
    status: SceneIndexStatus,
    reader: Option<SegmentIndexReader>,
}

/// Cloneable freshness gate over immutable segment snapshots.
#[derive(Clone)]
pub struct ResourceIndexReader {
    state: Arc<RwLock<ResourceIndexState>>,
}

/// Cloneable scene freshness gate over the same immutable segment snapshots.
#[derive(Clone)]
pub struct SceneIndexReader {
    state: Arc<RwLock<SceneIndexState>>,
}

impl Default for SceneIndexReader {
    fn default() -> Self {
        Self::new()
    }
}

impl SceneIndexReader {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(SceneIndexState {
                status: SceneIndexStatus::ProjectNotBound,
                reader: None,
            })),
        }
    }

    /// Creates a current scene reader after exact checkpoint validation.
    pub fn from_validated_store(
        store: &SegmentStore,
        editor_session_id: &str,
        scene_graph_revision: u64,
    ) -> Result<Self, StoreError> {
        let generation = store.active_generation()?;
        if generation.scene.editor_session_id != editor_session_id
            || generation.scene.scene_graph_revision != scene_graph_revision
            || !generation.scene.source_complete
        {
            return Err(StoreError::ValidationFailed(
                "validated scene checkpoint does not match active generation".to_owned(),
            ));
        }
        let reader = Self::new();
        reader.install_reader(store.reader(), true);
        reader.publish_current(store)?;
        Ok(reader)
    }

    #[must_use]
    pub fn status(&self) -> SceneIndexStatus {
        self.state.read().map_or(
            SceneIndexStatus::NotCurrent {
                reason: SceneIndexStaleReason::StoreUnavailable,
            },
            |state| state.status.clone(),
        )
    }

    /// Pins one generation only when its scene checkpoint is current.
    pub fn pin_current(&self) -> Result<IndexReadSnapshot, SceneIndexReadError> {
        let state = self
            .state
            .read()
            .map_err(|_| SceneIndexReadError::NotCurrent)?;
        let (generation_id, index_revision, scene_graph_revision) = match &state.status {
            SceneIndexStatus::ProjectNotBound => return Err(SceneIndexReadError::ProjectNotBound),
            SceneIndexStatus::NotReady => return Err(SceneIndexReadError::NotReady),
            SceneIndexStatus::NotCurrent { .. } => return Err(SceneIndexReadError::NotCurrent),
            SceneIndexStatus::CapabilityUnavailable => {
                return Err(SceneIndexReadError::CapabilityUnavailable);
            }
            SceneIndexStatus::Current {
                generation_id,
                index_revision,
                scene_graph_revision,
                ..
            } => (generation_id, *index_revision, *scene_graph_revision),
        };
        let snapshot = state
            .reader
            .as_ref()
            .ok_or(SceneIndexReadError::NotReady)?
            .snapshot()
            .map_err(|_| SceneIndexReadError::NotReady)?;
        let generation = snapshot.generation();
        if generation.generation_id != *generation_id
            || generation.index_revision != index_revision
            || generation.scene.scene_graph_revision != scene_graph_revision
            || !generation.scene.source_complete
        {
            return Err(SceneIndexReadError::NotCurrent);
        }
        Ok(snapshot)
    }

    fn install_reader(&self, reader: SegmentIndexReader, has_scene_generation: bool) {
        if let Ok(mut state) = self.state.write() {
            state.reader = Some(reader);
            state.status = if has_scene_generation {
                SceneIndexStatus::NotCurrent {
                    reason: SceneIndexStaleReason::StartupValidation,
                }
            } else {
                SceneIndexStatus::NotReady
            };
        }
    }

    fn set_status(&self, status: SceneIndexStatus) {
        if let Ok(mut state) = self.state.write() {
            state.status = status;
        }
    }

    fn publish_current(&self, store: &SegmentStore) -> Result<(), StoreError> {
        let generation = store.active_generation()?;
        if generation.scene.is_empty() || !generation.scene.source_complete {
            self.set_status(SceneIndexStatus::NotReady);
            return Ok(());
        }
        self.set_status(SceneIndexStatus::Current {
            project_id: generation.project_id.clone(),
            editor_session_id: generation.scene.editor_session_id.clone(),
            generation_id: generation.generation_id,
            index_revision: generation.index_revision,
            resource_revision: generation.scene.resource_revision,
            scene_graph_revision: generation.scene.scene_graph_revision,
        });
        Ok(())
    }
}

impl Default for ResourceIndexReader {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceIndexReader {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(ResourceIndexState {
                status: ResourceIndexStatus::ProjectNotBound,
                reader: None,
            })),
        }
    }

    /// Creates a current reader only after the caller has independently
    /// confirmed exact Bridge session/revision continuity for this store.
    pub fn from_validated_store(
        store: &SegmentStore,
        editor_session_id: &str,
        resource_revision: u64,
    ) -> Result<Self, StoreError> {
        let generation = store.active_generation()?;
        if generation.checkpoint.editor_session_id != editor_session_id
            || generation.checkpoint.resource_revision != resource_revision
        {
            return Err(StoreError::ValidationFailed(
                "validated checkpoint does not match active generation".to_owned(),
            ));
        }
        let reader = Self::new();
        reader.install_reader(store.reader(), true);
        reader.publish_current(store)?;
        Ok(reader)
    }

    /// Returns a safe status snapshot without exposing store or project paths.
    #[must_use]
    pub fn status(&self) -> ResourceIndexStatus {
        self.state.read().map_or(
            ResourceIndexStatus::NotCurrent {
                reason: ResourceIndexStaleReason::StoreUnavailable,
            },
            |state| state.status.clone(),
        )
    }

    /// Pins exactly one current generation for a complete MCP request.
    pub fn pin_current(&self) -> Result<IndexReadSnapshot, ResourceIndexReadError> {
        let state = self
            .state
            .read()
            .map_err(|_| ResourceIndexReadError::NotCurrent)?;
        let (generation_id, index_revision) = match &state.status {
            ResourceIndexStatus::ProjectNotBound => {
                return Err(ResourceIndexReadError::ProjectNotBound);
            }
            ResourceIndexStatus::NotReady => return Err(ResourceIndexReadError::NotReady),
            ResourceIndexStatus::NotCurrent { .. } => {
                return Err(ResourceIndexReadError::NotCurrent);
            }
            ResourceIndexStatus::CapabilityUnavailable => {
                return Err(ResourceIndexReadError::CapabilityUnavailable);
            }
            ResourceIndexStatus::Current {
                generation_id,
                index_revision,
                ..
            } => (generation_id, *index_revision),
        };
        let snapshot = state
            .reader
            .as_ref()
            .ok_or(ResourceIndexReadError::NotReady)?
            .snapshot()
            .map_err(|_| ResourceIndexReadError::NotReady)?;
        if snapshot.generation().generation_id != *generation_id
            || snapshot.generation().index_revision != index_revision
        {
            return Err(ResourceIndexReadError::NotCurrent);
        }
        Ok(snapshot)
    }

    fn install_reader(&self, reader: SegmentIndexReader, has_generation: bool) {
        if let Ok(mut state) = self.state.write() {
            state.reader = Some(reader);
            state.status = if has_generation {
                ResourceIndexStatus::NotCurrent {
                    reason: ResourceIndexStaleReason::StartupValidation,
                }
            } else {
                ResourceIndexStatus::NotReady
            };
        }
    }

    fn set_status(&self, status: ResourceIndexStatus) {
        if let Ok(mut state) = self.state.write() {
            state.status = status;
        }
    }

    fn publish_current(&self, store: &SegmentStore) -> Result<(), StoreError> {
        let generation = store.active_generation()?;
        self.set_status(ResourceIndexStatus::Current {
            project_id: generation.project_id.clone(),
            editor_session_id: generation.checkpoint.editor_session_id.clone(),
            generation_id: generation.generation_id,
            index_revision: generation.index_revision,
            resource_revision: generation.checkpoint.resource_revision,
            project_revision: generation.checkpoint.project_revision,
        });
        Ok(())
    }
}

/// Internal coordinator failure. Its display text is never forwarded to MCP.
#[derive(Debug, Error)]
pub enum CoordinatorError {
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    #[error(transparent)]
    Indexer(#[from] IndexerError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("index quarantine failed")]
    Quarantine,
}

impl CoordinatorError {
    fn safe_code(&self) -> &str {
        match self {
            Self::Bridge(BridgeError::Rpc { code, .. }) => code,
            Self::Bridge(BridgeError::Invalid(message))
                if !message.contains(['/', '\\']) && message.len() <= 128 =>
            {
                message
            }
            Self::Bridge(error) => error.safe_summary(),
            Self::Indexer(IndexerError::InvalidPath(code))
            | Self::Indexer(IndexerError::ObservationConflict(code))
            | Self::Indexer(IndexerError::HashUnavailable(code))
            | Self::Indexer(IndexerError::Spool(code))
            | Self::Indexer(IndexerError::Scene(code)) => code,
            Self::Indexer(IndexerError::InvalidUid) => "invalid_resource_uid",
            Self::Indexer(IndexerError::UnsafeResourcePath) => "unsafe_resource_path",
            Self::Indexer(IndexerError::Serialization) => "serialization_failed",
            Self::Indexer(IndexerError::Store(_)) | Self::Store(_) => "index_store_failed",
            Self::Quarantine => "index_quarantine_failed",
        }
    }
}

/// Owns the single writer lease and reconciles Bridge state into generations.
pub struct ResourceIndexCoordinator {
    project_root: PathBuf,
    normalizer: ResourceNormalizer,
    scene_normalizer: SceneNormalizer,
    reader: ResourceIndexReader,
    scene_reader: SceneIndexReader,
    scene_catalog: BTreeMap<String, SceneObservation>,
    project_context: Vec<ProjectContextObservation>,
    scene_diagnostics: Vec<SceneDiagnostic>,
}

impl ResourceIndexCoordinator {
    /// Creates the coordinator and its independently cloneable MCP reader.
    pub fn new(
        project_root: impl AsRef<Path>,
    ) -> Result<(Self, ResourceIndexReader), IndexerError> {
        let (coordinator, resource_reader, _) = Self::new_semantic(project_root)?;
        Ok((coordinator, resource_reader))
    }

    /// Creates the single semantic coordinator and both independent readers.
    pub fn new_semantic(
        project_root: impl AsRef<Path>,
    ) -> Result<(Self, ResourceIndexReader, SceneIndexReader), IndexerError> {
        let normalizer = ResourceNormalizer::new(project_root.as_ref())?;
        let project_root = fs::canonicalize(project_root.as_ref())
            .map_err(|_| IndexerError::UnsafeResourcePath)?;
        let reader = ResourceIndexReader::new();
        let scene_reader = SceneIndexReader::new();
        Ok((
            Self {
                project_root,
                normalizer,
                scene_normalizer: SceneNormalizer,
                reader: reader.clone(),
                scene_reader: scene_reader.clone(),
                scene_catalog: BTreeMap::new(),
                project_context: Vec::new(),
                scene_diagnostics: Vec::new(),
            },
            reader,
            scene_reader,
        ))
    }

    /// Runs until shutdown. Disconnects never make persisted data readable as current.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        let mut store = None;
        let mut project_id = None;
        let mut retry = RETRY_MIN;
        while !*shutdown.borrow() {
            let connect = BridgeClient::connect(&self.project_root);
            let client = tokio::select! {
                result = connect => match result {
                    Ok(client) => client,
                    Err(error) => {
                        eprintln!("[godot-codex-index] connect failed: {}", error.safe_summary());
                        self.mark_disconnected(&store);
                        if wait_or_shutdown(retry, &mut shutdown).await { break; }
                        retry = (retry * 2).min(RETRY_MAX);
                        continue;
                    }
                },
                _ = shutdown.changed() => break,
            };
            retry = RETRY_MIN;
            if !client.negotiated_profile().resource_graph_available {
                self.reader
                    .set_status(ResourceIndexStatus::CapabilityUnavailable);
                self.scene_reader
                    .set_status(SceneIndexStatus::CapabilityUnavailable);
                if wait_or_shutdown(RETRY_MAX, &mut shutdown).await {
                    break;
                }
                continue;
            }
            if !client.negotiated_profile().scene_graph_available {
                self.scene_reader
                    .set_status(SceneIndexStatus::CapabilityUnavailable);
            }
            if project_id
                .as_deref()
                .is_some_and(|known| known != client.project_id())
            {
                self.reader.set_status(ResourceIndexStatus::ProjectNotBound);
                self.scene_reader
                    .set_status(SceneIndexStatus::ProjectNotBound);
                break;
            }
            project_id.get_or_insert_with(|| client.project_id().to_owned());
            if store.is_none() {
                match self.open_store(client.project_id()) {
                    Ok(opened) => store = Some(opened),
                    Err(error) => {
                        eprintln!("[godot-codex-index] open failed: {}", error.safe_code());
                        self.reader.set_status(ResourceIndexStatus::NotReady);
                        self.scene_reader.set_status(SceneIndexStatus::NotReady);
                        if wait_or_shutdown(retry, &mut shutdown).await {
                            break;
                        }
                        retry = (retry * 2).min(RETRY_MAX);
                        continue;
                    }
                }
            }
            let mut client = client;
            let result = tokio::select! {
                result = self.sync_connected(&mut client, store.as_mut().expect("store opened")) => result,
                _ = shutdown.changed() => break,
            };
            if let Err(error) = result {
                eprintln!("[godot-codex-index] sync failed: {}", error.safe_code());
                self.mark_disconnected(&store);
                if wait_or_shutdown(retry, &mut shutdown).await {
                    break;
                }
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
        self.mark_disconnected(&store);
    }

    fn open_store(&self, project_id: &str) -> Result<SegmentStore, CoordinatorError> {
        let mut store = match SegmentStore::open(&self.project_root, project_id) {
            Ok(store) => store,
            Err(
                StoreError::CorruptStore(_)
                | StoreError::IncompatibleSchema
                | StoreError::ProjectMismatch,
            ) => {
                quarantine_index(&self.project_root)?;
                SegmentStore::open(&self.project_root, project_id)?
            }
            Err(error) => return Err(error.into()),
        };
        if store.active_generation().is_ok()
            && store.physical_version()? < godot_codex_index_store::SEGMENT_PHYSICAL_VERSION
        {
            store.migrate_current()?;
        }
        let has_generation = store.active_generation().is_ok();
        let has_scene_generation = store
            .active_generation()
            .is_ok_and(|generation| !generation.scene.is_empty());
        self.reader.install_reader(store.reader(), has_generation);
        self.scene_reader
            .install_reader(store.reader(), has_scene_generation);
        Ok(store)
    }

    async fn sync_connected(
        &mut self,
        client: &mut BridgeClient,
        store: &mut SegmentStore,
    ) -> Result<(), CoordinatorError> {
        let scene_available = client.negotiated_profile().scene_graph_available;
        let active = match store.active_generation() {
            Ok(generation) => Some(generation),
            Err(StoreError::NotReady) => None,
            Err(error) => return Err(error.into()),
        };
        match active {
            None => self.full_snapshot(client, store).await?,
            Some(generation)
                if generation.checkpoint.editor_session_id != client.editor_session_id() =>
            {
                self.validate_reopened_editor(client, store, &generation)
                    .await?;
            }
            Some(_) => {}
        }
        self.reader.publish_current(store)?;
        if scene_available {
            self.full_scene_snapshot(client, store).await?;
        } else {
            self.scene_reader
                .set_status(SceneIndexStatus::CapabilityUnavailable);
        }

        loop {
            let mut changed = false;
            let base = store.active_generation()?;
            match client
                .get_next_resource_delta(base.checkpoint.resource_revision)
                .await?
            {
                ResourceDeltaPoll::Current { .. } => {
                    self.reader.publish_current(store)?;
                }
                ResourceDeltaPoll::Batch { batch, .. } => {
                    changed = true;
                    let invalidates_scene = resource_batch_affects_scene(&batch);
                    if invalidates_scene {
                        self.scene_reader.set_status(SceneIndexStatus::NotCurrent {
                            reason: SceneIndexStaleReason::ResourceChanged,
                        });
                    }
                    self.reader.set_status(ResourceIndexStatus::NotCurrent {
                        reason: ResourceIndexStaleReason::Rebuilding,
                    });
                    match self.normalizer.normalize_incremental_batch(&base, &batch) {
                        Ok(Some(batch)) => {
                            let next = base.apply_incremental_batch(&batch)?;
                            store.activate(&next, None)?;
                            self.reader.publish_current(store)?;
                            if scene_available && !invalidates_scene {
                                self.scene_reader.publish_current(store)?;
                            }
                        }
                        Ok(None) => {}
                        Err(_) => {
                            self.reader.set_status(ResourceIndexStatus::NotCurrent {
                                reason: ResourceIndexStaleReason::JournalGap,
                            });
                            self.full_snapshot(client, store).await?;
                        }
                    }
                }
                ResourceDeltaPoll::Gap { .. } => {
                    changed = true;
                    self.reader.set_status(ResourceIndexStatus::NotCurrent {
                        reason: ResourceIndexStaleReason::JournalGap,
                    });
                    if scene_available {
                        self.scene_reader.set_status(SceneIndexStatus::NotCurrent {
                            reason: SceneIndexStaleReason::ResourceChanged,
                        });
                    }
                    self.full_snapshot(client, store).await?;
                    self.reader.publish_current(store)?;
                }
            }

            if scene_available {
                let active = store.active_generation()?;
                if active.scene.is_empty()
                    || active.scene.editor_session_id != client.editor_session_id()
                {
                    changed = true;
                    self.full_scene_snapshot(client, store).await?;
                } else {
                    match client
                        .get_next_scene_delta(active.scene.scene_graph_revision)
                        .await?
                    {
                        SceneDeltaPoll::Current { .. } => {
                            if active.scene.resource_revision == active.checkpoint.resource_revision
                            {
                                self.scene_reader.publish_current(store)?;
                            }
                        }
                        SceneDeltaPoll::Batch { batch, .. } => {
                            changed = true;
                            self.scene_reader.set_status(SceneIndexStatus::NotCurrent {
                                reason: SceneIndexStaleReason::Rebuilding,
                            });
                            if batch.resource_revision
                                != store.active_generation()?.checkpoint.resource_revision
                            {
                                self.full_snapshot(client, store).await?;
                                self.reader.publish_current(store)?;
                                self.full_scene_snapshot(client, store).await?;
                            } else if self.apply_scene_batch(store, &batch).is_err() {
                                self.scene_reader.set_status(SceneIndexStatus::NotCurrent {
                                    reason: SceneIndexStaleReason::JournalGap,
                                });
                                self.full_scene_snapshot(client, store).await?;
                            }
                        }
                        SceneDeltaPoll::Gap { .. } => {
                            changed = true;
                            self.scene_reader.set_status(SceneIndexStatus::NotCurrent {
                                reason: SceneIndexStaleReason::JournalGap,
                            });
                            self.full_scene_snapshot(client, store).await?;
                        }
                    }
                }
            }
            if !changed {
                tokio::time::sleep(DELTA_POLL_INTERVAL).await;
            }
        }
    }

    async fn full_snapshot(
        &mut self,
        client: &mut BridgeClient,
        store: &mut SegmentStore,
    ) -> Result<(), CoordinatorError> {
        let next_index_revision = next_index_revision(store)?;
        self.reader.set_status(if next_index_revision == 1 {
            ResourceIndexStatus::NotReady
        } else {
            ResourceIndexStatus::NotCurrent {
                reason: ResourceIndexStaleReason::Rebuilding,
            }
        });
        let generation = self
            .capture_full_snapshot(client, next_index_revision)
            .await?;
        store.activate(&generation, None)?;
        Ok(())
    }

    async fn validate_reopened_editor(
        &mut self,
        client: &mut BridgeClient,
        store: &mut SegmentStore,
        active: &godot_codex_index_store::IndexGeneration,
    ) -> Result<(), CoordinatorError> {
        let next_index_revision = active
            .index_revision
            .checked_add(1)
            .ok_or(IndexerError::ObservationConflict("index_revision_overflow"))?;
        self.reader.set_status(ResourceIndexStatus::NotCurrent {
            reason: ResourceIndexStaleReason::StartupValidation,
        });
        let observed = self
            .capture_full_snapshot(client, next_index_revision)
            .await?;
        if !store.reuse_compatible_generation(&observed)? {
            self.reader.set_status(ResourceIndexStatus::NotCurrent {
                reason: ResourceIndexStaleReason::Rebuilding,
            });
            store.activate(&observed, None)?;
        }
        Ok(())
    }

    async fn capture_full_snapshot(
        &self,
        client: &mut BridgeClient,
        index_revision: u64,
    ) -> Result<godot_codex_index_store::IndexGeneration, CoordinatorError> {
        let staging = self
            .project_root
            .join(".godot")
            .join("codex")
            .join("index")
            .join("staging");
        let mut spool = ResourceSnapshotSpool::create(&staging)?;
        let transfer = client.stream_resource_snapshot(&mut spool).await?;
        validate_transfer_binding(client, &transfer)?;
        let snapshot = spool.confirmed_snapshot()?;
        let generation = self.normalizer.normalize_full_snapshot(
            client.project_id(),
            index_revision,
            &snapshot,
        )?;
        Ok(generation)
    }

    async fn full_scene_snapshot(
        &mut self,
        client: &mut BridgeClient,
        store: &mut SegmentStore,
    ) -> Result<(), CoordinatorError> {
        self.scene_reader.set_status(
            if store
                .active_generation()
                .is_ok_and(|generation| generation.scene.is_empty())
            {
                SceneIndexStatus::NotReady
            } else {
                SceneIndexStatus::NotCurrent {
                    reason: SceneIndexStaleReason::Rebuilding,
                }
            },
        );
        let snapshot = client.get_scene_snapshot().await?;
        validate_scene_snapshot_binding(client, store, &snapshot)?;
        self.scene_catalog = snapshot
            .payload
            .scenes
            .iter()
            .cloned()
            .map(|scene| (scene.path.clone(), scene))
            .collect();
        self.project_context = snapshot.payload.project_context.clone();
        self.scene_diagnostics = snapshot.payload.diagnostics.clone();
        let base = store.active_generation()?;
        let scene = self
            .scene_normalizer
            .normalize_full_snapshot(&base, &snapshot)?;
        self.activate_scene_domain(store, scene, "scene_full_snapshot")?;
        Ok(())
    }

    fn apply_scene_batch(
        &mut self,
        store: &mut SegmentStore,
        batch: &SceneDeltaBatch,
    ) -> Result<(), CoordinatorError> {
        let base = store.active_generation()?;
        if base.scene.is_empty()
            || batch.previous_scene_graph_revision != base.scene.scene_graph_revision
            || batch.scene_graph_revision <= batch.previous_scene_graph_revision
            || batch.resource_revision != base.checkpoint.resource_revision
            || !batch.source_complete
        {
            return Err(IndexerError::Scene("scene_delta_continuity").into());
        }
        let mut catalog = self.scene_catalog.clone();
        let mut project_context = self.project_context.clone();
        for operation in &batch.operations {
            match operation {
                SceneDeltaOperation::Upsert { value } => {
                    catalog.insert(value.path.clone(), (**value).clone());
                }
                SceneDeltaOperation::Remove { path, .. } => {
                    if catalog.remove(path).is_none() {
                        return Err(IndexerError::Scene("scene_delta_remove_missing").into());
                    }
                }
                SceneDeltaOperation::ProjectContext { values } => {
                    project_context.clone_from(values);
                }
            }
        }
        let observations: Vec<_> = catalog.values().cloned().collect();
        let scene = self.scene_normalizer.normalize_observations(
            &base,
            &base.scene.editor_session_id,
            batch.resource_revision,
            batch.scene_graph_revision,
            &batch.checksum,
            &observations,
            &project_context,
            &[],
        )?;
        self.activate_scene_domain(store, scene, "scene_incremental_batch")?;
        self.scene_catalog = catalog;
        self.project_context = project_context;
        self.scene_diagnostics.clear();
        Ok(())
    }

    fn activate_scene_domain(
        &self,
        store: &mut SegmentStore,
        scene: godot_codex_index_store::SceneDomainGeneration,
        creation_reason: &str,
    ) -> Result<(), CoordinatorError> {
        let base = store.active_generation()?;
        let mut next = base.clone();
        next.parent_generation_id = Some(base.generation_id);
        next.index_revision = next
            .index_revision
            .checked_add(1)
            .ok_or(IndexerError::Scene("index_revision_overflow"))?;
        next.checkpoint.index_revision = next.index_revision;
        next.generation_id = scene_generation_id(
            &next.project_id,
            &scene.editor_session_id,
            scene.scene_graph_revision,
            next.index_revision,
            &scene.validation_digest,
        );
        next.creation_reason = creation_reason.to_owned();
        next.scene = scene;
        next.canonicalize();
        next.validation_digest.clear();
        next.validation_digest = next.compute_validation_digest();
        next.validate()?;
        store.activate(&next, None)?;
        self.reader.publish_current(store)?;
        self.scene_reader.publish_current(store)?;
        Ok(())
    }

    fn mark_disconnected(&self, store: &Option<SegmentStore>) {
        self.reader.set_status(
            if store
                .as_ref()
                .is_some_and(|store| store.active_generation().is_ok())
            {
                ResourceIndexStatus::NotCurrent {
                    reason: ResourceIndexStaleReason::BridgeDisconnected,
                }
            } else {
                ResourceIndexStatus::NotReady
            },
        );
        self.scene_reader.set_status(
            if store.as_ref().is_some_and(|store| {
                store
                    .active_generation()
                    .is_ok_and(|generation| !generation.scene.is_empty())
            }) {
                SceneIndexStatus::NotCurrent {
                    reason: SceneIndexStaleReason::BridgeDisconnected,
                }
            } else {
                SceneIndexStatus::NotReady
            },
        );
    }
}

fn next_index_revision(store: &SegmentStore) -> Result<u64, CoordinatorError> {
    match store.active_generation() {
        Ok(generation) => generation
            .index_revision
            .checked_add(1)
            .ok_or_else(|| IndexerError::ObservationConflict("index_revision_overflow").into()),
        Err(StoreError::NotReady) => Ok(1),
        Err(error) => Err(error.into()),
    }
}

fn validate_transfer_binding(
    client: &BridgeClient,
    transfer: &ResourceSnapshotTransfer,
) -> Result<(), CoordinatorError> {
    if transfer.accepted.revisions.editor_session_id != client.editor_session_id()
        || transfer.end.revisions.editor_session_id != client.editor_session_id()
        || transfer.accepted.resource_revision != transfer.end.resource_revision
    {
        return Err(IndexerError::ObservationConflict("snapshot_session_changed").into());
    }
    Ok(())
}

fn validate_scene_snapshot_binding(
    client: &BridgeClient,
    store: &SegmentStore,
    snapshot: &SceneSnapshot,
) -> Result<(), CoordinatorError> {
    let active = store.active_generation()?;
    if snapshot.accepted.revisions.editor_session_id != client.editor_session_id()
        || snapshot.end.revisions.editor_session_id != client.editor_session_id()
        || snapshot.accepted.resource_revision != snapshot.end.resource_revision
        || snapshot.accepted.scene_graph_revision != snapshot.end.scene_graph_revision
        || snapshot.end.resource_revision != active.checkpoint.resource_revision
    {
        return Err(IndexerError::Scene("scene_snapshot_session_changed").into());
    }
    Ok(())
}

fn resource_batch_affects_scene(batch: &ResourceDeltaBatch) -> bool {
    batch.operations.iter().any(|operation| match operation {
        ResourceDeltaOperation::Upsert { value }
        | ResourceDeltaOperation::Reimport { value }
        | ResourceDeltaOperation::Move { value, .. } => {
            value.resource.godot_type == "PackedScene" || is_scene_path(&value.resource.path)
        }
        ResourceDeltaOperation::Remove { path, .. } => is_scene_path(path),
    })
}

fn is_scene_path(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| matches!(extension, "tscn" | "scn"))
}

fn scene_generation_id(
    project_id: &str,
    editor_session_id: &str,
    scene_graph_revision: u64,
    index_revision: u64,
    validation_digest: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"godot-codex/scene-generation-id/v1\0");
    for value in [
        project_id,
        editor_session_id,
        &scene_graph_revision.to_string(),
        &index_revision.to_string(),
        validation_digest,
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("generation:scene:sha256:{:x}", hasher.finalize())
}

fn quarantine_index(project_root: &Path) -> Result<(), CoordinatorError> {
    let codex = project_root.join(".godot").join("codex");
    let index = codex.join("index");
    if !index.exists() {
        return Ok(());
    }
    let quarantine = codex.join("quarantine");
    fs::create_dir_all(&quarantine).map_err(|_| CoordinatorError::Quarantine)?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CoordinatorError::Quarantine)?
        .as_nanos();
    fs::rename(index, quarantine.join(format!("index-{timestamp:032x}")))
        .map_err(|_| CoordinatorError::Quarantine)
}

async fn wait_or_shutdown(duration: Duration, shutdown: &mut watch::Receiver<bool>) -> bool {
    if *shutdown.borrow() {
        return true;
    }
    tokio::select! {
        () = tokio::time::sleep(duration) => false,
        result = shutdown.changed() => result.is_err() || *shutdown.borrow(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_never_serves_non_current_states() {
        let reader = ResourceIndexReader::new();
        assert_eq!(
            reader.pin_current().err(),
            Some(ResourceIndexReadError::ProjectNotBound)
        );
        reader.set_status(ResourceIndexStatus::NotReady);
        assert_eq!(
            reader.pin_current().err(),
            Some(ResourceIndexReadError::NotReady)
        );
        reader.set_status(ResourceIndexStatus::NotCurrent {
            reason: ResourceIndexStaleReason::JournalGap,
        });
        assert_eq!(
            reader.pin_current().err(),
            Some(ResourceIndexReadError::NotCurrent)
        );
        reader.set_status(ResourceIndexStatus::CapabilityUnavailable);
        assert_eq!(
            reader.pin_current().err(),
            Some(ResourceIndexReadError::CapabilityUnavailable)
        );

        let scene_reader = SceneIndexReader::new();
        assert_eq!(
            scene_reader.pin_current().err(),
            Some(SceneIndexReadError::ProjectNotBound)
        );
        scene_reader.set_status(SceneIndexStatus::NotReady);
        assert_eq!(
            scene_reader.pin_current().err(),
            Some(SceneIndexReadError::NotReady)
        );
        scene_reader.set_status(SceneIndexStatus::NotCurrent {
            reason: SceneIndexStaleReason::ResourceChanged,
        });
        assert_eq!(
            scene_reader.pin_current().err(),
            Some(SceneIndexReadError::NotCurrent)
        );
        scene_reader.set_status(SceneIndexStatus::CapabilityUnavailable);
        assert_eq!(
            scene_reader.pin_current().err(),
            Some(SceneIndexReadError::CapabilityUnavailable)
        );
    }

    #[test]
    fn only_scene_resource_batches_invalidate_scene_freshness() {
        let batch = |path: &str, godot_type: &str| -> ResourceDeltaBatch {
            serde_json::from_value(serde_json::json!({
                "batch_id": "resource-batch:2",
                "previous_resource_revision": 1,
                "resource_revision": 2,
                "project_revision": 2,
                "operations": [{
                    "kind": "upsert",
                    "value": {
                        "resource": {
                            "resource_ref": {"uid": "uid://test"},
                            "path": path,
                            "godot_type": godot_type,
                            "source_kind": "source",
                            "import_state": "valid",
                            "modified_time_unix_seconds": 1,
                            "byte_size": 1,
                            "validity": "valid",
                            "authority": "editor_file_system",
                            "resource_revision": 2
                        },
                        "dependencies": []
                    }
                }],
                "source_complete": true,
                "checksum": "checksum"
            }))
            .expect("resource delta")
        };
        assert!(resource_batch_affects_scene(&batch(
            "res://main.tscn",
            "PackedScene"
        )));
        assert!(!resource_batch_affects_scene(&batch(
            "res://theme.tres",
            "Theme"
        )));
    }
}
