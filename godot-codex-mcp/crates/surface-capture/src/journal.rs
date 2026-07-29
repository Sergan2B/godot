use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::JOURNAL_SCHEMA_VERSION;
use crate::ToolObservation;
use crate::observation::{
    form_request_observation, form_result_observation, request_observation, result_observation,
    supported_tool,
};

const DEFAULT_EVENT_CAPACITY: usize = 4_096;
const MAX_EVENT_CAPACITY: usize = 4_096;
const MAX_RETAINED_EVENT_BYTES: usize = 480 * 1024;
const MAX_SINGLE_EVENT_BYTES: usize = 144 * 1024;
const MAX_PENDING_REQUESTS: usize = 128;
const MAX_REGISTRY_NAMES: usize = 256;
const MAX_SAFE_ATOM: usize = 128;
const ZERO_HASH: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";
const SPRINT11_ASSERTION_IDS: &[&str] = &[
    "approval.accept",
    "approval.cancel",
    "approval.decline",
    "approval.timeout",
    "connection.status",
    "host.offline_status",
    "multi_project.reject",
    "offline.saved_query",
    "runtime.error_stack",
    "saved.current_scene",
    "transaction.apply",
    "transaction.preview",
    "transaction.undo",
    "validation.result",
];

/// Direction of one observed MCP JSON-RPC message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureDirection {
    /// Message sent by the MCP client.
    ClientToServer,
    /// Message sent by the MCP server.
    ServerToClient,
}

impl CaptureDirection {
    fn opposite(self) -> Self {
        match self {
            Self::ClientToServer => Self::ServerToClient,
            Self::ServerToClient => Self::ClientToServer,
        }
    }
}

/// Content-minimized class of an observed protocol event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureEventClass {
    /// MCP initialization handshake.
    Initialize,
    /// Public registry discovery.
    Registry,
    /// Tool invocation or result.
    Tool,
    /// Elicitation/approval form exchange.
    Form,
    /// Godot connection status invocation or result.
    Status,
    /// JSON-RPC error; only its numeric code is retained.
    Error,
    /// Other protocol traffic with no content retained.
    Protocol,
    /// Explicit, allowlisted semantic projection.
    SemanticProjection,
}

/// Explicit allowlist for extensible semantic projections.
#[derive(Debug, Clone, Default)]
pub struct SemanticProjectionAllowlist {
    projection_ids: BTreeSet<String>,
}

impl SemanticProjectionAllowlist {
    /// Create an empty allowlist.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            projection_ids: BTreeSet::new(),
        }
    }

    /// Canonical Sprint 11 projection ids.
    #[must_use]
    pub fn sprint11() -> Self {
        let mut allowlist = Self::new();
        for id in SPRINT11_ASSERTION_IDS {
            allowlist.projection_ids.insert(format!("assertion.{id}"));
        }
        for action in ["accept", "cancel", "decline", "timeout"] {
            allowlist
                .projection_ids
                .insert(format!("form_outcome.{action}"));
        }
        for revision in [
            "initial",
            "prepared",
            "applied",
            "restarted",
            "stale_guard_rejected",
        ] {
            allowlist
                .projection_ids
                .insert(format!("revision.{revision}"));
        }
        allowlist
    }

    /// Add one source-declared id matching the safe dotted grammar.
    pub fn allow_projection_id(
        &mut self,
        projection_id: impl Into<String>,
    ) -> Result<(), ProjectionError> {
        let projection_id = projection_id.into();
        validate_projection_id(&projection_id)?;
        self.projection_ids.insert(projection_id);
        Ok(())
    }

    fn project(
        &self,
        projection_id: &str,
        source_event_seq: u64,
        value: Value,
    ) -> Result<SemanticProjection, ProjectionError> {
        if !self.projection_ids.contains(projection_id) {
            return Err(ProjectionError::ProjectionDenied);
        }
        if source_event_seq == 0 {
            return Err(ProjectionError::SourceEventInvalid);
        }
        validate_projection_value(&value, 0)?;
        Ok(SemanticProjection {
            projection_id: projection_id.to_owned(),
            source_event_seq,
            value,
        })
    }
}

/// Validated semantic projection stored in the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SemanticProjection {
    /// Allowlisted semantic id.
    pub projection_id: String,
    /// Sequence of the retained supporting transport event.
    pub source_event_seq: u64,
    /// Bounded, path/secret-rejecting semantic value.
    pub value: Value,
}

/// An ordered, hash-chained journal event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureEvent {
    /// One-based sequence among retained events.
    pub sequence: u64,
    /// Observation time in Unix milliseconds.
    pub observed_at_unix_ms: u64,
    /// Protocol direction.
    pub direction: CaptureDirection,
    /// Safe event class.
    pub class: CaptureEventClass,
    /// `request`, `response`, `notification`, `error`, or `projection`.
    pub phase: String,
    /// Safe public MCP method, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    /// Safe public tool name, when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Bounded public names returned by a registry response.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registry_names: Vec<String>,
    /// Negotiated protocol version, when safely available.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    /// Safe client product name from the initialize request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_name: Option<String>,
    /// Safe client product version from the initialize request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_version: Option<String>,
    /// SHA-256 of exact UTF-8 server instructions; text is never retained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions_sha256: Option<String>,
    /// Numeric JSON-RPC error code. Error messages/data are never retained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<i64>,
    /// Explicit semantic projection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_projection: Option<SemanticProjection>,
    /// Safe allowlisted request/result facts for a supported tool response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_observation: Option<ToolObservation>,
    /// Hash of the previous retained event.
    pub previous_event_sha256: String,
    /// Hash over the preceding fields, including `previous_event_sha256`.
    pub event_sha256: String,
}

