use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::protocol::{BridgeError, Session};

const MAX_RESOURCE_RECORDS: usize = 250_000;
const MAX_RESOURCE_DEPENDENCIES: usize = 2_000_000;
const MAX_DEPENDENCIES_PER_RESOURCE: usize = 4_096;
const MAX_RESOURCE_PATH_BYTES: usize = 1_024;
const MAX_SNAPSHOT_CHUNK_BYTES: usize = 256 * 1_024;
const MAX_DELTA_BATCH_BYTES: usize = 512 * 1_024;
const MAX_SAFE_REVISION: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RpcContext {
    pub project_id: String,
    pub editor_session_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceRevisionVector {
    pub editor_session_id: String,
    pub event_seq: u64,
    pub project_revision: u64,
    pub operation_seq: u64,
    pub resource_revision: u64,
    pub scene_revisions: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ResourceRef {
    Uid(UidResourceRef),
    Path(PathResourceRef),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UidResourceRef {
    pub uid: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathResourceRef {
    pub uid_missing: bool,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceSourceKind {
    Source,
    ImportedSource,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceImportState {
    NotImported,
    Valid,
    Invalid,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceValidity {
    Valid,
    Partial,
    Invalid,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceObservation {
    pub resource_ref: ResourceRef,
    pub path: String,
    pub godot_type: String,
    pub source_kind: ResourceSourceKind,
    pub import_state: ResourceImportState,
    pub modified_time_unix_seconds: u64,
    pub byte_size: u64,
    pub validity: ResourceValidity,
    pub authority: String,
    pub resource_revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyResolution {
    Resolved,
    Missing,
    StaleUid,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyObservation {
    pub source_ref: ResourceRef,
    #[serde(default)]
    pub target_uid: Option<String>,
    pub fallback_path: String,
    #[serde(default)]
    pub resolved_path: Option<String>,
    #[serde(default)]
    pub declared_type: Option<String>,
    pub resolution: DependencyResolution,
    pub authority: String,
    pub resource_revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceDiagnosticCode {
    MissingDependency,
    StaleResourceUid,
    ResourceUidPathMismatch,
    DuplicateResourceUid,
    InvalidImport,
    ResourceLimitExceeded,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDiagnostic {
    pub code: ResourceDiagnosticCode,
    pub subject: ResourceRef,
    #[serde(default)]
    pub target_reference: Option<String>,
    pub resource_revision: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceWithDependencies {
    pub resource: ResourceObservation,
    pub dependencies: Vec<DependencyObservation>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceDeltaOperation {
    Upsert {
        value: ResourceWithDependencies,
    },
    Move {
        uid: String,
        from_path: String,
        to_path: String,
        value: ResourceWithDependencies,
    },
    Remove {
        resource_ref: ResourceRef,
        path: String,
    },
    Reimport {
        value: ResourceWithDependencies,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDeltaBatch {
    pub batch_id: String,
    pub previous_resource_revision: u64,
    pub resource_revision: u64,
    pub project_revision: u64,
    pub operations: Vec<ResourceDeltaOperation>,
    pub source_complete: bool,
    pub checksum: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourceDeltaPoll {
    Current {
        current_resource_revision: u64,
    },
    Batch {
        current_resource_revision: u64,
        batch: ResourceDeltaBatch,
    },
    Gap {
        requested_after_resource_revision: u64,
        oldest_available_resource_revision: u64,
        current_resource_revision: u64,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSnapshotLimits {
    pub resource_records: usize,
    pub resource_dependencies: usize,
    pub resource_dependencies_per_record: usize,
    pub resource_path_bytes: usize,
    pub snapshot_chunk_bytes: usize,
    pub snapshot_window_bytes: usize,
    pub snapshot_timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSnapshotAccepted {
    pub snapshot_id: String,
    pub domain: String,
    pub resource_revision: u64,
    pub revisions: ResourceRevisionVector,
    pub limits_applied: ResourceSnapshotLimits,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSnapshotBeginParams {
    pub snapshot_id: String,
    pub domain: String,
    pub resource_revision: u64,
    pub revisions: ResourceRevisionVector,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ResourceSnapshotBeginMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: ResourceSnapshotBeginParams,
    context: RpcContext,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSnapshotPayload {
    pub resources: Vec<ResourceObservation>,
    pub dependencies: Vec<DependencyObservation>,
    pub diagnostics: Vec<ResourceDiagnostic>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSnapshotChunk {
    pub protocol_version: String,
    pub kind: String,
    pub snapshot_id: String,
    pub domain: String,
    pub chunk_index: usize,
    pub payload: ResourceSnapshotPayload,
    pub payload_json: String,
    pub checksum: String,
    pub context: RpcContext,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceSnapshotEndParams {
    pub snapshot_id: String,
    pub domain: String,
    pub resource_revision: u64,
    pub chunk_count: usize,
    pub resource_count: usize,
    pub dependency_count: usize,
    pub diagnostic_count: usize,
    pub checksum: String,
    pub revisions: ResourceRevisionVector,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
struct ResourceSnapshotEndMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: ResourceSnapshotEndParams,
    context: RpcContext,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResourceSnapshotTransfer {
    pub accepted: ResourceSnapshotAccepted,
    pub end: ResourceSnapshotEndParams,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ResourceSnapshot {
    pub accepted: ResourceSnapshotAccepted,
    pub begin: ResourceSnapshotBeginParams,
    pub payload: ResourceSnapshotPayload,
    pub end: ResourceSnapshotEndParams,
}

pub trait ResourceSnapshotSink {
    fn begin(
        &mut self,
        accepted: &ResourceSnapshotAccepted,
        begin: &ResourceSnapshotBeginParams,
    ) -> Result<(), BridgeError>;

    fn chunk(&mut self, chunk: &ResourceSnapshotChunk) -> Result<(), BridgeError>;

    fn end(&mut self, end: &ResourceSnapshotEndParams) -> Result<(), BridgeError>;
}

#[derive(Default)]
struct CollectingSink {
    accepted: Option<ResourceSnapshotAccepted>,
    begin: Option<ResourceSnapshotBeginParams>,
    resources: Vec<ResourceObservation>,
    dependencies: Vec<DependencyObservation>,
    diagnostics: Vec<ResourceDiagnostic>,
    end: Option<ResourceSnapshotEndParams>,
}

impl ResourceSnapshotSink for CollectingSink {
    fn begin(
        &mut self,
        accepted: &ResourceSnapshotAccepted,
        begin: &ResourceSnapshotBeginParams,
    ) -> Result<(), BridgeError> {
        self.accepted = Some(accepted.clone());
        self.begin = Some(begin.clone());
        Ok(())
    }

    fn chunk(&mut self, chunk: &ResourceSnapshotChunk) -> Result<(), BridgeError> {
        self.resources.extend(chunk.payload.resources.clone());
        self.dependencies.extend(chunk.payload.dependencies.clone());
        self.diagnostics.extend(chunk.payload.diagnostics.clone());
        Ok(())
    }

    fn end(&mut self, end: &ResourceSnapshotEndParams) -> Result<(), BridgeError> {
        self.end = Some(end.clone());
        Ok(())
    }
}

impl CollectingSink {
    fn finish(self) -> Result<ResourceSnapshot, BridgeError> {
        Ok(ResourceSnapshot {
            accepted: self
                .accepted
                .ok_or_else(|| BridgeError::Invalid("snapshot acceptance is missing".to_owned()))?,
            begin: self
                .begin
                .ok_or_else(|| BridgeError::Invalid("snapshot begin is missing".to_owned()))?,
            payload: ResourceSnapshotPayload {
                resources: self.resources,
                dependencies: self.dependencies,
                diagnostics: self.diagnostics,
            },
            end: self
                .end
                .ok_or_else(|| BridgeError::Invalid("snapshot end is missing".to_owned()))?,
        })
    }
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
        || value.len() > MAX_RESOURCE_PATH_BYTES
        || value
            .bytes()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'\\' | b'?' | b'#' | b'\0'))
        || value[6..]
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
    {
        return Err(BridgeError::Invalid("resource path is invalid".to_owned()));
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
            "resource reference is invalid".to_owned(),
        )),
    }
}

fn validate_resource_value(
    value: &ResourceWithDependencies,
    expected_revision: u64,
) -> Result<(), BridgeError> {
    validate_resource_ref(&value.resource.resource_ref)?;
    validate_resource_path(&value.resource.path)?;
    if value.resource.authority != "editor_file_system"
        || value.resource.godot_type.is_empty()
        || value.resource.godot_type.len() > 256
        || value.resource.resource_revision != expected_revision
        || value.resource.resource_revision > MAX_SAFE_REVISION
        || value.dependencies.len() > MAX_DEPENDENCIES_PER_RESOURCE
    {
        return Err(BridgeError::Invalid(
            "resource observation is invalid".to_owned(),
        ));
    }
    for dependency in &value.dependencies {
        validate_resource_ref(&dependency.source_ref)?;
        validate_resource_path(&dependency.fallback_path)?;
        if let Some(path) = &dependency.resolved_path {
            validate_resource_path(path)?;
        }
        if dependency
            .target_uid
            .as_deref()
            .is_some_and(|uid| !valid_uid(uid))
            || dependency.authority != "resource_loader_dependencies"
            || dependency.resource_revision != expected_revision
            || dependency.resource_revision > MAX_SAFE_REVISION
            || dependency
                .declared_type
                .as_ref()
                .is_some_and(|kind| kind.len() > 256)
        {
            return Err(BridgeError::Invalid(
                "dependency observation is invalid".to_owned(),
            ));
        }
        if dependency.source_ref != value.resource.resource_ref {
            return Err(BridgeError::Invalid(
                "dependency source does not match its resource".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_snapshot_limits(limits: &ResourceSnapshotLimits) -> Result<(), BridgeError> {
    if limits.resource_records != MAX_RESOURCE_RECORDS
        || limits.resource_dependencies != MAX_RESOURCE_DEPENDENCIES
        || limits.resource_dependencies_per_record != MAX_DEPENDENCIES_PER_RESOURCE
        || limits.resource_path_bytes != MAX_RESOURCE_PATH_BYTES
        || limits.snapshot_chunk_bytes != MAX_SNAPSHOT_CHUNK_BYTES
        || limits.snapshot_window_bytes != 32 * 1_024 * 1_024
        || limits.snapshot_timeout_ms != 120_000
    {
        return Err(BridgeError::Invalid(
            "resource snapshot limits are invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_snapshot_chunk(
    chunk: &ResourceSnapshotChunk,
    accepted: &ResourceSnapshotAccepted,
    expected_index: usize,
) -> Result<(), BridgeError> {
    if chunk.protocol_version != "1.2"
        || chunk.kind != "chunk"
        || chunk.domain != "resource_graph"
        || chunk.snapshot_id != accepted.snapshot_id
        || chunk.chunk_index != expected_index
        || chunk.payload_json.len() > MAX_SNAPSHOT_CHUNK_BYTES
        || sha256_hex(chunk.payload_json.as_bytes()) != chunk.checksum
    {
        return Err(BridgeError::Invalid(
            "resource snapshot chunk envelope is invalid".to_owned(),
        ));
    }
    let payload_from_json: ResourceSnapshotPayload = serde_json::from_str(&chunk.payload_json)?;
    if payload_from_json != chunk.payload {
        return Err(BridgeError::Invalid(
            "resource snapshot payload_json differs from payload".to_owned(),
        ));
    }
    if chunk.payload.resources.len() > MAX_RESOURCE_RECORDS
        || chunk.payload.dependencies.len() > MAX_RESOURCE_DEPENDENCIES
    {
        return Err(BridgeError::Invalid(
            "resource snapshot chunk count is invalid".to_owned(),
        ));
    }
    for resource in &chunk.payload.resources {
        validate_resource_value(
            &ResourceWithDependencies {
                resource: resource.clone(),
                dependencies: Vec::new(),
            },
            accepted.resource_revision,
        )?;
    }
    for dependency in &chunk.payload.dependencies {
        validate_resource_ref(&dependency.source_ref)?;
        validate_resource_path(&dependency.fallback_path)?;
        if dependency.authority != "resource_loader_dependencies"
            || dependency.resource_revision != accepted.resource_revision
        {
            return Err(BridgeError::Invalid(
                "resource snapshot dependency is invalid".to_owned(),
            ));
        }
    }
    for diagnostic in &chunk.payload.diagnostics {
        validate_resource_ref(&diagnostic.subject)?;
        if diagnostic.resource_revision != accepted.resource_revision
            || diagnostic
                .target_reference
                .as_ref()
                .is_some_and(|target| target.len() > MAX_RESOURCE_PATH_BYTES)
        {
            return Err(BridgeError::Invalid(
                "resource snapshot diagnostic is invalid".to_owned(),
            ));
        }
    }
    Ok(())
}

pub(crate) async fn stream_resource_snapshot<S: ResourceSnapshotSink>(
    session: &mut Session,
    sink: &mut S,
) -> Result<ResourceSnapshotTransfer, BridgeError> {
    require_resource_graph(session)?;
    let response = session.request("resource.snapshot.get", json!({})).await?;
    let request_id = response
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::Invalid("resource snapshot request ID is missing".to_owned()))?
        .to_owned();
    let result = receive_resource_snapshot(session, sink, &response).await;
    if result.is_err() {
        let _ = session
            .send_cancel(&request_id, "resource snapshot validation failed")
            .await;
    }
    result
}

async fn receive_resource_snapshot<S: ResourceSnapshotSink>(
    session: &mut Session,
    sink: &mut S,
    response: &Value,
) -> Result<ResourceSnapshotTransfer, BridgeError> {
    let accepted: ResourceSnapshotAccepted =
        serde_json::from_value(response.get("result").cloned().ok_or_else(|| {
            BridgeError::Invalid("resource snapshot result is missing".to_owned())
        })?)?;
    if accepted.domain != "resource_graph"
        || accepted.revisions.resource_revision != accepted.resource_revision
        || accepted.resource_revision > MAX_SAFE_REVISION
        || !accepted.snapshot_id.starts_with("snapshot:")
    {
        return Err(BridgeError::Invalid(
            "resource snapshot acceptance is invalid".to_owned(),
        ));
    }
    validate_snapshot_limits(&accepted.limits_applied)?;

    let begin_message: ResourceSnapshotBeginMessage =
        serde_json::from_value(session.receive_non_sync_timed().await?)?;
    if begin_message.protocol_version != "1.2"
        || begin_message.kind != "notification"
        || begin_message.method != "snapshot.begin"
        || begin_message.params.snapshot_id != accepted.snapshot_id
        || begin_message.params.domain != "resource_graph"
        || begin_message.params.resource_revision != accepted.resource_revision
        || begin_message.params.revisions != accepted.revisions
    {
        return Err(BridgeError::Invalid(
            "resource snapshot begin is invalid".to_owned(),
        ));
    }
    sink.begin(&accepted, &begin_message.params)?;

    let mut expected_index = 0_usize;
    let mut checksum_input = String::new();
    let mut resource_count = 0_usize;
    let mut dependency_count = 0_usize;
    let mut diagnostic_count = 0_usize;
    let end = loop {
        let message = session.receive_non_sync_timed().await?;
        if message.get("kind").and_then(Value::as_str) == Some("chunk") {
            let chunk: ResourceSnapshotChunk = serde_json::from_value(message)?;
            validate_snapshot_chunk(&chunk, &accepted, expected_index)?;
            resource_count = resource_count
                .checked_add(chunk.payload.resources.len())
                .ok_or_else(|| BridgeError::Invalid("resource count overflow".to_owned()))?;
            dependency_count = dependency_count
                .checked_add(chunk.payload.dependencies.len())
                .ok_or_else(|| BridgeError::Invalid("dependency count overflow".to_owned()))?;
            diagnostic_count = diagnostic_count
                .checked_add(chunk.payload.diagnostics.len())
                .ok_or_else(|| BridgeError::Invalid("diagnostic count overflow".to_owned()))?;
            if resource_count > MAX_RESOURCE_RECORDS
                || dependency_count > MAX_RESOURCE_DEPENDENCIES
                || diagnostic_count > MAX_RESOURCE_DEPENDENCIES
            {
                return Err(BridgeError::Invalid(
                    "resource snapshot total count is invalid".to_owned(),
                ));
            }
            checksum_input.push_str(&chunk.checksum);
            sink.chunk(&chunk)?;
            session
                .send_ack(
                    &accepted.snapshot_id,
                    Some("resource_graph"),
                    expected_index,
                )
                .await?;
            expected_index += 1;
            continue;
        }
        let end_message: ResourceSnapshotEndMessage = serde_json::from_value(message)?;
        break end_message;
    };
    if end.protocol_version != "1.2"
        || end.kind != "notification"
        || end.method != "snapshot.end"
        || end.params.snapshot_id != accepted.snapshot_id
        || end.params.domain != "resource_graph"
        || end.params.resource_revision != accepted.resource_revision
        || end.params.revisions != accepted.revisions
        || end.params.chunk_count != expected_index
        || end.params.resource_count != resource_count
        || end.params.dependency_count != dependency_count
        || end.params.diagnostic_count != diagnostic_count
        || end.params.checksum != sha256_hex(checksum_input.as_bytes())
    {
        return Err(BridgeError::Invalid(
            "resource snapshot end is invalid".to_owned(),
        ));
    }
    sink.end(&end.params)?;
    Ok(ResourceSnapshotTransfer {
        accepted,
        end: end.params,
    })
}

pub(crate) async fn get_resource_snapshot(
    session: &mut Session,
) -> Result<ResourceSnapshot, BridgeError> {
    let mut sink = CollectingSink::default();
    stream_resource_snapshot(session, &mut sink).await?;
    sink.finish()
}

fn validate_delta_batch(
    batch: &ResourceDeltaBatch,
    requested_after: u64,
    current_resource_revision: u64,
) -> Result<(), BridgeError> {
    if !batch.batch_id.starts_with("resource-batch:")
        || batch.batch_id.len() != 47
        || !batch.batch_id[15..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || batch.previous_resource_revision != requested_after
        || requested_after
            .checked_add(1)
            .is_none_or(|next| batch.resource_revision != next)
        || batch.resource_revision > current_resource_revision
        || batch.project_revision > MAX_SAFE_REVISION
        || current_resource_revision > MAX_SAFE_REVISION
        || batch.operations.is_empty()
        || batch.operations.len() > MAX_RESOURCE_RECORDS
        || !batch.source_complete
    {
        return Err(BridgeError::Invalid(
            "resource delta continuity is invalid".to_owned(),
        ));
    }
    let operations_json = canonical_serialized(&batch.operations)?;
    if sha256_hex(operations_json.as_bytes()) != batch.checksum
        || serde_json::to_vec(batch)?.len() > MAX_DELTA_BATCH_BYTES
    {
        return Err(BridgeError::Invalid(
            "resource delta checksum or size is invalid".to_owned(),
        ));
    }
    for operation in &batch.operations {
        match operation {
            ResourceDeltaOperation::Upsert { value }
            | ResourceDeltaOperation::Reimport { value } => {
                validate_resource_value(value, batch.resource_revision)?;
            }
            ResourceDeltaOperation::Move {
                uid,
                from_path,
                to_path,
                value,
            } => {
                if !valid_uid(uid) {
                    return Err(BridgeError::Invalid("move UID is invalid".to_owned()));
                }
                validate_resource_path(from_path)?;
                validate_resource_path(to_path)?;
                validate_resource_value(value, batch.resource_revision)?;
                if !matches!(&value.resource.resource_ref, ResourceRef::Uid(reference) if reference.uid == *uid)
                    || value.resource.path != *to_path
                {
                    return Err(BridgeError::Invalid(
                        "move identity is inconsistent".to_owned(),
                    ));
                }
            }
            ResourceDeltaOperation::Remove { resource_ref, path } => {
                validate_resource_ref(resource_ref)?;
                validate_resource_path(path)?;
            }
        }
    }
    Ok(())
}

pub(crate) async fn get_next_resource_delta(
    session: &mut Session,
    after_resource_revision: u64,
) -> Result<ResourceDeltaPoll, BridgeError> {
    require_resource_graph(session)?;
    let response = match session
        .request(
            "resource.delta.get",
            json!({"after_resource_revision": after_resource_revision}),
        )
        .await
    {
        Ok(response) => response,
        Err(BridgeError::Rpc {
            code,
            retryable,
            data,
            ..
        }) if code == "resource_journal_gap" && retryable => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct GapData {
                requested_after: u64,
                oldest_available: u64,
                current_resource_revision: u64,
            }
            let gap: GapData = serde_json::from_value(data)?;
            if gap.requested_after != after_resource_revision
                || gap.requested_after >= gap.oldest_available
                || gap.oldest_available > gap.current_resource_revision
                || gap.current_resource_revision > MAX_SAFE_REVISION
            {
                return Err(BridgeError::Invalid(
                    "resource journal gap metadata is invalid".to_owned(),
                ));
            }
            return Ok(ResourceDeltaPoll::Gap {
                requested_after_resource_revision: gap.requested_after,
                oldest_available_resource_revision: gap.oldest_available,
                current_resource_revision: gap.current_resource_revision,
            });
        }
        Err(error) => return Err(error),
    };
    let result: ResourceDeltaPoll = serde_json::from_value(
        response
            .get("result")
            .cloned()
            .ok_or_else(|| BridgeError::Invalid("resource delta result is missing".to_owned()))?,
    )?;
    match &result {
        ResourceDeltaPoll::Current {
            current_resource_revision,
        } if *current_resource_revision == after_resource_revision => {}
        ResourceDeltaPoll::Batch {
            current_resource_revision,
            batch,
        } => validate_delta_batch(batch, after_resource_revision, *current_resource_revision)?,
        ResourceDeltaPoll::Gap { .. } => {
            return Err(BridgeError::Invalid(
                "resource gap must use the RPC error envelope".to_owned(),
            ));
        }
        _ => {
            return Err(BridgeError::Invalid(
                "resource delta result is inconsistent".to_owned(),
            ));
        }
    }
    Ok(result)
}

fn require_resource_graph(session: &Session) -> Result<(), BridgeError> {
    if session.protocol_version() != "1.2"
        || !session.capabilities().contains("resource.uid_dependencies")
        || !session
            .capabilities()
            .contains("resource.incremental_index")
    {
        return Err(BridgeError::CapabilityUnavailable {
            capability: "resource.incremental_index",
            negotiated_version: session.protocol_version().to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_snapshot_fixtures_are_strict_and_checksum_valid() {
        let begin: ResourceSnapshotBeginMessage = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/resource-snapshot-begin.json"
        ))
        .unwrap();
        let chunk: ResourceSnapshotChunk = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/resource-snapshot-chunk.json"
        ))
        .unwrap();
        let end: ResourceSnapshotEndMessage = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/resource-snapshot-end.json"
        ))
        .unwrap();
        assert_eq!(begin.params.resource_revision, 1);
        assert_eq!(sha256_hex(chunk.payload_json.as_bytes()), chunk.checksum);
        assert_eq!(end.params.chunk_count, 1);

        let unknown: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/invalid/resource-delta-unknown-field.json"
        ))
        .unwrap();
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct DeltaParams {
            #[allow(dead_code)]
            after_resource_revision: u64,
        }
        assert!(serde_json::from_value::<DeltaParams>(unknown["params"].clone()).is_err());
    }

    #[test]
    fn resource_delta_fixture_validates_continuity_and_all_operation_variants() {
        let response: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/resource-delta-batch-response.json"
        ))
        .unwrap();
        let result: ResourceDeltaPoll = serde_json::from_value(response["result"].clone()).unwrap();
        let ResourceDeltaPoll::Batch {
            current_resource_revision,
            batch,
        } = result
        else {
            panic!("fixture must contain a batch");
        };
        assert_eq!(batch.operations.len(), 4);
        validate_delta_batch(&batch, 1, current_resource_revision).unwrap();
    }

    #[test]
    fn resource_identity_and_path_validation_rejects_unsafe_values() {
        for invalid in [
            "res://../escape.tres",
            "res://folder/./item.tres",
            "res://folder\\item.tres",
            "res://folder/item.tres?query",
            "user://item.tres",
        ] {
            assert!(validate_resource_path(invalid).is_err(), "{invalid}");
        }
        assert!(valid_uid("uid://abc_DEF-123"));
        assert!(!valid_uid("uid://"));
        assert!(!valid_uid("uid://bad/value"));
    }
}
