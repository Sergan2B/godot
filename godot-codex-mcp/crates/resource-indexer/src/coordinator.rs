use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use godot_codex_bridge_client::{
    BridgeClient, BridgeError, ResourceDeltaPoll, ResourceSnapshotTransfer,
};
use godot_codex_index_store::{IndexReadSnapshot, SegmentIndexReader, SegmentStore, StoreError};
use thiserror::Error;
use tokio::sync::watch;

use crate::{IndexerError, ResourceNormalizer, ResourceSnapshotSpool};

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

struct ResourceIndexState {
    status: ResourceIndexStatus,
    reader: Option<SegmentIndexReader>,
}

/// Cloneable freshness gate over immutable segment snapshots.
#[derive(Clone)]
pub struct ResourceIndexReader {
    state: Arc<RwLock<ResourceIndexState>>,
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
            | Self::Indexer(IndexerError::Spool(code)) => code,
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
    reader: ResourceIndexReader,
}

impl ResourceIndexCoordinator {
    /// Creates the coordinator and its independently cloneable MCP reader.
    pub fn new(
        project_root: impl AsRef<Path>,
    ) -> Result<(Self, ResourceIndexReader), IndexerError> {
        let normalizer = ResourceNormalizer::new(project_root.as_ref())?;
        let project_root = fs::canonicalize(project_root.as_ref())
            .map_err(|_| IndexerError::UnsafeResourcePath)?;
        let reader = ResourceIndexReader::new();
        Ok((
            Self {
                project_root,
                normalizer,
                reader: reader.clone(),
            },
            reader,
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
                if wait_or_shutdown(RETRY_MAX, &mut shutdown).await {
                    break;
                }
                continue;
            }
            if project_id
                .as_deref()
                .is_some_and(|known| known != client.project_id())
            {
                self.reader.set_status(ResourceIndexStatus::ProjectNotBound);
                break;
            }
            project_id.get_or_insert_with(|| client.project_id().to_owned());
            if store.is_none() {
                match self.open_store(client.project_id()) {
                    Ok(opened) => store = Some(opened),
                    Err(error) => {
                        eprintln!("[godot-codex-index] open failed: {}", error.safe_code());
                        self.reader.set_status(ResourceIndexStatus::NotReady);
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
        let store = match SegmentStore::open(&self.project_root, project_id) {
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
        let has_generation = store.active_generation().is_ok();
        self.reader.install_reader(store.reader(), has_generation);
        Ok(store)
    }

    async fn sync_connected(
        &mut self,
        client: &mut BridgeClient,
        store: &mut SegmentStore,
    ) -> Result<(), CoordinatorError> {
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

        loop {
            let base = store.active_generation()?;
            match client
                .get_next_resource_delta(base.checkpoint.resource_revision)
                .await?
            {
                ResourceDeltaPoll::Current { .. } => {
                    self.reader.publish_current(store)?;
                    tokio::time::sleep(DELTA_POLL_INTERVAL).await;
                }
                ResourceDeltaPoll::Batch { batch, .. } => {
                    self.reader.set_status(ResourceIndexStatus::NotCurrent {
                        reason: ResourceIndexStaleReason::Rebuilding,
                    });
                    match self.normalizer.normalize_incremental_batch(&base, &batch) {
                        Ok(Some(batch)) => {
                            let next = base.apply_incremental_batch(&batch)?;
                            store.activate(&next, None)?;
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
                    self.reader.set_status(ResourceIndexStatus::NotCurrent {
                        reason: ResourceIndexStaleReason::JournalGap,
                    });
                    self.full_snapshot(client, store).await?;
                }
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
    }
}
