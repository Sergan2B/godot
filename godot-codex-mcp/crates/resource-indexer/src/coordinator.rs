use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use godot_codex_bridge_client::{
    BridgeClient, BridgeError, NormalizedScriptGraph, ProjectContextObservation,
    ResourceDeltaBatch, ResourceDeltaOperation, ResourceDeltaPoll, ResourceRef,
    ResourceSnapshotTransfer, SceneDeltaBatch, SceneDeltaOperation, SceneDeltaPoll,
    SceneDiagnostic, SceneObservation, SceneSnapshot, ScriptDeltaBatch, ScriptDeltaOperation,
    ScriptDeltaPoll, ScriptSnapshot, ScriptSnapshotPayload, normalize_script_graph,
    normalize_script_snapshot,
};
use godot_codex_index_store::{
    IndexReadSnapshot, ScriptDomainGeneration, SegmentIndexReader, SegmentStore,
    SemanticQueryIndex, StoreError,
};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::watch;

use crate::{
    IndexerError, ResourceNormalizer, ResourceSnapshotSpool, SceneNormalizer, ScriptNormalizer,
    ScriptSnapshotSpool, load_verified_offline_authority, normalize_resource_path,
    persist_offline_authority,
};

const DELTA_POLL_INTERVAL: Duration = Duration::from_millis(20);
const CATALOG_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const RETRY_MIN: Duration = Duration::from_millis(200);
const RETRY_MAX: Duration = Duration::from_secs(5);

/// Process-local observation of the canonical `index.lock` project lease.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectSessionState {
    Acquiring,
    Owner,
    Busy,
}

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
    ProjectSessionBusy,
    NotReady,
    Current {
        project_id: String,
        editor_session_id: String,
        generation_id: String,
        index_revision: u64,
        resource_revision: u64,
        project_revision: u64,
    },
    /// A complete persisted generation independently matched every saved source.
    OfflineCurrent {
        project_id: String,
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
    #[error("project_session_busy")]
    ProjectSessionBusy,
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
    ProjectSessionBusy,
    NotReady,
    Current {
        project_id: String,
        editor_session_id: String,
        generation_id: String,
        index_revision: u64,
        resource_revision: u64,
        scene_graph_revision: u64,
    },
    /// A saved scene domain covered by verified offline authority.
    OfflineCurrent {
        project_id: String,
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
    #[error("project_session_busy")]
    ProjectSessionBusy,
    #[error("index_not_ready")]
    NotReady,
    #[error("index_not_current")]
    NotCurrent,
    #[error("capability_unavailable")]
    CapabilityUnavailable,
}

/// Safe public reason why script facts cannot be served as current.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScriptIndexStaleReason {
    StartupValidation,
    BridgeDisconnected,
    JournalGap,
    ResourceChanged,
    SceneChanged,
    Rebuilding,
    CompositionFailed,
    StoreUnavailable,
}

/// Observable freshness of the independently checkpointed script domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptIndexStatus {
    ProjectNotBound,
    ProjectSessionBusy,
    NotReady,
    Current {
        project_id: String,
        editor_session_id: String,
        generation_id: String,
        index_revision: u64,
        resource_revision: u64,
        scene_graph_revision: u64,
        script_graph_revision: u64,
    },
    /// A saved script domain covered by verified offline authority.
    OfflineCurrent {
        project_id: String,
        generation_id: String,
        index_revision: u64,
        resource_revision: u64,
        scene_graph_revision: u64,
        script_graph_revision: u64,
    },
    NotCurrent {
        reason: ScriptIndexStaleReason,
    },
    CapabilityUnavailable,
}

/// Stable availability failures consumed by the Sprint 5 symbol tools.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ScriptIndexReadError {
    #[error("project_not_bound")]
    ProjectNotBound,
    #[error("project_session_busy")]
    ProjectSessionBusy,
    #[error("index_not_ready")]
    NotReady,
    #[error("index_not_current")]
    NotCurrent,
    #[error("capability_unavailable")]
    CapabilityUnavailable,
}

/// Independently freshness-gated domain omitted from one semantic read.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SemanticPartialDomain {
    Scene,
    Script,
}

/// Stable reason why one optional semantic domain was excluded.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SemanticPartialCode {
    ProjectNotBound,
    ProjectSessionBusy,
    NotReady,
    NotCurrent,
    CapabilityUnavailable,
}

/// One explicit partial marker returned alongside current facts.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SemanticPartialReason {
    pub domain: SemanticPartialDomain,
    pub code: SemanticPartialCode,
}

/// Stable failures for the required resource-domain semantic snapshot.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum SemanticIndexReadError {
    #[error("project_not_bound")]
    ProjectNotBound,
    #[error("project_session_busy")]
    ProjectSessionBusy,
    #[error("index_not_ready")]
    NotReady,
    #[error("index_not_current")]
    NotCurrent,
    #[error("capability_unavailable")]
    CapabilityUnavailable,
    #[error("torn_generation")]
    TornGeneration,
}

struct ResourceIndexState {
    status: ResourceIndexStatus,
    reader: Option<SegmentIndexReader>,
}

struct SceneIndexState {
    status: SceneIndexStatus,
    reader: Option<SegmentIndexReader>,
}

struct ScriptIndexState {
    status: ScriptIndexStatus,
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

/// Cloneable script freshness gate over immutable segment snapshots.
#[derive(Clone)]
pub struct ScriptIndexReader {
    state: Arc<RwLock<ScriptIndexState>>,
}

/// Joins independently gated resource, scene, and script readers for one request.
#[derive(Clone)]
pub struct SemanticIndexReader {
    resource: ResourceIndexReader,
    scene: SceneIndexReader,
    script: ScriptIndexReader,
}

/// One immutable semantic view with explicit exclusions for non-current domains.
pub struct SemanticIndexSnapshot {
    snapshot: IndexReadSnapshot,
    query_index: Arc<SemanticQueryIndex>,
    partial_reasons: Vec<SemanticPartialReason>,
    freshness: SemanticIndexFreshness,
}

/// Authority attached to one semantic response.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticIndexFreshness {
    OnlineCurrent,
    OfflineCached,
}

impl SemanticIndexSnapshot {
    #[must_use]
    pub fn generation(&self) -> &godot_codex_index_store::IndexGeneration {
        self.snapshot.generation()
    }

    #[must_use]
    pub fn query_index(&self) -> &SemanticQueryIndex {
        &self.query_index
    }

    #[must_use]
    pub fn partial_reasons(&self) -> &[SemanticPartialReason] {
        &self.partial_reasons
    }

    #[must_use]
    pub fn freshness(&self) -> SemanticIndexFreshness {
        self.freshness
    }
}

impl SemanticIndexReader {
    #[must_use]
    pub fn new(
        resource: ResourceIndexReader,
        scene: SceneIndexReader,
        script: ScriptIndexReader,
    ) -> Self {
        Self {
            resource,
            scene,
            script,
        }
    }

    /// Pins the required resource domain and includes only scene/script domains
    /// that independently validate against the exact same generation.
    pub fn pin_current(&self) -> Result<SemanticIndexSnapshot, SemanticIndexReadError> {
        let snapshot = self.resource.pin_current().map_err(map_resource_error)?;
        let freshness = match self.resource.status() {
            ResourceIndexStatus::OfflineCurrent {
                generation_id,
                index_revision,
                ..
            } if generation_id == snapshot.generation().generation_id
                && index_revision == snapshot.generation().index_revision =>
            {
                SemanticIndexFreshness::OfflineCached
            }
            ResourceIndexStatus::Current {
                generation_id,
                index_revision,
                ..
            } if generation_id == snapshot.generation().generation_id
                && index_revision == snapshot.generation().index_revision =>
            {
                SemanticIndexFreshness::OnlineCurrent
            }
            _ => return Err(SemanticIndexReadError::NotCurrent),
        };
        let mut partial_reasons = Vec::new();
        let scene_current = match self.scene.pin_current() {
            Ok(scene) => {
                ensure_same_generation(snapshot.generation(), scene.generation(), false)?;
                true
            }
            Err(error) => {
                partial_reasons.push(SemanticPartialReason {
                    domain: SemanticPartialDomain::Scene,
                    code: map_scene_partial(error),
                });
                false
            }
        };
        let script_current = match self.script.pin_current() {
            Ok(script) => {
                ensure_same_generation(snapshot.generation(), script.generation(), true)?;
                true
            }
            Err(error) => {
                partial_reasons.push(SemanticPartialReason {
                    domain: SemanticPartialDomain::Script,
                    code: map_script_partial(error),
                });
                false
            }
        };
        let query_index = if scene_current && script_current {
            snapshot.semantic_query_index()
        } else {
            Arc::new(SemanticQueryIndex::build_available(
                snapshot.generation(),
                scene_current,
                script_current,
            ))
        };
        Ok(SemanticIndexSnapshot {
            snapshot,
            query_index,
            partial_reasons,
            freshness,
        })
    }
}

fn ensure_same_generation(
    expected: &godot_codex_index_store::IndexGeneration,
    actual: &godot_codex_index_store::IndexGeneration,
    include_script: bool,
) -> Result<(), SemanticIndexReadError> {
    if expected.generation_id != actual.generation_id
        || expected.index_revision != actual.index_revision
        || expected.checkpoint.resource_revision != actual.checkpoint.resource_revision
        || expected.scene.scene_graph_revision != actual.scene.scene_graph_revision
        || (include_script
            && expected.script.script_graph_revision != actual.script.script_graph_revision)
    {
        return Err(SemanticIndexReadError::TornGeneration);
    }
    Ok(())
}

fn map_resource_error(error: ResourceIndexReadError) -> SemanticIndexReadError {
    match error {
        ResourceIndexReadError::ProjectNotBound => SemanticIndexReadError::ProjectNotBound,
        ResourceIndexReadError::ProjectSessionBusy => SemanticIndexReadError::ProjectSessionBusy,
        ResourceIndexReadError::NotReady => SemanticIndexReadError::NotReady,
        ResourceIndexReadError::NotCurrent => SemanticIndexReadError::NotCurrent,
        ResourceIndexReadError::CapabilityUnavailable => {
            SemanticIndexReadError::CapabilityUnavailable
        }
    }
}