/// Immutable snapshot of a bounded capture journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureJournal {
    /// Journal schema.
    pub schema_version: String,
    /// Opaque capture run id.
    pub run_id: String,
    /// Origin asserted by the official host-facing stdio launch path.
    pub transport_origin: String,
    /// Surface label (`app`, `cli`, or `ide`).
    pub surface: String,
    /// Hashed canonical project identity.
    pub project_identity: String,
    /// Exact metadata document digest.
    pub metadata_sha256: String,
    /// Exact installed launcher digest.
    pub package_launcher_sha256: String,
    /// Exact project configuration digest.
    pub project_config_sha256: String,
    /// Exact setup receipt digest.
    pub setup_receipt_sha256: String,
    /// Maximum number of retained events.
    pub event_capacity: usize,
    /// Total messages/projections observed, including dropped events.
    pub observed_event_count: u64,
    /// Events omitted after the fixed capacity was reached.
    pub dropped_event_count: u64,
    /// Hash of the final retained event.
    pub final_event_sha256: String,
    /// Ordered retained events.
    pub events: Vec<CaptureEvent>,
}

/// Header fixed by a claimed lease.
#[derive(Debug, Clone)]
pub(crate) struct JournalBinding {
    pub(crate) run_id: String,
    pub(crate) surface: String,
    pub(crate) project_identity: String,
    pub(crate) metadata_sha256: String,
    pub(crate) package_launcher_sha256: String,
    pub(crate) project_config_sha256: String,
    pub(crate) setup_receipt_sha256: String,
}

/// Shared recorder used by the typed transport wrapper.
#[derive(Debug, Clone)]
pub struct CaptureRecorder {
    inner: Arc<Mutex<RecorderState>>,
    allowlist: Arc<SemanticProjectionAllowlist>,
}

impl CaptureRecorder {
    pub(crate) fn new(binding: JournalBinding) -> Self {
        Self::with_capacity_and_allowlist(
            binding,
            DEFAULT_EVENT_CAPACITY,
            SemanticProjectionAllowlist::sprint11(),
        )
    }

    pub(crate) fn with_capacity_and_allowlist(
        binding: JournalBinding,
        capacity: usize,
        allowlist: SemanticProjectionAllowlist,
    ) -> Self {
        let capacity = capacity.clamp(1, MAX_EVENT_CAPACITY);
        Self {
            inner: Arc::new(Mutex::new(RecorderState::new(binding, capacity))),
            allowlist: Arc::new(allowlist),
        }
    }

    /// Return an immutable snapshot.
    #[must_use]
    pub fn snapshot(&self) -> CaptureJournal {
        self.lock().snapshot()
    }

    /// Record one explicitly allowlisted semantic projection.
    pub fn record_semantic_projection(
        &self,
        projection_id: &str,
        source_event_seq: u64,
        value: Value,
    ) -> Result<(), ProjectionError> {
        let projection = self
            .allowlist
            .project(projection_id, source_event_seq, value)?;
        self.lock().push(EventDraft {
            direction: CaptureDirection::ServerToClient,
            class: CaptureEventClass::SemanticProjection,
            phase: "projection",
            method: None,
            tool: None,
            registry_names: Vec::new(),
            protocol_version: None,
            client_name: None,
            client_version: None,
            instructions_sha256: None,
            error_code: None,
            semantic_projection: Some(projection),
            tool_observation: None,
        });
        Ok(())
    }

    pub(crate) fn observe_serializable<T: Serialize>(
        &self,
        direction: CaptureDirection,
        message: &T,
    ) {
        let Ok(value) = serde_json::to_value(message) else {
            return;
        };
        self.lock().observe(direction, &value);
    }

    fn lock(&self) -> MutexGuard<'_, RecorderState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Semantic projection validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ProjectionError {
    /// Projection name is absent from the allowlist.
    #[error("semantic projection is not allowlisted")]
    ProjectionDenied,
    /// Supporting transport event sequence is absent/invalid.
    #[error("semantic projection source event is invalid")]
    SourceEventInvalid,
    /// Value violates size, depth, redaction, or shape constraints.
    #[error("semantic projection value is invalid")]
    ValueInvalid,
    /// Allowlist rule itself is invalid.
    #[error("semantic projection rule is invalid")]
    InvalidRule,
}

#[derive(Debug, Clone)]
struct PendingRequest {
    class: CaptureEventClass,
    method: Option<String>,
    tool: Option<String>,
    request_observation: Option<Value>,
}

#[derive(Debug)]
struct RecorderState {
    journal: CaptureJournal,
    pending: BTreeMap<(CaptureDirection, String), PendingRequest>,
    retained_event_bytes: usize,
}

