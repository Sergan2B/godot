use std::collections::HashSet;
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
    let exact_keys = |expected: &[&str]| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    };
    if kind == "nil" {
        return exact_keys(&["type"])
            .then_some(())
            .ok_or("property_value_unsupported");
    }
    if !exact_keys(&["type", "value"]) {
        return Err("property_value_unsupported");
    }
    match kind {
        "string" | "string_name" | "node_path" => {
            let text = object
                .get("value")
                .and_then(Value::as_str)
                .ok_or("property_value_unsupported")?;
            if text.chars().count() > MAX_STRING_CHARACTERS {
                return Err("property_value_unsupported");
            }
            if kind == "node_path" && text.starts_with('/') {
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
            let mut keys = HashSet::new();
            for entry in entries {
                let entry = entry.as_object().ok_or("property_value_unsupported")?;
                if entry.len() != 2 || !entry.contains_key("key") || !entry.contains_key("value") {
                    return Err("property_value_unsupported");
                }
                let key = entry
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or("property_value_unsupported")?;
                if key.chars().count() > MAX_STRING_CHARACTERS {
                    return Err("property_value_unsupported");
                }
                if !keys.insert(key) {
                    return Err("property_value_unsupported");
                }
                validate_variant(
                    entry.get("value").ok_or("property_value_unsupported")?,
                    depth + 1,
                    items,
                )?;
            }
        }
        "bool" if object["value"].is_boolean() => {}
        "int"
            if object["value"]
                .as_i64()
                .is_some_and(|number| number.unsigned_abs() <= 9_007_199_254_740_991) => {}
        "float" if object["value"].as_f64().is_some_and(f64::is_finite) => {}
        "resource" if object["value"].is_object() => {}
        "vector2" | "vector2i" | "vector3" | "vector3i" | "vector4" | "vector4i" | "rect2"
        | "rect2i" | "transform2d" | "plane" | "quaternion" | "aabb" | "basis" | "transform3d"
        | "projection" | "color" => {
            let arity = match kind {
                "vector2" | "vector2i" => 2,
                "vector3" | "vector3i" => 3,
                "vector4" | "vector4i" | "rect2" | "rect2i" | "plane" | "quaternion" | "color" => 4,
                "transform2d" | "aabb" => 6,
                "basis" => 9,
                "transform3d" => 12,
                "projection" => 16,
                _ => unreachable!(),
            };
            let components = object["value"]
                .as_array()
                .ok_or("property_value_unsupported")?;
            if components.len() != arity
                || !components
                    .iter()
                    .all(|value| value.as_f64().is_some_and(f64::is_finite))
            {
                return Err("property_value_unsupported");
            }
            if matches!(kind, "vector2i" | "vector3i" | "vector4i" | "rect2i")
                && !components.iter().all(Value::is_i64)
            {
                return Err("property_value_unsupported");
            }
        }
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
#[allow(clippy::too_many_lines)]
fn transaction_canonical_preview_vectors_match_all_operations() {
    let vector = strict_json(&bundle_root().join("fixtures/test-vectors/transaction.json"));
    let operations = vector["operations"].as_array().expect("operations");
    let expected = vector["canonical_preview_digests"]
        .as_array()
        .expect("canonical preview digests");
    assert_eq!(operations.len(), 8);
    assert_eq!(expected.len(), operations.len());

    for (operation, expected) in operations.iter().zip(expected) {
        let kind = operation["kind"].as_str().expect("operation kind");
        let (risk, scope, summary, specific_precondition) = match kind {
            "create_node" => (
                "write",
                "scene.node.create",
                "Create Node2D named CreatedActor under the resolved parent.",
                "parent remains editable and the requested name remains available",
            ),
            "delete_node" => (
                "destructive",
                "scene.node.delete",
                "Delete the resolved node and its bounded subtree (3 nodes).",
                "target remains editable with the same owner and bounded subtree",
            ),
            "reparent_node" => (
                "destructive",
                "scene.node.reparent",
                "Move the resolved node under the resolved new parent.",
                "target and new parent remain editable and outside a cycle",
            ),
            "set_property" => (
                "destructive",
                "scene.property.set",
                "Replace property position on the resolved node.",
                "native property remains editor-writable without a custom getter",
            ),
            "attach_script" => (
                "destructive",
                "scene.script.attach",
                "Replace the script attached to the resolved node.",
                "target script identity and compatible script resource remain unchanged",
            ),
            "detach_script" => (
                "destructive",
                "scene.script.detach",
                "Detach the script from the resolved node.",
                "target remains editable with the same attached script",
            ),
            "connect_signal" => (
                "write",
                "scene.signal.connect",
                "Connect signal health_changed to method _on_health_changed on the resolved receiver.",
                "signal endpoints and exact connection state remain unchanged",
            ),
            "disconnect_signal" => (
                "destructive",
                "scene.signal.disconnect",
                "Disconnect signal health_changed from method _on_health_changed on the resolved receiver.",
                "signal endpoints and exact connection state remain unchanged",
            ),
            _ => panic!("unexpected operation {kind}"),
        };
        let affected_entities = match kind {
            "create_node" => serde_json::json!([
                {"node_id":"node:11111111111111111111111111111111","role":"parent"}
            ]),
            "reparent_node" => serde_json::json!([
                {"node_id":"node:22222222222222222222222222222222","role":"target"},
                {"node_id":"node:33333333333333333333333333333333","role":"new_parent"}
            ]),
            "connect_signal" | "disconnect_signal" => serde_json::json!([
                {"node_id":"node:22222222222222222222222222222222","role":"emitter"},
                {"node_id":"node:33333333333333333333333333333333","role":"receiver"}
            ]),
            _ => serde_json::json!([
                {"node_id":"node:22222222222222222222222222222222","role":"target"}
            ]),
        };
        let operation_bytes = serde_json::to_vec(operation).expect("canonical operation");
        let mut operation_hasher = Sha256::new();
        operation_hasher.update(b"godot-codex-preview-operation/v1\n");
        operation_hasher.update(&operation_bytes);
        let operation_digest = format!("sha256:{:x}", operation_hasher.finalize());
        let redacted_operation = match kind {
            "set_property" => serde_json::json!({
                "kind": kind,
                "node_id": operation["node_id"],
                "property": operation["property"],
                "value_summary": {"type":"value", "redacted":true, "digest":operation_digest}
            }),
            "attach_script" => serde_json::json!({
                "kind": kind,
                "node_id": operation["node_id"],
                "script_summary": {"type":"script", "redacted":true, "digest":operation_digest}
            }),
            "connect_signal" | "disconnect_signal" => serde_json::json!({
                "kind": kind,
                "emitter_node_id": operation["emitter_node_id"],
                "signal": operation["signal"],
                "receiver_node_id": operation["receiver_node_id"],
                "method": operation["method"],
                "flags": operation["flags"],
                "unbinds": operation["unbinds"],
                "binds_summary": {"type":"binds", "redacted":true, "digest":operation_digest}
            }),
            _ => operation.clone(),
        };
        let payload = serde_json::json!({
            "schema_version": "canonical-transaction-preview/1.1",
            "coordinates": {
                "transaction_id": "transaction:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "scene_id": "scene:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "history_id": "history:cccccccccccccccccccccccccccccccc",
                "scene_revision": 7,
                "operation_seq": 11,
                "transaction_seq": 2
            },
            "operation": redacted_operation,
            "operation_digest": operation_digest,
            "risk": risk,
            "scope": scope,
            "affected_entities": affected_entities,
            "preview": {
                "operation_kind": kind,
                "summary": summary,
                "dirty_effect": "marks_scene_dirty",
                "save_effect": "not_saved",
                "preconditions": [
                    "scene and native history revisions remain unchanged",
                    specific_precondition
                ],
                "truncated": false
            },
            "created_at_ms": 1_784_690_000_000_u64,
            "expires_at_ms": 1_784_690_300_000_u64,
            "limits_applied": vector["capability"]["limits"].clone()
        });
        let bytes = serde_json::to_vec(&payload).expect("canonical payload");
        assert_eq!(kind, expected["operation_kind"]);
        assert_eq!(bytes.len() as u64, expected["preview_payload_bytes"]);
        assert_eq!(
            format!("sha256:{:x}", Sha256::digest(&bytes)),
            expected["preview_digest"]
        );
    }
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
    let wrong_arity = serde_json::json!({"type":"vector2", "value":[1.0, 2.0, 3.0]});
    assert_eq!(
        validate_variant(&wrong_arity, 1, &mut 0),
        Err("property_value_unsupported")
    );
    let absolute_node_path = serde_json::json!({"type":"node_path", "value":"/root/native"});
    assert_eq!(
        validate_variant(&absolute_node_path, 1, &mut 0),
        Err("property_value_unsupported")
    );
    let duplicate_dictionary = serde_json::json!({
        "type":"dictionary",
        "value":[
            {"key":"duplicate", "value":{"type":"nil"}},
            {"key":"duplicate", "value":{"type":"bool", "value":true}}
        ]
    });
    assert_eq!(
        validate_variant(&duplicate_dictionary, 1, &mut 0),
        Err("property_value_unsupported")
    );
}

#[test]
fn transaction_schema_rejects_wrong_arity_absolute_paths_and_unsafe_signal_flags() {
    for value in [
        serde_json::json!({"type":"vector2", "value":[1.0, 2.0, 3.0]}),
        serde_json::json!({"type":"node_path", "value":"/root/native"}),
    ] {
        assert!(crate::bundle::validate_instance(
            "transaction.schema.json#/$defs/writableVariant",
            &value
        )
        .is_err());
    }
    for flags in [0, 8, 10] {
        let operation = serde_json::json!({
            "kind":"connect_signal",
            "emitter_node_id":"node:22222222222222222222222222222222",
            "signal":"pulse",
            "receiver_node_id":"node:33333333333333333333333333333333",
            "method":"_on_pulse",
            "flags":flags,
            "unbinds":0,
            "binds":[]
        });
        assert!(crate::bundle::validate_instance(
            "transaction.schema.json#/$defs/operation",
            &operation
        )
        .is_err());
    }
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
