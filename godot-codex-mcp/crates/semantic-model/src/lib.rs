use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
pub struct RevisionVector {
    pub editor_session_id: String,
    pub event_seq: u64,
    pub project_revision: u64,
    pub operation_seq: u64,
    #[serde(default)]
    pub scene_revisions: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SnapshotMetadata {
    pub snapshot_id: String,
    pub project_id: String,
    pub editor_session_id: String,
    pub base_event_seq: u64,
    pub revisions: RevisionVector,
    pub chunk_count: usize,
    pub limits_applied: Value,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SnapshotChunk {
    pub snapshot_id: String,
    pub chunk_index: usize,
    pub payload: Value,
    pub payload_json: String,
    pub checksum: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SnapshotEnd {
    pub snapshot_id: String,
    pub chunk_count: usize,
    pub entity_count: usize,
    pub checksum: String,
    pub revisions: RevisionVector,
}

#[derive(Clone, Debug, Serialize)]
pub struct SemanticSnapshot {
    pub snapshot_id: String,
    pub project_id: String,
    pub editor_session_id: String,
    pub base_event_seq: u64,
    pub revisions: RevisionVector,
    pub editor_state: Value,
    pub scenes: BTreeMap<String, Value>,
    pub nodes: BTreeMap<String, Value>,
    pub current_scene_id: Option<String>,
    pub selected_node_ids: Vec<String>,
    pub truncated: bool,
    pub limits_applied: Value,
}

impl SemanticSnapshot {
    fn from_entities(
        metadata: SnapshotMetadata,
        entities: Vec<Value>,
    ) -> Result<Self, ReplicaError> {
        let mut editor_state = None;
        let mut scenes = BTreeMap::new();
        let mut nodes = BTreeMap::new();
        let mut truncated = metadata
            .limits_applied
            .get("truncated")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        for entity in entities {
            let kind = entity
                .get("kind")
                .and_then(Value::as_str)
                .ok_or(ReplicaError::InvalidEntity("missing kind"))?;
            let entity_id = entity
                .get("entity_id")
                .and_then(Value::as_str)
                .ok_or(ReplicaError::InvalidEntity("missing entity_id"))?
                .to_owned();
            truncated |= entity
                .get("properties_truncated")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            match kind {
                "editor_state" => {
                    if editor_state.replace(entity).is_some() {
                        return Err(ReplicaError::DuplicateEntity(entity_id));
                    }
                }
                "scene" => {
                    if scenes.insert(entity_id.clone(), entity).is_some() {
                        return Err(ReplicaError::DuplicateEntity(entity_id));
                    }
                }
                "node" => {
                    if nodes.insert(entity_id.clone(), entity).is_some() {
                        return Err(ReplicaError::DuplicateEntity(entity_id));
                    }
                }
                _ => return Err(ReplicaError::InvalidEntity("unknown kind")),
            }
        }

        let editor_state =
            editor_state.ok_or(ReplicaError::InvalidEntity("missing editor_state"))?;
        let current_scene_id = editor_state
            .get("current_scene_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let selected_node_ids = current_scene_id
            .as_ref()
            .and_then(|scene_id| scenes.get(scene_id))
            .and_then(|scene| scene.get("selected_node_ids"))
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        Ok(Self {
            snapshot_id: metadata.snapshot_id,
            project_id: metadata.project_id,
            editor_session_id: metadata.editor_session_id,
            base_event_seq: metadata.base_event_seq,
            revisions: metadata.revisions,
            editor_state,
            scenes,
            nodes,
            current_scene_id,
            selected_node_ids,
            truncated,
            limits_applied: metadata.limits_applied,
        })
    }

    pub fn editor_state_result(&self) -> Value {
        let partial_reasons = if self.truncated {
            vec!["bounded_projection"]
        } else {
            Vec::new()
        };
        json!({
            "schema_version": "1.0",
            "project_id": self.project_id,
            "editor_session_id": self.editor_session_id,
            "snapshot_id": self.snapshot_id,
            "revision_vector": self.revisions,
            "capabilities_used": ["editor.context", "sync.full_snapshot_v1", "sync.event_stream_v1"],
            "status": "ready",
            "freshness": "current",
            "truncated": self.truncated,
            "partial_reasons": partial_reasons,
            "diagnostics": [],
            "limits_applied": self.limits_applied,
            "entities": [self.editor_state.clone()],
            "facts": [],
            "evidence": [{"source": "live_editor_snapshot", "freshness": "current"}],
            "editor": self.editor_state,
        })
    }

    pub fn current_scene_result(&self) -> Value {
        let scene = self
            .current_scene_id
            .as_ref()
            .and_then(|scene_id| self.scenes.get(scene_id))
            .cloned();
        let nodes = self
            .current_scene_id
            .as_ref()
            .map(|scene_id| {
                self.nodes
                    .values()
                    .filter(|node| node.get("scene_id").and_then(Value::as_str) == Some(scene_id))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let mut entities = Vec::new();
        if let Some(scene) = &scene {
            entities.push(scene.clone());
        }
        entities.extend(nodes.iter().cloned());
        let partial_reasons = if self.truncated {
            vec!["bounded_projection"]
        } else {
            Vec::new()
        };
        json!({
            "schema_version": "1.0",
            "project_id": self.project_id,
            "editor_session_id": self.editor_session_id,
            "snapshot_id": self.snapshot_id,
            "revision_vector": self.revisions,
            "capabilities_used": ["editor.context", "sync.full_snapshot_v1", "sync.event_stream_v1"],
            "status": "ready",
            "freshness": "current",
            "truncated": self.truncated,
            "partial_reasons": partial_reasons,
            "diagnostics": [],
            "limits_applied": self.limits_applied,
            "entities": entities,
            "facts": [],
            "evidence": [{"source": "live_editor_snapshot", "freshness": "current"}],
            "scene": scene,
            "nodes": nodes,
        })
    }

    pub fn selected_nodes_result(&self) -> Value {
        let selected = self
            .selected_node_ids
            .iter()
            .filter_map(|node_id| self.nodes.get(node_id))
            .cloned()
            .collect::<Vec<_>>();
        let scene = self
            .current_scene_id
            .as_ref()
            .and_then(|scene_id| self.scenes.get(scene_id))
            .cloned();
        let scene_dirty = scene
            .as_ref()
            .and_then(|value| value.get("dirty"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut entities = Vec::new();
        if let Some(scene) = &scene {
            entities.push(scene.clone());
        }
        entities.extend(selected.iter().cloned());
        let partial_reasons = if self.truncated {
            vec!["bounded_projection"]
        } else {
            Vec::new()
        };
        json!({
            "schema_version": "1.0",
            "project_id": self.project_id,
            "editor_session_id": self.editor_session_id,
            "snapshot_id": self.snapshot_id,
            "revision_vector": self.revisions,
            "capabilities_used": ["editor.context", "editor.inspector", "sync.full_snapshot_v1", "sync.event_stream_v1"],
            "status": "ready",
            "freshness": "current",
            "truncated": self.truncated,
            "partial_reasons": partial_reasons,
            "diagnostics": [],
            "limits_applied": self.limits_applied,
            "entities": entities,
            "facts": [],
            "evidence": [{"source": "live_editor_snapshot", "freshness": "current"}],
            "scene": scene,
            "scene_dirty": scene_dirty,
            "selected_nodes": selected,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicaStatus {
    Disconnected,
    Syncing,
    Ready,
    Stale,
}

#[derive(Debug)]
struct StagingSnapshot {
    metadata: SnapshotMetadata,
    chunks: Vec<SnapshotChunk>,
    entities: Vec<Value>,
}

#[derive(Debug)]
struct ReplicaState {
    status: ReplicaStatus,
    current: Option<Arc<SemanticSnapshot>>,
    staging: Option<StagingSnapshot>,
    last_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SnapshotReplicator {
    inner: Arc<RwLock<ReplicaState>>,
}

impl Default for SnapshotReplicator {
    fn default() -> Self {
        Self::new()
    }
}

impl SnapshotReplicator {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(ReplicaState {
                status: ReplicaStatus::Disconnected,
                current: None,
                staging: None,
                last_error: None,
            })),
        }
    }

    pub fn status(&self) -> ReplicaStatus {
        self.inner.read().expect("replica lock poisoned").status
    }

    pub fn last_error(&self) -> Option<String> {
        self.inner
            .read()
            .expect("replica lock poisoned")
            .last_error
            .clone()
    }

    pub fn mark_disconnected(&self, error: impl Into<String>) {
        let mut state = self.inner.write().expect("replica lock poisoned");
        state.status = ReplicaStatus::Disconnected;
        state.staging = None;
        state.last_error = Some(error.into());
    }

    pub fn invalidate(&self, reason: impl Into<String>) {
        let mut state = self.inner.write().expect("replica lock poisoned");
        state.status = ReplicaStatus::Stale;
        state.staging = None;
        state.last_error = Some(reason.into());
    }

    pub fn begin(&self, metadata: SnapshotMetadata) -> Result<(), ReplicaError> {
        if metadata.snapshot_id.is_empty() || metadata.chunk_count == 0 {
            return Err(ReplicaError::InvalidSnapshot("invalid begin metadata"));
        }
        if metadata.revisions.editor_session_id != metadata.editor_session_id
            || metadata.revisions.event_seq != metadata.base_event_seq
        {
            return Err(ReplicaError::RevisionMismatch);
        }
        let mut state = self.inner.write().expect("replica lock poisoned");
        state.status = ReplicaStatus::Syncing;
        state.last_error = None;
        state.staging = Some(StagingSnapshot {
            metadata,
            chunks: Vec::new(),
            entities: Vec::new(),
        });
        Ok(())
    }

    pub fn push_chunk(&self, chunk: SnapshotChunk) -> Result<(), ReplicaError> {
        let mut state = self.inner.write().expect("replica lock poisoned");
        let staging = state
            .staging
            .as_mut()
            .ok_or(ReplicaError::NoSnapshotInProgress)?;
        if chunk.snapshot_id != staging.metadata.snapshot_id {
            return Err(ReplicaError::SnapshotIdMismatch);
        }
        if chunk.chunk_index != staging.chunks.len() {
            return Err(ReplicaError::ChunkOrder);
        }
        let actual_checksum = format!("{:x}", Sha256::digest(chunk.payload_json.as_bytes()));
        if actual_checksum != chunk.checksum {
            return Err(ReplicaError::ChecksumMismatch);
        }
        let canonical_payload: Value = serde_json::from_str(&chunk.payload_json)
            .map_err(|_| ReplicaError::InvalidSnapshot("payload_json is invalid"))?;
        if canonical_payload != chunk.payload {
            return Err(ReplicaError::InvalidSnapshot(
                "payload and payload_json differ",
            ));
        }
        let entities = canonical_payload
            .get("entities")
            .and_then(Value::as_array)
            .ok_or(ReplicaError::InvalidSnapshot("chunk entities are missing"))?;
        staging.entities.extend(entities.iter().cloned());
        staging.chunks.push(chunk);
        Ok(())
    }

    pub fn end(&self, end: SnapshotEnd) -> Result<Arc<SemanticSnapshot>, ReplicaError> {
        let mut state = self.inner.write().expect("replica lock poisoned");
        let staging = state
            .staging
            .take()
            .ok_or(ReplicaError::NoSnapshotInProgress)?;
        if end.snapshot_id != staging.metadata.snapshot_id {
            return Err(ReplicaError::SnapshotIdMismatch);
        }
        if end.chunk_count != staging.metadata.chunk_count
            || end.chunk_count != staging.chunks.len()
        {
            return Err(ReplicaError::ChunkCountMismatch);
        }
        if end.entity_count != staging.entities.len() {
            return Err(ReplicaError::EntityCountMismatch);
        }
        if end.revisions != staging.metadata.revisions {
            return Err(ReplicaError::RevisionMismatch);
        }
        let checksum_input = staging
            .chunks
            .iter()
            .map(|chunk| chunk.checksum.as_str())
            .collect::<String>();
        let checksum = format!("{:x}", Sha256::digest(checksum_input.as_bytes()));
        if checksum != end.checksum {
            return Err(ReplicaError::ChecksumMismatch);
        }
        let snapshot = Arc::new(SemanticSnapshot::from_entities(
            staging.metadata,
            staging.entities,
        )?);
        state.current = Some(snapshot.clone());
        state.status = ReplicaStatus::Ready;
        state.last_error = None;
        Ok(snapshot)
    }

    pub fn read(&self) -> Result<Arc<SemanticSnapshot>, ReplicaReadError> {
        let state = self.inner.read().expect("replica lock poisoned");
        if state.status != ReplicaStatus::Ready {
            return Err(ReplicaReadError {
                status: state.status,
                message: state
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "editor snapshot is not ready".to_owned()),
            });
        }
        state.current.clone().ok_or(ReplicaReadError {
            status: state.status,
            message: "ready replica has no snapshot".to_owned(),
        })
    }
}

#[derive(Clone, Debug, Error)]
#[error("semantic replica is {status:?}: {message}")]
pub struct ReplicaReadError {
    pub status: ReplicaStatus,
    pub message: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ReplicaError {
    #[error("no snapshot is in progress")]
    NoSnapshotInProgress,
    #[error("snapshot identifier mismatch")]
    SnapshotIdMismatch,
    #[error("snapshot chunks are out of order")]
    ChunkOrder,
    #[error("snapshot chunk count mismatch")]
    ChunkCountMismatch,
    #[error("snapshot entity count mismatch")]
    EntityCountMismatch,
    #[error("snapshot checksum mismatch")]
    ChecksumMismatch,
    #[error("snapshot revision mismatch")]
    RevisionMismatch,
    #[error("duplicate entity: {0}")]
    DuplicateEntity(String),
    #[error("invalid entity: {0}")]
    InvalidEntity(&'static str),
    #[error("invalid snapshot: {0}")]
    InvalidSnapshot(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn revisions() -> RevisionVector {
        RevisionVector {
            editor_session_id: "editor:0123456789abcdef0123456789abcdef".to_owned(),
            event_seq: 7,
            project_revision: 3,
            operation_seq: 0,
            scene_revisions: BTreeMap::new(),
        }
    }

    fn metadata() -> SnapshotMetadata {
        SnapshotMetadata {
            snapshot_id: "snapshot:0123456789abcdef0123456789abcdef".to_owned(),
            project_id:
                "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
                    .to_owned(),
            editor_session_id: revisions().editor_session_id.clone(),
            base_event_seq: 7,
            revisions: revisions(),
            chunk_count: 1,
            limits_applied: json!({"variant_depth": 8, "container_items": 1000}),
        }
    }

    #[test]
    fn snapshot_becomes_visible_only_after_verified_end() {
        let replicator = SnapshotReplicator::new();
        replicator.begin(metadata()).unwrap();
        assert_eq!(replicator.status(), ReplicaStatus::Syncing);
        assert!(replicator.read().is_err());

        let payload = json!({"entities": [{
            "kind": "editor_state",
            "entity_id": "editor:0123456789abcdef0123456789abcdef",
            "current_scene_id": ""
        }]});
        let payload_json = serde_json::to_string(&payload).unwrap();
        let chunk_checksum = format!("{:x}", Sha256::digest(payload_json.as_bytes()));
        replicator
            .push_chunk(SnapshotChunk {
                snapshot_id: metadata().snapshot_id,
                chunk_index: 0,
                payload,
                payload_json,
                checksum: chunk_checksum.clone(),
            })
            .unwrap();
        assert!(replicator.read().is_err());

        let end_checksum = format!("{:x}", Sha256::digest(chunk_checksum.as_bytes()));
        replicator
            .end(SnapshotEnd {
                snapshot_id: metadata().snapshot_id,
                chunk_count: 1,
                entity_count: 1,
                checksum: end_checksum,
                revisions: revisions(),
            })
            .unwrap();
        assert_eq!(replicator.status(), ReplicaStatus::Ready);
        assert!(replicator.read().is_ok());
    }

    #[test]
    fn corrupt_chunk_never_replaces_the_current_generation() {
        let replicator = SnapshotReplicator::new();
        replicator.begin(metadata()).unwrap();
        let result = replicator.push_chunk(SnapshotChunk {
            snapshot_id: metadata().snapshot_id,
            chunk_index: 0,
            payload: json!({"entities": []}),
            payload_json: "{\"entities\":[]}".to_owned(),
            checksum: "0".repeat(64),
        });
        assert_eq!(result, Err(ReplicaError::ChecksumMismatch));
        assert!(replicator.read().is_err());
    }

    #[test]
    fn invalidation_hides_the_old_generation_until_resync_commits() {
        let replicator = SnapshotReplicator::new();
        let first_payload = json!({"entities": [{
            "kind": "editor_state",
            "entity_id": "editor:0123456789abcdef0123456789abcdef",
            "current_scene_id": ""
        }]});
        let first_json = serde_json::to_string(&first_payload).unwrap();
        let first_checksum = format!("{:x}", Sha256::digest(first_json.as_bytes()));
        replicator.begin(metadata()).unwrap();
        replicator
            .push_chunk(SnapshotChunk {
                snapshot_id: metadata().snapshot_id,
                chunk_index: 0,
                payload: first_payload,
                payload_json: first_json,
                checksum: first_checksum.clone(),
            })
            .unwrap();
        replicator
            .end(SnapshotEnd {
                snapshot_id: metadata().snapshot_id,
                chunk_count: 1,
                entity_count: 1,
                checksum: format!("{:x}", Sha256::digest(first_checksum.as_bytes())),
                revisions: revisions(),
            })
            .unwrap();
        assert!(replicator.read().is_ok());

        replicator.invalidate("event sequence gap");
        assert_eq!(replicator.status(), ReplicaStatus::Stale);
        assert!(replicator.read().is_err());

        let mut next_revisions = revisions();
        next_revisions.event_seq = 8;
        next_revisions.project_revision = 4;
        let next_id = "snapshot:11111111111111111111111111111111".to_owned();
        replicator
            .begin(SnapshotMetadata {
                snapshot_id: next_id.clone(),
                project_id: metadata().project_id,
                editor_session_id: next_revisions.editor_session_id.clone(),
                base_event_seq: 8,
                revisions: next_revisions.clone(),
                chunk_count: 1,
                limits_applied: json!({"variant_depth": 8, "container_items": 1000}),
            })
            .unwrap();
        assert!(replicator.read().is_err());
        let next_payload = json!({"entities": [{
            "kind": "editor_state",
            "entity_id": "editor:0123456789abcdef0123456789abcdef",
            "current_scene_id": "",
            "generation": 2
        }]});
        let next_json = serde_json::to_string(&next_payload).unwrap();
        let next_checksum = format!("{:x}", Sha256::digest(next_json.as_bytes()));
        replicator
            .push_chunk(SnapshotChunk {
                snapshot_id: next_id.clone(),
                chunk_index: 0,
                payload: next_payload,
                payload_json: next_json,
                checksum: next_checksum.clone(),
            })
            .unwrap();
        let current = replicator
            .end(SnapshotEnd {
                snapshot_id: next_id.clone(),
                chunk_count: 1,
                entity_count: 1,
                checksum: format!("{:x}", Sha256::digest(next_checksum.as_bytes())),
                revisions: next_revisions,
            })
            .unwrap();
        assert_eq!(current.snapshot_id, next_id);
        assert_eq!(current.revisions.event_seq, 8);
        assert_eq!(current.editor_state["generation"], 2);
    }

    #[test]
    fn canonical_bridge_1_1_fixture_commits() {
        let response: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/rpc-snapshot-response.json"
        ))
        .unwrap();
        let begin: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/snapshot-begin.json"
        ))
        .unwrap();
        let chunk: SnapshotChunk = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/snapshot-chunk.json"
        ))
        .unwrap();
        let end_message: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/snapshot-end.json"
        ))
        .unwrap();
        let result = &response["result"];
        let metadata = SnapshotMetadata {
            snapshot_id: result["snapshot_id"].as_str().unwrap().to_owned(),
            project_id: response["context"]["project_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            editor_session_id: response["context"]["editor_session_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            base_event_seq: result["base_event_seq"].as_u64().unwrap(),
            revisions: serde_json::from_value(result["revisions"].clone()).unwrap(),
            chunk_count: usize::try_from(begin["params"]["chunk_count"].as_u64().unwrap()).unwrap(),
            limits_applied: result["limits_applied"].clone(),
        };
        let end = SnapshotEnd {
            snapshot_id: end_message["params"]["snapshot_id"]
                .as_str()
                .unwrap()
                .to_owned(),
            chunk_count: usize::try_from(end_message["params"]["chunk_count"].as_u64().unwrap())
                .unwrap(),
            entity_count: usize::try_from(end_message["params"]["entity_count"].as_u64().unwrap())
                .unwrap(),
            checksum: end_message["params"]["checksum"]
                .as_str()
                .unwrap()
                .to_owned(),
            revisions: serde_json::from_value(end_message["params"]["revisions"].clone()).unwrap(),
        };
        let replicator = SnapshotReplicator::new();
        replicator.begin(metadata).unwrap();
        replicator.push_chunk(chunk).unwrap();
        let snapshot = replicator.end(end).unwrap();
        assert_eq!(snapshot.revisions.event_seq, 7);
        assert!(snapshot.editor_state.is_object());
        let selected = snapshot.selected_nodes_result();
        assert_eq!(
            selected.pointer("/selected_nodes/0/node_path"),
            Some(&Value::String("Player".to_owned()))
        );
        assert_eq!(
            selected.pointer("/selected_nodes/0/script_path"),
            Some(&Value::String("res://player.gd".to_owned()))
        );
        assert_eq!(
            selected.pointer("/selected_nodes/0/properties/0/value"),
            Some(&json!(275.0))
        );
        assert_eq!(
            selected.pointer("/selected_nodes/0/properties/0/source"),
            Some(&Value::String("live_editor_property".to_owned()))
        );
        assert_eq!(selected.pointer("/scene_dirty"), Some(&Value::Bool(true)));
    }
}