impl RecorderState {
    fn new(binding: JournalBinding, capacity: usize) -> Self {
        Self {
            journal: CaptureJournal {
                schema_version: JOURNAL_SCHEMA_VERSION.to_owned(),
                run_id: binding.run_id,
                transport_origin: "official_host_stdio".to_owned(),
                surface: binding.surface,
                project_identity: binding.project_identity,
                metadata_sha256: binding.metadata_sha256,
                package_launcher_sha256: binding.package_launcher_sha256,
                project_config_sha256: binding.project_config_sha256,
                setup_receipt_sha256: binding.setup_receipt_sha256,
                event_capacity: capacity,
                observed_event_count: 0,
                dropped_event_count: 0,
                final_event_sha256: ZERO_HASH.to_owned(),
                events: Vec::with_capacity(capacity),
            },
            pending: BTreeMap::new(),
            retained_event_bytes: 0,
        }
    }

    fn snapshot(&self) -> CaptureJournal {
        self.journal.clone()
    }

    fn observe(&mut self, direction: CaptureDirection, value: &Value) {
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            let safe_method = safe_atom(method);
            let tool = if method == "tools/call" {
                value
                    .pointer("/params/name")
                    .and_then(Value::as_str)
                    .and_then(safe_atom)
            } else {
                None
            };
            let class = classify_method(method, tool.as_deref());
            let phase = if value.get("id").is_some() {
                "request"
            } else {
                "notification"
            };
            let safe_request = if method == "tools/call" {
                tool.as_deref()
                    .and_then(|tool| request_observation(tool, value.pointer("/params/arguments")))
            } else if method == "elicitation/create" {
                form_request_observation(self.pending.values().filter_map(|pending| {
                    (pending.tool.as_deref() == Some("godot_apply_transaction"))
                        .then_some(pending.request_observation.as_ref())
                        .flatten()
                }))
            } else {
                None
            };
            if phase == "request"
                && let Some(key) = request_key(direction, value.get("id"))
            {
                if self.pending.len() >= MAX_PENDING_REQUESTS {
                    let first = self.pending.keys().next().cloned();
                    if let Some(first) = first {
                        self.pending.remove(&first);
                    }
                }
                self.pending.insert(
                    key,
                    PendingRequest {
                        class,
                        method: safe_method.clone(),
                        tool: tool.clone(),
                        request_observation: safe_request,
                    },
                );
            }
            self.push(EventDraft {
                direction,
                class,
                phase,
                method: safe_method,
                tool,
                registry_names: Vec::new(),
                protocol_version: protocol_version(value),
                client_name: client_info(value, "name"),
                client_version: client_info(value, "version"),
                instructions_sha256: instructions_sha256(value),
                error_code: None,
                semantic_projection: None,
                tool_observation: None,
            });
            return;
        }

        let request = request_key(direction.opposite(), value.get("id"))
            .and_then(|key| self.pending.remove(&key));
        let tool_observation = response_tool_observation(request.as_ref(), value);
        if let Some(error) = value.get("error") {
            self.push(EventDraft {
                direction,
                class: CaptureEventClass::Error,
                phase: "error",
                method: request.as_ref().and_then(|entry| entry.method.clone()),
                tool: request.as_ref().and_then(|entry| entry.tool.clone()),
                registry_names: Vec::new(),
                protocol_version: None,
                client_name: None,
                client_version: None,
                instructions_sha256: None,
                error_code: error.get("code").and_then(Value::as_i64),
                semantic_projection: None,
                tool_observation,
            });
            return;
        }

        let class = request
            .as_ref()
            .map_or(CaptureEventClass::Protocol, |entry| entry.class);
        self.push(EventDraft {
            direction,
            class,
            phase: "response",
            method: request.as_ref().and_then(|entry| entry.method.clone()),
            tool: request.as_ref().and_then(|entry| entry.tool.clone()),
            registry_names: registry_names(value),
            protocol_version: protocol_version(value),
            client_name: None,
            client_version: None,
            instructions_sha256: instructions_sha256(value),
            error_code: None,
            semantic_projection: None,
            tool_observation,
        });
    }

    fn push(&mut self, draft: EventDraft) {
        self.journal.observed_event_count = self.journal.observed_event_count.saturating_add(1);
        if self.journal.events.len() >= self.journal.event_capacity {
            self.journal.dropped_event_count = self.journal.dropped_event_count.saturating_add(1);
            return;
        }
        let sequence = u64::try_from(self.journal.events.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let previous = self.journal.final_event_sha256.clone();
        let mut event = CaptureEvent {
            sequence,
            observed_at_unix_ms: unix_millis(),
            direction: draft.direction,
            class: draft.class,
            phase: draft.phase.to_owned(),
            method: draft.method,
            tool: draft.tool,
            registry_names: draft.registry_names,
            protocol_version: draft.protocol_version,
            client_name: draft.client_name,
            client_version: draft.client_version,
            instructions_sha256: draft.instructions_sha256,
            error_code: draft.error_code,
            semantic_projection: draft.semantic_projection,
            tool_observation: draft.tool_observation,
            previous_event_sha256: previous,
            event_sha256: String::new(),
        };
        event.event_sha256 = event_hash(&event);
        let encoded_size = serde_json::to_vec(&event).map_or(usize::MAX, |bytes| bytes.len());
        if encoded_size > MAX_SINGLE_EVENT_BYTES
            || self.retained_event_bytes.saturating_add(encoded_size) > MAX_RETAINED_EVENT_BYTES
        {
            self.journal.dropped_event_count = self.journal.dropped_event_count.saturating_add(1);
            return;
        }
        self.retained_event_bytes = self.retained_event_bytes.saturating_add(encoded_size);
        self.journal.final_event_sha256 = event.event_sha256.clone();
        self.journal.events.push(event);
    }
}