fn map_scene_partial(error: SceneIndexReadError) -> SemanticPartialCode {
    match error {
        SceneIndexReadError::ProjectNotBound => SemanticPartialCode::ProjectNotBound,
        SceneIndexReadError::ProjectSessionBusy => SemanticPartialCode::ProjectSessionBusy,
        SceneIndexReadError::NotReady => SemanticPartialCode::NotReady,
        SceneIndexReadError::NotCurrent => SemanticPartialCode::NotCurrent,
        SceneIndexReadError::CapabilityUnavailable => SemanticPartialCode::CapabilityUnavailable,
    }
}

fn map_script_partial(error: ScriptIndexReadError) -> SemanticPartialCode {
    match error {
        ScriptIndexReadError::ProjectNotBound => SemanticPartialCode::ProjectNotBound,
        ScriptIndexReadError::ProjectSessionBusy => SemanticPartialCode::ProjectSessionBusy,
        ScriptIndexReadError::NotReady => SemanticPartialCode::NotReady,
        ScriptIndexReadError::NotCurrent => SemanticPartialCode::NotCurrent,
        ScriptIndexReadError::CapabilityUnavailable => SemanticPartialCode::CapabilityUnavailable,
    }
}

impl Default for ScriptIndexReader {
    fn default() -> Self {
        Self::new()
    }
}

