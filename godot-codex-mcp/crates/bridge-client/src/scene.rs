use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::protocol::{BridgeError, Session};
use crate::resource::{ResourceRef, RpcContext};

const MAX_SCENES: usize = 100_000;
const MAX_NODES: usize = 1_000_000;
const MAX_PROPERTIES: usize = 4_000_000;
const MAX_RELATIONS: usize = 4_000_000;
const MAX_DIAGNOSTICS: usize = 100_000;
const MAX_SNAPSHOT_CHUNK_BYTES: usize = 256 * 1_024;
const MAX_DELTA_BATCH_BYTES: usize = 512 * 1_024;
const MAX_PROJECTED_VALUE_BYTES: usize = 65_536;
const MAX_SAFE_REVISION: u64 = 9_007_199_254_740_991;
const SCENE_SNAPSHOT_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const SCENE_SNAPSHOT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneRevisionVector {
    pub editor_session_id: String,
    pub event_seq: u64,
    pub project_revision: u64,
    pub operation_seq: u64,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_graph_revision: Option<u64>,
    pub scene_revisions: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectedVariant {
    #[serde(rename = "type")]
    pub variant_type: String,
    pub value: Value,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PropertyObservation {
    pub name: String,
    pub value: ProjectedVariant,
    pub deferred_node_path: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneIdentityScope {
    Persistent,
    ContentRevision,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NodeObservation {
    pub node_path: String,
    pub parent_path: Option<String>,
    pub owner_path: Option<String>,
    pub name: String,
    pub godot_type: String,
    pub node_index: i32,
    pub unique_scene_id: Option<u32>,
    pub identity_scope: SceneIdentityScope,
    pub owned: bool,
    pub internal: bool,
    pub instance: Option<ResourceRef>,
    pub instance_placeholder: Option<String>,
    pub groups: Vec<String>,
    pub properties: Vec<PropertyObservation>,
    pub attached_script: Option<ResourceRef>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionObservation {
    pub emitter: String,
    pub signal: String,
    pub receiver: String,
    pub method: String,
    pub flags: u32,
    pub unbinds: usize,
    pub binds: Vec<ProjectedVariant>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SubresourceObservation {
    pub scene_unique_id: Option<String>,
    pub godot_type: String,
    pub identity_scope: SceneIdentityScope,
    pub ownership_paths: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationTrackResolution {
    Resolved,
    Broken,
    Unresolved,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnimationTrackObservation {
    pub mixer: String,
    pub library: String,
    pub animation: String,
    pub track_index: usize,
    pub node_path: String,
    pub resolution: AnimationTrackResolution,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneObservation {
    pub scene_ref: ResourceRef,
    pub path: String,
    pub content_generation: String,
    pub identity_scope: SceneIdentityScope,
    pub base_scene_ref: Option<ResourceRef>,
    pub nodes: Vec<NodeObservation>,
    pub connections: Vec<ConnectionObservation>,
    pub editable_instances: Vec<String>,
    pub subresources: Vec<SubresourceObservation>,
    pub animation_tracks: Vec<AnimationTrackObservation>,
    pub authority: String,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub source_complete: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectContextObservation {
    pub key: String,
    pub value: ProjectedVariant,
    pub authority: String,
    pub scene_graph_revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneDiagnosticCode {
    WeakSceneIdentity,
    WeakNodeIdentity,
    WeakSubresourceIdentity,
    DuplicateSceneNodeId,
    DuplicateSubresourceId,
    BrokenOverrideTarget,
    BrokenNodePath,
    BrokenAnimationNodePath,
    SceneCompositionCycle,
    SceneLimitExceeded,
    SceneLoadFailed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneDiagnostic {
    pub code: SceneDiagnosticCode,
    pub subject: String,
    #[serde(default)]
    pub detail: Option<String>,
    pub scene_graph_revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSnapshotPayload {
    pub scenes: Vec<SceneObservation>,
    pub project_context: Vec<ProjectContextObservation>,
    pub diagnostics: Vec<SceneDiagnostic>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSnapshotLimits {
    pub scene_records: usize,
    pub scene_nodes: usize,
    pub scene_properties: usize,
    pub scene_relations: usize,
    pub instance_depth: usize,
    pub snapshot_chunk_bytes: usize,
    pub snapshot_window_bytes: usize,
    pub snapshot_timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSnapshotAccepted {
    pub snapshot_id: String,
    pub domain: String,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub revisions: SceneRevisionVector,
    pub limits_applied: SceneSnapshotLimits,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSnapshotBeginParams {
    pub snapshot_id: String,
    pub domain: String,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub revisions: SceneRevisionVector,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneSnapshotBeginMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: SceneSnapshotBeginParams,
    context: RpcContext,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSnapshotChunk {
    pub protocol_version: String,
    pub kind: String,
    pub snapshot_id: String,
    pub domain: String,
    pub chunk_index: usize,
    pub payload: SceneSnapshotPayload,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_json: Option<String>,
    pub checksum: String,
    pub context: RpcContext,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSnapshotEndParams {
    pub snapshot_id: String,
    pub domain: String,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub revisions: SceneRevisionVector,
    pub chunk_count: usize,
    pub checksum: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct SceneSnapshotEndMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: SceneSnapshotEndParams,
    context: RpcContext,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SceneSnapshotTransfer {
    pub accepted: SceneSnapshotAccepted,
    pub end: SceneSnapshotEndParams,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SceneSnapshot {
    pub accepted: SceneSnapshotAccepted,
    pub begin: SceneSnapshotBeginParams,
    pub payload: SceneSnapshotPayload,
    pub end: SceneSnapshotEndParams,
}

pub trait SceneSnapshotSink {
    fn begin(
        &mut self,
        accepted: &SceneSnapshotAccepted,
        begin: &SceneSnapshotBeginParams,
    ) -> Result<(), BridgeError>;

    fn chunk(&mut self, chunk: &SceneSnapshotChunk) -> Result<(), BridgeError>;

    fn end(&mut self, end: &SceneSnapshotEndParams) -> Result<(), BridgeError>;
}

#[derive(Default)]
struct CollectingSink {
    accepted: Option<SceneSnapshotAccepted>,
    begin: Option<SceneSnapshotBeginParams>,
    scenes: Vec<SceneObservation>,
    project_context: Vec<ProjectContextObservation>,
    diagnostics: Vec<SceneDiagnostic>,
    end: Option<SceneSnapshotEndParams>,
}

impl SceneSnapshotSink for CollectingSink {
    fn begin(
        &mut self,
        accepted: &SceneSnapshotAccepted,
        begin: &SceneSnapshotBeginParams,
    ) -> Result<(), BridgeError> {
        self.accepted = Some(accepted.clone());
        self.begin = Some(begin.clone());
        Ok(())
    }

    fn chunk(&mut self, chunk: &SceneSnapshotChunk) -> Result<(), BridgeError> {
        self.scenes.extend(chunk.payload.scenes.clone());
        self.project_context
            .extend(chunk.payload.project_context.clone());
        self.diagnostics.extend(chunk.payload.diagnostics.clone());
        Ok(())
    }

    fn end(&mut self, end: &SceneSnapshotEndParams) -> Result<(), BridgeError> {
        self.end = Some(end.clone());
        Ok(())
    }
}

impl CollectingSink {
    fn finish(self) -> Result<SceneSnapshot, BridgeError> {
        Ok(SceneSnapshot {
            accepted: self.accepted.ok_or_else(|| {
                BridgeError::Invalid("scene snapshot acceptance is missing".to_owned())
            })?,
            begin: self.begin.ok_or_else(|| {
                BridgeError::Invalid("scene snapshot begin is missing".to_owned())
            })?,
            payload: SceneSnapshotPayload {
                scenes: self.scenes,
                project_context: self.project_context,
                diagnostics: self.diagnostics,
            },
            end: self
                .end
                .ok_or_else(|| BridgeError::Invalid("scene snapshot end is missing".to_owned()))?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SceneDeltaOperation {
    Upsert {
        value: Box<SceneObservation>,
    },
    Remove {
        scene_ref: ResourceRef,
        path: String,
    },
    ProjectContext {
        values: Vec<ProjectContextObservation>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SceneDeltaBatch {
    pub batch_id: String,
    pub previous_scene_graph_revision: u64,
    pub scene_graph_revision: u64,
    pub resource_revision: u64,
    pub project_revision: u64,
    pub operations: Vec<SceneDeltaOperation>,
    pub source_complete: bool,
    pub checksum: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum SceneDeltaPoll {
    Current {
        current_scene_graph_revision: u64,
    },
    Batch {
        current_scene_graph_revision: u64,
        batch: SceneDeltaBatch,
    },
    Gap {
        requested_after_scene_graph_revision: u64,
        oldest_available_scene_graph_revision: u64,
        current_scene_graph_revision: u64,
    },
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn canonical_json(value: &Value, output: &mut String) -> Result<(), BridgeError> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            output.push_str(&serde_json::to_string(value)?);
        }
        Value::Array(values) => {
            output.push('[');
            for (index, entry) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                canonical_json(entry, output)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort_unstable();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key)?);
                output.push(':');
                canonical_json(&values[key], output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn canonical_serialized<T: Serialize>(value: &T) -> Result<String, BridgeError> {
    let mut output = String::new();
    canonical_json(&serde_json::to_value(value)?, &mut output)?;
    Ok(output)
}

fn valid_uid(value: &str) -> bool {
    value
        .strip_prefix("uid://")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.len() <= 122)
        && value
            .bytes()
            .skip(6)
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn validate_resource_path(value: &str) -> Result<(), BridgeError> {
    if !value.starts_with("res://")
        || value.len() > 1_024
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'\\' | b'?' | b'#' | b'\0'))
        || value[6..]
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(BridgeError::Invalid(
            "scene resource path is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_resource_ref(value: &ResourceRef) -> Result<(), BridgeError> {
    match value {
        ResourceRef::Uid(reference) if valid_uid(&reference.uid) => Ok(()),
        ResourceRef::Path(reference) if reference.uid_missing => {
            validate_resource_path(&reference.path)
        }
        _ => Err(BridgeError::Invalid(
            "scene resource reference is invalid".to_owned(),
        )),
    }
}

fn valid_node_path(value: &str, allow_empty: bool) -> bool {
    if value.is_empty() {
        return allow_empty;
    }
    if value == "." {
        return true;
    }
    !value.starts_with('/')
        && value.len() <= 2_048
        && !value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'\\' | b':'))
        && value
            .split('/')
            .all(|component| !component.is_empty() && !matches!(component, "." | ".."))
}

fn validate_projected_json(value: &Value, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    match value {
        Value::String(text) => text.chars().count() <= 16_384,
        Value::Array(values) => {
            values.len() <= 1_000
                && values
                    .iter()
                    .all(|entry| validate_projected_json(entry, depth + 1))
        }
        Value::Object(values) => {
            values.len() <= 1_000
                && values.iter().all(|(key, entry)| {
                    key.chars().count() <= 16_384 && validate_projected_json(entry, depth + 1)
                })
        }
        _ => true,
    }
}

fn validate_projected_variant(value: &ProjectedVariant) -> Result<(), BridgeError> {
    const TYPES: &[&str] = &[
        "nil",
        "bool",
        "int",
        "float",
        "string",
        "string_name",
        "node_path",
        "vector2",
        "vector2i",
        "vector3",
        "vector3i",
        "vector4",
        "vector4i",
        "rect2",
        "rect2i",
        "transform2d",
        "plane",
        "quaternion",
        "aabb",
        "basis",
        "transform3d",
        "projection",
        "color",
        "rid",
        "callable",
        "signal",
        "dictionary",
        "array",
        "packed_array",
        "resource",
        "unsupported",
    ];
    if !TYPES.contains(&value.variant_type.as_str())
        || !validate_projected_json(&value.value, 0)
        || serde_json::to_vec(value)?.len() > MAX_PROJECTED_VALUE_BYTES
    {
        return Err(BridgeError::Invalid(
            "projected Variant is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_scene(
    scene: &SceneObservation,
    accepted: &SceneSnapshotAccepted,
) -> Result<(), BridgeError> {
    validate_resource_ref(&scene.scene_ref)?;
    validate_resource_path(&scene.path)?;
    if !scene.path.ends_with(".tscn") && !scene.path.ends_with(".scn") {
        return Err(BridgeError::Invalid(
            "scene path extension is invalid".to_owned(),
        ));
    }
    if let Some(base) = &scene.base_scene_ref {
        validate_resource_ref(base)?;
    }
    let generation = scene
        .content_generation
        .strip_prefix("sha256:")
        .unwrap_or_default();
    if generation.len() != 64
        || !generation
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || scene.authority != "packed_scene_state"
        || !scene.source_complete
        || scene.resource_revision > accepted.resource_revision
        || scene.scene_graph_revision > accepted.scene_graph_revision
        || scene.nodes.len() > MAX_NODES
        || scene.connections.len() > MAX_RELATIONS
        || scene.editable_instances.len() > MAX_NODES
        || scene.subresources.len() > MAX_RELATIONS
        || scene.animation_tracks.len() > MAX_RELATIONS
    {
        return Err(BridgeError::Invalid(
            "scene observation is invalid".to_owned(),
        ));
    }

    let mut node_paths = BTreeSet::new();
    let mut property_count = 0_usize;
    for node in &scene.nodes {
        if !valid_node_path(&node.node_path, false)
            || node
                .parent_path
                .as_deref()
                .is_some_and(|path| !valid_node_path(path, false))
            || node
                .owner_path
                .as_deref()
                .is_some_and(|path| !valid_node_path(path, false))
            || node.name.is_empty()
            || node.name.chars().count() > 512
            || node.godot_type.chars().count() > 256
            || node.unique_scene_id == Some(0)
            || !node_paths.insert(&node.node_path)
            || node.groups.len() > 1_024
            || node.properties.len() > 16_384
        {
            return Err(BridgeError::Invalid(format!(
                "scene node is invalid: scene={} path={} parent={:?} owner={:?} name={:?} type={:?} unique_id={:?}",
                scene.path,
                node.node_path,
                node.parent_path,
                node.owner_path,
                node.name,
                node.godot_type,
                node.unique_scene_id
            )));
        }
        if let Some(reference) = &node.instance {
            validate_resource_ref(reference)?;
        }
        if let Some(path) = &node.instance_placeholder {
            validate_resource_path(path)?;
        }
        if let Some(reference) = &node.attached_script {
            validate_resource_ref(reference)?;
        }
        let mut groups = BTreeSet::new();
        for group in &node.groups {
            if group.is_empty() || group.chars().count() > 512 || !groups.insert(group) {
                return Err(BridgeError::Invalid(
                    "scene node group is invalid".to_owned(),
                ));
            }
        }
        property_count = property_count
            .checked_add(node.properties.len())
            .ok_or_else(|| BridgeError::Invalid("scene property count overflow".to_owned()))?;
        for property in &node.properties {
            if property.name.is_empty() || property.name.chars().count() > 512 {
                return Err(BridgeError::Invalid("scene property is invalid".to_owned()));
            }
            validate_projected_variant(&property.value)?;
        }
    }
    if property_count > MAX_PROPERTIES {
        return Err(BridgeError::Invalid(
            "scene property count is invalid".to_owned(),
        ));
    }
    for connection in &scene.connections {
        if !valid_node_path(&connection.emitter, false)
            || !valid_node_path(&connection.receiver, false)
            || connection.signal.is_empty()
            || connection.signal.chars().count() > 512
            || connection.method.is_empty()
            || connection.method.chars().count() > 512
            || connection.unbinds > 1_024
            || connection.binds.len() > 1_024
        {
            return Err(BridgeError::Invalid(
                "scene connection is invalid".to_owned(),
            ));
        }
        for bind in &connection.binds {
            validate_projected_variant(bind)?;
        }
    }
    let mut editable_instances = BTreeSet::new();
    for path in &scene.editable_instances {
        if !valid_node_path(path, false) || !editable_instances.insert(path) {
            return Err(BridgeError::Invalid(
                "editable instance path is invalid".to_owned(),
            ));
        }
    }
    for subresource in &scene.subresources {
        if subresource
            .scene_unique_id
            .as_ref()
            .is_some_and(|id| id.chars().count() > 256)
            || subresource.godot_type.is_empty()
            || subresource.godot_type.chars().count() > 256
            || subresource.ownership_paths.is_empty()
            || subresource.ownership_paths.len() > 4_096
        {
            return Err(BridgeError::Invalid(
                "scene subresource is invalid".to_owned(),
            ));
        }
        let mut ownership = BTreeSet::new();
        for path in &subresource.ownership_paths {
            if path.is_empty() || path.chars().count() > 2_048 || !ownership.insert(path) {
                return Err(BridgeError::Invalid(
                    "subresource ownership path is invalid".to_owned(),
                ));
            }
        }
    }
    for track in &scene.animation_tracks {
        if !valid_node_path(&track.mixer, false)
            || track.library.chars().count() > 512
            || track.animation.is_empty()
            || track.animation.chars().count() > 512
            || track.track_index > 1_000_000
            || track.node_path.is_empty()
            || track.node_path.chars().count() > 2_048
        {
            return Err(BridgeError::Invalid(
                "animation track observation is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

fn valid_project_key(key: &str) -> bool {
    if key == "application/run/main_scene" {
        return true;
    }
    if let Some(name) = key
        .strip_prefix("autoload/")
        .or_else(|| key.strip_prefix("input/"))
    {
        return !name.is_empty() && !name.contains('/');
    }
    const LAYER_PREFIXES: &[&str] = &[
        "layer_names/2d_physics/",
        "layer_names/2d_render/",
        "layer_names/3d_physics/",
        "layer_names/3d_render/",
        "layer_names/navigation/",
    ];
    LAYER_PREFIXES.iter().any(|prefix| {
        key.strip_prefix(prefix)
            .and_then(|number| number.parse::<u32>().ok())
            .is_some_and(|number| number > 0)
    })
}

fn validate_project_context(
    value: &ProjectContextObservation,
    scene_graph_revision: u64,
) -> Result<(), BridgeError> {
    if !valid_project_key(&value.key)
        || value.key.chars().count() > 512
        || !matches!(value.authority.as_str(), "project_settings" | "input_map")
        || value.scene_graph_revision > scene_graph_revision
    {
        return Err(BridgeError::Invalid(
            "project context observation is invalid".to_owned(),
        ));
    }
    validate_projected_variant(&value.value)
}

fn validate_snapshot_limits(limits: &SceneSnapshotLimits) -> Result<(), BridgeError> {
    if limits.scene_records != MAX_SCENES
        || limits.scene_nodes != MAX_NODES
        || limits.scene_properties != MAX_PROPERTIES
        || limits.scene_relations != MAX_RELATIONS
        || limits.instance_depth != 64
        || limits.snapshot_chunk_bytes != MAX_SNAPSHOT_CHUNK_BYTES
        || limits.snapshot_window_bytes != 32 * 1_024 * 1_024
        || limits.snapshot_timeout_ms != 120_000
    {
        return Err(BridgeError::Invalid(
            "scene snapshot limits are invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_snapshot_chunk(
    chunk: &SceneSnapshotChunk,
    accepted: &SceneSnapshotAccepted,
    expected_index: usize,
) -> Result<(), BridgeError> {
    let canonical_payload = canonical_serialized(&chunk.payload)?;
    let checksum_input = chunk.payload_json.as_deref().unwrap_or(&canonical_payload);
    if !matches!(chunk.protocol_version.as_str(), "1.3" | "1.4")
        || chunk.kind != "chunk"
        || chunk.domain != "scene_graph"
        || chunk.snapshot_id != accepted.snapshot_id
        || chunk.chunk_index != expected_index
        || checksum_input.len() > MAX_SNAPSHOT_CHUNK_BYTES
        || sha256_hex(checksum_input.as_bytes()) != chunk.checksum
    {
        return Err(BridgeError::Invalid(
            "scene snapshot chunk envelope is invalid".to_owned(),
        ));
    }
    if let Some(payload_json) = &chunk.payload_json {
        let payload_from_json: SceneSnapshotPayload =
            serde_json::from_str(payload_json).map_err(|error| {
                BridgeError::Invalid(format!("scene snapshot payload_json is invalid: {error}"))
            })?;
        if payload_from_json != chunk.payload {
            return Err(BridgeError::Invalid(
                "scene snapshot payload_json differs from payload".to_owned(),
            ));
        }
    }
    if chunk.payload.scenes.len() > MAX_SCENES
        || chunk.payload.project_context.len() > MAX_SCENES
        || chunk.payload.diagnostics.len() > MAX_DIAGNOSTICS
    {
        return Err(BridgeError::Invalid(
            "scene snapshot chunk count is invalid".to_owned(),
        ));
    }
    for scene in &chunk.payload.scenes {
        validate_scene(scene, accepted)?;
    }
    for value in &chunk.payload.project_context {
        validate_project_context(value, accepted.scene_graph_revision)?;
    }
    for diagnostic in &chunk.payload.diagnostics {
        if diagnostic.subject.is_empty()
            || diagnostic.subject.chars().count() > 2_048
            || diagnostic
                .detail
                .as_ref()
                .is_some_and(|detail| detail.chars().count() > 2_048)
            || diagnostic.scene_graph_revision > accepted.scene_graph_revision
        {
            return Err(BridgeError::Invalid(
                "scene diagnostic is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

pub(crate) async fn stream_scene_snapshot<S: SceneSnapshotSink>(
    session: &mut Session,
    sink: &mut S,
) -> Result<SceneSnapshotTransfer, BridgeError> {
    require_scene_graph(session)?;
    let response = session
        .request_with_deadline_and_timeout(
            "scene.snapshot.get",
            json!({}),
            SCENE_SNAPSHOT_REQUEST_TIMEOUT,
            SCENE_SNAPSHOT_TIMEOUT,
        )
        .await?;
    let request_id = response
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::Invalid("scene snapshot request ID is missing".to_owned()))?
        .to_owned();
    let result = receive_scene_snapshot(session, sink, &response).await;
    if result.is_err() {
        let _ = session
            .send_cancel(&request_id, "scene snapshot validation failed")
            .await;
    }
    result
}

async fn receive_scene_snapshot<S: SceneSnapshotSink>(
    session: &mut Session,
    sink: &mut S,
    response: &Value,
) -> Result<SceneSnapshotTransfer, BridgeError> {
    let accepted: SceneSnapshotAccepted = serde_json::from_value(
        response
            .get("result")
            .cloned()
            .ok_or_else(|| BridgeError::Invalid("scene snapshot result is missing".to_owned()))?,
    )
    .map_err(|error| {
        BridgeError::Invalid(format!("scene snapshot acceptance is invalid: {error}"))
    })?;
    if accepted.domain != "scene_graph"
        || accepted.revisions.resource_revision != accepted.resource_revision
        || accepted.revisions.scene_graph_revision != accepted.scene_graph_revision
        || match session.protocol_version() {
            "1.4" => accepted
                .revisions
                .script_graph_revision
                .is_none_or(|revision| revision > MAX_SAFE_REVISION),
            _ => accepted.revisions.script_graph_revision.is_some(),
        }
        || accepted.resource_revision > MAX_SAFE_REVISION
        || accepted.scene_graph_revision > MAX_SAFE_REVISION
        || !accepted.snapshot_id.starts_with("snapshot:")
    {
        return Err(BridgeError::Invalid(
            "scene snapshot acceptance is invalid".to_owned(),
        ));
    }
    validate_snapshot_limits(&accepted.limits_applied)?;

    let begin_message: SceneSnapshotBeginMessage = serde_json::from_value(
        session
            .receive_non_sync_with_timeout(SCENE_SNAPSHOT_TIMEOUT)
            .await?,
    )
    .map_err(|error| BridgeError::Invalid(format!("scene snapshot begin is invalid: {error}")))?;
    if begin_message.protocol_version != session.protocol_version()
        || begin_message.kind != "notification"
        || begin_message.method != "snapshot.begin"
        || begin_message.params.snapshot_id != accepted.snapshot_id
        || begin_message.params.domain != "scene_graph"
        || begin_message.params.resource_revision != accepted.resource_revision
        || begin_message.params.scene_graph_revision != accepted.scene_graph_revision
        || begin_message.params.revisions != accepted.revisions
    {
        return Err(BridgeError::Invalid(
            "scene snapshot begin is invalid".to_owned(),
        ));
    }
    sink.begin(&accepted, &begin_message.params)?;

    let mut expected_index = 0_usize;
    let mut checksum_input = String::new();
    let mut scene_count = 0_usize;
    let mut node_count = 0_usize;
    let mut property_count = 0_usize;
    let mut relation_count = 0_usize;
    let mut diagnostic_count = 0_usize;
    let end = loop {
        let message = session
            .receive_non_sync_with_timeout(SCENE_SNAPSHOT_TIMEOUT)
            .await?;
        if message.get("kind").and_then(Value::as_str) == Some("chunk") {
            let chunk: SceneSnapshotChunk = serde_json::from_value(message).map_err(|error| {
                BridgeError::Invalid(format!("scene snapshot chunk is invalid: {error}"))
            })?;
            validate_snapshot_chunk(&chunk, &accepted, expected_index)?;
            scene_count = scene_count
                .checked_add(chunk.payload.scenes.len())
                .ok_or_else(|| BridgeError::Invalid("scene count overflow".to_owned()))?;
            diagnostic_count = diagnostic_count
                .checked_add(chunk.payload.diagnostics.len())
                .ok_or_else(|| BridgeError::Invalid("diagnostic count overflow".to_owned()))?;
            for scene in &chunk.payload.scenes {
                node_count = node_count
                    .checked_add(scene.nodes.len())
                    .ok_or_else(|| BridgeError::Invalid("node count overflow".to_owned()))?;
                relation_count = relation_count
                    .checked_add(scene.connections.len())
                    .and_then(|count| count.checked_add(scene.subresources.len()))
                    .and_then(|count| count.checked_add(scene.animation_tracks.len()))
                    .ok_or_else(|| BridgeError::Invalid("relation count overflow".to_owned()))?;
                for node in &scene.nodes {
                    property_count = property_count
                        .checked_add(node.properties.len())
                        .ok_or_else(|| {
                            BridgeError::Invalid("property count overflow".to_owned())
                        })?;
                }
            }
            if scene_count > MAX_SCENES
                || node_count > MAX_NODES
                || property_count > MAX_PROPERTIES
                || relation_count > MAX_RELATIONS
                || diagnostic_count > MAX_DIAGNOSTICS
            {
                return Err(BridgeError::Invalid(
                    "scene snapshot total count is invalid".to_owned(),
                ));
            }
            checksum_input.push_str(&chunk.checksum);
            sink.chunk(&chunk)?;
            session
                .send_ack(&accepted.snapshot_id, Some("scene_graph"), expected_index)
                .await?;
            expected_index += 1;
            continue;
        }
        let end_message: SceneSnapshotEndMessage =
            serde_json::from_value(message).map_err(|error| {
                BridgeError::Invalid(format!("scene snapshot end is invalid: {error}"))
            })?;
        break end_message;
    };
    if end.protocol_version != session.protocol_version()
        || end.kind != "notification"
        || end.method != "snapshot.end"
        || end.params.snapshot_id != accepted.snapshot_id
        || end.params.domain != "scene_graph"
        || end.params.resource_revision != accepted.resource_revision
        || end.params.scene_graph_revision != accepted.scene_graph_revision
        || end.params.revisions != accepted.revisions
        || end.params.chunk_count != expected_index
        || end.params.checksum != sha256_hex(checksum_input.as_bytes())
    {
        return Err(BridgeError::Invalid(
            "scene snapshot end is invalid".to_owned(),
        ));
    }
    sink.end(&end.params)?;
    Ok(SceneSnapshotTransfer {
        accepted,
        end: end.params,
    })
}

pub(crate) async fn get_scene_snapshot(
    session: &mut Session,
) -> Result<SceneSnapshot, BridgeError> {
    let mut sink = CollectingSink::default();
    stream_scene_snapshot(session, &mut sink).await?;
    sink.finish()
}

fn validate_delta_batch(
    batch: &SceneDeltaBatch,
    requested_after: u64,
    current_scene_graph_revision: u64,
) -> Result<(), BridgeError> {
    if !batch.batch_id.starts_with("scene-batch:")
        || batch.batch_id.len() != 44
        || !batch.batch_id[12..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || batch.previous_scene_graph_revision != requested_after
        || requested_after
            .checked_add(1)
            .is_none_or(|next| batch.scene_graph_revision != next)
        || batch.scene_graph_revision > current_scene_graph_revision
        || batch.resource_revision > MAX_SAFE_REVISION
        || batch.project_revision > MAX_SAFE_REVISION
        || current_scene_graph_revision > MAX_SAFE_REVISION
        || batch.operations.is_empty()
        || batch.operations.len() > MAX_SCENES
        || !batch.source_complete
    {
        return Err(BridgeError::Invalid(
            "scene delta continuity is invalid".to_owned(),
        ));
    }
    let operations_json = canonical_serialized(&batch.operations)?;
    if sha256_hex(operations_json.as_bytes()) != batch.checksum
        || serde_json::to_vec(batch)?.len() > MAX_DELTA_BATCH_BYTES
    {
        return Err(BridgeError::Invalid(
            "scene delta checksum or size is invalid".to_owned(),
        ));
    }
    let accepted = SceneSnapshotAccepted {
        snapshot_id: "snapshot:00000000000000000000000000000000".to_owned(),
        domain: "scene_graph".to_owned(),
        resource_revision: batch.resource_revision,
        scene_graph_revision: batch.scene_graph_revision,
        revisions: SceneRevisionVector {
            editor_session_id: String::new(),
            event_seq: 0,
            project_revision: batch.project_revision,
            operation_seq: 0,
            resource_revision: batch.resource_revision,
            scene_graph_revision: batch.scene_graph_revision,
            script_graph_revision: None,
            scene_revisions: BTreeMap::new(),
        },
        limits_applied: SceneSnapshotLimits {
            scene_records: MAX_SCENES,
            scene_nodes: MAX_NODES,
            scene_properties: MAX_PROPERTIES,
            scene_relations: MAX_RELATIONS,
            instance_depth: 64,
            snapshot_chunk_bytes: MAX_SNAPSHOT_CHUNK_BYTES,
            snapshot_window_bytes: 32 * 1_024 * 1_024,
            snapshot_timeout_ms: 120_000,
        },
    };
    for operation in &batch.operations {
        match operation {
            SceneDeltaOperation::Upsert { value } => validate_scene(value, &accepted)?,
            SceneDeltaOperation::Remove { scene_ref, path } => {
                validate_resource_ref(scene_ref)?;
                validate_resource_path(path)?;
            }
            SceneDeltaOperation::ProjectContext { values } => {
                if values.len() > MAX_SCENES {
                    return Err(BridgeError::Invalid(
                        "scene project context delta is too large".to_owned(),
                    ));
                }
                for value in values {
                    validate_project_context(value, batch.scene_graph_revision)?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) async fn get_next_scene_delta(
    session: &mut Session,
    after_scene_graph_revision: u64,
) -> Result<SceneDeltaPoll, BridgeError> {
    require_scene_graph(session)?;
    let response = match session
        .request(
            "scene.delta.get",
            json!({"after_scene_graph_revision": after_scene_graph_revision}),
        )
        .await
    {
        Ok(response) => response,
        Err(BridgeError::Rpc {
            code,
            retryable,
            data,
            ..
        }) if code == "scene_journal_gap" && retryable => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct GapData {
                requested_after: u64,
                oldest_available: u64,
                current_scene_graph_revision: u64,
            }
            let gap: GapData = serde_json::from_value(data)?;
            if gap.requested_after != after_scene_graph_revision
                || gap.requested_after >= gap.oldest_available
                || gap.oldest_available > gap.current_scene_graph_revision
                || gap.current_scene_graph_revision > MAX_SAFE_REVISION
            {
                return Err(BridgeError::Invalid(
                    "scene journal gap metadata is invalid".to_owned(),
                ));
            }
            return Ok(SceneDeltaPoll::Gap {
                requested_after_scene_graph_revision: gap.requested_after,
                oldest_available_scene_graph_revision: gap.oldest_available,
                current_scene_graph_revision: gap.current_scene_graph_revision,
            });
        }
        Err(error) => return Err(error),
    };
    let result: SceneDeltaPoll = serde_json::from_value(
        response
            .get("result")
            .cloned()
            .ok_or_else(|| BridgeError::Invalid("scene delta result is missing".to_owned()))?,
    )?;
    match &result {
        SceneDeltaPoll::Current {
            current_scene_graph_revision,
        } if *current_scene_graph_revision == after_scene_graph_revision => {}
        SceneDeltaPoll::Batch {
            current_scene_graph_revision,
            batch,
        } => validate_delta_batch(
            batch,
            after_scene_graph_revision,
            *current_scene_graph_revision,
        )?,
        SceneDeltaPoll::Gap { .. } => {
            return Err(BridgeError::Invalid(
                "scene gap must use the RPC error envelope".to_owned(),
            ));
        }
        _ => {
            return Err(BridgeError::Invalid(
                "scene delta result is inconsistent".to_owned(),
            ));
        }
    }
    Ok(result)
}

fn require_scene_graph(session: &Session) -> Result<(), BridgeError> {
    if !matches!(session.protocol_version(), "1.3" | "1.4")
        || !session.capabilities().contains("scene.packed_state")
        || !session.capabilities().contains("scene.incremental_index")
        || !session.capabilities().contains("scene.project_context")
    {
        return Err(BridgeError::CapabilityUnavailable {
            capability: "scene.incremental_index",
            negotiated_version: session.protocol_version().to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scene_snapshot_fixtures_are_strict_and_checksum_valid() {
        let response: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/scene-snapshot-response.json"
        ))
        .unwrap();
        let begin: SceneSnapshotBeginMessage = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/scene-snapshot-begin.json"
        ))
        .unwrap();
        let mut chunk: SceneSnapshotChunk = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/scene-snapshot-chunk.json"
        ))
        .unwrap();
        let end: SceneSnapshotEndMessage = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/scene-snapshot-end.json"
        ))
        .unwrap();
        let accepted: SceneSnapshotAccepted =
            serde_json::from_value(response["result"].clone()).unwrap();
        assert_eq!(begin.params.scene_graph_revision, 1);
        chunk.payload_json = Some(canonical_serialized(&chunk.payload).unwrap());
        chunk.checksum = sha256_hex(chunk.payload_json.as_deref().unwrap().as_bytes());
        validate_snapshot_chunk(&chunk, &accepted, 0).unwrap();
        assert_eq!(end.params.chunk_count, 1);

        let mut invalid = chunk;
        invalid.payload.scenes[0].nodes[0].node_path = "/root/Child".to_owned();
        invalid.payload_json = Some(canonical_serialized(&invalid.payload).unwrap());
        invalid.checksum = sha256_hex(invalid.payload_json.as_deref().unwrap().as_bytes());
        assert!(validate_snapshot_chunk(&invalid, &accepted, 0).is_err());
    }

    #[test]
    fn scene_delta_checksum_and_continuity_are_strict() {
        let operations = vec![SceneDeltaOperation::ProjectContext { values: Vec::new() }];
        let operations_json = canonical_serialized(&operations).unwrap();
        let batch = SceneDeltaBatch {
            batch_id: "scene-batch:0123456789abcdef0123456789abcdef".to_owned(),
            previous_scene_graph_revision: 1,
            scene_graph_revision: 2,
            resource_revision: 3,
            project_revision: 4,
            checksum: sha256_hex(operations_json.as_bytes()),
            operations,
            source_complete: true,
        };
        validate_delta_batch(&batch, 1, 2).unwrap();

        let mut broken = batch;
        broken.previous_scene_graph_revision = 0;
        assert!(validate_delta_batch(&broken, 1, 2).is_err());
    }

    #[test]
    fn scene_paths_and_project_allowlist_reject_unsafe_values() {
        assert!(valid_node_path(".", false));
        assert!(valid_node_path("Root/Child", false));
        assert!(!valid_node_path("/root/Child", false));
        assert!(!valid_node_path("Root:position", false));
        assert!(valid_project_key("input/jump"));
        assert!(valid_project_key("layer_names/2d_physics/1"));
        assert!(!valid_project_key("input/editor/orbit"));
        assert!(!valid_project_key("rendering/renderer/rendering_method"));
    }
}
