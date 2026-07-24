use std::time::Duration;

use base64::Engine as _;
use hmac::{Hmac, Mac};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::protocol::{BridgeError, Session};

type HmacSha256 = Hmac<Sha256>;

pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_PREVIEW_BYTES: usize = 65_536;
pub const MAX_OPERATION_BYTES: usize = 65_536;
pub const MAX_STATUS_BYTES: usize = 65_536;
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(5);
pub const RECEIPT_TTL_MS: u64 = 30_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionState {
    Preparing,
    Previewed,
    AwaitingApproval,
    Applying,
    Applied,
    Validating,
    Committed,
    Undone,
    Conflicted,
    Expired,
    Rejected,
    Failed,
    FailedRolledBack,
    InDoubt,
}

impl TransactionState {
    #[must_use]
    pub fn is_protected_from_gc(self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Previewed
                | Self::AwaitingApproval
                | Self::Applying
                | Self::Applied
                | Self::Validating
                | Self::InDoubt
        )
    }

    #[must_use]
    pub fn forbids_apply_replay(self) -> bool {
        matches!(
            self,
            Self::Applying
                | Self::Applied
                | Self::Validating
                | Self::Committed
                | Self::Undone
                | Self::InDoubt
        )
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    CreateNode,
    DeleteNode,
    ReparentNode,
    SetProperty,
    AttachScript,
    DetachScript,
    ConnectSignal,
    DisconnectSignal,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    Low,
    Write,
    Destructive,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub enum ApprovalScope {
    #[serde(rename = "change_set.atomic")]
    #[schemars(rename = "change_set.atomic")]
    ChangeSetAtomic,
    #[serde(rename = "scene.node.create")]
    #[schemars(rename = "scene.node.create")]
    SceneNodeCreate,
    #[serde(rename = "scene.node.delete")]
    #[schemars(rename = "scene.node.delete")]
    SceneNodeDelete,
    #[serde(rename = "scene.node.reparent")]
    #[schemars(rename = "scene.node.reparent")]
    SceneNodeReparent,
    #[serde(rename = "scene.property.set")]
    #[schemars(rename = "scene.property.set")]
    ScenePropertySet,
    #[serde(rename = "scene.script.attach")]
    #[schemars(rename = "scene.script.attach")]
    SceneScriptAttach,
    #[serde(rename = "scene.script.detach")]
    #[schemars(rename = "scene.script.detach")]
    SceneScriptDetach,
    #[serde(rename = "scene.signal.connect")]
    #[schemars(rename = "scene.signal.connect")]
    SceneSignalConnect,
    #[serde(rename = "scene.signal.disconnect")]
    #[schemars(rename = "scene.signal.disconnect")]
    SceneSignalDisconnect,
}

impl ApprovalScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::ChangeSetAtomic => "change_set.atomic",
            Self::SceneNodeCreate => "scene.node.create",
            Self::SceneNodeDelete => "scene.node.delete",
            Self::SceneNodeReparent => "scene.node.reparent",
            Self::ScenePropertySet => "scene.property.set",
            Self::SceneScriptAttach => "scene.script.attach",
            Self::SceneScriptDetach => "scene.script.detach",
            Self::SceneSignalConnect => "scene.signal.connect",
            Self::SceneSignalDisconnect => "scene.signal.disconnect",
        }
    }
}

