use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::json::{parse_strict, parse_strict_object};

const MAX_ENCODED_BYTES: usize = 65_536;
const MAX_VARIANT_DEPTH: usize = 8;
const MAX_CONTAINER_ITEMS: usize = 1_000;
const MAX_STRING_CHARACTERS: usize = 16_384;

fn bundle_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/codex_bridge/v1")
}

fn strict_json(path: &Path) -> Value {
    parse_strict(&fs::read(path).unwrap_or_else(|error| {
        panic!("cannot read transaction vector {}: {error}", path.display())
    }))
    .unwrap_or_else(|error| panic!("transaction vector {} is invalid: {error}", path.display()))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionLimits {
    pub operations: usize,
    pub prepared_records: usize,
    pub applying_per_history: usize,
    pub prepared_ttl_ms: u64,
    pub approval_timeout_ms: u64,
    pub receipt_ttl_ms: u64,
    pub clock_skew_ms: u64,
    pub approval_message_bytes: usize,
    pub preview_bytes: usize,
    pub operation_bytes: usize,
    pub variant_depth: usize,
    pub container_items: usize,
    pub string_characters: usize,
    pub structural_nodes: usize,
    pub journal_records: usize,
    pub journal_bytes: usize,
    pub status_bytes: usize,
    pub request_deadline_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneReadiness {
    NotEvaluated,
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalReadiness {
    Available,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessReason {
    Ready,
    TransactionCoordinatorUnavailable,
    EditorUnavailable,
    SceneNotOpen,
    ApprovalUnavailable,
    TransactionBusy,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionReadiness {
    pub scene_state: SceneReadiness,
    pub approval_state: ApprovalReadiness,
    pub busy: bool,
    pub reason: ReadinessReason,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityReadiness {
    Ready,
    Unavailable,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionCapability {
    pub name: String,
    pub version: String,
    pub readiness: CapabilityReadiness,
    pub transaction_readiness: TransactionReadiness,
    pub limits: TransactionLimits,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UidRef {
    pub uid: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PathRef {
    pub uid_missing: bool,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ResourceRef {
    Uid(UidRef),
    Path(PathRef),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
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
    Vector2i(Vec<f64>),
    Vector3(Vec<f64>),
    Vector3i(Vec<f64>),
    Vector4(Vec<f64>),
    Vector4i(Vec<f64>),
    Rect2(Vec<f64>),
    Rect2i(Vec<f64>),
    Transform2d(Vec<f64>),
    Plane(Vec<f64>),
    Quaternion(Vec<f64>),
    Aabb(Vec<f64>),
    Basis(Vec<f64>),
    Transform3d(Vec<f64>),
    Projection(Vec<f64>),
    Color(Vec<f64>),
    Resource(ResourceRef),
    Array(Vec<WritableVariant>),
    Dictionary(Vec<DictionaryEntry>),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DictionaryEntry {
    pub key: String,
    pub value: WritableVariant,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
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
        script_ref: ResourceRef,
    },
    DetachScript {
        node_id: String,
    },
    ConnectSignal {
        emitter_node_id: String,
        signal: String,
        receiver_node_id: String,
        method: String,
        flags: u32,
        unbinds: usize,
        binds: Vec<WritableVariant>,
    },
    DisconnectSignal {
        emitter_node_id: String,
        signal: String,
        receiver_node_id: String,
        method: String,
        flags: u32,
        unbinds: usize,
        binds: Vec<WritableVariant>,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionCoordinates {
    pub scene_id: String,
    pub history_id: String,
    pub scene_revision: u64,
    pub operation_seq: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareParams {
    pub idempotency_key: String,
    pub coordinates: RevisionCoordinates,
    pub operation: TransactionOperation,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalReceipt {
    pub kind: String,
    pub scope: String,
    pub nonce: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
    pub mac: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyParams {
    pub transaction_id: String,
    pub preview_digest: String,
    pub expected_scene_revision: u64,
    pub expected_operation_seq: u64,
    pub approval: ApprovalReceipt,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StatusParams {
    pub transaction_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UndoParams {
    pub transaction_id: String,
    pub expected_transaction_seq: u64,
    pub expected_scene_revision: u64,
    pub expected_operation_seq: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
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

fn legal_transition(from: TransactionState, to: TransactionState) -> bool {
    use TransactionState::{
        Applied, Applying, AwaitingApproval, Committed, Conflicted, Expired, Failed,
        FailedRolledBack, InDoubt, Preparing, Previewed, Rejected, Undone, Validating,
    };
    matches!(
        (from, to),
        (Preparing, Previewed | Failed)
            | (
                Previewed,
                AwaitingApproval | Conflicted | Expired | Rejected
            )
            | (AwaitingApproval, Applying | Conflicted | Expired | Rejected)
            | (Applying, Applied | Failed | FailedRolledBack | InDoubt)
            | (Applied, Validating)
            | (Validating, Committed | FailedRolledBack | InDoubt)
            | (Committed, Undone)
            | (Undone, Committed)
            | (InDoubt, Committed | Undone | FailedRolledBack)
    )
}

fn validate_variant(value: &Value, depth: usize, items: &mut usize) -> Result<(), &'static str> {
    if depth > MAX_VARIANT_DEPTH {
        return Err("property_value_unsupported");
    }
    let object = value.as_object().ok_or("property_value_unsupported")?;
    let kind = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or("property_value_unsupported")?;
    match kind {
        "string" | "string_name" | "node_path" => {
            let text = object
                .get("value")
                .and_then(Value::as_str)
                .ok_or("property_value_unsupported")?;
            if text.chars().count() > MAX_STRING_CHARACTERS {
                return Err("property_value_unsupported");
            }
        }
        "array" => {
            let values = object
                .get("value")
                .and_then(Value::as_array)
                .ok_or("property_value_unsupported")?;
            *items += values.len();
            if *items > MAX_CONTAINER_ITEMS {
                return Err("property_value_unsupported");
            }
            for child in values {
                validate_variant(child, depth + 1, items)?;
            }
        }
        "dictionary" => {
            let entries = object
                .get("value")
                .and_then(Value::as_array)
                .ok_or("property_value_unsupported")?;
            *items += entries.len();
            if *items > MAX_CONTAINER_ITEMS {
                return Err("property_value_unsupported");
            }
            for entry in entries {
                let entry = entry.as_object().ok_or("property_value_unsupported")?;
                let key = entry
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or("property_value_unsupported")?;
                if key.chars().count() > MAX_STRING_CHARACTERS {
                    return Err("property_value_unsupported");
                }
                validate_variant(
                    entry.get("value").ok_or("property_value_unsupported")?,
                    depth + 1,
                    items,
                )?;
            }
        }
        "nil" | "bool" | "int" | "float" | "vector2" | "vector2i" | "vector3" | "vector3i"
        | "vector4" | "vector4i" | "rect2" | "rect2i" | "transform2d" | "plane" | "quaternion"
        | "aabb" | "basis" | "transform3d" | "projection" | "color" | "resource" => {}
        _ => return Err("property_value_unsupported"),
    }
    Ok(())
}

#[test]
fn transaction_contract_deserializes_every_operation_and_method_dto() {
    let vector = strict_json(&bundle_root().join("fixtures/test-vectors/transaction.json"));
    let capability: TransactionCapability =
        serde_json::from_value(vector["capability"].clone()).expect("capability DTO");
    assert_eq!(
        serde_json::to_value(capability).unwrap(),
        vector["capability"]
    );

    let operations = vector["operations"].as_array().expect("operations");
    assert_eq!(operations.len(), 8);
    for operation in operations {
        let dto: TransactionOperation =
            serde_json::from_value(operation.clone()).expect("operation DTO");
        assert_eq!(serde_json::to_value(dto).unwrap(), *operation);
    }

    let prepare: PrepareParams =
        serde_json::from_value(vector["prepare_request"]["params"].clone())
            .expect("prepare params DTO");
    assert_eq!(
        serde_json::to_value(prepare).unwrap(),
        vector["prepare_request"]["params"]
    );
    let apply: ApplyParams = serde_json::from_value(vector["apply_request"]["params"].clone())
        .expect("apply params DTO");
    assert_eq!(
        serde_json::to_value(apply).unwrap(),
        vector["apply_request"]["params"]
    );
    let status: StatusParams = serde_json::from_value(vector["status_request"]["params"].clone())
        .expect("status params DTO");
    assert_eq!(
        serde_json::to_value(status).unwrap(),
        vector["status_request"]["params"]
    );
    let undo: UndoParams =
        serde_json::from_value(vector["undo_request"]["params"].clone()).expect("undo params DTO");
    assert_eq!(
        serde_json::to_value(undo).unwrap(),
        vector["undo_request"]["params"]
    );
}

#[test]
fn transaction_preview_digest_binds_exact_bytes() {
    let vector = strict_json(&bundle_root().join("fixtures/test-vectors/transaction.json"));
    let payload = vector["preview_payload_json"]
        .as_str()
        .expect("preview payload");
    assert_eq!(payload.len() as u64, vector["preview_payload_bytes"]);
    assert_eq!(
        format!("sha256:{:x}", Sha256::digest(payload.as_bytes())),
        vector["preview_digest"]
    );
    assert_eq!(vector["prepare_result"]["preview_payload_json"], payload);
    assert_eq!(
        vector["prepare_result"]["preview_digest"],
        vector["preview_digest"]
    );
}

#[test]
fn transaction_lifecycle_matrix_is_closed() {
    let vector = strict_json(&bundle_root().join("fixtures/test-vectors/transaction.json"));
    let legal: Vec<[TransactionState; 2]> =
        serde_json::from_value(vector["legal_transitions"].clone()).expect("legal transitions");
    let illegal: Vec<[TransactionState; 2]> =
        serde_json::from_value(vector["illegal_transitions"].clone()).expect("illegal transitions");
    assert!(legal.iter().all(|pair| legal_transition(pair[0], pair[1])));
    assert!(illegal
        .iter()
        .all(|pair| !legal_transition(pair[0], pair[1])));
    assert_eq!(legal.len(), 23);
    assert_eq!(illegal.len(), 5);
}

#[test]
fn transaction_semantic_limits_fail_closed() {
    let vector = strict_json(&bundle_root().join("fixtures/test-vectors/transaction.json"));
    for operation in vector["operations"].as_array().expect("operations") {
        assert!(serde_json::to_vec(operation).unwrap().len() <= MAX_ENCODED_BYTES);
    }
    assert!(serde_json::to_vec(&vector["status_result"]).unwrap().len() <= MAX_ENCODED_BYTES);

    let mut nested = serde_json::json!({"type": "nil"});
    for _ in 0..MAX_VARIANT_DEPTH {
        nested = serde_json::json!({"type": "array", "value": [nested]});
    }
    assert_eq!(
        validate_variant(&nested, 1, &mut 0),
        Err("property_value_unsupported")
    );
    let oversized_items = serde_json::json!({
        "type": "array",
        "value": (0..=MAX_CONTAINER_ITEMS).map(|_| serde_json::json!({"type": "nil"})).collect::<Vec<_>>()
    });
    assert_eq!(
        validate_variant(&oversized_items, 1, &mut 0),
        Err("property_value_unsupported")
    );
    let oversized_string = serde_json::json!({
        "type": "string",
        "value": "x".repeat(MAX_STRING_CHARACTERS + 1)
    });
    assert_eq!(
        validate_variant(&oversized_string, 1, &mut 0),
        Err("property_value_unsupported")
    );
}

#[test]
fn transaction_receipt_shape_matches_s9_approval_contract() {
    let vector = strict_json(&bundle_root().join("fixtures/test-vectors/transaction.json"));
    let approval = vector["apply_request"]["params"]["approval"]
        .as_object()
        .expect("approval");
    let receipt = strict_json(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/approval_boundary_contract/receipt-vectors.json"),
    );
    let s9_receipt = receipt["positive"]["receipt"]
        .as_object()
        .expect("S9 receipt");
    assert_eq!(
        approval.keys().collect::<Vec<_>>(),
        s9_receipt.keys().collect::<Vec<_>>()
    );
    assert_eq!(approval["kind"], "mcp_form_v1");
    assert_eq!(approval["nonce"].as_str().unwrap().len(), 43);
    assert_eq!(approval["mac"].as_str().unwrap().len(), 43);
}

#[test]
fn transaction_strict_json_rejects_duplicate_and_non_finite_values() {
    assert!(parse_strict_object(br#"{"transaction_id":"transaction:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","transaction_id":"transaction:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#).is_err());
    assert!(parse_strict_object(br#"{"value":NaN}"#).is_err());
    assert!(parse_strict_object(br#"{"value":Infinity}"#).is_err());
}