struct EventDraft {
    direction: CaptureDirection,
    class: CaptureEventClass,
    phase: &'static str,
    method: Option<String>,
    tool: Option<String>,
    registry_names: Vec<String>,
    protocol_version: Option<String>,
    client_name: Option<String>,
    client_version: Option<String>,
    instructions_sha256: Option<String>,
    error_code: Option<i64>,
    semantic_projection: Option<SemanticProjection>,
    tool_observation: Option<ToolObservation>,
}

fn response_tool_observation(
    request: Option<&PendingRequest>,
    message: &Value,
) -> Option<ToolObservation> {
    let request = request?;
    let safe_request = request.request_observation.clone()?;
    let safe_result = if request.class == CaptureEventClass::Form {
        form_result_observation(message)?
    } else {
        let tool = request.tool.as_deref()?;
        if !supported_tool(tool) {
            return None;
        }
        result_observation(tool, message)?
    };
    Some(ToolObservation {
        request: safe_request,
        result: safe_result,
    })
}

fn classify_method(method: &str, tool: Option<&str>) -> CaptureEventClass {
    match method {
        "initialize" | "notifications/initialized" => CaptureEventClass::Initialize,
        "tools/list"
        | "resources/list"
        | "resources/templates/list"
        | "prompts/list"
        | "notifications/tools/list_changed"
        | "notifications/resources/list_changed"
        | "notifications/prompts/list_changed" => CaptureEventClass::Registry,
        "elicitation/create" | "notifications/elicitation/complete" => CaptureEventClass::Form,
        "tools/call" if tool == Some("godot_get_connection_status") => CaptureEventClass::Status,
        "tools/call" => CaptureEventClass::Tool,
        _ => CaptureEventClass::Protocol,
    }
}

fn protocol_version(value: &Value) -> Option<String> {
    value
        .pointer("/params/protocolVersion")
        .or_else(|| value.pointer("/result/protocolVersion"))
        .and_then(Value::as_str)
        .and_then(safe_atom)
}

fn client_info(value: &Value, field: &str) -> Option<String> {
    value
        .pointer(&format!("/params/clientInfo/{field}"))
        .and_then(Value::as_str)
        .and_then(safe_atom)
}

fn instructions_sha256(value: &Value) -> Option<String> {
    value
        .pointer("/result/instructions")
        .and_then(Value::as_str)
        .map(|instructions| sha256_bytes(instructions.as_bytes()))
}

fn registry_names(value: &Value) -> Vec<String> {
    ["tools", "resources", "resourceTemplates", "prompts"]
        .into_iter()
        .find_map(|key| value.pointer(&format!("/result/{key}"))?.as_array())
        .into_iter()
        .flatten()
        .take(MAX_REGISTRY_NAMES)
        .filter_map(|entry| {
            entry
                .get("name")
                .or_else(|| entry.get("uri"))
                .or_else(|| entry.get("uriTemplate"))
                .and_then(Value::as_str)
                .and_then(safe_atom)
        })
        .collect()
}

fn request_key(
    direction: CaptureDirection,
    id: Option<&Value>,
) -> Option<(CaptureDirection, String)> {
    let id = id?;
    if !id.is_number() && !id.is_string() {
        return None;
    }
    let encoded = serde_json::to_vec(id).ok()?;
    Some((
        direction,
        sha256_tagged(b"sprint11-jsonrpc-request-id-v1\0", &encoded),
    ))
}

fn safe_atom(value: &str) -> Option<String> {
    validate_safe_atom(value).ok()?;
    Some(value.to_owned())
}