impl Risk {
    fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Write => "write",
            Self::Destructive => "destructive",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum TransactionResourceRef {
    Uid(TransactionUidRef),
    Path(TransactionPathRef),
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionUidRef {
    pub uid: String,
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionPathRef {
    pub uid_missing: bool,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    tag = "type",
    content = "value",
    rename_all = "snake_case"
)]
pub enum WritableVariant {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    StringName(String),
    NodePath(String),
    Vector2(Vec<f64>),
    Vector2i(Vec<i64>),
    Vector3(Vec<f64>),
    Vector3i(Vec<i64>),
    Vector4(Vec<f64>),
    Vector4i(Vec<i64>),
    Rect2(Vec<f64>),
    Rect2i(Vec<i64>),
    Plane(Vec<f64>),
    Quaternion(Vec<f64>),
    Color(Vec<f64>),
    Transform2d(Vec<f64>),
    Aabb(Vec<f64>),
    Basis(Vec<f64>),
    Transform3d(Vec<f64>),
    Projection(Vec<f64>),
    Resource(TransactionResourceRef),
    Array(Vec<WritableVariant>),
    Dictionary(Vec<WritableDictionaryEntry>),
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WritableDictionaryEntry {
    pub key: String,
    pub value: WritableVariant,
}

#[derive(Clone, Debug, Deserialize, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, tag = "kind", rename_all = "snake_case")]
pub enum TransactionOperation {
    CreateNode {
        parent_node_id: String,
        godot_type: String,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        insertion_index: Option<u32>,
    },
    DeleteNode {
        node_id: String,
    },
    ReparentNode {
        node_id: String,
        new_parent_node_id: String,
        insertion_index: u32,
        keep_global_transform: bool,
    },
    SetProperty {
        node_id: String,
        property: String,
        value: WritableVariant,
    },
    AttachScript {
        node_id: String,
        script_ref: TransactionResourceRef,
    },
    DetachScript {
        node_id: String,
    },
    ConnectSignal {
        emitter_node_id: String,
        signal: String,
        receiver_node_id: String,
        method: String,
        flags: u8,
        unbinds: u16,
        binds: Vec<WritableVariant>,
    },
    DisconnectSignal {
        emitter_node_id: String,
        signal: String,
        receiver_node_id: String,
        method: String,
        flags: u8,
        unbinds: u16,
        binds: Vec<WritableVariant>,
    },
}

impl TransactionOperation {
    #[must_use]
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::CreateNode { .. } => OperationKind::CreateNode,
            Self::DeleteNode { .. } => OperationKind::DeleteNode,
            Self::ReparentNode { .. } => OperationKind::ReparentNode,
            Self::SetProperty { .. } => OperationKind::SetProperty,
            Self::AttachScript { .. } => OperationKind::AttachScript,
            Self::DetachScript { .. } => OperationKind::DetachScript,
            Self::ConnectSignal { .. } => OperationKind::ConnectSignal,
            Self::DisconnectSignal { .. } => OperationKind::DisconnectSignal,
        }
    }

    pub fn validate(&self) -> Result<(), BridgeError> {
        let valid_node = |value: &str| valid_prefixed_hex(value, "node:");
        match self {
            Self::CreateNode {
                parent_node_id,
                godot_type,
                name,
                ..
            } => {
                if !valid_node(parent_node_id)
                    || !valid_godot_type(godot_type)
                    || !valid_node_name(name)
                {
                    return invalid_operation();
                }
            }
            Self::DeleteNode { node_id } | Self::DetachScript { node_id } => {
                if !valid_node(node_id) {
                    return invalid_operation();
                }
            }
            Self::ReparentNode {
                node_id,
                new_parent_node_id,
                ..
            } => {
                if !valid_node(node_id) || !valid_node(new_parent_node_id) {
                    return invalid_operation();
                }
            }
            Self::SetProperty {
                node_id,
                property,
                value,
            } => {
                if !valid_node(node_id) || !valid_member_name(property) {
                    return invalid_operation();
                }
                let mut items = 0;
                value.validate(0, &mut items)?;
            }
            Self::AttachScript {
                node_id,
                script_ref,
            } => {
                if !valid_node(node_id) || !valid_resource_ref(script_ref) {
                    return invalid_operation();
                }
            }
            Self::ConnectSignal {
                emitter_node_id,
                signal,
                receiver_node_id,
                method,
                flags,
                unbinds,
                binds,
            }
            | Self::DisconnectSignal {
                emitter_node_id,
                signal,
                receiver_node_id,
                method,
                flags,
                unbinds,
                binds,
            } => {
                if !valid_node(emitter_node_id)
                    || !valid_node(receiver_node_id)
                    || !valid_member_name(signal)
                    || !valid_member_name(method)
                    || !matches!(flags, 2 | 3 | 6 | 7)
                    || usize::from(*unbinds) > 1_000
                    || binds.len() > 1_000
                {
                    return invalid_operation();
                }
                let mut items = 0;
                for value in binds {
                    value.validate(0, &mut items)?;
                }
            }
        }
        let encoded = serde_json::to_vec(self)?;
        if encoded.len() > MAX_OPERATION_BYTES {
            return invalid_operation();
        }
        Ok(())
    }
}

impl WritableVariant {
    fn validate(&self, depth: usize, items: &mut usize) -> Result<(), BridgeError> {
        if depth > 8 {
            return invalid_operation();
        }
        *items = items.saturating_add(1);
        if *items > 1_000 {
            return invalid_operation();
        }
        let valid_floats = |values: &[f64], length: usize| {
            values.len() == length && values.iter().all(|value| value.is_finite())
        };
        let valid_ints = |values: &[i64], length: usize| values.len() == length;
        match self {
            Self::Int(value) if value.unsigned_abs() > MAX_SAFE_INTEGER => {
                return invalid_operation();
            }
            Self::Float(value) if !value.is_finite() => return invalid_operation(),
            Self::String(value) | Self::StringName(value) | Self::NodePath(value)
                if value.chars().count() > 16_384 =>
            {
                return invalid_operation();
            }
            Self::NodePath(value) if value.starts_with('/') => return invalid_operation(),
            Self::Vector2(values) if !valid_floats(values, 2) => return invalid_operation(),
            Self::Vector2i(values) if !valid_ints(values, 2) => return invalid_operation(),
            Self::Vector3(values) if !valid_floats(values, 3) => return invalid_operation(),
            Self::Vector3i(values) if !valid_ints(values, 3) => return invalid_operation(),
            Self::Vector4(values)
            | Self::Rect2(values)
            | Self::Plane(values)
            | Self::Quaternion(values)
            | Self::Color(values)
                if !valid_floats(values, 4) =>
            {
                return invalid_operation();
            }
            Self::Vector4i(values) | Self::Rect2i(values) if !valid_ints(values, 4) => {
                return invalid_operation();
            }
            Self::Transform2d(values) | Self::Aabb(values) if !valid_floats(values, 6) => {
                return invalid_operation();
            }
            Self::Basis(values) if !valid_floats(values, 9) => return invalid_operation(),
            Self::Transform3d(values) if !valid_floats(values, 12) => {
                return invalid_operation();
            }
            Self::Projection(values) if !valid_floats(values, 16) => return invalid_operation(),
            Self::Resource(resource_ref) if !valid_resource_ref(resource_ref) => {
                return invalid_operation();
            }
            Self::Array(values) => {
                if values.len() > 1_000 {
                    return invalid_operation();
                }
                for value in values {
                    value.validate(depth + 1, items)?;
                }
            }
            Self::Dictionary(entries) => {
                if entries.len() > 1_000 {
                    return invalid_operation();
                }
                let mut keys = std::collections::BTreeSet::new();
                for entry in entries {
                    if entry.key.chars().count() > 16_384 || !keys.insert(entry.key.as_str()) {
                        return invalid_operation();
                    }
                    entry.value.validate(depth + 1, items)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionCoordinates {
    pub scene_id: String,
    pub history_id: String,
    pub scene_revision: u64,
    pub operation_seq: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionCoordinates {
    pub transaction_id: String,
    pub scene_id: String,
    pub history_id: String,
    pub scene_revision: u64,
    pub operation_seq: u64,
    pub transaction_seq: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionLimits {
    pub operations: u64,
    pub prepared_records: u64,
    pub applying_per_history: u64,
    pub prepared_ttl_ms: u64,
    pub approval_timeout_ms: u64,
    pub receipt_ttl_ms: u64,
    pub clock_skew_ms: u64,
    pub approval_message_bytes: u64,
    pub preview_bytes: u64,
    pub operation_bytes: u64,
    pub variant_depth: u64,
    pub container_items: u64,
    pub string_characters: u64,
    pub structural_nodes: u64,
    pub journal_records: u64,
    pub journal_bytes: u64,
    pub status_bytes: u64,
    pub request_deadline_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AffectedEntity {
    pub node_id: String,
    pub role: AffectedEntityRole,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AffectedEntityRole {
    Target,
    Parent,
    NewParent,
    Emitter,
    Receiver,
    Created,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionPreview {
    pub operation_kind: OperationKind,
    pub summary: String,
    pub dirty_effect: DirtyEffect,
    pub save_effect: SaveEffect,
    pub preconditions: Vec<String>,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DirtyEffect {
    MarksSceneDirty,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveEffect {
    NotSaved,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareResult {
    pub schema_version: String,
    pub coordinates: TransactionCoordinates,
    pub state: TransactionState,
    pub operation_kind: OperationKind,
    pub risk: Risk,
    pub scope: ApprovalScope,
    pub affected_entities: Vec<AffectedEntity>,
    pub preview: TransactionPreview,
    pub preview_payload_json: String,
    pub preview_digest: String,
    pub created_at_ms: u64,
    pub expires_at_ms: u64,
    pub limits_applied: TransactionLimits,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafeTransactionError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoEligibilityReason {
    Eligible,
    NotCommitted,
    NotNewestAction,
    HistoryUnavailable,
    RevisionMismatch,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UndoEligibility {
    pub eligible: bool,
    pub reason: UndoEligibilityReason,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionOutcome {
    None,
    Applied,
    Committed,
    Undone,
    RolledBack,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionStatus {
    pub schema_version: String,
    pub coordinates: TransactionCoordinates,
    pub state: TransactionState,
    pub operation_kind: OperationKind,
    pub risk: Risk,
    pub scope: ApprovalScope,
    pub preview_digest: String,
    pub current_scene_revision: u64,
    pub current_operation_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<TransactionOutcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<SafeTransactionError>,
    pub undo_eligibility: UndoEligibility,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub committed_entities: Option<Vec<AffectedEntity>>,
    pub updated_at_ms: u64,
    pub limits_applied: TransactionLimits,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalReceipt {
    pub kind: ApprovalReceiptKind,
    pub scope: ApprovalScope,
    pub nonce: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub mac: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum ApprovalReceiptKind {
    #[serde(rename = "mcp_form_v1")]
    McpFormV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalBinding {
    pub project_id: String,
    pub editor_session_id: String,
    pub scene_id: String,
    pub transaction_id: String,
    pub preview_digest: String,
    pub scope: ApprovalScope,
    pub risk: Risk,
    pub scene_revision: u64,
    pub operation_seq: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedApprovalReceipt {
    pub receipt: ApprovalReceipt,
    pub receipt_hash: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionEventReason {
    Prepared,
    ApprovalRequested,
    ApprovalCancelled,
    ApprovalDeclined,
    ApplyStarted,
    NativeActionCommitted,
    ValidationStarted,
    ValidationSucceeded,
    NativeUndoObserved,
    NativeRedoObserved,
    RevisionConflict,
    PreparedExpired,
    PrecommitFailed,
    PostconditionFailedRolledBack,
    ReconciliationRequired,
    ReconciliationCommitted,
    ReconciliationUndone,
    ReconciliationRolledBack,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionEvent {
    pub coordinates: TransactionCoordinates,
    pub previous_state: Option<TransactionState>,
    pub state: TransactionState,
    pub reason: TransactionEventReason,
    pub revisions: godot_codex_semantic_model::RevisionVector,
    pub timestamp_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransactionEventEnvelope {
    protocol_version: String,
    kind: String,
    method: String,
    params: TransactionEvent,
    context: EventContext,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventContext {
    project_id: String,
    editor_session_id: String,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct PrepareParams<'a> {
    idempotency_key: &'a str,
    coordinates: RevisionCoordinates,
    operation: TransactionOperation,
}

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct ApplyParams<'a> {
    transaction_id: &'a str,
    preview_digest: &'a str,
    expected_scene_revision: u64,
    expected_operation_seq: u64,
    approval: ApprovalReceipt,
}

fn ensure_available(session: &Session) -> Result<(), BridgeError> {
    if !matches!(session.protocol_version(), "1.7" | "1.8")
        || !session.capabilities().contains("transaction.scene_v1")
    {
        return Err(BridgeError::CapabilityUnavailable {
            capability: "transaction.scene_v1",
            negotiated_version: session.protocol_version().to_owned(),
        });
    }
    Ok(())
}

pub(crate) async fn prepare(
    session: &mut Session,
    idempotency_key: &str,
    coordinates: RevisionCoordinates,
    operation: TransactionOperation,
) -> Result<PrepareResult, BridgeError> {
    ensure_available(session)?;
    if !valid_prefixed_hex(idempotency_key, "idempotency:")
        || !valid_revision_coordinates(&coordinates)
    {
        return Err(BridgeError::Invalid(
            "transaction prepare coordinates are invalid".to_owned(),
        ));
    }
    operation.validate()?;
    let response = session
        .request(
            "transaction.prepare",
            serde_json::to_value(PrepareParams {
                idempotency_key,
                coordinates,
                operation,
            })?,
        )
        .await?;
    let result: PrepareResult = parse_result(response, MAX_PREVIEW_BYTES * 2)?;
    validate_prepare_result(session, &result)?;
    Ok(result)
}

pub(crate) async fn apply(
    session: &mut Session,
    transaction_id: &str,
    preview_digest: &str,
    expected_scene_revision: u64,
    expected_operation_seq: u64,
    approval: ApprovalReceipt,
) -> Result<TransactionStatus, BridgeError> {
    ensure_available(session)?;
    if !valid_prefixed_hex(transaction_id, "transaction:")
        || !valid_digest(preview_digest)
        || expected_scene_revision > MAX_SAFE_INTEGER
        || expected_operation_seq > MAX_SAFE_INTEGER
    {
        return Err(BridgeError::Invalid(
            "transaction apply coordinates are invalid".to_owned(),
        ));
    }
    let response = session
        .request_with_deadline_and_timeout(
            "transaction.apply",
            serde_json::to_value(ApplyParams {
                transaction_id,
                preview_digest,
                expected_scene_revision,
                expected_operation_seq,
                approval,
            })?,
            REQUEST_DEADLINE,
            REQUEST_DEADLINE,
        )
        .await?;
    let result: TransactionStatus = parse_result(response, MAX_STATUS_BYTES)?;
    validate_status(session, &result)?;
    Ok(result)
}

pub(crate) async fn status(
    session: &mut Session,
    transaction_id: &str,
) -> Result<TransactionStatus, BridgeError> {
    ensure_available(session)?;
    if !valid_prefixed_hex(transaction_id, "transaction:") {
        return Err(BridgeError::Invalid(
            "transaction identifier is invalid".to_owned(),
        ));
    }
    let response = session
        .request(
            "transaction.status",
            json!({"transaction_id": transaction_id}),
        )
        .await?;
    let result: TransactionStatus = parse_result(response, MAX_STATUS_BYTES)?;
    validate_status(session, &result)?;
    Ok(result)
}

pub(crate) async fn undo(
    session: &mut Session,
    transaction_id: &str,
    expected_transaction_seq: u64,
    expected_scene_revision: u64,
    expected_operation_seq: u64,
) -> Result<TransactionStatus, BridgeError> {
    ensure_available(session)?;
    if !valid_prefixed_hex(transaction_id, "transaction:")
        || expected_transaction_seq == 0
        || expected_transaction_seq > MAX_SAFE_INTEGER
        || expected_scene_revision > MAX_SAFE_INTEGER
        || expected_operation_seq > MAX_SAFE_INTEGER
    {
        return Err(BridgeError::Invalid(
            "transaction Undo coordinates are invalid".to_owned(),
        ));
    }
    let response = session
        .request_with_deadline_and_timeout(
            "transaction.undo",
            json!({
                "transaction_id": transaction_id,
                "expected_transaction_seq": expected_transaction_seq,
                "expected_scene_revision": expected_scene_revision,
                "expected_operation_seq": expected_operation_seq,
            }),
            REQUEST_DEADLINE,
            REQUEST_DEADLINE,
        )
        .await?;
    let result: TransactionStatus = parse_result(response, MAX_STATUS_BYTES)?;
    validate_status(session, &result)?;
    Ok(result)
}

pub(crate) async fn next_event(session: &mut Session) -> Result<TransactionEvent, BridgeError> {
    ensure_available(session)?;
    loop {
        let mut value = session.receive_transaction_notification().await?;
        if value
            .pointer("/params/change_set_id")
            .and_then(Value::as_str)
            .is_some()
        {
            // Compound events have their own RPC 1.8 projection. The Sprint 9
            // coordinator must not deserialize or terminate on them; compound
            // state is reconciled through its opaque status endpoint.
            continue;
        }
        normalize_wire_integers(&mut value);
        let envelope: TransactionEventEnvelope = serde_json::from_value(value)?;
        if !matches!(envelope.protocol_version.as_str(), "1.7" | "1.8")
            || envelope.kind != "notification"
            || envelope.method != "transaction.event"
            || envelope.context.project_id != session.project_id()
            || envelope.context.editor_session_id != session.editor_session_id()
        {
            return Err(BridgeError::Invalid(
                "transaction event binding is invalid".to_owned(),
            ));
        }
        validate_coordinates(&envelope.params.coordinates)?;
        if envelope.params.revisions.editor_session_id != session.editor_session_id() {
            return Err(BridgeError::Invalid(
                "transaction event revision binding is invalid".to_owned(),
            ));
        }
        return Ok(envelope.params);
    }
}

pub(crate) fn issue_approval(
    session: &Session,
    binding: &ApprovalBinding,
    issued_at_ms: u64,
) -> Result<SignedApprovalReceipt, BridgeError> {
    ensure_available(session)?;
    if binding.project_id != session.project_id()
        || binding.editor_session_id != session.editor_session_id()
        || !valid_prefixed_hex(&binding.scene_id, "scene:")
        || !(valid_prefixed_hex(&binding.transaction_id, "transaction:")
            || (session.protocol_version() == "1.8"
                && session.capabilities().contains("transaction.change_set_v1")
                && valid_prefixed_hex(&binding.transaction_id, "change-set:")
                && binding.scope == ApprovalScope::ChangeSetAtomic
                && matches!(binding.risk, Risk::Low | Risk::Destructive)))
        || !valid_digest(&binding.preview_digest)
        || binding.scene_revision > MAX_SAFE_INTEGER
        || binding.operation_seq > MAX_SAFE_INTEGER
        || issued_at_ms > MAX_SAFE_INTEGER.saturating_sub(RECEIPT_TTL_MS)
    {
        return Err(BridgeError::Invalid(
            "approval binding is invalid".to_owned(),
        ));
    }
    let expires_at_ms = issued_at_ms + RECEIPT_TTL_MS;
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce)
        .map_err(|error| BridgeError::Invalid(format!("approval nonce failed: {error}")))?;
    let canonical = approval_canonical(binding, issued_at_ms, expires_at_ms, &nonce)?;
    let approval_key = session.transaction_approval_key();
    let mut mac =
        HmacSha256::new_from_slice(&approval_key).expect("HMAC accepts a 32-byte approval key");
    mac.update(&canonical);
    let mac: [u8; 32] = mac.finalize().into_bytes().into();
    let mut receipt_hash_input = canonical;
    receipt_hash_input.extend_from_slice(&mac);
    Ok(SignedApprovalReceipt {
        receipt: ApprovalReceipt {
            kind: ApprovalReceiptKind::McpFormV1,
            scope: binding.scope,
            nonce: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(nonce),
            issued_at_ms,
            expires_at_ms,
            mac: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac),
        },
        receipt_hash: format!("sha256:{:x}", Sha256::digest(receipt_hash_input)),
    })
}

fn approval_canonical(
    binding: &ApprovalBinding,
    issued_at_ms: u64,
    expires_at_ms: u64,
    nonce: &[u8; 32],
) -> Result<Vec<u8>, BridgeError> {
    let mut canonical = b"godot-codex/approval-receipt/v1\0".to_vec();
    for value in [
        binding.project_id.as_str(),
        binding.editor_session_id.as_str(),
        binding.scene_id.as_str(),
        binding.transaction_id.as_str(),
        binding.preview_digest.as_str(),
        binding.scope.as_str(),
        binding.risk.as_str(),
    ] {
        let bytes = value.as_bytes();
        canonical.extend_from_slice(
            &u32::try_from(bytes.len())
                .map_err(|_| BridgeError::Invalid("approval binding is too large".to_owned()))?
                .to_be_bytes(),
        );
        canonical.extend_from_slice(bytes);
    }
    canonical.extend_from_slice(&binding.scene_revision.to_be_bytes());
    canonical.extend_from_slice(&binding.operation_seq.to_be_bytes());
    canonical.extend_from_slice(&issued_at_ms.to_be_bytes());
    canonical.extend_from_slice(&expires_at_ms.to_be_bytes());
    canonical.extend_from_slice(nonce);
    Ok(canonical)
}

fn parse_result<T: for<'de> Deserialize<'de>>(
    response: Value,
    maximum_bytes: usize,
) -> Result<T, BridgeError> {
    let mut result = response
        .get("result")
        .cloned()
        .ok_or_else(|| BridgeError::Invalid("transaction result is missing".to_owned()))?;
    if serde_json::to_vec(&result)?.len() > maximum_bytes {
        return Err(BridgeError::Invalid(
            "transaction result exceeds its safe limit".to_owned(),
        ));
    }
    normalize_wire_integers(&mut result);
    Ok(serde_json::from_value(result)?)
}

fn normalize_wire_integers(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                normalize_wire_integers(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                normalize_wire_integers(value);
            }
        }
        Value::Number(number) if number.is_f64() => {
            let Some(value) = number.as_f64() else {
                return;
            };
            if !value.is_finite() || value.fract() != 0.0 || value.abs() > MAX_SAFE_INTEGER as f64 {
                return;
            }
            *number = if value >= 0.0 {
                serde_json::Number::from(value as u64)
            } else {
                serde_json::Number::from(value as i64)
            };
        }
        _ => {}
    }
}

fn validate_prepare_result(session: &Session, result: &PrepareResult) -> Result<(), BridgeError> {
    validate_coordinates(&result.coordinates)?;
    if result.schema_version != "transaction/1.0"
        || result.state != TransactionState::Previewed
        || result.preview.operation_kind != result.operation_kind
        || !valid_risk(result.operation_kind, result.risk)
        || result.scope != expected_scope(result.operation_kind)
        || result.coordinates.transaction_seq == 0
        || result.preview.summary.is_empty()
        || result.preview.summary.len() > 8_192
        || result.preview.preconditions.len() > 64
        || result
            .preview
            .preconditions
            .iter()
            .any(|value| value.is_empty() || value.len() > 512)
        || result.preview_payload_json.len() > MAX_PREVIEW_BYTES
        || result.affected_entities.is_empty()
        || result.affected_entities.len() > 16
        || result.created_at_ms > MAX_SAFE_INTEGER
        || result.expires_at_ms > MAX_SAFE_INTEGER
        || result.expires_at_ms < result.created_at_ms
        || !valid_digest(&result.preview_digest)
        || format!(
            "sha256:{:x}",
            Sha256::digest(result.preview_payload_json.as_bytes())
        ) != result.preview_digest
        || result.limits_applied != expected_limits()
        || result.affected_entities.iter().any(|entity| {
            !valid_prefixed_hex(&entity.node_id, "node:")
                || !allowed_role(result.operation_kind, entity.role)
        })
        || session.editor_session_id().is_empty()
    {
        return Err(BridgeError::Invalid(
            "transaction prepare result is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn validate_status(session: &Session, result: &TransactionStatus) -> Result<(), BridgeError> {
    validate_coordinates(&result.coordinates)?;
    if result.schema_version != "transaction/1.0"
        || !valid_digest(&result.preview_digest)
        || !valid_risk(result.operation_kind, result.risk)
        || result.scope != expected_scope(result.operation_kind)
        || result.current_scene_revision > MAX_SAFE_INTEGER
        || result.current_operation_seq > MAX_SAFE_INTEGER
        || result.updated_at_ms > MAX_SAFE_INTEGER
        || result.limits_applied != expected_limits()
        || result.error.as_ref().is_some_and(|error| {
            !valid_error_code(&error.code) || !valid_safe_error_message(&error.message)
        })
        || result.committed_entities.as_ref().is_some_and(|entities| {
            entities.is_empty()
                || entities.len() > 16
                || entities.iter().any(|entity| {
                    !valid_prefixed_hex(&entity.node_id, "node:")
                        || !allowed_role(result.operation_kind, entity.role)
                })
        })
        || session.editor_session_id().is_empty()
    {
        return Err(BridgeError::Invalid(
            "transaction status result is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn valid_risk(kind: OperationKind, risk: Risk) -> bool {
    match kind {
        OperationKind::DeleteNode
        | OperationKind::ReparentNode
        | OperationKind::SetProperty
        | OperationKind::DetachScript
        | OperationKind::DisconnectSignal => risk == Risk::Destructive,
        // Attaching over an existing script is a destructive replacement.
        OperationKind::AttachScript => matches!(risk, Risk::Write | Risk::Destructive),
        OperationKind::CreateNode | OperationKind::ConnectSignal => risk == Risk::Write,
    }
}

fn expected_scope(kind: OperationKind) -> ApprovalScope {
    match kind {
        OperationKind::CreateNode => ApprovalScope::SceneNodeCreate,
        OperationKind::DeleteNode => ApprovalScope::SceneNodeDelete,
        OperationKind::ReparentNode => ApprovalScope::SceneNodeReparent,
        OperationKind::SetProperty => ApprovalScope::ScenePropertySet,
        OperationKind::AttachScript => ApprovalScope::SceneScriptAttach,
        OperationKind::DetachScript => ApprovalScope::SceneScriptDetach,
        OperationKind::ConnectSignal => ApprovalScope::SceneSignalConnect,
        OperationKind::DisconnectSignal => ApprovalScope::SceneSignalDisconnect,
    }
}

fn allowed_role(kind: OperationKind, role: AffectedEntityRole) -> bool {
    match kind {
        OperationKind::CreateNode => {
            matches!(
                role,
                AffectedEntityRole::Parent | AffectedEntityRole::Created
            )
        }
        OperationKind::DeleteNode
        | OperationKind::SetProperty
        | OperationKind::AttachScript
        | OperationKind::DetachScript => role == AffectedEntityRole::Target,
        OperationKind::ReparentNode => {
            matches!(
                role,
                AffectedEntityRole::Target | AffectedEntityRole::NewParent
            )
        }
        OperationKind::ConnectSignal | OperationKind::DisconnectSignal => {
            matches!(
                role,
                AffectedEntityRole::Emitter | AffectedEntityRole::Receiver
            )
        }
    }
}

fn valid_error_code(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn valid_safe_error_message(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || value.contains("/Users/")
        || value.contains("/home/")
        || value.contains('\\')
        || value.starts_with('/')
    {
        return false;
    }
    let bytes = value.as_bytes();
    !(bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'/')
}

fn expected_limits() -> TransactionLimits {
    TransactionLimits {
        operations: 1,
        prepared_records: 64,
        applying_per_history: 1,
        prepared_ttl_ms: 300_000,
        approval_timeout_ms: 120_000,
        receipt_ttl_ms: 30_000,
        clock_skew_ms: 2_000,
        approval_message_bytes: 8_192,
        preview_bytes: 65_536,
        operation_bytes: 65_536,
        variant_depth: 8,
        container_items: 1_000,
        string_characters: 16_384,
        structural_nodes: 1_000,
        journal_records: 1_024,
        journal_bytes: 8_388_608,
        status_bytes: 65_536,
        request_deadline_ms: 5_000,
    }
}

fn valid_revision_coordinates(value: &RevisionCoordinates) -> bool {
    valid_prefixed_hex(&value.scene_id, "scene:")
        && valid_prefixed_hex(&value.history_id, "history:")
        && value.scene_revision <= MAX_SAFE_INTEGER
        && value.operation_seq <= MAX_SAFE_INTEGER
}

fn validate_coordinates(value: &TransactionCoordinates) -> Result<(), BridgeError> {
    if valid_prefixed_hex(&value.transaction_id, "transaction:")
        && valid_prefixed_hex(&value.scene_id, "scene:")
        && valid_prefixed_hex(&value.history_id, "history:")
        && value.scene_revision <= MAX_SAFE_INTEGER
        && value.operation_seq <= MAX_SAFE_INTEGER
        && (1..=MAX_SAFE_INTEGER).contains(&value.transaction_seq)
    {
        Ok(())
    } else {
        Err(BridgeError::Invalid(
            "transaction result coordinates are invalid".to_owned(),
        ))
    }
}

fn valid_prefixed_hex(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|tail| {
        tail.len() == 32
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|tail| {
        tail.len() == 64
            && tail
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn valid_godot_type(value: &str) -> bool {
    (1..=256).contains(&value.len())
        && value.bytes().enumerate().all(|(index, byte)| {
            if index == 0 {
                byte == b'_' || byte.is_ascii_alphabetic()
            } else {
                byte == b'_' || byte.is_ascii_alphanumeric()
            }
        })
}

fn valid_node_name(value: &str) -> bool {
    (1..=512).contains(&value.chars().count())
        && !value
            .chars()
            .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':'))
}

fn valid_member_name(value: &str) -> bool {
    (1..=512).contains(&value.chars().count()) && !value.chars().any(char::is_control)
}

fn valid_resource_ref(value: &TransactionResourceRef) -> bool {
    match value {
        TransactionResourceRef::Uid(value) => {
            value.uid.len() <= 128
                && value.uid.strip_prefix("uid://").is_some_and(|tail| {
                    !tail.is_empty()
                        && tail
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
                })
        }
        TransactionResourceRef::Path(value) => {
            value.uid_missing
                && (7..=1_024).contains(&value.path.len())
                && value.path.starts_with("res://")
                && !value.path.contains('\\')
                && !value.path.contains('?')
                && !value.path.contains('#')
                && !value.path.chars().any(char::is_control)
                && !value
                    .path
                    .split('/')
                    .any(|component| matches!(component, "." | ".."))
        }
    }
}

fn invalid_operation<T>() -> Result<T, BridgeError> {
    Err(BridgeError::Invalid(
        "transaction operation is outside the safe write projection".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_canonical_matches_the_frozen_cpp_vector() {
        let binding = ApprovalBinding {
            project_id:
                "project:sha256:94cc0c8419cfeecadbe62dfba93b8949acf1d9bcc48ed602b194d0dc4c53bdcd"
                    .to_owned(),
            editor_session_id: "editor:0123456789abcdef0123456789abcdef".to_owned(),
            scene_id: format!("scene:{}", "1".repeat(32)),
            transaction_id: format!("transaction:{}", "2".repeat(32)),
            preview_digest: format!("sha256:{}", "3".repeat(64)),
            scope: ApprovalScope::SceneNodeDelete,
            risk: Risk::Destructive,
            scene_revision: 7,
            operation_seq: 11,
        };
        let nonce = std::array::from_fn(|index| 0x20 + u8::try_from(index).unwrap());
        let canonical =
            approval_canonical(&binding, 1_784_690_000_000, 1_784_690_030_000, &nonce).unwrap();
        assert!(canonical.starts_with(b"godot-codex/approval-receipt/v1\0"));
        assert!(canonical.ends_with(&nonce));
        let token: [u8; 32] = std::array::from_fn(|index| u8::try_from(index).unwrap());
        let mut key_hmac = HmacSha256::new_from_slice(&token).unwrap();
        key_hmac.update(b"godot-codex/approval-key/v1");
        let key: [u8; 32] = key_hmac.finalize().into_bytes().into();
        assert_eq!(
            key.iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>(),
            "bd5c8a753ee9611953024d9dc26ae4cbf6a5b7d451534f6429e150b23b201d6d"
        );
        let mut receipt_hmac = HmacSha256::new_from_slice(&key).unwrap();
        receipt_hmac.update(&canonical);
        assert_eq!(
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(receipt_hmac.finalize().into_bytes()),
            "UA22qIiXEf_HN0OEg83xEyJjdrSlNZBop2DYeAMaeEM"
        );
    }

    #[test]
    fn operation_projection_rejects_unsafe_values() {
        let operation = TransactionOperation::SetProperty {
            node_id: format!("node:{}", "a".repeat(32)),
            property: "position".to_owned(),
            value: WritableVariant::Float(f64::NAN),
        };
        assert!(operation.validate().is_err());

        let operation = TransactionOperation::AttachScript {
            node_id: format!("node:{}", "a".repeat(32)),
            script_ref: TransactionResourceRef::Path(TransactionPathRef {
                uid_missing: true,
                path: "res://../secret.gd".to_owned(),
            }),
        };
        assert!(operation.validate().is_err());

        let duplicate_dictionary = TransactionOperation::SetProperty {
            node_id: format!("node:{}", "a".repeat(32)),
            property: "metadata".to_owned(),
            value: WritableVariant::Dictionary(vec![
                WritableDictionaryEntry {
                    key: "duplicate".to_owned(),
                    value: WritableVariant::Bool(true),
                },
                WritableDictionaryEntry {
                    key: "duplicate".to_owned(),
                    value: WritableVariant::Bool(false),
                },
            ]),
        };
        assert!(duplicate_dictionary.validate().is_err());

        let unsafe_integer = TransactionOperation::SetProperty {
            node_id: format!("node:{}", "a".repeat(32)),
            property: "count".to_owned(),
            value: WritableVariant::Int(9_007_199_254_740_992),
        };
        assert!(unsafe_integer.validate().is_err());
    }

    #[test]
    fn operation_risk_matches_destructive_and_replacement_semantics() {
        assert!(valid_risk(OperationKind::DeleteNode, Risk::Destructive));
        assert!(valid_risk(OperationKind::DetachScript, Risk::Destructive));
        assert!(valid_risk(
            OperationKind::DisconnectSignal,
            Risk::Destructive
        ));
        assert!(valid_risk(OperationKind::ReparentNode, Risk::Destructive));
        assert!(valid_risk(OperationKind::SetProperty, Risk::Destructive));
        assert!(valid_risk(OperationKind::AttachScript, Risk::Write));
        assert!(valid_risk(OperationKind::AttachScript, Risk::Destructive));
        assert!(!valid_risk(OperationKind::DetachScript, Risk::Write));
        assert!(!valid_risk(OperationKind::CreateNode, Risk::Destructive));
    }

    #[test]
    fn safe_transaction_errors_reject_host_paths_and_unbounded_codes() {
        assert!(valid_error_code("stale_scene_revision"));
        assert!(!valid_error_code("../private"));
        assert!(valid_safe_error_message(
            "The requested scene revision is stale."
        ));
        assert!(!valid_safe_error_message(
            "Failed at /Users/private/project/main.tscn"
        ));
        assert!(!valid_safe_error_message(r"Failed at C:\private\main.tscn"));
    }

    #[test]
    fn expected_limits_match_write_001() {
        let limits = expected_limits();
        assert_eq!(limits.prepared_records, 64);
        assert_eq!(limits.journal_records, 1_024);
        assert_eq!(limits.journal_bytes, 8 * 1024 * 1024);
        assert_eq!(limits.request_deadline_ms, 5_000);
    }

    #[test]
    fn godot_integral_variant_numbers_normalize_before_typed_projection() {
        let mut value = serde_json::json!({
            "coordinates": {
                "scene_revision": 7.0,
                "operation_seq": 8.0,
                "transaction_seq": 1.0
            },
            "limits": [64.0, 65_536.0],
            "fractional": 1.5,
            "unsafe": 9_007_199_254_740_992.0
        });
        normalize_wire_integers(&mut value);
        assert_eq!(value["coordinates"]["scene_revision"], 7);
        assert_eq!(value["coordinates"]["operation_seq"], 8);
        assert_eq!(value["coordinates"]["transaction_seq"], 1);
        assert_eq!(value["limits"], serde_json::json!([64, 65_536]));
        assert_eq!(value["fractional"], 1.5);
        assert!(value["unsafe"].as_f64().is_some());
    }

    #[test]
    fn writable_variant_json_is_the_wire_shape() {
        assert_eq!(
            serde_json::to_value(WritableVariant::Vector2(vec![1.0, 2.0])).unwrap(),
            json!({"type": "vector2", "value": [1.0, 2.0]})
        );
        assert_eq!(
            serde_json::to_value(WritableVariant::Nil).unwrap(),
            json!({"type": "nil"})
        );
    }

    #[test]
    fn rpc_1_7_transaction_golden_and_negative_vectors_match_the_strict_dtos() {
        let fixture: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../schemas/codex_bridge/v1/fixtures/test-vectors/transaction.json"
        )))
        .unwrap();
        let prepare: PrepareResult =
            serde_json::from_value(fixture["prepare_result"].clone()).unwrap();
        assert_eq!(
            format!(
                "sha256:{:x}",
                Sha256::digest(prepare.preview_payload_json.as_bytes())
            ),
            prepare.preview_digest
        );
        assert_eq!(prepare.limits_applied, expected_limits());
        let status: TransactionStatus =
            serde_json::from_value(fixture["status_result"].clone()).unwrap();
        assert_eq!(
            status.coordinates.transaction_id,
            prepare.coordinates.transaction_id
        );
        let event: TransactionEventEnvelope =
            serde_json::from_value(fixture["event"].clone()).unwrap();
        assert!(event.params.coordinates.transaction_seq >= prepare.coordinates.transaction_seq);
        let operations = fixture["operations"].as_array().unwrap();
        assert_eq!(operations.len(), 8);
        for value in operations {
            let operation: TransactionOperation = serde_json::from_value(value.clone()).unwrap();
            operation.validate().unwrap();
        }

        let invalid: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../schemas/codex_bridge/v1/fixtures/test-vectors/transaction-invalid.json"
        )))
        .unwrap();
        assert!(
            serde_json::from_value::<TransactionOperation>(invalid["unknown_operation"].clone())
                .is_err()
        );
        for key in [
            "raw_object_id",
            "absolute_script_path",
            "non_persistent_signal",
            "reference_counted_signal",
        ] {
            let operation: TransactionOperation =
                serde_json::from_value(invalid[key].clone()).unwrap();
            assert!(operation.validate().is_err(), "{key} must fail closed");
        }
        assert!(
            serde_json::from_value::<WritableVariant>(invalid["unsafe_variant"].clone()).is_err()
        );
        for key in ["wrong_math_arity", "absolute_node_path"] {
            let value: WritableVariant = serde_json::from_value(invalid[key].clone()).unwrap();
            let mut items = 0;
            assert!(
                value.validate(0, &mut items).is_err(),
                "{key} must fail closed"
            );
        }
        assert!(
            serde_json::from_value::<TransactionEvent>(invalid["extra_event_field"].clone())
                .is_err()
        );
        assert!(
            serde_json::from_value::<AffectedEntity>(invalid["committed_entity_native_id"].clone())
                .is_err()
        );
    }
}
