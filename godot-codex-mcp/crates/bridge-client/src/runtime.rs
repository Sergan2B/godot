use std::collections::BTreeMap;
use std::time::Duration;

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::protocol::{BridgeError, Session};
use crate::resource::RpcContext;

const MAX_SAFE_REVISION: u64 = 9_007_199_254_740_991;
const MAX_RUNTIME_ENTITIES: usize = 10_265;
const MAX_RUNTIME_CHUNKS: usize = 65_536;
const MAX_RUNTIME_CHUNK_BYTES: usize = 512 * 1_024;
const MAX_SCREENSHOT_BYTES: usize = 512 * 1_024;
const RUNTIME_START_DEADLINE: Duration = Duration::from_secs(10);
const RUNTIME_START_RESPONSE_TIMEOUT: Duration = Duration::from_secs(11);
const RUNTIME_CONTROL_DEADLINE: Duration = Duration::from_secs(3);
const RUNTIME_STOP_DEADLINE: Duration = Duration::from_secs(5);
const RUNTIME_OBSERVATION_DEADLINE: Duration = Duration::from_secs(3);
const RUNTIME_RESPONSE_TIMEOUT: Duration = Duration::from_secs(4);
const RUNTIME_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTarget {
    Project,
    CurrentScene,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Starting,
    Running,
    Paused,
    Stopping,
    Disconnected,
    Stopped,
    Crashed,
    Failed,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeOrigin {
    Editor,
    Mcp,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDomain {
    RuntimeState,
    RuntimeTree,
    RuntimeDiagnostics,
    RuntimeStacks,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEventType {
    SessionStarted,
    DebuggerConnected,
    Paused,
    Continued,
    Stopping,
    TreeChanged,
    DiagnosticAdded,
    StackChanged,
    ObjectObserved,
    ViewportCaptured,
    Disconnected,
    Stopped,
    Crashed,
    Failed,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeInvalidationReason {
    JournalGap,
    JournalOverflow,
    SessionReplaced,
    SnapshotCancelled,
    RecoveryFailed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeRevisionVector {
    pub editor_session_id: String,
    pub event_seq: u64,
    pub project_revision: u64,
    pub operation_seq: u64,
    pub resource_revision: u64,
    pub scene_graph_revision: u64,
    pub script_graph_revision: u64,
    pub scene_revisions: BTreeMap<String, u64>,
    pub runtime_session_id: String,
    pub runtime_event_seq: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEvent {
    pub runtime_session_id: String,
    pub runtime_event_seq: u64,
    pub event_type: RuntimeEventType,
    pub state: RuntimeState,
    pub changed_domains: Vec<RuntimeDomain>,
    pub revisions: RuntimeRevisionVector,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInvalidated {
    pub runtime_session_id: String,
    pub last_contiguous_runtime_event_seq: u64,
    pub reason: RuntimeInvalidationReason,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum RuntimeNotification {
    Event(RuntimeEvent),
    Invalidated(RuntimeInvalidated),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeEventMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: RuntimeEvent,
    context: RpcContext,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeInvalidatedMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: RuntimeInvalidated,
    context: RpcContext,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeLimits {
    pub tree_nodes: usize,
    pub tree_depth: usize,
    pub snapshot_bytes: usize,
    pub snapshot_chunk_bytes: usize,
    pub snapshot_window_bytes: usize,
    pub snapshot_timeout_ms: u64,
    pub properties: usize,
    pub variant_depth: usize,
    pub container_items: usize,
    pub string_characters: usize,
    pub projected_value_bytes: usize,
    pub object_bytes: usize,
    pub diagnostics: usize,
    pub diagnostic_message_bytes: usize,
    pub diagnostics_bytes: usize,
    pub stacks: usize,
    pub stack_frames: usize,
    pub stacks_bytes: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStateResult {
    pub schema_version: String,
    pub project_id: String,
    pub editor_session_id: String,
    pub runtime_session_id: String,
    pub runtime_event_seq: u64,
    pub state: RuntimeState,
    pub origin: RuntimeOrigin,
    pub target: RuntimeTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSourceHint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_node_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_scene_paths: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeVisibility {
    pub available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visible_in_tree: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeNode {
    pub kind: String,
    pub entity_id: String,
    pub runtime_object_id: String,
    pub parent_runtime_object_id: Option<String>,
    pub name: String,
    pub godot_type: String,
    pub runtime_node_path: String,
    pub depth: usize,
    pub child_count: usize,
    pub visibility: RuntimeVisibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hint: Option<RuntimeSourceHint>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStateEntity {
    pub kind: String,
    pub entity_id: String,
    pub runtime_session_id: String,
    pub runtime_event_seq: u64,
    pub state: RuntimeState,
    pub origin: RuntimeOrigin,
    pub target: RuntimeTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_stack_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeStackKind {
    Diagnostic,
    Pause,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStackFrame {
    pub frame: usize,
    pub script_path: Option<String>,
    pub function: String,
    pub line: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStack {
    pub kind: String,
    pub entity_id: String,
    pub runtime_stack_id: String,
    pub stack_kind: RuntimeStackKind,
    pub frames: Vec<RuntimeStackFrame>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDiagnosticSource {
    Engine,
    Script,
    Output,
    Bridge,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDiagnostic {
    pub kind: String,
    pub entity_id: String,
    pub runtime_diagnostic_id: String,
    pub severity: RuntimeDiagnosticSeverity,
    pub source: RuntimeDiagnosticSource,
    pub message: String,
    pub repeat_count: u64,
    pub runtime_event_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_stack_id: Option<String>,
    pub redacted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum RuntimeEntity {
    RuntimeState(RuntimeStateEntity),
    RuntimeNode(RuntimeNode),
    RuntimeDiagnostic(RuntimeDiagnostic),
    RuntimeStack(RuntimeStack),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProjectedValue {
    #[serde(rename = "type")]
    pub variant_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_object_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProperty {
    pub name: String,
    pub variant_type: String,
    pub read_only: bool,
    pub usage: u32,
    pub value: RuntimeProjectedValue,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeObjectResult {
    pub schema_version: String,
    pub runtime_session_id: String,
    pub runtime_event_seq: u64,
    pub state: RuntimeState,
    pub runtime_object_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub godot_type: Option<String>,
    pub properties: Vec<RuntimeProperty>,
    pub limits_applied: RuntimeLimits,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStackResult {
    pub schema_version: String,
    pub runtime_session_id: String,
    pub runtime_event_seq: u64,
    pub state: RuntimeState,
    pub stack: RuntimeStack,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeViewportCapture {
    pub schema_version: String,
    pub runtime_session_id: String,
    pub runtime_event_seq: u64,
    pub state: RuntimeState,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub byte_length: usize,
    pub sha256: String,
    pub data_base64url: String,
}

impl RuntimeViewportCapture {
    pub fn decode_png(&self) -> Result<Vec<u8>, BridgeError> {
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&self.data_base64url)
            .map_err(|_| {
                BridgeError::Invalid("runtime screenshot encoding is invalid".to_owned())
            })?;
        if bytes.len() != self.byte_length
            || bytes.is_empty()
            || bytes.len() > MAX_SCREENSHOT_BYTES
            || Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                != self.sha256
            || !bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            return Err(BridgeError::Invalid(
                "runtime screenshot payload is invalid".to_owned(),
            ));
        }
        Ok(bytes)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshotAccepted {
    pub snapshot_id: String,
    pub runtime_session_id: String,
    pub runtime_event_seq: u64,
    pub state: RuntimeState,
    pub revisions: RuntimeRevisionVector,
    pub domains: Vec<RuntimeDomain>,
    pub limits_applied: RuntimeLimits,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSnapshotBeginParams {
    snapshot_id: String,
    domain: String,
    runtime_session_id: String,
    runtime_event_seq: u64,
    revisions: RuntimeRevisionVector,
    chunk_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSnapshotBeginMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: RuntimeSnapshotBeginParams,
    context: RpcContext,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSnapshotPayload {
    domain: String,
    runtime_session_id: String,
    runtime_event_seq: u64,
    entities: Vec<RuntimeEntity>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSnapshotChunk {
    protocol_version: String,
    kind: String,
    snapshot_id: String,
    domain: String,
    chunk_index: usize,
    payload: RuntimeSnapshotPayload,
    payload_json: String,
    checksum: String,
    context: RpcContext,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSnapshotEnd {
    pub snapshot_id: String,
    pub domain: String,
    pub runtime_session_id: String,
    pub runtime_event_seq: u64,
    pub chunk_count: usize,
    pub entity_count: usize,
    pub checksum: String,
    pub revisions: RuntimeRevisionVector,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSnapshotEndMessage {
    protocol_version: String,
    kind: String,
    method: String,
    params: RuntimeSnapshotEnd,
    context: RpcContext,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeSnapshot {
    pub accepted: RuntimeSnapshotAccepted,
    pub entities: Vec<RuntimeEntity>,
    pub end: RuntimeSnapshotEnd,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_prefixed_hex(value: &str, prefix: &str, digits: usize) -> bool {
    value.strip_prefix(prefix).is_some_and(|tail| {
        tail.len() == digits
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_opaque(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|tail| {
        tail.len() == 43
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    })
}

pub(crate) fn parse_runtime_notification(
    session: &Session,
    value: Value,
) -> Result<RuntimeNotification, BridgeError> {
    match value.get("method").and_then(Value::as_str) {
        Some("runtime.event") => {
            let message: RuntimeEventMessage = serde_json::from_value(value).map_err(|error| {
                BridgeError::Invalid(format!("runtime.event is invalid: {error}"))
            })?;
            let event = message.params;
            let mut domains = event.changed_domains.clone();
            domains.sort_by_key(|domain| *domain as u8);
            domains.dedup();
            if message.protocol_version != "1.6"
                || message.kind != "notification"
                || message.method != "runtime.event"
                || message.context.project_id != session.project_id()
                || message.context.editor_session_id != session.editor_session_id()
                || !valid_prefixed_hex(&event.runtime_session_id, "runtime:", 32)
                || event.runtime_event_seq == 0
                || event.runtime_event_seq > MAX_SAFE_REVISION
                || event.changed_domains.is_empty()
                || event.changed_domains.len() > 4
                || domains.len() != event.changed_domains.len()
                || event.revisions.runtime_session_id != event.runtime_session_id
                || event.revisions.runtime_event_seq != event.runtime_event_seq
            {
                return Err(BridgeError::Invalid("runtime.event is invalid".to_owned()));
            }
            Ok(RuntimeNotification::Event(event))
        }
        Some("runtime.invalidated") => {
            let message: RuntimeInvalidatedMessage =
                serde_json::from_value(value).map_err(|error| {
                    BridgeError::Invalid(format!("runtime.invalidated is invalid: {error}"))
                })?;
            let invalidated = message.params;
            if message.protocol_version != "1.6"
                || message.kind != "notification"
                || message.method != "runtime.invalidated"
                || message.context.project_id != session.project_id()
                || message.context.editor_session_id != session.editor_session_id()
                || !valid_prefixed_hex(&invalidated.runtime_session_id, "runtime:", 32)
                || invalidated.last_contiguous_runtime_event_seq > MAX_SAFE_REVISION
            {
                return Err(BridgeError::Invalid(
                    "runtime.invalidated is invalid".to_owned(),
                ));
            }
            Ok(RuntimeNotification::Invalidated(invalidated))
        }
        _ => Err(BridgeError::Invalid(
            "message is not a runtime notification".to_owned(),
        )),
    }
}

fn valid_res_path(value: &str) -> bool {
    value.starts_with("res://")
        && value.chars().count() <= 1_024
        && !value.replace('\\', "/").split('/').any(|part| part == "..")
}

fn require_runtime(session: &Session) -> Result<(), BridgeError> {
    const CAPABILITIES: [&str; 6] = [
        "runtime.debugger",
        "runtime.process_control",
        "runtime.remote_tree",
        "runtime.bounded_properties",
        "runtime.diagnostics",
        "runtime.viewport_capture",
    ];
    if session.protocol_version() != "1.6"
        || CAPABILITIES
            .iter()
            .any(|capability| !session.capabilities().contains(*capability))
    {
        return Err(BridgeError::CapabilityUnavailable {
            capability: "runtime.debugger",
            negotiated_version: session.protocol_version().to_owned(),
        });
    }
    Ok(())
}

fn validate_limits(limits: &RuntimeLimits) -> Result<(), BridgeError> {
    if limits.tree_nodes != 10_000
        || limits.tree_depth != 256
        || limits.snapshot_bytes != 16 * 1_024 * 1_024
        || limits.snapshot_chunk_bytes != MAX_RUNTIME_CHUNK_BYTES
        || limits.snapshot_window_bytes != 32 * 1_024 * 1_024
        || limits.snapshot_timeout_ms != 10_000
        || limits.properties != 512
        || limits.variant_depth != 8
        || limits.container_items != 1_000
        || limits.string_characters != 16_384
        || limits.projected_value_bytes != 65_536
        || limits.object_bytes != 262_144
        || limits.diagnostics != 200
        || limits.diagnostic_message_bytes != 16_384
        || limits.diagnostics_bytes != 262_144
        || limits.stacks != 64
        || limits.stack_frames != 128
        || limits.stacks_bytes != 262_144
    {
        return Err(BridgeError::Invalid(
            "runtime limits are invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_coordinates(
    session_id: &str,
    event_seq: u64,
    expected_session_id: &str,
) -> Result<(), BridgeError> {
    if !valid_prefixed_hex(session_id, "runtime:", 32)
        || session_id != expected_session_id
        || event_seq == 0
        || event_seq > MAX_SAFE_REVISION
    {
        return Err(BridgeError::Invalid(
            "runtime coordinates are invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_state_result(
    session: &Session,
    value: RuntimeStateResult,
) -> Result<RuntimeStateResult, BridgeError> {
    if value.schema_version != "runtime/1.0"
        || value.project_id != session.project_id()
        || value.editor_session_id != session.editor_session_id()
        || !valid_prefixed_hex(&value.runtime_session_id, "runtime:", 32)
        || value.runtime_event_seq == 0
        || value.runtime_event_seq > MAX_SAFE_REVISION
        || value
            .scene_path
            .as_deref()
            .is_some_and(|path| !valid_res_path(path))
    {
        return Err(BridgeError::Invalid("runtime state is invalid".to_owned()));
    }
    Ok(value)
}

fn result<T: for<'de> Deserialize<'de>>(response: &Value, label: &str) -> Result<T, BridgeError> {
    serde_json::from_value(
        response
            .get("result")
            .cloned()
            .ok_or_else(|| BridgeError::Invalid(format!("{label} result is missing")))?,
    )
    .map_err(|error| BridgeError::Invalid(format!("{label} result is invalid: {error}")))
}

fn guarded_params(runtime_session_id: &str, expected_runtime_event_seq: Option<u64>) -> Value {
    let mut params = json!({"runtime_session_id": runtime_session_id});
    if let Some(expected) = expected_runtime_event_seq {
        params["expected_runtime_event_seq"] = json!(expected);
    }
    params
}

pub(crate) async fn run_runtime(
    session: &mut Session,
    target: RuntimeTarget,
) -> Result<RuntimeStateResult, BridgeError> {
    require_runtime(session)?;
    let response = session
        .request_with_deadline_and_timeout(
            "runtime.run",
            json!({"target": target}),
            RUNTIME_START_DEADLINE,
            RUNTIME_START_RESPONSE_TIMEOUT,
        )
        .await?;
    let value = result(&response, "runtime run")?;
    validate_state_result(session, value)
}

async fn control_runtime(
    session: &mut Session,
    method: &str,
    runtime_session_id: &str,
    expected_runtime_event_seq: Option<u64>,
) -> Result<RuntimeStateResult, BridgeError> {
    require_runtime(session)?;
    if !valid_prefixed_hex(runtime_session_id, "runtime:", 32) {
        return Err(BridgeError::Invalid(
            "runtime session ID is invalid".to_owned(),
        ));
    }
    let deadline = if method == "runtime.stop" {
        RUNTIME_STOP_DEADLINE
    } else {
        RUNTIME_CONTROL_DEADLINE
    };
    let response = session
        .request_with_deadline_and_timeout(
            method,
            guarded_params(runtime_session_id, expected_runtime_event_seq),
            deadline,
            deadline + Duration::from_secs(1),
        )
        .await?;
    let value = result(&response, "runtime control")?;
    validate_state_result(session, value)
}

pub(crate) async fn stop_runtime(
    session: &mut Session,
    runtime_session_id: &str,
    expected: Option<u64>,
) -> Result<RuntimeStateResult, BridgeError> {
    control_runtime(session, "runtime.stop", runtime_session_id, expected).await
}

pub(crate) async fn pause_runtime(
    session: &mut Session,
    runtime_session_id: &str,
    expected: Option<u64>,
) -> Result<RuntimeStateResult, BridgeError> {
    control_runtime(session, "runtime.pause", runtime_session_id, expected).await
}

pub(crate) async fn continue_runtime(
    session: &mut Session,
    runtime_session_id: &str,
    expected: Option<u64>,
) -> Result<RuntimeStateResult, BridgeError> {
    control_runtime(session, "runtime.continue", runtime_session_id, expected).await
}

pub(crate) async fn get_runtime_snapshot(
    session: &mut Session,
    runtime_session_id: &str,
    expected_runtime_event_seq: Option<u64>,
    domains: Option<Vec<RuntimeDomain>>,
) -> Result<RuntimeSnapshot, BridgeError> {
    require_runtime(session)?;
    if !valid_prefixed_hex(runtime_session_id, "runtime:", 32) {
        return Err(BridgeError::Invalid(
            "runtime session ID is invalid".to_owned(),
        ));
    }
    let mut params = guarded_params(runtime_session_id, expected_runtime_event_seq);
    if let Some(domains) = domains {
        params["domains"] = json!(domains);
    }
    let response = session
        .request_with_deadline_and_timeout(
            "runtime.snapshot.get",
            params,
            RUNTIME_OBSERVATION_DEADLINE,
            RUNTIME_RESPONSE_TIMEOUT,
        )
        .await?;
    let request_id = response
        .get("request_id")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::Invalid("runtime snapshot request ID is missing".to_owned()))?
        .to_owned();
    let snapshot = receive_runtime_snapshot(session, runtime_session_id, &response).await;
    if snapshot.is_err() {
        let _ = session
            .send_cancel(&request_id, "runtime snapshot validation failed")
            .await;
    }
    snapshot
}

async fn receive_runtime_snapshot(
    session: &mut Session,
    runtime_session_id: &str,
    response: &Value,
) -> Result<RuntimeSnapshot, BridgeError> {
    let accepted: RuntimeSnapshotAccepted = result(response, "runtime snapshot")?;
    validate_coordinates(
        &accepted.runtime_session_id,
        accepted.runtime_event_seq,
        runtime_session_id,
    )?;
    validate_limits(&accepted.limits_applied)?;
    if accepted.snapshot_id.len() != 41
        || !valid_prefixed_hex(&accepted.snapshot_id, "snapshot:", 32)
        || accepted.domains.is_empty()
        || accepted.domains.len() > 4
        || accepted.revisions.runtime_session_id != accepted.runtime_session_id
        || accepted.revisions.runtime_event_seq != accepted.runtime_event_seq
    {
        return Err(BridgeError::Invalid(
            "runtime snapshot acceptance is invalid".to_owned(),
        ));
    }
    let begin: RuntimeSnapshotBeginMessage = serde_json::from_value(
        session
            .receive_non_sync_with_timeout(RUNTIME_SNAPSHOT_TIMEOUT)
            .await?,
    )
    .map_err(|error| BridgeError::Invalid(format!("runtime snapshot begin is invalid: {error}")))?;
    if begin.protocol_version != "1.6"
        || begin.kind != "notification"
        || begin.method != "snapshot.begin"
        || begin.params.snapshot_id != accepted.snapshot_id
        || begin.params.domain != "runtime"
        || begin.params.runtime_session_id != accepted.runtime_session_id
        || begin.params.runtime_event_seq != accepted.runtime_event_seq
        || begin.params.revisions != accepted.revisions
        || begin.params.chunk_count == 0
        || begin.params.chunk_count > MAX_RUNTIME_CHUNKS
    {
        return Err(BridgeError::Invalid(
            "runtime snapshot begin is invalid".to_owned(),
        ));
    }

    let mut entities = Vec::new();
    let mut checksums = String::new();
    for expected_index in 0..begin.params.chunk_count {
        let chunk: RuntimeSnapshotChunk = serde_json::from_value(
            session
                .receive_non_sync_with_timeout(RUNTIME_SNAPSHOT_TIMEOUT)
                .await?,
        )
        .map_err(|error| {
            BridgeError::Invalid(format!("runtime snapshot chunk is invalid: {error}"))
        })?;
        let parsed_payload: RuntimeSnapshotPayload = serde_json::from_str(&chunk.payload_json)
            .map_err(|error| {
                BridgeError::Invalid(format!("runtime payload_json is invalid: {error}"))
            })?;
        if chunk.protocol_version != "1.6"
            || chunk.kind != "chunk"
            || chunk.snapshot_id != accepted.snapshot_id
            || chunk.domain != "runtime"
            || chunk.chunk_index != expected_index
            || chunk.payload_json.len() > MAX_RUNTIME_CHUNK_BYTES
            || parsed_payload != chunk.payload
            || sha256_hex(chunk.payload_json.as_bytes()) != chunk.checksum
            || chunk.payload.domain != "runtime"
            || chunk.payload.runtime_session_id != accepted.runtime_session_id
            || chunk.payload.runtime_event_seq != accepted.runtime_event_seq
            || chunk.payload.entities.len() > 1_000
        {
            return Err(BridgeError::Invalid(
                "runtime snapshot chunk is invalid".to_owned(),
            ));
        }
        validate_entities(&chunk.payload.entities, &accepted)?;
        entities.extend(chunk.payload.entities);
        if entities.len() > MAX_RUNTIME_ENTITIES {
            return Err(BridgeError::Invalid(
                "runtime snapshot is too large".to_owned(),
            ));
        }
        checksums.push_str(&chunk.checksum);
        session
            .send_ack(&accepted.snapshot_id, Some("runtime"), expected_index)
            .await?;
    }
    let end: RuntimeSnapshotEndMessage = serde_json::from_value(
        session
            .receive_non_sync_with_timeout(RUNTIME_SNAPSHOT_TIMEOUT)
            .await?,
    )
    .map_err(|error| BridgeError::Invalid(format!("runtime snapshot end is invalid: {error}")))?;
    if end.protocol_version != "1.6"
        || end.kind != "notification"
        || end.method != "snapshot.end"
        || end.params.snapshot_id != accepted.snapshot_id
        || end.params.domain != "runtime"
        || end.params.runtime_session_id != accepted.runtime_session_id
        || end.params.runtime_event_seq != accepted.runtime_event_seq
        || end.params.chunk_count != begin.params.chunk_count
        || end.params.entity_count != entities.len()
        || end.params.revisions != accepted.revisions
        || end.params.checksum != sha256_hex(checksums.as_bytes())
    {
        return Err(BridgeError::Invalid(
            "runtime snapshot end is invalid".to_owned(),
        ));
    }
    Ok(RuntimeSnapshot {
        accepted,
        entities,
        end: end.params,
    })
}

fn validate_entities(
    entities: &[RuntimeEntity],
    accepted: &RuntimeSnapshotAccepted,
) -> Result<(), BridgeError> {
    for entity in entities {
        match entity {
            RuntimeEntity::RuntimeState(value) => {
                validate_coordinates(
                    &value.runtime_session_id,
                    value.runtime_event_seq,
                    &accepted.runtime_session_id,
                )?;
                if value.entity_id != value.runtime_session_id
                    || value.kind != "runtime_state"
                    || value
                        .scene_path
                        .as_deref()
                        .is_some_and(|path| !valid_res_path(path))
                    || value
                        .active_stack_id
                        .as_deref()
                        .is_some_and(|id| !valid_opaque(id, "runtime-stack:"))
                {
                    return Err(BridgeError::Invalid(
                        "runtime state entity is invalid".to_owned(),
                    ));
                }
            }
            RuntimeEntity::RuntimeNode(value) => {
                if value.entity_id != value.runtime_object_id
                    || value.kind != "runtime_node"
                    || !valid_opaque(&value.runtime_object_id, "runtime-object:")
                    || value
                        .parent_runtime_object_id
                        .as_deref()
                        .is_some_and(|id| !valid_opaque(id, "runtime-object:"))
                    || value.depth >= accepted.limits_applied.tree_depth
                    || value.child_count > accepted.limits_applied.tree_nodes
                    || value.runtime_node_path.chars().count() > 1_024
                    || value.source_hint.as_ref().is_some_and(|hint| {
                        hint.scene_path
                            .as_deref()
                            .is_some_and(|path| !valid_res_path(path))
                            || hint.instance_scene_paths.as_ref().is_some_and(|paths| {
                                paths.len() > 256 || paths.iter().any(|path| !valid_res_path(path))
                            })
                    })
                {
                    return Err(BridgeError::Invalid("runtime node is invalid".to_owned()));
                }
            }
            RuntimeEntity::RuntimeDiagnostic(value) => {
                if value.entity_id != value.runtime_diagnostic_id
                    || value.kind != "runtime_diagnostic"
                    || !valid_opaque(&value.runtime_diagnostic_id, "runtime-diagnostic:")
                    || value.message.len() > accepted.limits_applied.diagnostic_message_bytes
                    || value
                        .script_path
                        .as_deref()
                        .is_some_and(|path| !valid_res_path(path))
                    || value
                        .runtime_stack_id
                        .as_deref()
                        .is_some_and(|id| !valid_opaque(id, "runtime-stack:"))
                {
                    return Err(BridgeError::Invalid(
                        "runtime diagnostic is invalid".to_owned(),
                    ));
                }
            }
            RuntimeEntity::RuntimeStack(value) => validate_stack(value, &accepted.limits_applied)?,
        }
    }
    Ok(())
}

fn validate_stack(stack: &RuntimeStack, limits: &RuntimeLimits) -> Result<(), BridgeError> {
    if stack.entity_id != stack.runtime_stack_id
        || stack.kind != "runtime_stack"
        || !valid_opaque(&stack.runtime_stack_id, "runtime-stack:")
        || stack.frames.len() > limits.stack_frames
        || stack.frames.iter().enumerate().any(|(index, frame)| {
            frame.frame != index
                || frame.line == 0
                || frame
                    .script_path
                    .as_deref()
                    .is_some_and(|path| !valid_res_path(path))
        })
    {
        return Err(BridgeError::Invalid("runtime stack is invalid".to_owned()));
    }
    Ok(())
}

pub(crate) async fn inspect_runtime_object(
    session: &mut Session,
    runtime_session_id: &str,
    runtime_object_id: &str,
    expected_runtime_event_seq: Option<u64>,
) -> Result<RuntimeObjectResult, BridgeError> {
    require_runtime(session)?;
    if !valid_prefixed_hex(runtime_session_id, "runtime:", 32)
        || !valid_opaque(runtime_object_id, "runtime-object:")
    {
        return Err(BridgeError::Invalid(
            "runtime object selector is invalid".to_owned(),
        ));
    }
    let mut params = guarded_params(runtime_session_id, expected_runtime_event_seq);
    params["runtime_object_id"] = json!(runtime_object_id);
    let response = session
        .request_with_deadline_and_timeout(
            "runtime.object.inspect",
            params,
            RUNTIME_OBSERVATION_DEADLINE,
            RUNTIME_RESPONSE_TIMEOUT,
        )
        .await?;
    let value: RuntimeObjectResult = result(&response, "runtime object")?;
    validate_coordinates(
        &value.runtime_session_id,
        value.runtime_event_seq,
        runtime_session_id,
    )?;
    validate_limits(&value.limits_applied)?;
    if value.schema_version != "runtime/1.0"
        || value.runtime_object_id != runtime_object_id
        || value.properties.len() > value.limits_applied.properties
        || value.properties.iter().any(|property| !property.read_only)
        || serde_json::to_vec(&value)?.len() > value.limits_applied.object_bytes + 8_192
    {
        return Err(BridgeError::Invalid(
            "runtime object result is invalid".to_owned(),
        ));
    }
    Ok(value)
}

pub(crate) async fn get_runtime_stack(
    session: &mut Session,
    runtime_session_id: &str,
    runtime_stack_id: &str,
    expected_runtime_event_seq: Option<u64>,
) -> Result<RuntimeStackResult, BridgeError> {
    require_runtime(session)?;
    if !valid_prefixed_hex(runtime_session_id, "runtime:", 32)
        || !valid_opaque(runtime_stack_id, "runtime-stack:")
    {
        return Err(BridgeError::Invalid(
            "runtime stack selector is invalid".to_owned(),
        ));
    }
    let mut params = guarded_params(runtime_session_id, expected_runtime_event_seq);
    params["runtime_stack_id"] = json!(runtime_stack_id);
    let response = session
        .request_with_deadline_and_timeout(
            "runtime.stack.get",
            params,
            RUNTIME_OBSERVATION_DEADLINE,
            RUNTIME_RESPONSE_TIMEOUT,
        )
        .await?;
    let value: RuntimeStackResult = result(&response, "runtime stack")?;
    validate_coordinates(
        &value.runtime_session_id,
        value.runtime_event_seq,
        runtime_session_id,
    )?;
    if value.schema_version != "runtime/1.0" || value.stack.runtime_stack_id != runtime_stack_id {
        return Err(BridgeError::Invalid(
            "runtime stack result is invalid".to_owned(),
        ));
    }
    validate_stack(
        &value.stack,
        &RuntimeLimits {
            tree_nodes: 10_000,
            tree_depth: 256,
            snapshot_bytes: 16 * 1_024 * 1_024,
            snapshot_chunk_bytes: MAX_RUNTIME_CHUNK_BYTES,
            snapshot_window_bytes: 32 * 1_024 * 1_024,
            snapshot_timeout_ms: 10_000,
            properties: 512,
            variant_depth: 8,
            container_items: 1_000,
            string_characters: 16_384,
            projected_value_bytes: 65_536,
            object_bytes: 262_144,
            diagnostics: 200,
            diagnostic_message_bytes: 16_384,
            diagnostics_bytes: 262_144,
            stacks: 64,
            stack_frames: 128,
            stacks_bytes: 262_144,
            truncated: false,
        },
    )?;
    Ok(value)
}

pub(crate) async fn capture_runtime_viewport(
    session: &mut Session,
    runtime_session_id: &str,
    expected_runtime_event_seq: Option<u64>,
    max_width: u32,
    max_height: u32,
) -> Result<RuntimeViewportCapture, BridgeError> {
    require_runtime(session)?;
    if !valid_prefixed_hex(runtime_session_id, "runtime:", 32)
        || max_width == 0
        || max_width > 1_280
        || max_height == 0
        || max_height > 720
    {
        return Err(BridgeError::Invalid(
            "runtime capture parameters are invalid".to_owned(),
        ));
    }
    let mut params = guarded_params(runtime_session_id, expected_runtime_event_seq);
    params["max_width"] = json!(max_width);
    params["max_height"] = json!(max_height);
    let response = session
        .request_with_deadline_and_timeout(
            "runtime.viewport.capture",
            params,
            RUNTIME_OBSERVATION_DEADLINE,
            RUNTIME_RESPONSE_TIMEOUT,
        )
        .await?;
    let value: RuntimeViewportCapture = result(&response, "runtime capture")?;
    validate_coordinates(
        &value.runtime_session_id,
        value.runtime_event_seq,
        runtime_session_id,
    )?;
    if value.schema_version != "runtime/1.0"
        || value.mime_type != "image/png"
        || value.width == 0
        || value.width > max_width
        || value.height == 0
        || value.height > max_height
    {
        return Err(BridgeError::Invalid(
            "runtime capture result is invalid".to_owned(),
        ));
    }
    value.decode_png()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_response_fixtures_are_strict() {
        let response: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-run-response.json"
        ))
        .unwrap();
        let state: RuntimeStateResult = serde_json::from_value(response["result"].clone()).unwrap();
        assert_eq!(state.state, RuntimeState::Running);

        let object: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-object-inspect-response.json"
        ))
        .unwrap();
        let object: RuntimeObjectResult = serde_json::from_value(object["result"].clone()).unwrap();
        assert!(object.properties.iter().all(|property| property.read_only));

        let stack: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-stack-response.json"
        ))
        .unwrap();
        let stack: RuntimeStackResult = serde_json::from_value(stack["result"].clone()).unwrap();
        assert!(
            stack
                .stack
                .frames
                .iter()
                .all(|frame| frame.script_path.as_deref().is_none_or(valid_res_path))
        );

        let event: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-event.json"
        ))
        .unwrap();
        let event: RuntimeEventMessage = serde_json::from_value(event).unwrap();
        assert_eq!(
            event.params.runtime_session_id,
            event.params.revisions.runtime_session_id
        );
        assert_eq!(
            event.params.runtime_event_seq,
            event.params.revisions.runtime_event_seq
        );

        let invalidated: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-invalidated.json"
        ))
        .unwrap();
        let invalidated: RuntimeInvalidatedMessage = serde_json::from_value(invalidated).unwrap();
        assert_eq!(
            invalidated.params.reason,
            RuntimeInvalidationReason::JournalGap
        );
    }

    #[test]
    fn runtime_screenshot_rejects_non_png_and_digest_mismatch() {
        let response: Value = serde_json::from_str(include_str!(
            "../../../../schemas/codex_bridge/v1/fixtures/valid/runtime-viewport-capture-response.json"
        ))
        .unwrap();
        let capture: RuntimeViewportCapture =
            serde_json::from_value(response["result"].clone()).unwrap();
        assert!(capture.decode_png().is_err());
    }
}