fn validate_projection_id(value: &str) -> Result<(), ProjectionError> {
    if value.len() > 96
        || value.split('.').count() < 2
        || value.split('.').any(|part| {
            part.is_empty()
                || !part.as_bytes()[0].is_ascii_lowercase()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
    {
        return Err(ProjectionError::InvalidRule);
    }
    Ok(())
}

fn validate_projection_value(value: &Value, depth: usize) -> Result<(), ProjectionError> {
    if depth > 8 {
        return Err(ProjectionError::ValueInvalid);
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) => {
            let lower = value.to_ascii_lowercase();
            let safe_uri = safe_project_uri(value);
            if value.len() > 512
                || contains_private_absolute_path(value)
                || contains_private_secret_marker(value)
                || contains_account_identity(value)
                || (value.contains("://") && !safe_uri)
                || lower.contains("secret")
                || lower.contains("password")
                || lower.contains("token")
                || lower.contains("api_key")
                || lower.contains("apikey")
                || lower.starts_with("file:")
                || lower.starts_with("http:")
                || lower.starts_with("https:")
            {
                Err(ProjectionError::ValueInvalid)
            } else {
                Ok(())
            }
        }
        Value::Array(values) if values.len() <= 128 => {
            for value in values {
                validate_projection_value(value, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(values) if values.len() <= 128 => {
            for (key, value) in values {
                let lower = key.to_ascii_lowercase();
                if key.is_empty()
                    || key.len() > 64
                    || lower.contains("secret")
                    || lower.contains("password")
                    || lower.contains("token")
                    || lower.contains("authorization")
                    || lower.contains("credential")
                    || lower.contains("api_key")
                    || lower.contains("apikey")
                {
                    return Err(ProjectionError::ValueInvalid);
                }
                validate_projection_value(value, depth + 1)?;
            }
            Ok(())
        }
        _ => Err(ProjectionError::ValueInvalid),
    }
}

fn safe_project_uri(value: &str) -> bool {
    let path = value
        .strip_prefix("res://")
        .or_else(|| value.strip_prefix("scene:res://"));
    let Some(path) = path else {
        return false;
    };
    !path.is_empty()
        && !path.starts_with('/')
        && !path.split('/').any(|part| part.is_empty() || part == "..")
        && path.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'{' | b'}')
        })
}

fn validate_safe_atom(value: &str) -> Result<(), ProjectionError> {
    let lower = value.to_ascii_lowercase();
    let safe_uri = safe_registry_uri(value);
    if value.is_empty()
        || value.len() > MAX_SAFE_ATOM
        || contains_private_absolute_path(value)
        || contains_private_secret_marker(value)
        || contains_account_identity(value)
        || (value.contains("://") && !safe_uri)
        || lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'@' | b'{' | b'}')
        })
    {
        return Err(ProjectionError::ValueInvalid);
    }
    Ok(())
}

/// Reject private absolute host paths even when embedded in an otherwise
/// token-shaped string. URI scheme separators remain available to the
/// separately closed `godot://` and `res://` grammars.
pub(crate) fn contains_private_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    value.starts_with("~/")
        || value.contains("file://")
        || value.contains('\\')
        || [
            "/Users",
            "/home",
            "/root",
            "/private",
            "/tmp",
            "/var/folders",
            "/opt",
            "/Applications",
            "/Volumes",
        ]
        .into_iter()
        .any(|prefix| {
            value.match_indices(prefix).any(|(index, _)| {
                let boundary = index == 0
                    || bytes[index - 1].is_ascii_whitespace()
                    || matches!(
                        bytes[index - 1],
                        b'=' | b':' | b'(' | b'[' | b'{' | b'"' | b'\'' | b',' | b';'
                    );
                let end = index + prefix.len();
                boundary && (end == bytes.len() || bytes.get(end) == Some(&b'/'))
            })
        })
        || bytes.windows(3).enumerate().any(|(index, window)| {
            (index == 0 || !bytes[index - 1].is_ascii_alphanumeric())
                && window[0].is_ascii_alphabetic()
                && window[1] == b':'
                && matches!(window[2], b'/' | b'\\')
        })
}

pub(crate) fn contains_private_secret_marker(value: &str) -> bool {
    let lowercase = value.to_ascii_lowercase();
    contains_token_with_prefix(&lowercase, "bearer ", 12, |byte| {
        byte.is_ascii_alphanumeric()
            || matches!(byte, b'.' | b'_' | b'~' | b'+' | b'/' | b'=' | b'-')
    }) || contains_token_with_prefix(&lowercase, "sk-", 12, |byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
    }) || ["ghp_", "gho_", "ghs_", "ghu_"].into_iter().any(|prefix| {
        contains_token_with_prefix(&lowercase, prefix, 20, |byte| byte.is_ascii_alphanumeric())
    }) || ["s11_secret_", "s11_token_", "s11_proof_", "s11_canary_"]
        .into_iter()
        .any(|prefix| {
            contains_token_with_prefix(&lowercase, prefix, 1, |byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
            })
        })
}

pub(crate) fn contains_account_identity(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.iter().enumerate().any(|(at, byte)| {
        if *byte != b'@' || at == 0 || at + 1 >= bytes.len() {
            return false;
        }
        let local_start = bytes[..at]
            .iter()
            .rposition(|byte| {
                !(byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'%' | b'+' | b'-'))
            })
            .map_or(0, |index| index + 1);
        if local_start == at {
            return false;
        }
        let domain_end = bytes[at + 1..]
            .iter()
            .position(|byte| !(byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'-')))
            .map_or(bytes.len(), |index| at + 1 + index);
        let domain = &bytes[at + 1..domain_end];
        let Some(dot) = domain.iter().rposition(|byte| *byte == b'.') else {
            return false;
        };
        dot > 0
            && domain.len().saturating_sub(dot + 1) >= 2
            && domain[dot + 1..]
                .iter()
                .all(|byte| byte.is_ascii_alphabetic())
    })
}

fn contains_token_with_prefix(
    value: &str,
    prefix: &str,
    minimum_suffix_bytes: usize,
    allowed: impl Fn(u8) -> bool,
) -> bool {
    value.match_indices(prefix).any(|(index, _)| {
        (index == 0 || !value.as_bytes()[index - 1].is_ascii_alphanumeric())
            && value.as_bytes()[index + prefix.len()..]
                .iter()
                .copied()
                .take_while(|byte| allowed(*byte))
                .count()
                >= minimum_suffix_bytes
    })
}