impl ScriptIndexReader {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(ScriptIndexState {
                status: ScriptIndexStatus::ProjectNotBound,
                reader: None,
            })),
        }
    }

    /// Creates a current script reader after exact checkpoint validation.
    pub fn from_validated_store(
        store: &SegmentStore,
        editor_session_id: &str,
        resource_revision: u64,
        scene_graph_revision: u64,
        script_graph_revision: u64,
    ) -> Result<Self, StoreError> {
        let generation = store.active_generation()?;
        if generation.script.editor_session_id != editor_session_id
            || generation.script.resource_revision != resource_revision
            || generation.script.scene_graph_revision != scene_graph_revision
            || generation.script.script_graph_revision != script_graph_revision
            || !generation.script.source_complete
        {
            return Err(StoreError::ValidationFailed(
                "validated script checkpoint does not match active generation".to_owned(),
            ));
        }
        let reader = Self::new();
        reader.install_reader(store.reader(), true);
        reader.publish_current(store)?;
        Ok(reader)
    }

    #[must_use]
    pub fn status(&self) -> ScriptIndexStatus {
        self.state.read().map_or(
            ScriptIndexStatus::NotCurrent {
                reason: ScriptIndexStaleReason::StoreUnavailable,
            },
            |state| state.status.clone(),
        )
    }

    /// Pins one generation only when its complete script checkpoint is current.
    pub fn pin_current(&self) -> Result<IndexReadSnapshot, ScriptIndexReadError> {
        let state = self
            .state
            .read()
            .map_err(|_| ScriptIndexReadError::NotCurrent)?;
        let (generation_id, index_revision, script_graph_revision) = match &state.status {
            ScriptIndexStatus::ProjectNotBound => {
                return Err(ScriptIndexReadError::ProjectNotBound);
            }
            ScriptIndexStatus::ProjectSessionBusy => {
                return Err(ScriptIndexReadError::ProjectSessionBusy);
            }
            ScriptIndexStatus::NotReady => return Err(ScriptIndexReadError::NotReady),
            ScriptIndexStatus::NotCurrent { .. } => {
                return Err(ScriptIndexReadError::NotCurrent);
            }
            ScriptIndexStatus::CapabilityUnavailable => {
                return Err(ScriptIndexReadError::CapabilityUnavailable);
            }
            ScriptIndexStatus::Current {
                generation_id,
                index_revision,
                script_graph_revision,
                ..
            }
            | ScriptIndexStatus::OfflineCurrent {
                generation_id,
                index_revision,
                script_graph_revision,
                ..
            } => (generation_id, *index_revision, *script_graph_revision),
        };
        let snapshot = state
            .reader
            .as_ref()
            .ok_or(ScriptIndexReadError::NotReady)?
            .snapshot()
            .map_err(|_| ScriptIndexReadError::NotReady)?;
        let generation = snapshot.generation();
        if generation.generation_id != *generation_id
            || generation.index_revision != index_revision
            || generation.script.script_graph_revision != script_graph_revision
            || !generation.script.source_complete
        {
            return Err(ScriptIndexReadError::NotCurrent);
        }
        Ok(snapshot)
    }

    fn install_reader(&self, reader: SegmentIndexReader, has_script_generation: bool) {
        if let Ok(mut state) = self.state.write() {
            state.reader = Some(reader);
            state.status = if has_script_generation {
                ScriptIndexStatus::NotCurrent {
                    reason: ScriptIndexStaleReason::StartupValidation,
                }
            } else {
                ScriptIndexStatus::NotReady
            };
        }
    }

    fn set_status(&self, status: ScriptIndexStatus) {
        if let Ok(mut state) = self.state.write() {
            state.status = status;
        }
    }

    fn publish_current(&self, store: &SegmentStore) -> Result<(), StoreError> {
        let generation = store.active_generation()?;
        if generation.script.is_empty() || !generation.script.source_complete {
            self.set_status(ScriptIndexStatus::NotReady);
            return Ok(());
        }
        self.set_status(ScriptIndexStatus::Current {
            project_id: generation.project_id.clone(),
            editor_session_id: generation.script.editor_session_id.clone(),
            generation_id: generation.generation_id,
            index_revision: generation.index_revision,
            resource_revision: generation.script.resource_revision,
            scene_graph_revision: generation.script.scene_graph_revision,
            script_graph_revision: generation.script.script_graph_revision,
        });
        Ok(())
    }

    fn publish_offline_current(&self, store: &SegmentStore) -> Result<(), StoreError> {
        let generation = store.active_generation()?;
        if generation.script.is_empty() || !generation.script.source_complete {
            return Err(StoreError::ValidationFailed(
                "offline script generation is incomplete".to_owned(),
            ));
        }
        self.set_status(ScriptIndexStatus::OfflineCurrent {
            project_id: generation.project_id.clone(),
            generation_id: generation.generation_id,
            index_revision: generation.index_revision,
            resource_revision: generation.script.resource_revision,
            scene_graph_revision: generation.script.scene_graph_revision,
            script_graph_revision: generation.script.script_graph_revision,
        });
        Ok(())
    }
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
            SceneIndexStatus::ProjectSessionBusy => {
                return Err(SceneIndexReadError::ProjectSessionBusy);
            }
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
            }
            | SceneIndexStatus::OfflineCurrent {
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

    fn publish_offline_current(&self, store: &SegmentStore) -> Result<(), StoreError> {
        let generation = store.active_generation()?;
        if generation.scene.is_empty() || !generation.scene.source_complete {
            return Err(StoreError::ValidationFailed(
                "offline scene generation is incomplete".to_owned(),
            ));
        }
        self.set_status(SceneIndexStatus::OfflineCurrent {
            project_id: generation.project_id.clone(),
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
            ResourceIndexStatus::ProjectSessionBusy => {
                return Err(ResourceIndexReadError::ProjectSessionBusy);
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
            }
            | ResourceIndexStatus::OfflineCurrent {
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

    fn publish_offline_current(&self, store: &SegmentStore) -> Result<(), StoreError> {
        let generation = store.active_generation()?;
        if !generation.checkpoint.source_complete {
            return Err(StoreError::ValidationFailed(
                "offline resource generation is incomplete".to_owned(),
            ));
        }
        self.set_status(ResourceIndexStatus::OfflineCurrent {
            project_id: generation.project_id.clone(),
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
            Self::Bridge(error) => error.failure_class().as_str(),
            Self::Indexer(IndexerError::InvalidPath(code))
            | Self::Indexer(IndexerError::ObservationConflict(code))
            | Self::Indexer(IndexerError::HashUnavailable(code))
            | Self::Indexer(IndexerError::Spool(code))
            | Self::Indexer(IndexerError::Scene(code))
            | Self::Indexer(IndexerError::Script(code)) => code,
            Self::Indexer(IndexerError::InvalidUid) => "invalid_resource_uid",
            Self::Indexer(IndexerError::UnsafeResourcePath) => "unsafe_resource_path",
            Self::Indexer(IndexerError::Serialization) => "serialization_failed",
            Self::Indexer(IndexerError::Store(_)) | Self::Store(_) => "index_store_failed",
            Self::Quarantine => "index_quarantine_failed",
        }
    }
}

fn coordinator_sync_failure_line(error: &CoordinatorError) -> String {
    format!("[godot-codex-index] sync failed: {}", error.safe_code())
}

/// Owns the single writer lease and reconciles Bridge state into generations.
pub struct ResourceIndexCoordinator {
    project_root: PathBuf,
    expected_project_id: String,
    store: Option<SegmentStore>,
    project_session: watch::Sender<ProjectSessionState>,
    offline_authority_generation: Option<String>,
    online_activation_pending: bool,
    normalizer: ResourceNormalizer,
    scene_normalizer: SceneNormalizer,
    reader: ResourceIndexReader,
    scene_reader: SceneIndexReader,
    script_reader: ScriptIndexReader,
    script_normalizer: ScriptNormalizer,
    scene_catalog: BTreeMap<String, SceneObservation>,
    project_context: Vec<ProjectContextObservation>,
    scene_diagnostics: Vec<SceneDiagnostic>,
    script_payload: Option<ScriptSnapshotPayload>,
    script_graph: Option<NormalizedScriptGraph>,
}

impl ResourceIndexCoordinator {
    /// Creates the coordinator and its independently cloneable MCP reader.
    pub fn new(
        project_root: impl AsRef<Path>,
    ) -> Result<(Self, ResourceIndexReader), IndexerError> {
        let (coordinator, resource_reader, _, _) = Self::new_semantic(project_root)?;
        Ok((coordinator, resource_reader))
    }

    /// Creates the single semantic coordinator and all independent readers.
    pub fn new_semantic(
        project_root: impl AsRef<Path>,
    ) -> Result<
        (
            Self,
            ResourceIndexReader,
            SceneIndexReader,
            ScriptIndexReader,
        ),
        IndexerError,
    > {
        let normalizer = ResourceNormalizer::new(project_root.as_ref())?;
        let project_root = fs::canonicalize(project_root.as_ref())
            .map_err(|_| IndexerError::UnsafeResourcePath)?;
        let reader = ResourceIndexReader::new();
        let scene_reader = SceneIndexReader::new();
        let script_reader = ScriptIndexReader::new();
        let expected_project_id = godot_codex_bridge_client::project_id_for_path(&project_root)
            .map_err(|_| IndexerError::UnsafeResourcePath)?;
        let mut store = None;
        let mut offline_authority_generation = None;
        let (project_session, _) = watch::channel(ProjectSessionState::Acquiring);
        match SegmentStore::open(&project_root, &expected_project_id) {
            Ok(opened) => {
                project_session.send_replace(ProjectSessionState::Owner);
                let has_generation = opened.active_generation().is_ok();
                let has_scene_generation = opened
                    .active_generation()
                    .is_ok_and(|generation| !generation.scene.is_empty());
                let has_script_generation = opened
                    .active_generation()
                    .is_ok_and(|generation| !generation.script.is_empty());
                reader.install_reader(opened.reader(), has_generation);
                scene_reader.install_reader(opened.reader(), has_scene_generation);
                script_reader.install_reader(opened.reader(), has_script_generation);
                if let Ok(generation) = opened.active_generation()
                    && load_verified_offline_authority(
                        &project_root,
                        &expected_project_id,
                        &generation,
                    )
                    .is_ok()
                    && reader.publish_offline_current(&opened).is_ok()
                    && scene_reader.publish_offline_current(&opened).is_ok()
                    && script_reader.publish_offline_current(&opened).is_ok()
                {
                    offline_authority_generation = Some(generation.generation_id);
                }
                store = Some(opened);
            }
            Err(StoreError::StoreBusy) => {
                project_session.send_replace(ProjectSessionState::Busy);
                reader.set_status(ResourceIndexStatus::ProjectSessionBusy);
                scene_reader.set_status(SceneIndexStatus::ProjectSessionBusy);
                script_reader.set_status(ScriptIndexStatus::ProjectSessionBusy);
            }
            Err(StoreError::NotReady) => {}
            Err(_) => {
                reader.set_status(ResourceIndexStatus::NotCurrent {
                    reason: ResourceIndexStaleReason::StoreUnavailable,
                });
                scene_reader.set_status(SceneIndexStatus::NotCurrent {
                    reason: SceneIndexStaleReason::StoreUnavailable,
                });
                script_reader.set_status(ScriptIndexStatus::NotCurrent {
                    reason: ScriptIndexStaleReason::StoreUnavailable,
                });
            }
        }
        Ok((
            Self {
                project_root,
                expected_project_id,
                store,
                project_session,
                offline_authority_generation,
                online_activation_pending: false,
                normalizer,
                scene_normalizer: SceneNormalizer,
                reader: reader.clone(),
                scene_reader: scene_reader.clone(),
                script_reader: script_reader.clone(),
                script_normalizer: ScriptNormalizer,
                scene_catalog: BTreeMap::new(),
                project_context: Vec::new(),
                scene_diagnostics: Vec::new(),
                script_payload: None,
                script_graph: None,
            },
            reader,
            scene_reader,
            script_reader,
        ))
    }

    /// Subscribes to canonical same-project lease ownership transitions.
    #[must_use]
    pub fn subscribe_project_session(&self) -> watch::Receiver<ProjectSessionState> {
        self.project_session.subscribe()
    }

    /// Runs until shutdown. Only an independently verified source manifest can
    /// retain a persisted generation as offline-current.
    pub async fn run(mut self, mut shutdown: watch::Receiver<bool>) {
        let mut retry = RETRY_MIN;
        while !*shutdown.borrow() {
            if self.store.is_none() {
                match self.open_store(&self.expected_project_id) {
                    Ok(opened) => {
                        self.project_session
                            .send_replace(ProjectSessionState::Owner);
                        self.store = Some(opened);
                        retry = RETRY_MIN;
                    }
                    Err(CoordinatorError::Store(StoreError::StoreBusy)) => {
                        self.project_session.send_replace(ProjectSessionState::Busy);
                        self.reader
                            .set_status(ResourceIndexStatus::ProjectSessionBusy);
                        self.scene_reader
                            .set_status(SceneIndexStatus::ProjectSessionBusy);
                        self.script_reader
                            .set_status(ScriptIndexStatus::ProjectSessionBusy);
                        if wait_or_shutdown(retry, &mut shutdown).await {
                            break;
                        }
                        retry = (retry * 2).min(RETRY_MAX);
                        continue;
                    }
                    Err(error) => {
                        self.project_session
                            .send_replace(ProjectSessionState::Acquiring);
                        eprintln!("[godot-codex-index] open failed: {}", error.safe_code());
                        self.reader.set_status(ResourceIndexStatus::NotReady);
                        self.scene_reader.set_status(SceneIndexStatus::NotReady);
                        self.script_reader.set_status(ScriptIndexStatus::NotReady);
                        if wait_or_shutdown(retry, &mut shutdown).await {
                            break;
                        }
                        retry = (retry * 2).min(RETRY_MAX);
                        continue;
                    }
                }
            }
            let connect = BridgeClient::connect(&self.project_root);
            let client = tokio::select! {
                result = connect => match result {
                    Ok(client) => client,
                    Err(error) => {
                        eprintln!("[godot-codex-index] connect failed: {}", error.safe_summary());
                        self.mark_disconnected();
                        if wait_or_shutdown(retry, &mut shutdown).await { break; }
                        retry = (retry * 2).min(RETRY_MAX);
                        continue;
                    }
                },
                _ = shutdown.changed() => break,
            };
            if !client.negotiated_profile().resource_graph_available {
                self.reader
                    .set_status(ResourceIndexStatus::CapabilityUnavailable);
                self.scene_reader
                    .set_status(SceneIndexStatus::CapabilityUnavailable);
                self.script_reader
                    .set_status(ScriptIndexStatus::CapabilityUnavailable);
                if wait_or_shutdown(RETRY_MAX, &mut shutdown).await {
                    break;
                }
                continue;
            }
            if !client.negotiated_profile().scene_graph_available {
                self.scene_reader
                    .set_status(SceneIndexStatus::CapabilityUnavailable);
            }
            if !client.negotiated_profile().script_graph_available {
                self.script_reader
                    .set_status(ScriptIndexStatus::CapabilityUnavailable);
            }
            if client.project_id() != self.expected_project_id {
                self.reader.set_status(ResourceIndexStatus::ProjectNotBound);
                self.scene_reader
                    .set_status(SceneIndexStatus::ProjectNotBound);
                self.script_reader
                    .set_status(ScriptIndexStatus::ProjectNotBound);
                break;
            }
            let mut client = client;
            let mut store = self.store.take().expect("store opened");
            let result = tokio::select! {
                result = self.sync_connected(&mut client, &mut store) => result,
                _ = shutdown.changed() => {
                    self.store = Some(store);
                    break;
                },
            };
            self.store = Some(store);
            if let Err(error) = result {
                eprintln!("{}", coordinator_sync_failure_line(&error));
                self.mark_disconnected();
                if wait_or_shutdown(retry, &mut shutdown).await {
                    break;
                }
                retry = (retry * 2).min(RETRY_MAX);
            }
        }
        self.project_session
            .send_replace(ProjectSessionState::Acquiring);
        self.mark_disconnected();
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
        let has_script_generation = store
            .active_generation()
            .is_ok_and(|generation| !generation.script.is_empty());
        self.reader.install_reader(store.reader(), has_generation);
        self.scene_reader
            .install_reader(store.reader(), has_scene_generation);
        self.script_reader
            .install_reader(store.reader(), has_script_generation);
        Ok(store)
    }

    async fn sync_connected(
        &mut self,
        client: &mut BridgeClient,
        store: &mut SegmentStore,
    ) -> Result<(), CoordinatorError> {
        let scene_available = client.negotiated_profile().scene_graph_available;
        let script_available = client.negotiated_profile().script_graph_available;
        // Offline authority proves saved files, not continuity with a newly
        // authenticated editor. Every connection therefore starts from a full
        // three-domain observation before any online-current status is exposed.
        self.offline_authority_generation = None;
        self.online_activation_pending = true;
        self.reader.set_status(ResourceIndexStatus::NotCurrent {
            reason: ResourceIndexStaleReason::StartupValidation,
        });
        self.scene_reader.set_status(SceneIndexStatus::NotCurrent {
            reason: SceneIndexStaleReason::StartupValidation,
        });
        self.script_reader
            .set_status(ScriptIndexStatus::NotCurrent {
                reason: ScriptIndexStaleReason::StartupValidation,
            });
        self.full_snapshot(client, store).await?;
        if scene_available {
            self.full_scene_snapshot(client, store).await?;
        } else {
            self.scene_reader
                .set_status(SceneIndexStatus::CapabilityUnavailable);
        }
        if script_available {
            self.full_script_snapshot(client, store).await?;
        } else {
            self.script_reader
                .set_status(ScriptIndexStatus::CapabilityUnavailable);
        }
        self.online_activation_pending = false;
        self.reader.publish_current(store)?;
        if scene_available {
            self.scene_reader.publish_current(store)?;
        }
        if script_available {
            self.script_reader.publish_current(store)?;
        }
        self.refresh_offline_authority(store);

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
                    let script_was_current = matches!(
                        self.script_reader.status(),
                        ScriptIndexStatus::Current { .. }
                    );
                    let invalidates_script_for_scene = invalidates_scene
                        && base.script.relations.iter().any(|relation| {
                            relation.predicate
                                == godot_codex_index_store::ScriptPredicate::AttachesScript
                        });
                    let invalidates_script = script_available
                        && (invalidates_script_for_scene
                            || resource_batch_affects_script(&base, &batch));
                    if invalidates_scene {
                        self.scene_reader.set_status(SceneIndexStatus::NotCurrent {
                            reason: SceneIndexStaleReason::ResourceChanged,
                        });
                    }
                    if invalidates_script {
                        self.script_reader
                            .set_status(ScriptIndexStatus::NotCurrent {
                                reason: if invalidates_script_for_scene {
                                    ScriptIndexStaleReason::SceneChanged
                                } else {
                                    ScriptIndexStaleReason::ResourceChanged
                                },
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
                            if script_was_current && !invalidates_script {
                                self.script_reader.publish_current(store)?;
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
                    if script_available {
                        self.script_reader
                            .set_status(ScriptIndexStatus::NotCurrent {
                                reason: ScriptIndexStaleReason::ResourceChanged,
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
            if script_available {
                let active = store.active_generation()?;
                let waiting_for_scene =
                    matches!(
                        self.script_reader.status(),
                        ScriptIndexStatus::NotCurrent {
                            reason: ScriptIndexStaleReason::SceneChanged
                        }
                    ) && !matches!(self.scene_reader.status(), SceneIndexStatus::Current { .. });
                if waiting_for_scene {
                    if !changed {
                        tokio::time::sleep(DELTA_POLL_INTERVAL).await;
                    }
                    continue;
                }
                let must_resnapshot = active.script.is_empty()
                    || active.script.editor_session_id != client.editor_session_id()
                    || matches!(
                        self.script_reader.status(),
                        ScriptIndexStatus::NotCurrent {
                            reason: ScriptIndexStaleReason::SceneChanged
                                | ScriptIndexStaleReason::CompositionFailed
                                | ScriptIndexStaleReason::StartupValidation
                        }
                    );
                if must_resnapshot {
                    changed = true;
                    self.full_script_snapshot(client, store).await?;
                } else {
                    match next_script_delta_when_ready(client, active.script.script_graph_revision)
                        .await?
                    {
                        ScriptDeltaPoll::Current { .. } => {
                            if active.script.resource_revision
                                != active.checkpoint.resource_revision
                            {
                                changed = true;
                                self.full_script_snapshot(client, store).await?;
                            } else {
                                self.script_reader.publish_current(store)?;
                            }
                        }
                        ScriptDeltaPoll::Batch { batch, .. } => {
                            changed = true;
                            self.script_reader
                                .set_status(ScriptIndexStatus::NotCurrent {
                                    reason: ScriptIndexStaleReason::Rebuilding,
                                });
                            if batch.resource_revision
                                != store.active_generation()?.checkpoint.resource_revision
                            {
                                self.full_snapshot(client, store).await?;
                                self.reader.publish_current(store)?;
                                if scene_available {
                                    self.full_scene_snapshot(client, store).await?;
                                }
                                self.full_script_snapshot(client, store).await?;
                            } else if self.apply_script_batch(store, &batch).is_err() {
                                self.script_reader
                                    .set_status(ScriptIndexStatus::NotCurrent {
                                        reason: ScriptIndexStaleReason::JournalGap,
                                    });
                                self.full_script_snapshot(client, store).await?;
                            }
                        }
                        ScriptDeltaPoll::Gap { .. } => {
                            changed = true;
                            self.script_reader
                                .set_status(ScriptIndexStatus::NotCurrent {
                                    reason: ScriptIndexStaleReason::JournalGap,
                                });
                            self.full_script_snapshot(client, store).await?;
                        }
                    }
                }
            }
            self.refresh_offline_authority(store);
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
        self.scene_reader.set_status(
            if store
                .active_generation()
                .map_or(true, |generation| generation.scene.is_empty())
            {
                SceneIndexStatus::NotReady
            } else {
                SceneIndexStatus::NotCurrent {
                    reason: SceneIndexStaleReason::ResourceChanged,
                }
            },
        );
        self.script_reader.set_status(
            if store
                .active_generation()
                .map_or(true, |generation| generation.script.is_empty())
            {
                ScriptIndexStatus::NotReady
            } else {
                ScriptIndexStatus::NotCurrent {
                    reason: ScriptIndexStaleReason::ResourceChanged,
                }
            },
        );
        self.script_payload = None;
        self.script_graph = None;
        let generation = self
            .capture_full_snapshot(client, next_index_revision)
            .await?;
        store.activate(&generation, None)?;
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
        let (transfer, snapshot) = loop {
            let mut spool = ResourceSnapshotSpool::create(&staging)?;
            match client.stream_resource_snapshot(&mut spool).await {
                Ok(transfer) => break (transfer, spool.confirmed_snapshot()?),
                Err(error) if is_catalog_building(&error, "resource_catalog_building") => {
                    tokio::time::sleep(CATALOG_RETRY_INTERVAL).await;
                }
                Err(error) => return Err(error.into()),
            }
        };
        validate_transfer_binding(client, &transfer)?;
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
                .map_or(true, |generation| generation.scene.is_empty())
            {
                SceneIndexStatus::NotReady
            } else {
                SceneIndexStatus::NotCurrent {
                    reason: SceneIndexStaleReason::Rebuilding,
                }
            },
        );
        let mut snapshot = loop {
            match client.get_scene_snapshot().await {
                Ok(snapshot) => break snapshot,
                Err(error) if is_catalog_building(&error, "scene_catalog_building") => {
                    tokio::time::sleep(CATALOG_RETRY_INTERVAL).await;
                }
                Err(error) => return Err(error.into()),
            }
        };
        if snapshot.end.resource_revision != store.active_generation()?.checkpoint.resource_revision
        {
            // A saved file can advance the resource clock before the scene
            // adapter emits any scene delta. Rebase the resource domain first,
            // then capture one scene snapshot against that exact checkpoint.
            self.full_snapshot(client, store).await?;
            if !self.online_activation_pending {
                self.reader.publish_current(store)?;
            }
            snapshot = loop {
                match client.get_scene_snapshot().await {
                    Ok(snapshot) => break snapshot,
                    Err(error) if is_catalog_building(&error, "scene_catalog_building") => {
                        tokio::time::sleep(CATALOG_RETRY_INTERVAL).await;
                    }
                    Err(error) => return Err(error.into()),
                }
            };
        }
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
        &mut self,
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
        let script_can_recompose = matches!(
            self.script_reader.status(),
            ScriptIndexStatus::Current { .. }
                | ScriptIndexStatus::NotCurrent {
                    reason: ScriptIndexStaleReason::SceneChanged
                }
        );
        let mut script_recomposed = false;
        let mut script_composition_failed = false;
        if script_can_recompose && let Some(graph) = &self.script_graph {
            match self.script_normalizer.normalize_graph(
                &next,
                &next.checkpoint.editor_session_id,
                graph,
            ) {
                Ok(script) => {
                    next.script = script;
                    script_recomposed = true;
                }
                Err(_) => {
                    script_composition_failed = true;
                }
            }
        }
        next.canonicalize();
        next.validation_digest.clear();
        next.validation_digest = next.compute_validation_digest();
        next.validate()?;
        store.activate(&next, None)?;
        if !self.online_activation_pending {
            self.reader.publish_current(store)?;
            self.scene_reader.publish_current(store)?;
        }
        if script_recomposed && !self.online_activation_pending {
            self.script_reader.publish_current(store)?;
        } else if script_composition_failed {
            self.script_reader
                .set_status(ScriptIndexStatus::NotCurrent {
                    reason: ScriptIndexStaleReason::CompositionFailed,
                });
        } else if script_can_recompose {
            self.script_reader
                .set_status(ScriptIndexStatus::NotCurrent {
                    reason: ScriptIndexStaleReason::SceneChanged,
                });
        }
        Ok(())
    }

    async fn full_script_snapshot(
        &mut self,
        client: &mut BridgeClient,
        store: &mut SegmentStore,
    ) -> Result<(), CoordinatorError> {
        self.script_reader.set_status(
            if store
                .active_generation()
                .map_or(true, |generation| generation.script.is_empty())
            {
                ScriptIndexStatus::NotReady
            } else {
                ScriptIndexStatus::NotCurrent {
                    reason: ScriptIndexStaleReason::Rebuilding,
                }
            },
        );
        let staging = self
            .project_root
            .join(".godot")
            .join("codex")
            .join("index")
            .join("staging");
        let snapshot = loop {
            let mut spool = ScriptSnapshotSpool::create(&staging)?;
            match client.stream_script_snapshot(&mut spool).await {
                Ok(_) => break spool.confirmed_snapshot()?,
                Err(error) if is_catalog_building(&error, "script_catalog_building") => {
                    tokio::time::sleep(CATALOG_RETRY_INTERVAL).await;
                }
                Err(error) => return Err(error.into()),
            }
        };
        validate_script_snapshot_binding(client, store, &snapshot)?;
        let graph = normalize_script_snapshot(&snapshot)?;
        let base = store.active_generation()?;
        let composition_base = self.script_composition_base(&base);
        let script = self.script_normalizer.normalize_graph(
            &composition_base,
            client.editor_session_id(),
            &graph,
        )?;
        let payload = script_payload_from_graph(&graph);
        self.activate_script_domain(store, script, "script_full_snapshot")?;
        self.script_payload = Some(payload);
        self.script_graph = Some(graph);
        Ok(())
    }

    fn apply_script_batch(
        &mut self,
        store: &mut SegmentStore,
        batch: &ScriptDeltaBatch,
    ) -> Result<(), CoordinatorError> {
        let base = store.active_generation()?;
        if base.script.is_empty()
            || batch.previous_script_graph_revision != base.script.script_graph_revision
            || batch.script_graph_revision <= batch.previous_script_graph_revision
            || batch.resource_revision != base.checkpoint.resource_revision
            || !batch.source_complete
        {
            return Err(IndexerError::Script("script_delta_continuity").into());
        }
        if !matches!(
            self.scene_reader.status(),
            SceneIndexStatus::CapabilityUnavailable
        ) && !base.scene.is_empty()
            && batch.scene_graph_revision > base.scene.scene_graph_revision
        {
            return Err(IndexerError::Script("script_delta_scene_ahead").into());
        }
        let mut payload = self
            .script_payload
            .clone()
            .ok_or(IndexerError::Script("script_delta_catalog_missing"))?;
        apply_script_operations(&mut payload, batch)?;
        let graph = normalize_script_graph(
            &payload,
            batch.resource_revision,
            batch.scene_graph_revision,
            batch.script_graph_revision,
        )?;
        let composition_base = self.script_composition_base(&base);
        let script = self.script_normalizer.normalize_graph(
            &composition_base,
            &base.checkpoint.editor_session_id,
            &graph,
        )?;
        self.activate_script_domain(store, script, "script_incremental_batch")?;
        self.script_payload = Some(script_payload_from_graph(&graph));
        self.script_graph = Some(graph);
        Ok(())
    }

    fn script_composition_base(
        &self,
        base: &godot_codex_index_store::IndexGeneration,
    ) -> godot_codex_index_store::IndexGeneration {
        let mut composition = base.clone();
        if matches!(
            self.scene_reader.status(),
            SceneIndexStatus::CapabilityUnavailable
        ) {
            composition.scene = godot_codex_index_store::SceneDomainGeneration::default();
        }
        composition
    }

    fn activate_script_domain(
        &self,
        store: &mut SegmentStore,
        script: ScriptDomainGeneration,
        creation_reason: &str,
    ) -> Result<(), CoordinatorError> {
        let base = store.active_generation()?;
        let scene_was_current =
            matches!(self.scene_reader.status(), SceneIndexStatus::Current { .. });
        let mut next = base.clone();
        next.parent_generation_id = Some(base.generation_id);
        next.index_revision = next
            .index_revision
            .checked_add(1)
            .ok_or(IndexerError::Script("index_revision_overflow"))?;
        next.checkpoint.index_revision = next.index_revision;
        next.generation_id = script_generation_id(
            &next.project_id,
            &script.editor_session_id,
            script.script_graph_revision,
            next.index_revision,
            &script.validation_digest,
        );
        next.creation_reason = creation_reason.to_owned();
        next.script = script;
        next.canonicalize();
        next.validation_digest.clear();
        next.validation_digest = next.compute_validation_digest();
        next.validate()?;
        store.activate(&next, None)?;
        if !self.online_activation_pending {
            self.reader.publish_current(store)?;
            if scene_was_current {
                self.scene_reader.publish_current(store)?;
            }
            self.script_reader.publish_current(store)?;
        }
        Ok(())
    }

    fn refresh_offline_authority(&mut self, store: &SegmentStore) {
        let Ok(generation) = store.active_generation() else {
            return;
        };
        if self
            .offline_authority_generation
            .as_deref()
            .is_some_and(|current| current == generation.generation_id)
        {
            return;
        }
        let all_current = matches!(
            self.reader.status(),
            ResourceIndexStatus::Current {
                ref generation_id,
                ..
            } if generation_id == &generation.generation_id
        ) && matches!(
            self.scene_reader.status(),
            SceneIndexStatus::Current {
                ref generation_id,
                ..
            } if generation_id == &generation.generation_id
        ) && matches!(
            self.script_reader.status(),
            ScriptIndexStatus::Current {
                ref generation_id,
                ..
            } if generation_id == &generation.generation_id
        );
        if !all_current {
            return;
        }
        match persist_offline_authority(&self.project_root, &generation) {
            Ok(manifest) => {
                self.offline_authority_generation = Some(manifest.generation_id);
            }
            Err(error) => {
                eprintln!(
                    "[godot-codex-index] offline authority unavailable: {}",
                    error
                );
                self.offline_authority_generation = None;
            }
        }
    }

    fn mark_disconnected(&mut self) {
        if *self.project_session.borrow() == ProjectSessionState::Busy {
            self.reader
                .set_status(ResourceIndexStatus::ProjectSessionBusy);
            self.scene_reader
                .set_status(SceneIndexStatus::ProjectSessionBusy);
            self.script_reader
                .set_status(ScriptIndexStatus::ProjectSessionBusy);
            return;
        }
        let verified_offline = self.store.as_ref().is_some_and(|store| {
            store.active_generation().is_ok_and(|generation| {
                load_verified_offline_authority(
                    &self.project_root,
                    &self.expected_project_id,
                    &generation,
                )
                .is_ok()
                    && self.reader.publish_offline_current(store).is_ok()
                    && self.scene_reader.publish_offline_current(store).is_ok()
                    && self.script_reader.publish_offline_current(store).is_ok()
            })
        });
        if verified_offline {
            self.offline_authority_generation = self
                .store
                .as_ref()
                .and_then(|store| store.active_generation().ok())
                .map(|generation| generation.generation_id);
            return;
        }
        self.offline_authority_generation = None;
        self.reader.set_status(
            if self
                .store
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
            if self.store.as_ref().is_some_and(|store| {
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
        self.script_reader.set_status(
            if self.store.as_ref().is_some_and(|store| {
                store
                    .active_generation()
                    .is_ok_and(|generation| !generation.script.is_empty())
            }) {
                ScriptIndexStatus::NotCurrent {
                    reason: ScriptIndexStaleReason::BridgeDisconnected,
                }
            } else {
                ScriptIndexStatus::NotReady
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

fn is_catalog_building(error: &BridgeError, expected_code: &str) -> bool {
    matches!(
        error,
        BridgeError::Rpc {
            code,
            retryable: true,
            ..
        } if code == expected_code
    )
}

async fn next_script_delta_when_ready(
    client: &mut BridgeClient,
    after_script_graph_revision: u64,
) -> Result<ScriptDeltaPoll, CoordinatorError> {
    loop {
        match client
            .get_next_script_delta(after_script_graph_revision)
            .await
        {
            Ok(delta) => return Ok(delta),
            Err(error) if is_catalog_building(&error, "script_catalog_building") => {
                tokio::time::sleep(CATALOG_RETRY_INTERVAL).await;
            }
            Err(error) => return Err(error.into()),
        }
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
    {
        return Err(IndexerError::Scene("scene_snapshot_editor_session_changed").into());
    }
    if snapshot.accepted.resource_revision != snapshot.end.resource_revision
        || snapshot.accepted.scene_graph_revision != snapshot.end.scene_graph_revision
    {
        return Err(IndexerError::Scene("scene_snapshot_revision_changed").into());
    }
    if snapshot.end.resource_revision != active.checkpoint.resource_revision {
        return Err(IndexerError::Scene("scene_snapshot_resource_checkpoint").into());
    }
    Ok(())
}

fn validate_script_snapshot_binding(
    client: &BridgeClient,
    store: &SegmentStore,
    snapshot: &ScriptSnapshot,
) -> Result<(), CoordinatorError> {
    let active = store.active_generation()?;
    if snapshot.accepted.revisions.editor_session_id != client.editor_session_id()
        || snapshot.end.revisions.editor_session_id != client.editor_session_id()
        || snapshot.accepted.resource_revision != snapshot.end.resource_revision
        || snapshot.accepted.scene_graph_revision != snapshot.end.scene_graph_revision
        || snapshot.accepted.script_graph_revision != snapshot.end.script_graph_revision
        || snapshot.end.resource_revision != active.checkpoint.resource_revision
        || (client.negotiated_profile().scene_graph_available
            && !active.scene.is_empty()
            && snapshot.end.scene_graph_revision > active.scene.scene_graph_revision)
    {
        return Err(IndexerError::Script("script_snapshot_session_changed").into());
    }
    Ok(())
}

fn script_payload_from_graph(graph: &NormalizedScriptGraph) -> ScriptSnapshotPayload {
    ScriptSnapshotPayload {
        documents: graph.documents.clone(),
        symbols: graph.symbols.clone(),
        relations: graph.relations.clone(),
        diagnostics: graph.diagnostics.clone(),
        adapter_statuses: graph.adapter_statuses.clone(),
    }
}

fn apply_script_operations(
    payload: &mut ScriptSnapshotPayload,
    batch: &ScriptDeltaBatch,
) -> Result<(), CoordinatorError> {
    let mut document_operations = BTreeMap::new();
    let mut adapter_operations = BTreeMap::new();
    for operation in &batch.operations {
        match operation {
            ScriptDeltaOperation::UpsertDocument { value } => {
                let key = script_ref_key(&value.document.script_ref);
                if document_operations.insert(key.clone(), ()).is_some() {
                    return Err(IndexerError::Script("script_delta_duplicate_document").into());
                }
                remove_script_document(payload, &value.document.script_ref, false)?;
                payload.documents.push(value.document.clone());
                payload.symbols.extend(value.symbols.iter().cloned());
                payload.relations.extend(value.relations.iter().cloned());
                payload
                    .diagnostics
                    .extend(value.diagnostics.iter().cloned());
            }
            ScriptDeltaOperation::RemoveDocument { script_ref, path } => {
                let key = script_ref_key(script_ref);
                if document_operations.insert(key, ()).is_some() {
                    return Err(IndexerError::Script("script_delta_duplicate_document").into());
                }
                if !path_matches_ref(path, script_ref)
                    || !remove_script_document(payload, script_ref, true)?
                {
                    return Err(IndexerError::Script("script_delta_remove_missing").into());
                }
            }
            ScriptDeltaOperation::AdapterStatus { value } => {
                if adapter_operations.insert(value.language, ()).is_some() {
                    return Err(IndexerError::Script("script_delta_duplicate_adapter").into());
                }
                payload
                    .adapter_statuses
                    .retain(|status| status.language != value.language);
                payload.adapter_statuses.push(value.clone());
            }
        }
    }
    for document in &mut payload.documents {
        document.resource_revision = batch.resource_revision;
        document.script_graph_revision = batch.script_graph_revision;
    }
    for symbol in &mut payload.symbols {
        symbol.script_graph_revision = batch.script_graph_revision;
    }
    for relation in &mut payload.relations {
        relation.script_graph_revision = batch.script_graph_revision;
    }
    for diagnostic in &mut payload.diagnostics {
        diagnostic.script_graph_revision = batch.script_graph_revision;
    }
    Ok(())
}

fn remove_script_document(
    payload: &mut ScriptSnapshotPayload,
    reference: &ResourceRef,
    require_existing: bool,
) -> Result<bool, CoordinatorError> {
    let documents: Vec<_> = payload
        .documents
        .iter()
        .filter(|document| same_script_ref(&document.script_ref, reference))
        .cloned()
        .collect();
    if documents.len() > 1 {
        return Err(IndexerError::Script("script_delta_catalog_duplicate").into());
    }
    let Some(document) = documents.first() else {
        if require_existing {
            return Ok(false);
        }
        return Ok(false);
    };
    let removed_symbols: BTreeMap<_, _> = payload
        .symbols
        .iter()
        .filter(|symbol| same_script_ref(&symbol.script_ref, reference))
        .map(|symbol| (symbol.symbol_id.clone(), ()))
        .collect();
    payload
        .documents
        .retain(|value| !same_script_ref(&value.script_ref, reference));
    payload
        .symbols
        .retain(|value| !same_script_ref(&value.script_ref, reference));
    payload
        .diagnostics
        .retain(|value| !same_script_ref(&value.script_ref, reference));
    payload.relations.retain(|relation| {
        !relation_belongs_to_document(relation, reference, document, &removed_symbols)
    });
    Ok(true)
}

fn relation_belongs_to_document(
    relation: &godot_codex_bridge_client::ScriptRelation,
    reference: &ResourceRef,
    document: &godot_codex_bridge_client::ScriptDocument,
    symbols: &BTreeMap<String, ()>,
) -> bool {
    matches!(
        &relation.source,
        godot_codex_bridge_client::ScriptRelationEndpoint::Symbol { symbol_id }
            if symbols.contains_key(symbol_id)
    ) || matches!(
        &relation.source,
        godot_codex_bridge_client::ScriptRelationEndpoint::Resource { resource_ref }
            if same_script_ref(resource_ref, reference)
    ) || relation.evidence_range.as_ref().is_some_and(|range| {
        range.path == document.path && range.content_sha256 == document.content_sha256
    })
}

fn same_script_ref(left: &ResourceRef, right: &ResourceRef) -> bool {
    match (left, right) {
        (ResourceRef::Uid(left), ResourceRef::Uid(right)) => left.uid == right.uid,
        (ResourceRef::Path(left), ResourceRef::Path(right)) => left.path == right.path,
        (ResourceRef::Uid(_), ResourceRef::Path(_))
        | (ResourceRef::Path(_), ResourceRef::Uid(_)) => false,
    }
}

fn script_ref_key(reference: &ResourceRef) -> String {
    match reference {
        ResourceRef::Uid(reference) => format!("uid:{}", reference.uid),
        ResourceRef::Path(reference) => format!("path:{}", reference.path),
    }
}

fn path_matches_ref(path: &str, reference: &ResourceRef) -> bool {
    match reference {
        ResourceRef::Uid(_) => path.starts_with("res://"),
        ResourceRef::Path(reference) => reference.path == path,
    }
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

fn resource_batch_affects_script(
    base: &godot_codex_index_store::IndexGeneration,
    batch: &ResourceDeltaBatch,
) -> bool {
    let mut referenced = BTreeMap::new();
    for document in &base.script.documents {
        referenced.insert(document.script_resource_id.as_str(), ());
    }
    for relation in &base.script.relations {
        for endpoint in std::iter::once(&relation.source).chain(relation.target.as_ref()) {
            if let godot_codex_index_store::ScriptEndpoint::Resource { resource_entity_id } =
                endpoint
            {
                referenced.insert(resource_entity_id.as_str(), ());
            }
        }
    }
    let by_uid: BTreeMap<_, _> = base
        .resources
        .iter()
        .filter_map(|resource| resource.uid.as_deref().map(|uid| (uid, resource)))
        .collect();
    let by_path: BTreeMap<_, _> = base
        .resources
        .iter()
        .map(|resource| (resource.comparison_path.as_str(), resource))
        .collect();
    let path_referenced = |path: &str| {
        is_script_path(path)
            || normalize_resource_path(path).ok().is_some_and(|path| {
                by_path
                    .get(path.comparison.as_str())
                    .is_some_and(|resource| referenced.contains_key(resource.entity_id.as_str()))
            })
    };
    let reference_referenced = |reference: &ResourceRef| match reference {
        ResourceRef::Uid(reference) => by_uid
            .get(reference.uid.as_str())
            .is_some_and(|resource| referenced.contains_key(resource.entity_id.as_str())),
        ResourceRef::Path(reference) => path_referenced(&reference.path),
    };

    batch.operations.iter().any(|operation| match operation {
        ResourceDeltaOperation::Upsert { value } | ResourceDeltaOperation::Reimport { value } => {
            is_script_path(&value.resource.path)
                || matches!(value.resource.resource_ref, ResourceRef::Path(_))
                    && reference_referenced(&value.resource.resource_ref)
        }
        ResourceDeltaOperation::Move {
            from_path,
            to_path,
            value,
            ..
        } => {
            path_referenced(from_path)
                || path_referenced(to_path)
                || reference_referenced(&value.resource.resource_ref)
        }
        ResourceDeltaOperation::Remove { resource_ref, path } => {
            path_referenced(path) || reference_referenced(resource_ref)
        }
    })
}

fn is_scene_path(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| matches!(extension, "tscn" | "scn"))
}

fn is_script_path(path: &str) -> bool {
    path.rsplit_once('.')
        .is_some_and(|(_, extension)| matches!(extension, "gd" | "cs"))
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

fn script_generation_id(
    project_id: &str,
    editor_session_id: &str,
    script_graph_revision: u64,
    index_revision: u64,
    validation_digest: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"godot-codex/script-generation-id/v1\0");
    for value in [
        project_id,
        editor_session_id,
        &script_graph_revision.to_string(),
        &index_revision.to_string(),
        validation_digest,
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("generation:script:sha256:{:x}", hasher.finalize())
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
    use godot_codex_index_store::{
        GenerationState, IdentityStrength, IndexGeneration, IngestionCheckpoint, LOGICAL_SCHEMA_V1,
        RecordValidity, ResourceEntity, SceneDomainGeneration, ScriptAdapterAvailability,
        ScriptAdapterProfile, ScriptAdapterStatus, ScriptDomainGeneration, ScriptLanguage,
        SourceDocument,
    };
    use tempfile::TempDir;

    use super::*;

    fn offline_project() -> TempDir {
        let project = tempfile::tempdir().unwrap();
        fs::write(project.path().join("project.godot"), b"[application]\n").unwrap();
        fs::write(project.path().join("main.gd"), b"extends Node\n").unwrap();
        project
    }

    #[test]
    fn coordinator_debug_code_never_exposes_bridge_details() {
        let errors = [
            CoordinatorError::Bridge(BridgeError::Io(std::io::Error::other(
                "/Users/alice/private/.godot/codex/bridge.sock?token=top-secret",
            ))),
            CoordinatorError::Bridge(BridgeError::Invalid(
                "server path /Users/alice/private token=top-secret".to_owned(),
            )),
            CoordinatorError::Bridge(BridgeError::Rpc {
                code: "unknown_private_failure".to_owned(),
                message: "server path /Users/alice/private token=top-secret".to_owned(),
                retryable: false,
                data: serde_json::json!({"native_handle": 42}),
            }),
        ];
        for error in errors {
            assert_eq!(error.safe_code(), "transport_unavailable");
            let line = coordinator_sync_failure_line(&error);
            assert_eq!(
                line,
                "[godot-codex-index] sync failed: transport_unavailable"
            );
            assert!(!line.contains("/Users/"));
            assert!(!line.contains("top-secret"));
            assert!(!line.contains("unknown_private_failure"));
            assert!(!line.contains("native_handle"));
        }
    }

    fn complete_generation(project_id: &str) -> IndexGeneration {
        let source = b"extends Node\n";
        let content_generation = format!("sha256:{:x}", Sha256::digest(source));
        let resource = ResourceEntity {
            entity_id: "godot:resource:v1:main-script".to_owned(),
            identity_input: "uid://offline-main".to_owned(),
            uid: Some("uid://offline-main".to_owned()),
            display_path: "res://main.gd".to_owned(),
            comparison_path: "res://main.gd".to_owned(),
            identity_strength: IdentityStrength::ResourceUid,
            resource_type: "GDScript".to_owned(),
            source_kind: "source".to_owned(),
            import_state: "not_imported".to_owned(),
            authority: "editor_file_system".to_owned(),
            content_generation: Some(content_generation.clone()),
            mtime_ns: 1,
            byte_size: source.len() as u64,
            validity: RecordValidity::Valid,
            resource_revision: 2,
        };
        let mut generation = IndexGeneration {
            generation_id: format!("generation:sha256:{}", "1".repeat(64)),
            parent_generation_id: None,
            schema_version: LOGICAL_SCHEMA_V1,
            project_id: project_id.to_owned(),
            index_revision: 3,
            state: GenerationState::Active,
            creation_reason: "full_snapshot".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: "historical-editor-session".to_owned(),
                resource_revision: 2,
                project_revision: 2,
                index_revision: 3,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "2".repeat(64)),
                last_batch_id: None,
                last_batch_checksum: None,
            },
            resources: vec![resource.clone()],
            source_documents: vec![SourceDocument {
                entity_id: resource.entity_id,
                comparison_path: resource.comparison_path,
                size_before: source.len() as u64,
                size_after: source.len() as u64,
                mtime_before_ns: 1,
                mtime_after_ns: 1,
                content_generation: Some(content_generation),
                ingest_state: "ready".to_owned(),
            }],
            dependencies: Vec::new(),
            diagnostics: Vec::new(),
            tombstones: Vec::new(),
            scene: SceneDomainGeneration {
                editor_session_id: "historical-editor-session".to_owned(),
                resource_revision: 2,
                scene_graph_revision: 2,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "3".repeat(64)),
                ..SceneDomainGeneration::default()
            },
            script: ScriptDomainGeneration {
                editor_session_id: "historical-editor-session".to_owned(),
                resource_revision: 2,
                scene_graph_revision: 2,
                script_graph_revision: 2,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "4".repeat(64)),
                semantic_digest: format!("sha256:{}", "5".repeat(64)),
                adapter_statuses: vec![
                    ScriptAdapterStatus {
                        language: ScriptLanguage::Gdscript,
                        availability: ScriptAdapterAvailability::Available,
                        profile: Some(ScriptAdapterProfile::GdscriptParserAnalyzerV1),
                        version: Some("1".to_owned()),
                        diagnostic: None,
                    },
                    ScriptAdapterStatus {
                        language: ScriptLanguage::Csharp,
                        availability: ScriptAdapterAvailability::DiscoveryOnly,
                        profile: Some(ScriptAdapterProfile::CsharpDiscoveryOnlyV1),
                        version: Some("1".to_owned()),
                        diagnostic: None,
                    },
                ],
                ..ScriptDomainGeneration::default()
            },
            validation_digest: String::new(),
        };
        generation.scene.validation_digest = generation.scene.compute_validation_digest();
        generation.script.validation_digest = generation.script.compute_validation_digest();
        generation.validation_digest = generation.compute_validation_digest();
        generation.validate().unwrap();
        generation
    }

    fn install_complete_store(project: &TempDir) -> (String, IndexGeneration) {
        let project_id = godot_codex_bridge_client::project_id_for_path(project.path()).unwrap();
        let generation = complete_generation(&project_id);
        {
            let mut store = SegmentStore::open(project.path(), &project_id).unwrap();
            store.activate(&generation, None).unwrap();
        }
        (project_id, generation)
    }

    #[test]
    fn clean_offline_start_pins_exact_generation_without_editor_identity() {
        let project = offline_project();
        let (project_id, generation) = install_complete_store(&project);
        persist_offline_authority(project.path(), &generation).unwrap();

        let (_coordinator, resource, scene, script) =
            ResourceIndexCoordinator::new_semantic(project.path()).unwrap();
        assert!(matches!(
            resource.status(),
            ResourceIndexStatus::OfflineCurrent {
                project_id: current_project,
                generation_id: current_generation,
                ..
            } if current_project == project_id && current_generation == generation.generation_id
        ));
        assert!(matches!(
            scene.status(),
            SceneIndexStatus::OfflineCurrent { .. }
        ));
        assert!(matches!(
            script.status(),
            ScriptIndexStatus::OfflineCurrent { .. }
        ));
        let semantic = SemanticIndexReader::new(resource, scene, script)
            .pin_current()
            .unwrap();
        assert_eq!(semantic.freshness(), SemanticIndexFreshness::OfflineCached);
        assert_eq!(
            semantic.generation().generation_id,
            generation.generation_id
        );
    }

    #[test]
    fn same_project_contention_is_explicit_and_lock_release_allows_takeover() {
        let project = offline_project();
        let (project_id, _) = install_complete_store(&project);
        let (owner, _, _, _) = ResourceIndexCoordinator::new_semantic(project.path()).unwrap();
        assert_eq!(
            *owner.subscribe_project_session().borrow(),
            ProjectSessionState::Owner
        );

        let (standby, resource, scene, script) =
            ResourceIndexCoordinator::new_semantic(project.path()).unwrap();
        assert_eq!(
            *standby.subscribe_project_session().borrow(),
            ProjectSessionState::Busy
        );
        assert_eq!(
            resource.pin_current().err(),
            Some(ResourceIndexReadError::ProjectSessionBusy)
        );
        assert_eq!(
            scene.pin_current().err(),
            Some(SceneIndexReadError::ProjectSessionBusy)
        );
        assert_eq!(
            script.pin_current().err(),
            Some(ScriptIndexReadError::ProjectSessionBusy)
        );

        drop(owner);
        let takeover = standby.open_store(&project_id).unwrap();
        assert_eq!(takeover.active_generation().unwrap().project_id, project_id);
    }

    #[test]
    fn missing_or_corrupt_authority_and_store_never_become_current() {
        let missing = offline_project();
        let (_, _) = install_complete_store(&missing);
        let (_coordinator, resource, scene, script) =
            ResourceIndexCoordinator::new_semantic(missing.path()).unwrap();
        assert!(!matches!(
            resource.status(),
            ResourceIndexStatus::Current { .. } | ResourceIndexStatus::OfflineCurrent { .. }
        ));
        assert!(!matches!(
            scene.status(),
            SceneIndexStatus::Current { .. } | SceneIndexStatus::OfflineCurrent { .. }
        ));
        assert!(!matches!(
            script.status(),
            ScriptIndexStatus::Current { .. } | ScriptIndexStatus::OfflineCurrent { .. }
        ));

        let corrupt_authority = offline_project();
        let (_, generation) = install_complete_store(&corrupt_authority);
        persist_offline_authority(corrupt_authority.path(), &generation).unwrap();
        fs::write(
            corrupt_authority
                .path()
                .join(".godot/codex/offline-authority-v1.json"),
            b"{",
        )
        .unwrap();
        let (_coordinator, resource, _, _) =
            ResourceIndexCoordinator::new_semantic(corrupt_authority.path()).unwrap();
        assert!(!matches!(
            resource.status(),
            ResourceIndexStatus::Current { .. } | ResourceIndexStatus::OfflineCurrent { .. }
        ));

        let corrupt_store = offline_project();
        let (_, generation) = install_complete_store(&corrupt_store);
        persist_offline_authority(corrupt_store.path(), &generation).unwrap();
        let commit = fs::read_dir(corrupt_store.path().join(".godot/codex/index/commits"))
            .unwrap()
            .filter_map(Result::ok)
            .max_by_key(fs::DirEntry::file_name)
            .unwrap()
            .path();
        fs::write(commit, b"{").unwrap();
        let (_coordinator, resource, _, _) =
            ResourceIndexCoordinator::new_semantic(corrupt_store.path()).unwrap();
        assert!(matches!(
            resource.status(),
            ResourceIndexStatus::NotCurrent {
                reason: ResourceIndexStaleReason::StoreUnavailable
            }
        ));
    }

    #[test]
    fn disconnect_rechecks_disk_and_simulated_reconnect_publishes_new_authority() {
        let project = offline_project();
        let (_, generation) = install_complete_store(&project);
        persist_offline_authority(project.path(), &generation).unwrap();
        let (mut coordinator, resource, scene, script) =
            ResourceIndexCoordinator::new_semantic(project.path()).unwrap();

        fs::write(project.path().join("main.gd"), b"extends Node2D\n").unwrap();
        coordinator.mark_disconnected();
        assert!(matches!(
            resource.status(),
            ResourceIndexStatus::NotCurrent {
                reason: ResourceIndexStaleReason::BridgeDisconnected
            }
        ));

        let mut store = coordinator.store.take().unwrap();
        let mut next = store.active_generation().unwrap();
        next.parent_generation_id = Some(next.generation_id.clone());
        next.generation_id = format!("generation:sha256:{}", "6".repeat(64));
        next.index_revision += 1;
        next.checkpoint.index_revision = next.index_revision;
        let changed_source = b"extends Node2D\n";
        let changed_hash = format!("sha256:{:x}", Sha256::digest(changed_source));
        next.resources[0].content_generation = Some(changed_hash.clone());
        next.resources[0].byte_size = changed_source.len() as u64;
        next.source_documents[0].content_generation = Some(changed_hash);
        next.source_documents[0].size_before = changed_source.len() as u64;
        next.source_documents[0].size_after = changed_source.len() as u64;
        next.canonicalize();
        next.validation_digest.clear();
        next.validation_digest = next.compute_validation_digest();
        next.validate().unwrap();
        store.activate(&next, None).unwrap();
        resource.publish_current(&store).unwrap();
        scene.publish_current(&store).unwrap();
        script.publish_current(&store).unwrap();
        coordinator.refresh_offline_authority(&store);
        coordinator.store = Some(store);
        coordinator.mark_disconnected();
        assert!(matches!(
            resource.status(),
            ResourceIndexStatus::OfflineCurrent { .. }
        ));
        assert!(matches!(
            scene.status(),
            SceneIndexStatus::OfflineCurrent { .. }
        ));
        assert!(matches!(
            script.status(),
            ScriptIndexStatus::OfflineCurrent { .. }
        ));
    }

    #[test]
    fn canonical_roots_are_exactly_isolated() {
        let first = offline_project();
        let second = offline_project();
        let first_id = godot_codex_bridge_client::project_id_for_path(first.path()).unwrap();
        let second_id = godot_codex_bridge_client::project_id_for_path(second.path()).unwrap();
        assert_ne!(first_id, second_id);
        let first_generation = complete_generation(&first_id);
        {
            let mut store = SegmentStore::open(first.path(), &first_id).unwrap();
            store.activate(&first_generation, None).unwrap();
        }
        persist_offline_authority(first.path(), &first_generation).unwrap();
        let (_coordinator, first_reader, _, _) =
            ResourceIndexCoordinator::new_semantic(first.path()).unwrap();
        let (_coordinator, second_reader, _, _) =
            ResourceIndexCoordinator::new_semantic(second.path()).unwrap();
        assert!(matches!(
            first_reader.status(),
            ResourceIndexStatus::OfflineCurrent { .. }
        ));
        assert!(!matches!(
            second_reader.status(),
            ResourceIndexStatus::OfflineCurrent { .. }
        ));
    }

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
        reader.set_status(ResourceIndexStatus::ProjectSessionBusy);
        assert_eq!(
            reader.pin_current().err(),
            Some(ResourceIndexReadError::ProjectSessionBusy)
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
        scene_reader.set_status(SceneIndexStatus::ProjectSessionBusy);
        assert_eq!(
            scene_reader.pin_current().err(),
            Some(SceneIndexReadError::ProjectSessionBusy)
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

        let script_reader = ScriptIndexReader::new();
        assert_eq!(
            script_reader.pin_current().err(),
            Some(ScriptIndexReadError::ProjectNotBound)
        );
        script_reader.set_status(ScriptIndexStatus::NotReady);
        assert_eq!(
            script_reader.pin_current().err(),
            Some(ScriptIndexReadError::NotReady)
        );
        script_reader.set_status(ScriptIndexStatus::ProjectSessionBusy);
        assert_eq!(
            script_reader.pin_current().err(),
            Some(ScriptIndexReadError::ProjectSessionBusy)
        );
        script_reader.set_status(ScriptIndexStatus::NotCurrent {
            reason: ScriptIndexStaleReason::JournalGap,
        });
        assert_eq!(
            script_reader.pin_current().err(),
            Some(ScriptIndexReadError::NotCurrent)
        );
        script_reader.set_status(ScriptIndexStatus::CapabilityUnavailable);
        assert_eq!(
            script_reader.pin_current().err(),
            Some(ScriptIndexReadError::CapabilityUnavailable)
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

    #[test]
    fn only_script_documents_and_joined_targets_invalidate_script_freshness() {
        use godot_codex_index_store::{
            GenerationState, IdentityStrength, IndexGeneration, IngestionCheckpoint,
            RecordValidity, ResourceEntity, SceneDomainGeneration, SchemaVersion,
            ScriptAdapterAvailability, ScriptAdapterProfile, ScriptAdapterStatus,
            ScriptCompleteness, ScriptConfidence, ScriptDocument, ScriptDomainGeneration,
            ScriptEndpoint, ScriptLanguage, ScriptPredicate, ScriptReference, ScriptRelation,
            ScriptRelationAuthority,
        };

        let resource = |id: &str, uid: &str, path: &str| ResourceEntity {
            entity_id: id.to_owned(),
            identity_input: uid.to_owned(),
            uid: Some(uid.to_owned()),
            display_path: path.to_owned(),
            comparison_path: path.to_owned(),
            identity_strength: IdentityStrength::ResourceUid,
            resource_type: "Resource".to_owned(),
            source_kind: "source".to_owned(),
            import_state: "valid".to_owned(),
            authority: "editor_file_system".to_owned(),
            content_generation: Some(format!("sha256:{}", "a".repeat(64))),
            mtime_ns: 1,
            byte_size: 1,
            validity: RecordValidity::Valid,
            resource_revision: 1,
        };
        let source = ScriptEndpoint::Resource {
            resource_entity_id: "script-id".to_owned(),
        };
        let target = ScriptEndpoint::Resource {
            resource_entity_id: "target-id".to_owned(),
        };
        let relation = ScriptRelation {
            relation_id: "relation-load".to_owned(),
            script_resource_id: "script-id".to_owned(),
            source: source.clone(),
            predicate: ScriptPredicate::Loads,
            target: Some(target.clone()),
            confidence: ScriptConfidence::Exact,
            evidence_range: None,
            detail: Some("literal_uid".to_owned()),
            authority: ScriptRelationAuthority::ResourceGraph,
            script_graph_revision: 1,
        };
        let generation = IndexGeneration {
            generation_id: "generation-impact".to_owned(),
            parent_generation_id: None,
            schema_version: SchemaVersion { major: 1, minor: 3 },
            project_id: "project".to_owned(),
            index_revision: 1,
            state: GenerationState::Active,
            creation_reason: "fixture".to_owned(),
            checkpoint: IngestionCheckpoint {
                editor_session_id: "session".to_owned(),
                resource_revision: 1,
                project_revision: 1,
                index_revision: 1,
                source_complete: true,
                snapshot_checksum: "checksum".to_owned(),
                last_batch_id: None,
                last_batch_checksum: None,
            },
            resources: vec![
                resource("script-id", "uid://script", "res://player.gd"),
                resource("target-id", "uid://target", "res://target.tres"),
                resource("other-id", "uid://other", "res://other.tres"),
            ],
            source_documents: Vec::new(),
            dependencies: Vec::new(),
            diagnostics: Vec::new(),
            tombstones: Vec::new(),
            scene: SceneDomainGeneration::default(),
            script: ScriptDomainGeneration {
                editor_session_id: "session".to_owned(),
                resource_revision: 1,
                scene_graph_revision: 1,
                script_graph_revision: 1,
                source_complete: true,
                snapshot_checksum: format!("sha256:{}", "b".repeat(64)),
                semantic_digest: format!("sha256:{}", "c".repeat(64)),
                documents: vec![ScriptDocument {
                    script_resource_id: "script-id".to_owned(),
                    path: "res://player.gd".to_owned(),
                    language: ScriptLanguage::Gdscript,
                    content_sha256: format!("sha256:{}", "a".repeat(64)),
                    adapter_profile: ScriptAdapterProfile::GdscriptParserAnalyzerV1,
                    completeness: ScriptCompleteness::Complete,
                    resource_revision: 1,
                    script_graph_revision: 1,
                }],
                symbols: Vec::new(),
                relations: vec![relation],
                references: vec![ScriptReference {
                    reference_id: "reference-load".to_owned(),
                    relation_id: "relation-load".to_owned(),
                    script_resource_id: "script-id".to_owned(),
                    source,
                    predicate: ScriptPredicate::Loads,
                    target,
                    authority: ScriptRelationAuthority::ResourceGraph,
                    script_graph_revision: 1,
                }],
                diagnostics: Vec::new(),
                adapter_statuses: vec![
                    ScriptAdapterStatus {
                        language: ScriptLanguage::Gdscript,
                        availability: ScriptAdapterAvailability::Available,
                        profile: Some(ScriptAdapterProfile::GdscriptParserAnalyzerV1),
                        version: Some("1".to_owned()),
                        diagnostic: None,
                    },
                    ScriptAdapterStatus {
                        language: ScriptLanguage::Csharp,
                        availability: ScriptAdapterAvailability::DiscoveryOnly,
                        profile: Some(ScriptAdapterProfile::CsharpDiscoveryOnlyV1),
                        version: Some("1".to_owned()),
                        diagnostic: None,
                    },
                ],
                validation_digest: String::new(),
            },
            validation_digest: String::new(),
        };
        let upsert = |uid: &str, path: &str| -> ResourceDeltaBatch {
            serde_json::from_value(serde_json::json!({
                "batch_id": "resource-batch:2",
                "previous_resource_revision": 1,
                "resource_revision": 2,
                "project_revision": 2,
                "operations": [{
                    "kind": "upsert",
                    "value": {
                        "resource": {
                            "resource_ref": {"uid": uid},
                            "path": path,
                            "godot_type": "Resource",
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
        let remove = |uid: &str, path: &str| -> ResourceDeltaBatch {
            serde_json::from_value(serde_json::json!({
                "batch_id": "resource-batch:2",
                "previous_resource_revision": 1,
                "resource_revision": 2,
                "project_revision": 2,
                "operations": [{
                    "kind": "remove",
                    "resource_ref": {"uid": uid},
                    "path": path
                }],
                "source_complete": true,
                "checksum": "checksum"
            }))
            .expect("resource delta")
        };

        assert!(!resource_batch_affects_script(
            &generation,
            &upsert("uid://other", "res://other.tres")
        ));
        assert!(resource_batch_affects_script(
            &generation,
            &upsert("uid://script", "res://player.gd")
        ));
        assert!(resource_batch_affects_script(
            &generation,
            &remove("uid://target", "res://target.tres")
        ));
    }

    #[test]
    fn script_delta_catalog_removal_is_exact_and_retry_fails_closed() {
        let mut payload: ScriptSnapshotPayload = serde_json::from_value(serde_json::json!({
            "documents": [{
                "script_ref": {"uid": "uid://script"},
                "path": "res://player.gd",
                "language": "gdscript",
                "content_sha256": format!("sha256:{}", "a".repeat(64)),
                "adapter_profile": "gdscript_parser_analyzer_v1",
                "completeness": "complete",
                "resource_revision": 1,
                "script_graph_revision": 1
            }],
            "symbols": [],
            "relations": [],
            "diagnostics": [],
            "adapter_statuses": [{
                "language": "gdscript",
                "availability": "available",
                "profile": "gdscript_parser_analyzer_v1",
                "version": "1",
                "diagnostic": null
            }, {
                "language": "csharp",
                "availability": "discovery_only",
                "profile": "csharp_discovery_only_v1",
                "version": "1",
                "diagnostic": null
            }]
        }))
        .expect("script payload");
        let batch: ScriptDeltaBatch = serde_json::from_value(serde_json::json!({
            "batch_id": "script-batch:00000000000000000000000000000001",
            "previous_script_graph_revision": 1,
            "script_graph_revision": 2,
            "resource_revision": 1,
            "scene_graph_revision": 1,
            "project_revision": 1,
            "operations": [{
                "kind": "remove_document",
                "script_ref": {"uid": "uid://script"},
                "path": "res://player.gd"
            }],
            "source_complete": true,
            "checksum": "checksum"
        }))
        .expect("script batch");

        apply_script_operations(&mut payload, &batch).expect("first removal");
        assert!(payload.documents.is_empty());
        assert!(matches!(
            apply_script_operations(&mut payload, &batch),
            Err(CoordinatorError::Indexer(IndexerError::Script(
                "script_delta_remove_missing"
            )))
        ));
    }

    #[test]
    fn script_delta_rebinds_unchanged_records_to_the_atomic_batch_revision() {
        let mut payload: ScriptSnapshotPayload = serde_json::from_value(serde_json::json!({
            "documents": [{
                "script_ref": {"uid": "uid://changed"},
                "path": "res://changed.gd",
                "language": "gdscript",
                "content_sha256": format!("sha256:{}", "a".repeat(64)),
                "adapter_profile": "gdscript_parser_analyzer_v1",
                "completeness": "complete",
                "resource_revision": 1,
                "script_graph_revision": 1
            }, {
                "script_ref": {"uid": "uid://stable"},
                "path": "res://stable.gd",
                "language": "gdscript",
                "content_sha256": format!("sha256:{}", "b".repeat(64)),
                "adapter_profile": "gdscript_parser_analyzer_v1",
                "completeness": "complete",
                "resource_revision": 1,
                "script_graph_revision": 1
            }],
            "symbols": [],
            "relations": [],
            "diagnostics": [],
            "adapter_statuses": []
        }))
        .expect("script payload");
        let batch: ScriptDeltaBatch = serde_json::from_value(serde_json::json!({
            "batch_id": "script-batch:00000000000000000000000000000002",
            "previous_script_graph_revision": 1,
            "script_graph_revision": 2,
            "resource_revision": 2,
            "scene_graph_revision": 1,
            "project_revision": 2,
            "operations": [{
                "kind": "upsert_document",
                "value": {
                    "document": {
                        "script_ref": {"uid": "uid://changed"},
                        "path": "res://changed.gd",
                        "language": "gdscript",
                        "content_sha256": format!("sha256:{}", "c".repeat(64)),
                        "adapter_profile": "gdscript_parser_analyzer_v1",
                        "completeness": "complete",
                        "resource_revision": 2,
                        "script_graph_revision": 2
                    },
                    "symbols": [],
                    "relations": [],
                    "diagnostics": []
                }
            }],
            "source_complete": true,
            "checksum": "checksum"
        }))
        .expect("script batch");

        apply_script_operations(&mut payload, &batch).expect("script upsert");
        assert_eq!(payload.documents.len(), 2);
        assert!(
            payload
                .documents
                .iter()
                .all(|document| document.resource_revision == 2
                    && document.script_graph_revision == 2)
        );
    }
}