fn safe_registry_uri(value: &str) -> bool {
    let Some(path) = value.strip_prefix("godot://") else {
        return false;
    };
    !path.is_empty()
        && !path.starts_with('/')
        && !path.split('/').any(|part| part.is_empty() || part == "..")
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

pub(crate) fn validate_capture_journal(journal: &CaptureJournal) -> bool {
    let retained_count = match u64::try_from(journal.events.len()) {
        Ok(count) => count,
        Err(_) => return false,
    };
    if journal.schema_version != JOURNAL_SCHEMA_VERSION
        || journal.transport_origin != "official_host_stdio"
        || !matches!(journal.surface.as_str(), "app" | "cli" | "ide")
        || journal.event_capacity == 0
        || journal.event_capacity > MAX_EVENT_CAPACITY
        || journal.events.len() > journal.event_capacity
        || journal.observed_event_count.checked_sub(retained_count)
            != Some(journal.dropped_event_count)
    {
        return false;
    }

    let mut previous = ZERO_HASH;
    let mut retained_bytes = 0_usize;
    for (index, event) in journal.events.iter().enumerate() {
        let Ok(sequence) = u64::try_from(index + 1) else {
            return false;
        };
        let encoded_size = match serde_json::to_vec(event) {
            Ok(bytes) => bytes.len(),
            Err(_) => return false,
        };
        retained_bytes = retained_bytes.saturating_add(encoded_size);
        if event.sequence != sequence
            || event.previous_event_sha256 != previous
            || event.event_sha256 != event_hash(event)
            || encoded_size > MAX_SINGLE_EVENT_BYTES
            || retained_bytes > MAX_RETAINED_EVENT_BYTES
        {
            return false;
        }
        previous = &event.event_sha256;
    }
    journal.final_event_sha256 == previous
}

fn event_hash(event: &CaptureEvent) -> String {
    #[derive(Serialize)]
    struct HashPayload<'a> {
        sequence: u64,
        direction: CaptureDirection,
        class: CaptureEventClass,
        phase: &'a str,
        method: &'a Option<String>,
        tool: &'a Option<String>,
        registry_names: &'a [String],
        protocol_version: &'a Option<String>,
        client_name: &'a Option<String>,
        client_version: &'a Option<String>,
        instructions_sha256: &'a Option<String>,
        error_code: Option<i64>,
        semantic_projection: &'a Option<SemanticProjection>,
        tool_observation: &'a Option<ToolObservation>,
        previous_event_sha256: &'a str,
    }

    let payload = HashPayload {
        sequence: event.sequence,
        direction: event.direction,
        class: event.class,
        phase: &event.phase,
        method: &event.method,
        tool: &event.tool,
        registry_names: &event.registry_names,
        protocol_version: &event.protocol_version,
        client_name: &event.client_name,
        client_version: &event.client_version,
        instructions_sha256: &event.instructions_sha256,
        error_code: event.error_code,
        semantic_projection: &event.semantic_projection,
        tool_observation: &event.tool_observation,
        previous_event_sha256: &event.previous_event_sha256,
    };
    let encoded = serde_json::to_vec(&payload).unwrap_or_default();
    sha256_tagged(b"sprint11-surface-capture-event-v1.1\0", &encoded)
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    sha256_tagged(b"", bytes)
}

fn sha256_tagged(tag: &[u8], bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(tag);
    digest.update(bytes);
    let output = digest.finalize();
    let mut result = String::with_capacity(71);
    result.push_str("sha256:");
    for byte in output {
        use std::fmt::Write as _;
        let _ = write!(result, "{byte:02x}");
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> JournalBinding {
        JournalBinding {
            run_id: "11".repeat(32),
            surface: "cli".to_owned(),
            project_identity: format!("sha256:{}", "22".repeat(32)),
            metadata_sha256: format!("sha256:{}", "33".repeat(32)),
            package_launcher_sha256: format!("sha256:{}", "44".repeat(32)),
            project_config_sha256: format!("sha256:{}", "55".repeat(32)),
            setup_receipt_sha256: format!("sha256:{}", "66".repeat(32)),
        }
    }

    #[test]
    fn journal_is_bounded_hash_chained_and_content_minimized() {
        let recorder = CaptureRecorder::with_capacity_and_allowlist(
            binding(),
            2,
            SemanticProjectionAllowlist::new(),
        );
        let secret = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "secret-token-id",
            "method": "tools/call",
            "params": {
                "name": "godot_get_connection_status",
                "arguments": {
                    "password": "hunter2",
                    "path": "/Users/alice/private/project"
                }
            }
        });
        recorder.observe_serializable(CaptureDirection::ClientToServer, &secret);
        recorder.observe_serializable(
            CaptureDirection::ServerToClient,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "secret-token-id",
                "result": {"content": [{"type": "text", "text": "sk-secret /Users/alice"}]}
            }),
        );
        recorder.observe_serializable(
            CaptureDirection::ClientToServer,
            &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        );

        let journal = recorder.snapshot();
        assert_eq!(journal.events.len(), 2);
        assert_eq!(journal.observed_event_count, 3);
        assert_eq!(journal.dropped_event_count, 1);
        assert_eq!(journal.events[0].previous_event_sha256, ZERO_HASH);
        assert_eq!(
            journal.events[1].previous_event_sha256,
            journal.events[0].event_sha256
        );
        assert_eq!(journal.final_event_sha256, journal.events[1].event_sha256);
        assert_eq!(journal.events[0].class, CaptureEventClass::Status);
        assert_eq!(journal.events[1].class, CaptureEventClass::Status);
        let encoded = serde_json::to_string(&journal).unwrap();
        for forbidden in [
            "hunter2",
            "sk-secret",
            "/Users/alice",
            "secret-token-id",
            "arguments",
            "content",
        ] {
            assert!(!encoded.contains(forbidden), "retained {forbidden}");
        }
    }

    #[test]
    fn oversized_safe_projection_is_dropped_by_byte_budget() {
        let recorder = CaptureRecorder::with_capacity_and_allowlist(
            binding(),
            MAX_EVENT_CAPACITY,
            SemanticProjectionAllowlist::new(),
        );
        let names: Vec<Value> = (0..MAX_REGISTRY_NAMES)
            .map(|index| {
                serde_json::json!({
                    "name": format!("godot_{index:03}_{}", "a".repeat(110))
                })
            })
            .collect();
        for id in 0..32 {
            recorder.observe_serializable(
                CaptureDirection::ServerToClient,
                &serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":names}}),
            );
        }
        let journal = recorder.snapshot();
        assert!(!journal.events.is_empty());
        assert_eq!(journal.observed_event_count, 32);
        assert!(journal.dropped_event_count > 0);
        assert!(serde_json::to_vec(&journal).unwrap().len() < 600 * 1024);
    }

    #[test]
    fn registry_template_and_initialize_are_projected_without_content() {
        let recorder = CaptureRecorder::with_capacity_and_allowlist(
            binding(),
            16,
            SemanticProjectionAllowlist::sprint11(),
        );
        recorder.observe_serializable(
            CaptureDirection::ClientToServer,
            &serde_json::json!({
                "jsonrpc":"2.0",
                "id":1,
                "method":"initialize",
                "params":{
                    "protocolVersion":"2025-11-25",
                    "clientInfo":{"name":"codex-cli","version":"1.2.3"}
                }
            }),
        );
        let instructions = "Private operator instructions at /Users/alice/project";
        recorder.observe_serializable(
            CaptureDirection::ServerToClient,
            &serde_json::json!({
                "jsonrpc":"2.0",
                "id":1,
                "result":{
                    "protocolVersion":"2025-11-25",
                    "instructions":instructions
                }
            }),
        );
        recorder.observe_serializable(
            CaptureDirection::ClientToServer,
            &serde_json::json!({
                "jsonrpc":"2.0",
                "id":2,
                "method":"resources/templates/list"
            }),
        );
        recorder.observe_serializable(
            CaptureDirection::ServerToClient,
            &serde_json::json!({
                "jsonrpc":"2.0",
                "id":2,
                "result":{
                    "resourceTemplates":[
                        {"uriTemplate":"godot://scene/{scene_id}"}
                    ]
                }
            }),
        );
        let journal = recorder.snapshot();
        assert_eq!(journal.events[0].client_name.as_deref(), Some("codex-cli"));
        assert_eq!(journal.events[0].client_version.as_deref(), Some("1.2.3"));
        assert_eq!(
            journal.events[1].instructions_sha256,
            Some(sha256_bytes(instructions.as_bytes()))
        );
        assert_eq!(
            journal.events[3].registry_names,
            vec!["godot://scene/{scene_id}"]
        );
        assert!(
            !serde_json::to_string(&journal)
                .unwrap()
                .contains(instructions)
        );
    }

    #[test]
    fn tool_and_form_responses_are_correlated_without_recording_content() {
        let recorder = CaptureRecorder::with_capacity_and_allowlist(
            binding(),
            16,
            SemanticProjectionAllowlist::sprint11(),
        );
        let transaction_id = format!("change-set:{}", "7".repeat(32));
        recorder.observe_serializable(
            CaptureDirection::ClientToServer,
            &serde_json::json!({
                "jsonrpc":"2.0",
                "id":10,
                "method":"tools/call",
                "params":{
                    "name":"godot_apply_transaction",
                    "arguments":{
                        "transaction_id":transaction_id,
                        "preview_digest":format!("sha256:{}", "8".repeat(64)),
                        "expected_scene_revision":4,
                        "expected_operation_seq":5
                    }
                }
            }),
        );
        recorder.observe_serializable(
            CaptureDirection::ServerToClient,
            &serde_json::json!({
                "jsonrpc":"2.0",
                "id":99,
                "method":"elicitation/create",
                "params":{
                    "message":"Approve private node changes at /Users/alice/project?",
                    "requestedSchema":{"type":"object"}
                }
            }),
        );
        recorder.observe_serializable(
            CaptureDirection::ClientToServer,
            &serde_json::json!({
                "jsonrpc":"2.0",
                "id":99,
                "result":{
                    "action":"decline",
                    "content":{"reason":"Private operator response"}
                }
            }),
        );
        recorder.observe_serializable(
            CaptureDirection::ServerToClient,
            &serde_json::json!({
                "jsonrpc":"2.0",
                "id":10,
                "result":{
                    "isError":true,
                    "structuredContent":{
                        "error":{
                            "code":"approval_timeout",
                            "message":"Private approval timeout at /Users/alice/project",
                            "retryable":true,
                            "remediation_id":"retry_apply"
                        }
                    }
                }
            }),
        );

        let journal = recorder.snapshot();
        let form = journal.events[2]
            .tool_observation
            .as_ref()
            .expect("form response observation");
        assert_eq!(form.request["parent_tool"], "godot_apply_transaction");
        assert_eq!(form.request["transaction_id"], transaction_id);
        assert_eq!(form.result["action"], "decline");
        assert_eq!(form.result["content_recorded"], false);

        let apply = journal.events[3]
            .tool_observation
            .as_ref()
            .expect("apply response observation");
        assert_eq!(apply.request["transaction_id"], transaction_id);
        assert_eq!(apply.result["error"]["code"], "approval_timeout");
        assert_eq!(apply.result["error"]["remediation_id"], "retry_apply");
        assert_eq!(apply.result["is_error"], true);
        assert_eq!(apply.result["error"]["message_redacted"], true);

        let encoded = serde_json::to_string(&journal).unwrap();
        for forbidden in [
            "Private operator response",
            "Approve private node changes",
            "Private approval timeout",
            "/Users/alice",
            "requestedSchema",
        ] {
            assert!(!encoded.contains(forbidden), "retained {forbidden}");
        }
    }

    #[test]
    fn sprint11_allowlist_has_only_transport_assertions_plus_forms_and_revisions() {
        let allowlist = SemanticProjectionAllowlist::sprint11();
        assert_eq!(SPRINT11_ASSERTION_IDS.len(), 14);
        assert_eq!(allowlist.projection_ids.len(), 14 + 4 + 5);
        for excluded in [
            "assertion.host.approval_layers",
            "assertion.host.config_reload",
            "assertion.host.launcher",
        ] {
            assert!(!allowlist.projection_ids.contains(excluded));
        }
    }

    #[test]
    fn semantic_projection_requires_explicit_typed_allowlist() {
        let mut allowlist = SemanticProjectionAllowlist::new();
        allowlist
            .allow_projection_id("assertion.connection_status")
            .unwrap();
        let recorder =
            CaptureRecorder::with_capacity_and_allowlist(binding(), 8, allowlist.clone());
        recorder
            .record_semantic_projection(
                "assertion.connection_status",
                1,
                serde_json::json!({
                    "ready": true,
                    "condition": "ready",
                    "scene": "scene:res://levels/demo.tscn",
                    "resource": "res://scripts/player.gd"
                }),
            )
            .unwrap();
        let encoded = serde_json::to_string(&recorder.snapshot()).unwrap();
        assert!(encoded.contains("\"ready\":true"));
        assert_eq!(
            recorder.record_semantic_projection(
                "assertion.connection_status",
                1,
                serde_json::json!({"condition": "sk-secret"})
            ),
            Err(ProjectionError::ValueInvalid)
        );
        assert_eq!(
            recorder.record_semantic_projection("assertion.not_allowed", 1, Value::Bool(true)),
            Err(ProjectionError::ProjectionDenied)
        );
        assert_eq!(
            recorder.record_semantic_projection(
                "assertion.connection_status",
                0,
                Value::Bool(true)
            ),
            Err(ProjectionError::SourceEventInvalid)
        );
        for unsafe_value in [
            "/Users/alice/project",
            "Codex:/Users/alice/project",
            "ready:/Users/alice/project",
            "path=/Users/alice/project",
            "path=(/Users/alice/project)",
            "C:/Users/alice/project",
            "artifact=C:/Users/alice/project",
            r"path=\Users\alice\project",
            r"\\private-server\account\project",
            "file:///tmp/project",
            "https://example.test/result",
            "Bearer private-value",
            "ready:sk-proj-abcdefghijklmnop",
            "ready:ghp_abcdefghijklmnopqrst",
            "ready:S11_PROOF_not_public",
            "account:alice@example.com",
            "res://../outside",
        ] {
            assert_eq!(
                recorder.record_semantic_projection(
                    "assertion.connection_status",
                    1,
                    Value::String(unsafe_value.to_owned())
                ),
                Err(ProjectionError::ValueInvalid)
            );
        }
        for unsafe_atom in [
            "Codex:/Users/alice",
            "ready:/Users/alice",
            "path=/Users/alice",
            "artifact=C:/Users/alice",
            r"path=\Users\alice",
            r"\\private-server\account",
            "ready:sk-proj-abcdefghijklmnop",
            "ready:ghp_abcdefghijklmnopqrst",
            "account:alice@example.com",
            "godot://alice@example.com/resource",
        ] {
            assert_eq!(
                validate_safe_atom(unsafe_atom),
                Err(ProjectionError::ValueInvalid),
                "unsafe atom was accepted: {unsafe_atom}"
            );
        }
        for safe_atom in [
            "tools/call",
            "ready",
            "openai.chatgpt@26.721.41059",
            "godot://resource/{id}",
            "godot://resource@7/{id}",
            "/usr/bin/shasum",
        ] {
            assert!(
                validate_safe_atom(safe_atom).is_ok(),
                "closed safe atom was rejected: {safe_atom}"
            );
        }
        assert_eq!(
            recorder.record_semantic_projection(
                "assertion.connection_status",
                1,
                serde_json::json!({"api_key": "value"})
            ),
            Err(ProjectionError::ValueInvalid)
        );
    }
}
